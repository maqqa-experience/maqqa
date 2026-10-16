use crate::models::{
    EMBED_MODEL_DEFAULT, GHOST_MODEL_DEFAULT, REASON_MODEL_FALLBACK, REASON_MODEL_PRIMARY,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VibeConfig {
    pub ollama_url: String,
    pub ghost_model: String,
    pub reason_model: String,
    pub reason_model_fallback: String,
    /// Optional reasoning model used only to diagnose test failures; advice is
    /// passed into the reason_model repair prompt. Empty = disabled.
    #[serde(default)]
    pub debug_model: String,
    pub embed_model: String,
    pub heal_max_attempts: u32,
    pub ghost_context_tokens: usize,
    pub reason_context_tokens: usize,
    pub architecture_context_tokens: usize,
    pub ghost_debounce_ms: u64,
    pub index_top_k: usize,
    /// Agentic observe→critique→act loops until prompt intent is met.
    #[serde(default = "default_agent_max_turns")]
    pub agent_max_turns: u32,
    /// Retries for a single Ollama generate call (network blips / empty responses).
    #[serde(default = "default_ollama_generate_retries")]
    pub ollama_generate_retries: u32,
    /// Agent playbook: `auto` (infer from reason_model), `small`, or `default`.
    /// Small models get stricter heal/DONE gates and more imperative prompts.
    #[serde(default = "default_agent_profile")]
    pub agent_profile: String,
    /// Ollama `options.num_ctx` for generate calls. `0` = use `reason_context_tokens`.
    /// Caps KV cache so models with huge defaults (e.g. 262k) do not allocate that window.
    #[serde(default)]
    pub ollama_num_ctx: usize,
    /// Sampling temperature for Ollama generate (default 0.2).
    #[serde(default = "default_ollama_temperature")]
    pub ollama_temperature: f32,
    /// Optional cooler/hotter temp for checkpoint-heal agent runs.
    /// `None` = when heal A/B is active, default cool temp; else use `ollama_temperature`.
    #[serde(default)]
    pub ollama_heal_temperature: Option<f32>,
    /// Checkpoint-heal A/B (base vs heal temperature). `None` = follow `agent_profile`:
    /// `small` → on, `default`/`large` → off, `auto` → on when `reason_model` is &lt;20B.
    /// Set `true`/`false` to force regardless of profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_heal_ab: Option<bool>,
}

fn default_agent_max_turns() -> u32 {
    8
}

fn default_ollama_generate_retries() -> u32 {
    3
}

fn default_agent_profile() -> String {
    "auto".into()
}

fn default_ollama_temperature() -> f32 {
    0.2
}

fn default_auto_heal_temperature() -> f32 {
    0.05
}

/// Parse a parameter-size tag like `14b` / `32B` from a model name.
pub fn infer_model_size_b(model: &str) -> Option<u32> {
    let lower = model.to_ascii_lowercase();
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(\d+)\s*b").expect("size re"));
    re.captures(&lower)
        .and_then(|c| c[1].parse::<u32>().ok())
        .filter(|n| *n > 0)
}

/// True when the model name looks ≤ ~10B (e.g. `7b`, `9B-instruct`).
/// Used for the small-agent playbook (stricter CLI/heal gates).
pub fn infer_small_model(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    // Explicit large-ish coder tags without a size often behave like big models.
    if lower.contains("coder-next") || lower.contains("32b") || lower.contains("70b") {
        return false;
    }
    infer_model_size_b(model).is_some_and(|n| n <= 10)
}

/// Auto heal-A/B band: models strictly under 20B (e.g. 7b/14b on, 30b/70b off).
pub fn infer_heal_ab_model(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    if lower.contains("coder-next") || lower.contains("32b") || lower.contains("70b") {
        return false;
    }
    infer_model_size_b(model).is_some_and(|n| n < 20)
}

impl Default for VibeConfig {
    fn default() -> Self {
        Self {
            ollama_url: "http://127.0.0.1:11434".to_string(),
            ghost_model: GHOST_MODEL_DEFAULT.to_string(),
            reason_model: REASON_MODEL_PRIMARY.to_string(),
            reason_model_fallback: REASON_MODEL_FALLBACK.to_string(),
            debug_model: String::new(),
            embed_model: EMBED_MODEL_DEFAULT.to_string(),
            heal_max_attempts: 3,
            ghost_context_tokens: 4_000,
            reason_context_tokens: 12_000,
            architecture_context_tokens: 24_000,
            ghost_debounce_ms: 80,
            index_top_k: 8,
            agent_max_turns: default_agent_max_turns(),
            ollama_generate_retries: default_ollama_generate_retries(),
            agent_profile: default_agent_profile(),
            ollama_num_ctx: 0,
            ollama_temperature: default_ollama_temperature(),
            ollama_heal_temperature: None,
            ollama_heal_ab: None,
        }
    }
}

impl VibeConfig {
    /// Whether heal A/B is wanted (manual override, else agent_profile / size).
    pub fn heal_ab_wanted(&self) -> bool {
        if let Some(v) = self.ollama_heal_ab {
            return v;
        }
        // Profile `small` means the <20B defaults band (heal A/B on).
        // `auto` still sizes from the model tag; default/large stay off.
        match self.agent_profile.to_ascii_lowercase().as_str() {
            "small" => true,
            "default" | "large" => false,
            _ => infer_heal_ab_model(&self.reason_model),
        }
    }

    /// Heal-branch temperature when A/B (or single cool heal) is active.
    pub fn effective_heal_temperature(&self) -> Option<f32> {
        if let Some(t) = self.ollama_heal_temperature {
            return Some(t.clamp(0.0, 2.0));
        }
        if self.heal_ab_wanted() {
            return Some(default_auto_heal_temperature());
        }
        None
    }

    /// True when checkpoint heals should A/B base vs heal temperature.
    pub fn heal_ab_enabled(&self) -> bool {
        if !self.heal_ab_wanted() {
            return false;
        }
        let heal = self
            .effective_heal_temperature()
            .unwrap_or(self.ollama_temperature);
        (heal - self.ollama_temperature).abs() > 1e-6
    }

    /// Whether to use the small-model playbook (stricter CLI/heal gates).
    pub fn is_small_agent(&self) -> bool {
        match self.agent_profile.to_ascii_lowercase().as_str() {
            "small" => true,
            "default" | "large" => false,
            // auto / unknown
            _ => infer_small_model(&self.reason_model),
        }
    }

    pub fn global_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".vibe")
            .join("config.toml")
    }

    pub fn load() -> Result<Self> {
        // Optional override for demos / CI: VIBE_CONFIG=/path/to/config.toml
        let path = std::env::var_os("VIBE_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(Self::global_path);
        if path.exists() {
            let raw =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let cfg: Self =
                toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
            Ok(cfg)
        } else if std::env::var_os("VIBE_CONFIG").is_some() {
            anyhow::bail!("VIBE_CONFIG set but file missing: {}", path.display());
        } else {
            let cfg = Self::default();
            cfg.save()?;
            Ok(cfg)
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::global_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self)?;
        fs::write(&path, raw)?;
        Ok(())
    }

    pub fn ensure_workspace_vibe_dir(workspace: &Path) -> Result<PathBuf> {
        let vibe = workspace.join(".vibe");
        fs::create_dir_all(vibe.join("shadow"))?;
        fs::create_dir_all(vibe.join("index.db"))?;
        fs::create_dir_all(vibe.join("logs"))?;
        Ok(vibe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heal_ab_auto_on_under_20b() {
        let mut cfg = VibeConfig::default();
        cfg.agent_profile = "auto".into();
        cfg.reason_model = "Qwen2.5-Coder:14b-instruct-q4_K_M".into();
        assert!(cfg.heal_ab_wanted());
        assert!(cfg.heal_ab_enabled());
        assert_eq!(cfg.effective_heal_temperature(), Some(0.05));

        cfg.reason_model = "qwen3-coder:30b".into();
        assert!(!cfg.heal_ab_wanted());
        assert!(!cfg.heal_ab_enabled());
    }

    #[test]
    fn heal_ab_follows_agent_profile_small() {
        let mut cfg = VibeConfig::default();
        cfg.agent_profile = "small".into();
        cfg.reason_model = "qwen3-coder:30b".into(); // large tag, but profile forces small defaults
        assert!(cfg.heal_ab_wanted());
        assert!(cfg.heal_ab_enabled());

        cfg.agent_profile = "default".into();
        cfg.reason_model = "Qwen2.5-Coder:14b-instruct-q4_K_M".into();
        assert!(!cfg.heal_ab_wanted());
    }

    #[test]
    fn heal_ab_manual_override() {
        let mut cfg = VibeConfig::default();
        cfg.agent_profile = "auto".into();
        cfg.reason_model = "Qwen2.5-Coder:14b-instruct-q4_K_M".into();
        cfg.ollama_heal_ab = Some(false);
        assert!(!cfg.heal_ab_wanted());
        assert!(!cfg.heal_ab_enabled());

        cfg.reason_model = "qwen3-coder:30b".into();
        cfg.ollama_heal_ab = Some(true);
        cfg.ollama_heal_temperature = Some(0.05);
        assert!(cfg.heal_ab_wanted());
        assert!(cfg.heal_ab_enabled());
    }

    #[test]
    fn heal_ab_disabled_when_temps_equal() {
        let mut cfg = VibeConfig::default();
        cfg.reason_model = "Qwen2.5-Coder:7b".into();
        cfg.ollama_heal_ab = Some(true);
        cfg.ollama_heal_temperature = Some(cfg.ollama_temperature);
        assert!(!cfg.heal_ab_enabled());
    }

    #[test]
    fn infer_heal_ab_size_band() {
        assert!(infer_heal_ab_model("Qwen2.5-Coder:14b-instruct-q4_K_M"));
        assert!(infer_heal_ab_model("qwen3:4b-instruct"));
        assert!(!infer_heal_ab_model("qwen3-coder:30b"));
        assert!(!infer_heal_ab_model("llama3.3:70b"));
        assert!(!infer_heal_ab_model("qwen3-coder-next:q4_K_M"));
    }
}
