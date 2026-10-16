use crate::agent::AgentLoop;
use crate::config::VibeConfig;
use crate::mcp::LocalMcp;
use crate::multifile::parse_multifile_output;
use crate::ollama::OllamaClient;
use crate::shadow::{build_create_prompt, build_edit_prompt, ShadowWorkspace};
use anyhow::Result;
use chrono::Utc;
use dashmap::DashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use vibe_ipc::{EditIntent, EditResponse, FileChange, HealStatus};

pub struct IntentRouter {
    pub cfg: VibeConfig,
    pub ollama: OllamaClient,
    pub reason_model: String,
    cancel_flags: DashMap<String, Arc<AtomicBool>>,
}

impl IntentRouter {
    pub fn new(cfg: VibeConfig, ollama: OllamaClient, reason_model: String) -> Self {
        Self {
            cfg,
            ollama,
            reason_model,
            cancel_flags: DashMap::new(),
        }
    }

    pub fn cancel(&self, request_id: &str) {
        if let Some(flag) = self.cancel_flags.get(request_id) {
            flag.store(true, Ordering::SeqCst);
        }
    }

    pub fn register_cancel(&self, request_id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.cancel_flags
            .insert(request_id.to_string(), flag.clone());
        flag
    }

    pub fn clear_cancel(&self, request_id: &str) {
        self.cancel_flags.remove(request_id);
    }

    pub async fn ghost_complete_with_context(
        &self,
        path: &str,
        content: &str,
        prefix: &str,
        request_id: &str,
        context: &str,
    ) -> Result<(String, bool)> {
        let flag = self.register_cancel(request_id);
        let prompt = format!(
            "You are a code completion engine. Continue the code at the cursor. Output only the completion suffix, no markdown.\n\nFile: {path}\n\nContext:\n{context}\n\nCode before cursor:\n{prefix}\n"
        );

        if flag.load(Ordering::SeqCst) {
            self.clear_cancel(request_id);
            return Ok((String::new(), true));
        }

        let text = self
            .ollama
            .generate(&self.cfg.ghost_model, &prompt, Some(64))
            .await
            .unwrap_or_default();

        let cancelled = flag.load(Ordering::SeqCst);
        self.clear_cancel(request_id);
        if cancelled {
            return Ok((String::new(), true));
        }
        let cleaned = text
            .trim()
            .trim_start_matches("```")
            .lines()
            .take(8)
            .collect::<Vec<_>>()
            .join("\n");
        let _ = content;
        Ok((cleaned, false))
    }

