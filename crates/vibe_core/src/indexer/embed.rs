use crate::ollama::OllamaClient;
use anyhow::Result;

pub struct Embedder {
    client: OllamaClient,
    model: String,
}

impl Embedder {
    pub fn new(client: OllamaClient, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }

    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        // Truncate extremely long chunks for embed model context.
        let clipped = if text.len() > 6000 {
            &text[..6000]
        } else {
            text
        };
        self.client.embed(&self.model, clipped).await
    }

    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed_one(t).await?);
        }
        Ok(out)
    }
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Rough token estimate (~4 chars/token).
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}
