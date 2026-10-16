//! Local ANN store under `.vibe/index.db` (embedded cosine index; LanceDB-compatible path).

use super::chunker::CodeChunk;
use super::embed::{cosine_similarity, estimate_tokens};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredChunk {
    pub id: String,
    pub file_path: String,
    pub symbol_name: String,
    pub chunk_type: String,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexDisk {
    chunks: Vec<StoredChunk>,
}

pub struct VectorStore {
    path: PathBuf,
    data: IndexDisk,
}

impl VectorStore {
    pub fn open(workspace: &Path) -> Result<Self> {
        let dir = workspace.join(".vibe").join("index.db");
        fs::create_dir_all(&dir)?;
        let path = dir.join("chunks.json");
        let data = if path.exists() {
            let raw = fs::read_to_string(&path).context("read index")?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            IndexDisk::default()
        };
        Ok(Self { path, data })
    }

    pub fn len(&self) -> usize {
        self.data.chunks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.chunks.is_empty()
    }

    pub fn persist(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(&self.data)?;
        fs::write(&self.path, raw)?;
        Ok(())
    }

    pub fn delete_by_file(&mut self, file_path: &str) {
        self.data
            .chunks
            .retain(|c| c.file_path != file_path && !c.file_path.ends_with(file_path));
    }

    pub fn upsert_chunks(&mut self, file_path: &str, chunks: Vec<StoredChunk>) {
        self.delete_by_file(file_path);
        self.data.chunks.extend(chunks);
    }

    pub fn query(&self, query_embedding: &[f32], top_k: usize) -> Vec<(StoredChunk, f32)> {
        let mut scored: Vec<_> = self
            .data
            .chunks
            .iter()
            .map(|c| {
                let score = cosine_similarity(query_embedding, &c.embedding);
                (c.clone(), score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(top_k).collect()
    }

    pub fn query_with_budget(
        &self,
        query_embedding: &[f32],
        top_k: usize,
        token_budget: usize,
    ) -> Vec<StoredChunk> {
        let mut used = 0usize;
        let mut out = Vec::new();
        for (chunk, _score) in self.query(query_embedding, top_k.saturating_mul(3)) {
            let cost = estimate_tokens(&chunk.content);
            if used + cost > token_budget && !out.is_empty() {
                break;
            }
            used += cost;
            out.push(chunk);
            if out.len() >= top_k {
                break;
            }
        }
        out
    }
}

pub fn stored_from_chunk(chunk: CodeChunk, embedding: Vec<f32>) -> StoredChunk {
    let id = format!(
        "{}:{}:{}:{}",
        chunk.file_path, chunk.symbol_name, chunk.start_line, chunk.end_line
    );
    StoredChunk {
        id,
        file_path: chunk.file_path,
        symbol_name: chunk.symbol_name,
        chunk_type: chunk.chunk_type,
        content: chunk.content,
        start_line: chunk.start_line,
        end_line: chunk.end_line,
        embedding,
    }
}