    pub async fn run_edit_with_context(
        &self,
        workspace: &Path,
        path: &str,
        content: &str,
        selection: Option<&str>,
        user_prompt: &str,
        intent: EditIntent,
        context: &str,
    ) -> Result<(EditResponse, HealStatus)> {
        let hint = {
            let p = path.trim();
            if p.is_empty() || p == "." {
                ""
            } else {
                p
            }
        };

        // Create intent = greenfield: ignore whether a file with that hint already exists.
        let creating = matches!(intent, EditIntent::Create)
            || content.trim().is_empty()
            || hint.is_empty()
            || (!hint.is_empty() && !workspace.join(hint).exists());

        let wants_multi = matches!(intent, EditIntent::Create | EditIntent::Architecture)
            || hint.is_empty()
            || multi_file_request(user_prompt)
            || creating;

        let mcp = LocalMcp::new(workspace);
        let mcp_hint = if matches!(intent, EditIntent::Architecture) || wants_multi {
            let status = mcp.call("git_status", &serde_json::json!({}));
            let listed = mcp.call("list_dir", &serde_json::json!({ "path": "." }));
            format!(
                "\n### Workspace Tools\n{}\n\ngit_status:\n{}\n\nlist_dir:\n{}\n",
                LocalMcp::tool_catalog(),
                status.output,
                listed.output
            )
        } else {
            String::new()
        };

        let existing_note = if creating {
            "GREENFIELD CREATE: Do not reuse or rewrite unrelated existing workspace files. Invent a sensible new file/dir layout that fulfills the request on its own.".to_string()
        } else {
            format!(
                "Primary open file `{hint}` currently contains:\n```\n{content}\n```\n{}",
                selection
                    .map(|s| format!("Selection:\n```\n{s}\n```"))
                    .unwrap_or_default()
            )
        };

        let must_heal = !matches!(intent, EditIntent::Inline);
        let shadow = ShadowWorkspace::new(workspace)?;
        shadow.sync_from_origin()?;

        let kind = ShadowWorkspace::detect_kind(shadow.shadow_dir());
        let project_verify = !matches!(kind, crate::shadow::ProjectKind::Unknown);

        let mut agent_summary: Option<String> = None;
        let mut agent_log: Vec<String> = Vec::new();

        let (files, attempts, diagnostics, healed, error) = if wants_multi {
            let fallback = if hint.is_empty() { "README.md" } else { hint };
            // Keep the agent focused on the user prompt — tool dumps make small models thrash.
            let goal = if creating {
                format!("{user_prompt}\n\n### Mode notes:\n{existing_note}")
            } else {
                format!(
                    "{user_prompt}\n\n### Notes:\n{existing_note}\n\n### Context (trimmed):\n{}",
                    context.chars().take(2000).collect::<String>()
                )
            };
            let _ = (project_verify, mcp_hint);
            eprintln!(
                "[vibe] starting agentic loop (max_turns={})",
                self.cfg.agent_max_turns
            );
            let debug = self.ollama.resolve_debug_model(&self.cfg).await;
            if let Some(ref d) = debug {
                eprintln!("[vibe] debug_model={d}");
            }
            let agent = AgentLoop {
                cfg: &self.cfg,
                ollama: &self.ollama,
                reason_model: &self.reason_model,
                debug_model: debug.as_deref(),
            };
            let heal = agent
                .run(&goal, workspace, &shadow, fallback, creating)
                .await?;
            agent_summary = heal.summary.clone();
            agent_log = heal.log.clone();
            if let Some(ref s) = agent_summary {
                eprintln!("[vibe] agent summary:\n{s}");
            }
            let error = if heal.success {
                None
            } else {
                Some(
                    heal.diagnostics
                        .clone()
                        .unwrap_or_else(|| "agent did not reach DONE".into()),
                )
            };
            (
                heal.files,
                heal.attempts,
                heal.diagnostics,
                heal.success,
                error,
            )
        } else if creating {
            // Single-file create path (rare): still allow multifile parse if model emits it.
            let target = if hint.is_empty() { "main.py" } else { hint };
            let base_prompt = format!(
                "{}{mcp_hint}",
                build_create_prompt(target, user_prompt, context)
            );
            let heal = shadow
                .auto_heal_loop_single_file(
                    target,
                    &base_prompt,
                    &self.ollama,
                    &self.cfg,
                    &self.reason_model,
                )
                .await?;
            let parsed = parse_multifile_output(&heal.content, target);
            let files = if parsed.is_empty() {
                vec![crate::multifile::ParsedFile {
                    path: target.to_string(),
                    content: heal.content.clone(),
                }]
            } else {
                parsed
            };
            let error = if heal.success {
                None
            } else {
                Some(
                    heal.diagnostics
                        .clone()
                        .unwrap_or_else(|| "heal failed after max attempts".into()),
                )
            };
            (files, heal.attempts, heal.diagnostics, heal.success, error)
        } else {
            let base_prompt = format!(
                "{}{mcp_hint}",
                build_edit_prompt(hint, content, selection, user_prompt, context)
            );
            shadow.stage_file_change(hint, content)?;
            let use_single = matches!(
                Path::new(hint)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or(""),
                "py" | "js" | "mjs" | "cjs"
            );
            if must_heal && (use_single || project_verify) {
                let heal = if use_single {
                    shadow
                        .auto_heal_loop_single_file(
                            hint,
                            &base_prompt,
                            &self.ollama,
                            &self.cfg,
                            &self.reason_model,
                        )
                        .await?
                } else {
                    shadow
                        .auto_heal_loop(
                            hint,
                            &base_prompt,
                            &self.ollama,
                            &self.cfg,
                            &self.reason_model,
                        )
                        .await?
                };
                let parsed = parse_multifile_output(&heal.content, hint);
                let files = if parsed.len() > 1 {
                    parsed
                } else {
                    vec![crate::multifile::ParsedFile {
                        path: hint.to_string(),
                        content: heal.content.clone(),
                    }]
                };
                let error = if heal.success {
                    None
                } else {
                    Some(
                        heal.diagnostics
                            .clone()
                            .unwrap_or_else(|| "heal failed after max attempts".into()),
                    )
                };
                (files, heal.attempts, heal.diagnostics, heal.success, error)
            } else {
                let raw = self
                    .ollama
                    .generate(&self.reason_model, &base_prompt, Some(8192))
                    .await?;
                let files = parse_multifile_output(&raw, hint);
                (files, 1, None, false, None)
            }
        };

        let changes: Vec<FileChange> = files
            .iter()
            .map(|f| {
                let original = fs::read_to_string(workspace.join(&f.path)).unwrap_or_default();
                let diff = ShadowWorkspace::unified_diff(&original, &f.content, &f.path);
                FileChange {
                    path: f.path.clone(),
                    original,
                    proposed: f.content.clone(),
                    diff,
                }
            })
            .collect();

        let primary = changes.first().cloned().unwrap_or(FileChange {
            path: if hint.is_empty() {
                "main.py".into()
            } else {
                hint.to_string()
            },
            original: String::new(),
            proposed: String::new(),
            diff: String::new(),
        });

        let combined_diff = changes
            .iter()
            .map(|c| c.diff.clone())
            .collect::<Vec<_>>()
            .join("\n");

        let resp = EditResponse {
            path: primary.path.clone(),
            original: primary.original.clone(),
            proposed: primary.proposed.clone(),
            diff: combined_diff,
            files: changes,
            healed,
            attempts,
            diagnostics: diagnostics.clone(),
            error: error.clone(),
            summary: agent_summary.clone(),
            log: agent_log.clone(),
        };

        let status = HealStatus {
            at: Utc::now(),
            path: if resp.files.len() > 1 {
                format!("{} files", resp.files.len())
            } else {
                primary.path
            },
            success: healed || error.is_none(),
            attempts,
            message: agent_summary
                .clone()
                .or(error.clone())
                .or(diagnostics)
                .unwrap_or_else(|| {
                    if healed {
                        format!("ok ({} file(s))", resp.files.len())
                    } else {
                        format!("draft ({} file(s))", resp.files.len())
                    }
                }),
        };

        Ok((resp, status))
    }
}

fn multi_file_request(prompt: &str) -> bool {
    let re = regex::Regex::new(
        r"(?i)multi|project|package|directories|scaffold|from scratch|set up|create (a )?(app|tool|script|cli)",
    )
    .expect("multi-file prompt regex");
    re.is_match(prompt)
}
