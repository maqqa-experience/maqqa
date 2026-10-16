use crate::config::VibeConfig;
use crate::indexer::Indexer;
use crate::logutil;
use crate::mcp::LocalMcp;
use crate::ollama::OllamaClient;
use crate::router::IntentRouter;
use crate::shadow::ShadowWorkspace;
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use vibe_ipc::*;

pub struct Engine {
    pub cfg: VibeConfig,
    pub ollama: OllamaClient,
    pub router: IntentRouter,
    workspace: Mutex<Option<PathBuf>>,
    indexer: Mutex<Option<Indexer>>,
    last_heal: Mutex<Option<HealStatus>>,
    reason_model: String,
}

impl Engine {
    pub async fn bootstrap() -> Result<Arc<Self>> {
        logutil::init_tracing();
        let cfg = VibeConfig::load()?;
        let ollama = OllamaClient::new(&cfg);
        let reason_model = ollama.resolve_reason_model(&cfg).await;
        let router = IntentRouter::new(cfg.clone(), ollama.clone(), reason_model.clone());
        Ok(Arc::new(Self {
            cfg,
            ollama,
            router,
            workspace: Mutex::new(None),
            indexer: Mutex::new(None),
            last_heal: Mutex::new(None),
            reason_model,
        }))
    }

    pub async fn workspace_path(&self) -> Option<PathBuf> {
        self.workspace.lock().await.clone()
    }

    pub async fn open_workspace(&self, path: &str) -> Result<OpenWorkspaceResponse> {
        let root = PathBuf::from(path)
            .canonicalize()
            .with_context(|| format!("open workspace {path}"))?;
        if !root.is_dir() {
            return Err(anyhow!("not a directory: {path}"));
        }
        VibeConfig::ensure_workspace_vibe_dir(&root)?;
        logutil::append_local_log(&root, &format!("open_workspace {}", root.display()))?;

        let indexer = Indexer::open(&root, self.ollama.clone(), &self.cfg)
            .await
            .ok();
        *self.indexer.lock().await = indexer;
        *self.workspace.lock().await = Some(root.clone());

        let files = list_files(&root, 2000);
        Ok(OpenWorkspaceResponse {
            path: root.to_string_lossy().to_string(),
            files,
        })
    }

    pub async fn read_file(&self, path: &str) -> Result<ReadFileResponse> {
        let abs = self.resolve_path(path).await?;
        let content = fs::read_to_string(&abs)?;
        Ok(ReadFileResponse {
            path: path.to_string(),
            content,
        })
    }

