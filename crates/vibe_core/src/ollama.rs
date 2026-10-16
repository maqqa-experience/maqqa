use crate::config::VibeConfig;
use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{info, warn};

#[derive(Clone)]
pub struct OllamaClient {
    client: Client,
    base: String,
    /// Cap KV cache / context window. Without this, Ollama uses the model default
    /// (e.g. 262k for qwen3-coder:30b) even when prompts are a few thousand tokens.
    num_ctx: i32,
    temperature: f32,
    heal_temperature: Option<f32>,
    /// When true, generate uses `heal_temperature` (if set).
    heal_mode: Arc<AtomicBool>,
    /// Optional per-call override (A/B heal branches). Wins over heal_mode when set.
    temp_override: Arc<Mutex<Option<f32>>>,
}

#[derive(Debug, Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
    options: Option<GenerateOptions>,
}

#[derive(Debug, Serialize)]
struct GenerateOptions {
    num_predict: Option<i32>,
    temperature: Option<f32>,
    num_ctx: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct GenerateResponse {
    response: Option<String>,
    done: Option<bool>,
}

#[derive(Debug, Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    prompt: &'a str,
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Debug, Deserialize)]
struct TagModel {
    name: String,
}

impl OllamaClient {
    pub fn new(cfg: &VibeConfig) -> Self {
        let client = Client::builder()
            // Long agent repairs can take minutes; fail eventually so retries can run.
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| Client::new());
        // Prefer explicit ollama_num_ctx; else reason_context_tokens (retrieval budget).
        let raw = if cfg.ollama_num_ctx > 0 {
            cfg.ollama_num_ctx
        } else {
            cfg.reason_context_tokens
        };
        let num_ctx = raw.clamp(2_048, 131_072) as i32;
        let temperature = cfg.ollama_temperature.clamp(0.0, 2.0);
        let heal_temperature = cfg.effective_heal_temperature();
        eprintln!(
            "[vibe] ollama num_ctx={num_ctx} temperature={temperature} heal_temperature={heal_temperature:?} heal_ab={} (wanted={} auto_under_20b={})",
            cfg.heal_ab_enabled(),
            cfg.heal_ab_wanted(),
            crate::config::infer_heal_ab_model(&cfg.reason_model),
        );
        Self {
            client,
            base: cfg.ollama_url.trim_end_matches('/').to_string(),
            num_ctx,
            temperature,
            heal_temperature,
            heal_mode: Arc::new(AtomicBool::new(false)),
            temp_override: Arc::new(Mutex::new(None)),
        }
    }

    /// Use heal sampling for subsequent generate calls (checkpoint repair).
    pub fn set_heal_mode(&self, on: bool) {
        self.heal_mode.store(on, Ordering::Relaxed);
    }

    fn set_temperature_override(&self, temp: Option<f32>) {
        if let Ok(mut g) = self.temp_override.lock() {
            *g = temp.map(|t| t.clamp(0.0, 2.0));
        }
    }

    fn active_temperature(&self) -> f32 {
        if let Ok(g) = self.temp_override.lock() {
            if let Some(t) = *g {
                return t;
            }
        }
        if self.heal_mode.load(Ordering::Relaxed) {
            self.heal_temperature.unwrap_or(self.temperature)
        } else {
            self.temperature
        }
    }

    /// Clears heal mode when dropped (safe across early returns).
    pub fn heal_mode_guard(&self, on: bool) -> HealModeGuard<'_> {
        self.set_heal_mode(on);
        HealModeGuard { client: self }
    }

    /// Force sampling temperature until the guard drops (A/B heal branches).
    pub fn temperature_override_guard(&self, temperature: f32) -> TempOverrideGuard<'_> {
        self.set_temperature_override(Some(temperature));
        TempOverrideGuard { client: self }
    }

    pub fn base_temperature(&self) -> f32 {
        self.temperature
    }

    pub fn configured_heal_temperature(&self) -> Option<f32> {
        self.heal_temperature
    }

    pub async fn health(&self) -> bool {
        self.client
            .get(format!("{}/api/tags", self.base))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        let res = self
            .client
            .get(format!("{}/api/tags", self.base))
            .send()
            .await
            .context("ollama /api/tags")?
            .error_for_status()?
            .json::<TagsResponse>()
            .await?;
        Ok(res.models.into_iter().map(|m| m.name).collect())
    }

    pub async fn resolve_reason_model(&self, cfg: &VibeConfig) -> String {
        let models = self.list_models().await.unwrap_or_default();
        if models.iter().any(|m| {
            m == &cfg.reason_model
                || m.starts_with(&format!(
                    "{}:",
                    cfg.reason_model.split(':').next().unwrap_or("")
                ))
        }) || models.iter().any(|m| m.contains("deepseek-coder-v2"))
        {
            if models.iter().any(|m| m == &cfg.reason_model) {
                return cfg.reason_model.clone();
            }
            if let Some(m) = models.iter().find(|m| m.contains("deepseek-coder-v2")) {
                return m.clone();
            }
        }
        if models.iter().any(|m| m == &cfg.reason_model_fallback) {
            warn!(
                "primary reason model unavailable; using fallback {}",
                cfg.reason_model_fallback
            );
            return cfg.reason_model_fallback.clone();
        }
        // Prefer any installed large instruct model, else configured primary.
        models
            .into_iter()
            .find(|m| m.contains("70b") || m.contains("llama3"))
            .unwrap_or_else(|| cfg.reason_model_fallback.clone())
    }

    /// Resolve optional debug/diagnose model. `None` if unset or not installed.
    pub async fn resolve_debug_model(&self, cfg: &VibeConfig) -> Option<String> {
        let want = cfg.debug_model.trim();
        if want.is_empty() {
            return None;
        }
        let models = self.list_models().await.unwrap_or_default();
        if models.iter().any(|m| m == want) {
            return Some(want.to_string());
        }
        let prefix = want.split(':').next().unwrap_or(want);
        if let Some(m) = models.iter().find(|m| m.starts_with(&format!("{prefix}:"))) {
            warn!(configured = want, using = %m, "debug_model tag not exact; using prefix match");
            return Some(m.clone());
        }
        warn!(
            model = want,
            "debug_model configured but not installed; debug diagnose disabled"
        );
        None
    }

    pub async fn generate(
        &self,
        model: &str,
        prompt: &str,
        max_tokens: Option<i32>,
    ) -> Result<String> {
        let temperature = self.active_temperature();
        let body = GenerateRequest {
            model,
            prompt,
            stream: false,
            options: Some(GenerateOptions {
                num_predict: max_tokens,
                temperature: Some(temperature),
                num_ctx: Some(self.num_ctx),
            }),
        };
        let start = Instant::now();
        info!(model, prompt_chars = prompt.len(), "ollama generate start");
        eprintln!(
            "[vibe] ollama generate start model={model} prompt_chars={} num_ctx={} temperature={temperature}",
            prompt.len(),
            self.num_ctx
        );
        let res = self
            .client
            .post(format!("{}/api/generate", self.base))
            .json(&body)
            .send()
            .await
            .context("ollama generate")?
            .error_for_status()
            .context("ollama generate HTTP error")?
            .json::<GenerateResponse>()
            .await
            .context("ollama generate decode")?;
        let elapsed_ms = start.elapsed().as_millis() as u64;
        info!(model, elapsed_ms, "generate complete");
        eprintln!("[vibe] ollama generate done model={model} elapsed_ms={elapsed_ms}");
        res.response.ok_or_else(|| anyhow!("empty ollama response"))
    }

    /// Retry transient Ollama failures (ngrok blips, empty body, brief 5xx).
    pub async fn generate_with_retries(
        &self,
        model: &str,
        prompt: &str,
        max_tokens: Option<i32>,
        attempts: u32,
    ) -> Result<String> {
        let attempts = attempts.max(1);
        let mut last_err: Option<anyhow::Error> = None;
        for i in 1..=attempts {
            match self.generate(model, prompt, max_tokens).await {
                Ok(text) => return Ok(text),
                Err(e) => {
                    warn!(attempt = i, attempts, error = %e, "ollama generate failed");
                    eprintln!("[vibe] ollama generate failed attempt {i}/{attempts}: {e:#}");
                    last_err = Some(e);
                    if i < attempts {
                        let backoff_ms = 750u64.saturating_mul(i as u64);
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("ollama generate failed")))
    }

    /// Stream tokens; returns first-token latency and full text.
    pub async fn generate_stream<F>(
        &self,
        model: &str,
        prompt: &str,
        max_tokens: Option<i32>,
        mut on_token: F,
    ) -> Result<(String, u64)>
    where
        F: FnMut(&str) -> Result<()>,
    {
        // Stream stays slightly cooler than chat generate unless heal/override is active.
        let temperature = {
            let override_on = self.temp_override.lock().ok().and_then(|g| *g).is_some();
            if override_on || self.heal_mode.load(Ordering::Relaxed) {
                self.active_temperature()
            } else {
                (self.temperature * 0.5).clamp(0.0, 2.0)
            }
        };
        let body = GenerateRequest {
            model,
            prompt,
            stream: true,
            options: Some(GenerateOptions {
                num_predict: max_tokens,
                temperature: Some(temperature),
                num_ctx: Some(self.num_ctx),
            }),
        };
        let start = Instant::now();
        let mut first_token_ms = 0u64;
        let mut saw_first = false;
        let mut full = String::new();

        let response = self
            .client
            .post(format!("{}/api/generate", self.base))
            .json(&body)
            .send()
            .await
            .context("ollama generate stream")?
            .error_for_status()?;

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("stream chunk")?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer = buffer[pos + 1..].to_string();
                if line.is_empty() {
                    continue;
                }
                let parsed: GenerateResponse = serde_json::from_str(&line)
                    .with_context(|| format!("parse stream line: {line}"))?;
                if let Some(tok) = parsed.response {
                    if !saw_first && !tok.is_empty() {
                        first_token_ms = start.elapsed().as_millis() as u64;
                        saw_first = true;
                    }
                    full.push_str(&tok);
                    on_token(&tok)?;
                }
                if parsed.done.unwrap_or(false) {
                    return Ok((full, first_token_ms));
                }
            }
        }
        Ok((full, first_token_ms))
    }

    pub async fn embed(&self, model: &str, text: &str) -> Result<Vec<f32>> {
        let body = EmbedRequest {
            model,
            prompt: text,
        };
        let res = self
            .client
            .post(format!("{}/api/embeddings", self.base))
            .json(&body)
            .send()
            .await
            .context("ollama embeddings")?
            .error_for_status()?
            .json::<EmbedResponse>()
            .await?;
        Ok(res.embedding)
    }

    /// Warm both models concurrently (smoke / keep-resident).
    pub async fn warm_dual(&self, ghost: &str, reason: &str) -> Result<()> {
        let a = self.generate(ghost, "reply with ok", Some(8));
        let b = self.generate(reason, "reply with ok", Some(8));
        let (ra, rb) = tokio::join!(a, b);
        ra.context("warm ghost")?;
        rb.context("warm reason")?;
        Ok(())
    }
}

/// RAII clear of [`OllamaClient::set_heal_mode`].
pub struct HealModeGuard<'a> {
    client: &'a OllamaClient,
}

impl Drop for HealModeGuard<'_> {
    fn drop(&mut self) {
        self.client.set_heal_mode(false);
    }
}

/// RAII clear of temperature override.
pub struct TempOverrideGuard<'a> {
    client: &'a OllamaClient,
}

impl Drop for TempOverrideGuard<'_> {
    fn drop(&mut self) {
        self.client.set_temperature_override(None);
    }
}

/// Strip markdown fences from model file output.
pub fn strip_code_fences(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.find('\n').map(|i| &rest[i + 1..]).unwrap_or(rest);
        if let Some(end) = rest.rfind("```") {
            return rest[..end].trim().to_string();
        }
    }
    trimmed.to_string()
}
