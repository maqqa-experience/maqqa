pub mod chunker;
pub mod embed;
pub mod store;
pub mod watch;

use crate::config::VibeConfig;
use crate::ollama::OllamaClient;
use anyhow::{Context, Result};
use chunker::{chunk_source_file, language_for_path, CodeChunk};
use embed::Embedder;
use ignore::WalkBuilder;
use std::fs;
use std::path::{Path, PathBuf};
use store::{stored_from_chunk, VectorStore};
use tracing::{info, warn};
use vibe_ipc::CodeChunkDto;
use watch::WorkspaceWatcher;

pub struct Indexer {
    workspace: PathBuf,
    store: VectorStore,
    embedder: Embedder,
    watcher: Option<WorkspaceWatcher>,
    cfg: VibeConfig,
}

impl Indexer {
    pub async fn open(workspace: &Path, ollama: OllamaClient, cfg: &VibeConfig) -> Result<Self> {
        VibeConfig::ensure_workspace_vibe_dir(workspace)?;
        let store = VectorStore::open(workspace)?;
        let embedder = Embedder::new(ollama, cfg.embed_model.clone());
        let watcher = WorkspaceWatcher::start(workspace).ok();
        let mut idx = Self {
            workspace: workspace.to_path_buf(),
            store,
            embedder,
            watcher,
            cfg: cfg.clone(),
        };
        if idx.store.is_empty() {
            idx.reindex_all().await?;
        }
        Ok(idx)
    }

    pub fn chunk_count(&self) -> usize {
        self.store.len()
    }

    pub fn is_ready(&self) -> bool {
        !self.store.is_empty()
    }

    pub async fn reindex_all(&mut self) -> Result<()> {
        info!("full reindex starting");
        let mut files = Vec::new();
        let walker = WalkBuilder::new(&self.workspace)
            .hidden(false)
            .git_ignore(true)
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !matches!(
                    name.as_ref(),
                    "node_modules" | "target" | ".git" | ".vibe" | "dist" | "build" | ".venv"
                )
            })
            .build();
        for entry in walker.flatten() {
            let path = entry.path();
            if path.is_file() && language_for_path(path).is_some() {
                files.push(path.to_path_buf());
            }
        }
        for file in files {
            if let Err(e) = self.index_file(&file).await {
                warn!(error = %e, path = %file.display(), "index file failed");
            }
        }
        self.store.persist()?;
        info!(chunks = self.store.len(), "full reindex complete");
        Ok(())
    }

    pub async fn index_file(&mut self, path: &Path) -> Result<()> {
        let rel = path
            .strip_prefix(&self.workspace)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let Some(lang) = language_for_path(path) else {
            return Ok(());
        };
        let source =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let chunks = chunk_source_file(&rel, &source, lang);
        let mut stored = Vec::new();
        for chunk in chunks {
            let emb = self
                .embedder
                .embed_one(&format!(
                    "{} {}\n{}",
                    chunk.symbol_name, chunk.chunk_type, chunk.content
                ))
                .await?;
            stored.push(stored_from_chunk(chunk, emb));
        }
        self.store.upsert_chunks(&rel, stored);
        self.store.persist()?;
        Ok(())
    }

    pub fn poll_watcher(&mut self) -> Vec<PathBuf> {
        self.watcher.as_ref().map(|w| w.drain()).unwrap_or_default()
    }

    pub async fn apply_watcher_events(&mut self) -> Result<()> {
        let paths = self.poll_watcher();
        for p in paths {
            if p.is_file() {
                let _ = self.index_file(&p).await;
            }
        }
        Ok(())
    }

    pub async fn query_context(
        &self,
        prompt: &str,
        _active_file: Option<&str>,
        token_budget: usize,
    ) -> Result<Vec<CodeChunk>> {
        let emb = self.embedder.embed_one(prompt).await?;
        let stored = self
            .store
            .query_with_budget(&emb, self.cfg.index_top_k, token_budget);
        Ok(stored
            .into_iter()
            .map(|s| CodeChunk {
                file_path: s.file_path,
                symbol_name: s.symbol_name,
                chunk_type: s.chunk_type,
                content: s.content,
                start_line: s.start_line,
                end_line: s.end_line,
            })
            .collect())
    }

    pub fn format_context(chunks: &[CodeChunk]) -> String {
        if chunks.is_empty() {
            return "(no retrieved context)".to_string();
        }
        chunks
            .iter()
            .map(|c| {
                format!(
                    "// {}:{}-{} {}\n{}",
                    c.file_path, c.start_line, c.end_line, c.symbol_name, c.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn to_dto(chunks: &[CodeChunk]) -> Vec<CodeChunkDto> {
        chunks
            .iter()
            .map(|c| CodeChunkDto {
                file_path: c.file_path.clone(),
                symbol_name: c.symbol_name.clone(),
                chunk_type: c.chunk_type.clone(),
                content: c.content.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
            })
            .collect()
    }
}