    pub async fn write_file(&self, path: &str, content: &str) -> Result<WriteFileResponse> {
        let abs = self.resolve_path(path).await?;
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&abs, content)?;
        Ok(WriteFileResponse {
            path: path.to_string(),
            bytes_written: content.len(),
        })
    }

    async fn refresh_index(&self) {
        let paths = {
            let mut guard = self.indexer.lock().await;
            guard
                .as_mut()
                .map(|idx| idx.poll_watcher())
                .unwrap_or_default()
        };
        for p in paths {
            if p.is_file() {
                let mut guard = self.indexer.lock().await;
                if let Some(idx) = guard.as_mut() {
                    let _ = idx.index_file(&p).await;
                }
            }
        }
    }

    async fn retrieve_context(&self, prompt: &str, active: Option<&str>, budget: usize) -> String {
        let mut guard = self.indexer.lock().await;
        if let Some(idx) = guard.as_mut() {
            match idx.query_context(prompt, active, budget).await {
                Ok(chunks) => Indexer::format_context(&chunks),
                Err(_) => String::new(),
            }
        } else {
            String::new()
        }
    }

    pub async fn get_completions(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        if !self.cfg.ghost_enabled() {
            return Ok(CompletionResponse {
                request_id: req.request_id,
                text: String::new(),
                cancelled: false,
            });
        }
        self.refresh_index().await;
        let context = self
            .retrieve_context(&req.prefix, Some(&req.path), self.cfg.ghost_context_tokens)
            .await;
        let (text, cancelled) = self
            .router
            .ghost_complete_with_context(
                &req.path,
                &req.content,
                &req.prefix,
                &req.request_id,
                &context,
            )
            .await?;
        Ok(CompletionResponse {
            request_id: req.request_id,
            text,
            cancelled,
        })
    }

    pub fn cancel_completion(&self, request_id: &str) {
        self.router.cancel(request_id);
    }

    pub async fn run_edit(&self, req: EditRequest) -> Result<EditResponse> {
        let ws = self
            .workspace_path()
            .await
            .ok_or_else(|| anyhow!("no workspace open"))?;
        self.refresh_index().await;

        let budget = match req.intent {
            EditIntent::Inline => self.cfg.ghost_context_tokens,
            EditIntent::Selection | EditIntent::Create => self.cfg.reason_context_tokens,
            EditIntent::Architecture => self.cfg.architecture_context_tokens,
        };
        // Greenfield create: skip RAG so we don't drag unrelated existing files into the prompt.
        let context = if matches!(req.intent, EditIntent::Create)
            || req.path.trim().is_empty()
            || req.path.trim() == "."
        {
            "(greenfield — no retrieved context)".to_string()
        } else {
            self.retrieve_context(&req.prompt, Some(&req.path), budget)
                .await
        };

        let (resp, status) = self
            .router
            .run_edit_with_context(
                &ws,
                &req.path,
                &req.content,
                req.selection.as_deref(),
                &req.prompt,
                req.intent,
                &context,
            )
            .await?;
        *self.last_heal.lock().await = Some(status);
        logutil::append_local_log(
            &ws,
            &format!(
                "run_edit path={} healed={} attempts={}",
                resp.path, resp.healed, resp.attempts
            ),
        )?;
        Ok(resp)
    }

    pub async fn apply_edit(&self, req: ApplyEditRequest) -> Result<ApplyEditResponse> {
        let ws = self
            .workspace_path()
            .await
            .ok_or_else(|| anyhow!("no workspace open"))?;
        let shadow = ShadowWorkspace::new(&ws)?;
        let mut paths = Vec::new();
        if let Some(files) = &req.files {
            for f in files {
                shadow.apply_to_origin(&f.path, &f.content)?;
                paths.push(f.path.clone());
            }
        } else if let (Some(path), Some(content)) = (&req.path, &req.content) {
            shadow.apply_to_origin(path, content)?;
            paths.push(path.clone());
        } else {
            return Err(anyhow!("apply_edit requires files or path+content"));
        }
        // Refresh index for written files.
        {
            let mut guard = self.indexer.lock().await;
            if let Some(idx) = guard.as_mut() {
                for p in &paths {
                    let abs = ws.join(p);
                    let _ = idx.index_file(&abs).await;
                }
            }
        }
        Ok(ApplyEditResponse {
            paths,
            applied: true,
        })
    }

    pub async fn status(&self) -> StatusResponse {
        let ollama_ok = self.ollama.health().await;
        let guard = self.indexer.lock().await;
        let index_chunks = guard.as_ref().map(|i| i.chunk_count()).unwrap_or(0);
        let index_ready = guard.as_ref().map(|i| i.is_ready()).unwrap_or(false);
        drop(guard);
        StatusResponse {
            workspace: self
                .workspace_path()
                .await
                .map(|p| p.to_string_lossy().to_string()),
            ghost_model: if self.cfg.ghost_enabled() {
                self.cfg.ghost_model.clone()
            } else {
                String::new()
            },
            reason_model: self.reason_model.clone(),
            index_chunks,
            index_ready,
            last_heal: self.last_heal.lock().await.clone(),
            ollama_ok,
        }
    }

    pub async fn mcp_call(&self, tool: &str, args: serde_json::Value) -> Result<McpToolResult> {
        let ws = self
            .workspace_path()
            .await
            .ok_or_else(|| anyhow!("no workspace open"))?;
        Ok(LocalMcp::new(ws).call(tool, &args))
    }

    async fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            return Ok(p);
        }
        let ws = self
            .workspace_path()
            .await
            .ok_or_else(|| anyhow!("no workspace open"))?;
        Ok(ws.join(path))
    }
}

fn list_files(root: &Path, limit: usize) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                "node_modules" | "target" | ".git" | ".vibe" | "dist" | "build"
            )
        })
        .build();
    for entry in walker.flatten() {
        if out.len() >= limit {
            break;
        }
        let path = entry.path();
        if path == root {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir {
            out.push(FileEntry {
                path: rel,
                is_dir: false,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}
