use crate::config::VibeConfig;
use crate::ollama::{strip_code_fences, OllamaClient};
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use similar::{ChangeTag, TextDiff};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct HealResult {
    pub content: String,
    pub attempts: u32,
    pub diagnostics: Option<String>,
    pub success: bool,
}

#[derive(Debug, Clone)]
pub struct MultiHealResult {
    pub files: Vec<crate::multifile::ParsedFile>,
    pub attempts: u32,
    pub diagnostics: Option<String>,
    pub success: bool,
    pub summary: Option<String>,
    pub log: Vec<String>,
}

pub struct ShadowWorkspace {
    origin: PathBuf,
    shadow: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    Rust,
    TypeScript,
    Python,
    Unknown,
}

/// Syntax-check all `.py` files under `root` when pyright/ruff are unavailable.
fn verify_python_tree(root: &Path) -> Result<(), String> {
    let mut errors = Vec::new();
    let mut checked = 0u32;
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .build()
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("py") {
            continue;
        }
        // Skip vibe shadow internals if walked from origin; usually root is shadow.
        if path.components().any(|c| c.as_os_str() == ".vibe") {
            continue;
        }
        checked += 1;
        let output = Command::new("python3")
            .args(["-m", "py_compile"])
            .arg(path)
            .current_dir(root)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            errors.push(format!(
                "{}:\n{}\n{}",
                path.strip_prefix(root).unwrap_or(path).display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    if checked == 0 {
        return Ok(());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

impl ShadowWorkspace {
    pub fn new(origin: impl AsRef<Path>) -> Result<Self> {
        let origin = origin
            .as_ref()
            .canonicalize()
            .unwrap_or_else(|_| origin.as_ref().to_path_buf());
        let vibe = crate::config::VibeConfig::ensure_workspace_vibe_dir(&origin)?;
        let shadow = vibe.join("shadow");
        Ok(Self { origin, shadow })
    }

    pub fn shadow_dir(&self) -> &Path {
        &self.shadow
    }

    pub fn origin_dir(&self) -> &Path {
        &self.origin
    }

    pub fn detect_kind(root: &Path) -> ProjectKind {
        if root.join("Cargo.toml").exists() {
            ProjectKind::Rust
        } else if root.join("tsconfig.json").exists() || root.join("package.json").exists() {
            ProjectKind::TypeScript
        } else if root.join("pyproject.toml").exists()
            || root.join("requirements.txt").exists()
            || root.join("setup.py").exists()
        {
            ProjectKind::Python
        } else {
            ProjectKind::Unknown
        }
    }

    pub fn sync_from_origin(&self) -> Result<()> {
        if self.shadow.exists() {
            // Clear previous shadow contents except keep dir.
            for entry in fs::read_dir(&self.shadow)? {
                let entry = entry?;
                let p = entry.path();
                if p.is_dir() {
                    fs::remove_dir_all(&p)?;
                } else {
                    fs::remove_file(&p)?;
                }
            }
        } else {
            fs::create_dir_all(&self.shadow)?;
        }

        let walker = WalkBuilder::new(&self.origin)
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

        for result in walker {
            let entry = result?;
            let path = entry.path();
            if path == self.origin {
                continue;
            }
            let rel = path.strip_prefix(&self.origin).unwrap_or(path);
            let dest = self.shadow.join(rel);
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                fs::create_dir_all(&dest)?;
            } else if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(path, &dest)
                    .with_context(|| format!("copy {} -> {}", path.display(), dest.display()))?;
            }
        }
        Ok(())
    }

    pub fn stage_file_change(&self, relative_path: &str, content: &str) -> Result<()> {
        let destination = self.shadow.join(relative_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&destination, content)?;
        Ok(())
    }

    pub fn verify_build(&self) -> Result<(), String> {
        let kind = Self::detect_kind(&self.shadow);
        let (cmd, args): (String, Vec<&str>) = match kind {
            ProjectKind::Rust => ("cargo".into(), vec!["check", "--message-format=short"]),
            ProjectKind::TypeScript => {
                if self
                    .shadow
                    .join("node_modules")
                    .join(".bin")
                    .join("tsc")
                    .exists()
                    || which("tsc")
                {
                    ("tsc".into(), vec!["--noEmit"])
                } else if self.shadow.join("package.json").exists() {
                    ("npm".into(), vec!["run", "check", "--if-present"])
                } else {
                    return Err("no TypeScript checker found".into());
                }
            }
            ProjectKind::Python => {
                if which("pyright") {
                    ("pyright".into(), vec![])
                } else if which("ruff") {
                    ("ruff".into(), vec!["check", "."])
                } else {
                    // No dedicated linter — syntax-check every .py under shadow.
                    return verify_python_tree(&self.shadow);
                }
            }
            ProjectKind::Unknown => {
                return Err("unknown project type; cannot verify".into());
            }
        };

        let output = Command::new(&cmd)
            .args(&args)
            .current_dir(&self.shadow)
            .output()
            .map_err(|e| e.to_string())?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            Err(format!("{stdout}\n{stderr}").trim().to_string())
        }
    }

    pub async fn auto_heal_loop(
        &self,
        relative_path: &str,
        base_prompt: &str,
        ollama: &OllamaClient,
        cfg: &VibeConfig,
        reason_model: &str,
    ) -> Result<HealResult> {
        self.auto_heal_loop_with(relative_path, base_prompt, ollama, cfg, reason_model, false)
            .await
    }

    pub async fn auto_heal_loop_single_file(
        &self,
        relative_path: &str,
        base_prompt: &str,
        ollama: &OllamaClient,
        cfg: &VibeConfig,
        reason_model: &str,
    ) -> Result<HealResult> {
        self.auto_heal_loop_with(relative_path, base_prompt, ollama, cfg, reason_model, true)
            .await
    }

    /// Multi-file generate + stage + per-file (and optional project) verify.
    pub async fn auto_heal_multifile(
        &self,
        fallback_path: &str,
        base_prompt: &str,
        ollama: &OllamaClient,
        cfg: &VibeConfig,
        reason_model: &str,
        also_project_check: bool,
    ) -> Result<MultiHealResult> {
        use crate::multifile::{parse_multifile_output, ParsedFile};

        let mut current_prompt = base_prompt.to_string();
        let mut last_diag = None;
        let mut last_files: Vec<ParsedFile> = Vec::new();

        for attempt in 1..=cfg.heal_max_attempts {
            info!(attempt, "shadow multifile heal pass");
            let raw = ollama
                .generate(reason_model, &current_prompt, Some(12_288))
                .await
                .with_context(|| format!("multifile heal generate attempt {attempt}"))?;
            let files = parse_multifile_output(&raw, fallback_path);
            if files.is_empty() {
                last_diag = Some("model returned no parseable files".into());
                current_prompt = format!(
                    "{base_prompt}\n\n### Error:\nNo @@@ FILE: blocks parsed. Re-emit using the required multi-file format."
                );
                continue;
            }
            last_files = files.clone();
            for f in &files {
                self.stage_file_change(&f.path, &f.content)?;
            }

            let mut errors = Vec::new();
            for f in &files {
                if let Err(e) = self.verify_single_file(&f.path) {
                    errors.push(format!("{}: {e}", f.path));
                }
            }
            if also_project_check {
                if let Err(e) = self.verify_build() {
                    errors.push(format!("project: {e}"));
                }
            }

            if errors.is_empty() {
                info!(
                    attempt,
                    n_files = files.len(),
                    "multifile verification success"
                );
                return Ok(MultiHealResult {
                    files,
                    attempts: attempt,
                    diagnostics: last_diag,
                    success: true,
                    summary: None,
                    log: Vec::new(),
                });
            }

            warn!(attempt, "multifile verification failed");
            let compiler_errors = errors.join("\n");
            last_diag = Some(compiler_errors.clone());
            let bundle = files
                .iter()
                .map(|f| format!("@@@ FILE: {}\n{}", f.path, f.content))
                .collect::<Vec<_>>()
                .join("\n");
            current_prompt = format!(
                "The previous multi-file output had errors.\n\n### Original Prompt:\n{base_prompt}\n\n### Your Previous Output:\n{bundle}\n\n### Diagnostics:\n{compiler_errors}\n\nFix and re-emit ALL files using @@@ FILE: path markers. Output only the file blocks."
            );
        }

        Ok(MultiHealResult {
            files: last_files,
            attempts: cfg.heal_max_attempts,
            diagnostics: last_diag,
            success: false,
            summary: None,
            log: Vec::new(),
        })
    }

    async fn auto_heal_loop_with(
        &self,
        relative_path: &str,
        base_prompt: &str,
        ollama: &OllamaClient,
        cfg: &VibeConfig,
        reason_model: &str,
        single_file: bool,
    ) -> Result<HealResult> {
        let mut current_prompt = base_prompt.to_string();
        let mut last_diag = None;

        for attempt in 1..=cfg.heal_max_attempts {
            info!(
                attempt,
                path = relative_path,
                single_file,
                "shadow heal pass"
            );
            let raw = ollama
                .generate(reason_model, &current_prompt, Some(8192))
                .await
                .with_context(|| format!("heal generate attempt {attempt}"))?;
            let content = strip_code_fences(&raw);
            self.stage_file_change(relative_path, &content)?;

            let verify = if single_file {
                self.verify_single_file(relative_path)
            } else {
                self.verify_build()
            };

            match verify {
                Ok(()) => {
                    info!(attempt, "shadow verification success");
                    return Ok(HealResult {
                        content,
                        attempts: attempt,
                        diagnostics: last_diag,
                        success: true,
                    });
                }
                Err(compiler_errors) => {
                    warn!(attempt, "shadow verification failed");
                    last_diag = Some(compiler_errors.clone());
                    current_prompt = format!(
                        "The previous code generated compilation errors.\n\n### Original Prompt:\n{}\n\n### Your Previous Code:\n{}\n\n### Compilation Diagnostics:\n{}\n\nFix these errors and output only the complete clean file content without markdown wrapper text.",
                        base_prompt, content, compiler_errors
                    );
                }
            }
        }

        // Keep last draft so the UI can still show / accept something fail-open for review.
        let last_content = fs::read_to_string(self.shadow.join(relative_path)).unwrap_or_default();
        Ok(HealResult {
            content: last_content,
            attempts: cfg.heal_max_attempts,
            diagnostics: last_diag,
            success: false,
        })
    }

    pub fn unified_diff(original: &str, proposed: &str, path: &str) -> String {
        let diff = TextDiff::from_lines(original, proposed);
        let mut out = format!("--- a/{path}\n+++ b/{path}\n");
        for change in diff.iter_all_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            out.push_str(sign);
            out.push_str(change.value());
            if !change.value().ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }

    pub fn apply_to_origin(&self, relative_path: &str, content: &str) -> Result<()> {
        let dest = self.origin.join(relative_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(dest, content)?;
        Ok(())
    }
}

fn which(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn build_edit_prompt(
    path: &str,
    content: &str,
    selection: Option<&str>,
    user_prompt: &str,
    context_chunks: &str,
) -> String {
    build_edit_prompt_mode(path, content, selection, user_prompt, context_chunks, false)
}

pub fn build_create_prompt(path: &str, user_prompt: &str, context_chunks: &str) -> String {
    build_edit_prompt_mode(path, "", None, user_prompt, context_chunks, true)
}

fn build_edit_prompt_mode(
    path: &str,
    content: &str,
    selection: Option<&str>,
    user_prompt: &str,
    context_chunks: &str,
    create: bool,
) -> String {
    let selection_block = selection
        .map(|s| format!("### Selected Region:\n```\n{s}\n```\n"))
        .unwrap_or_default();
    let mode = if create {
        "Create a brand-new file from scratch that fully satisfies the user request. The file does not exist yet."
    } else {
        "Edit the file to satisfy the user request."
    };
    let body = if create || content.is_empty() {
        "### Current File:\n(empty — create from scratch)\n".to_string()
    } else {
        format!("### Current File:\n```\n{content}\n```\n")
    };
    format!(
        r#"You are a senior coding agent. {mode}
Return ONLY the full file contents. No markdown fences. No explanation.
Write complete, runnable code. Include a short module docstring or top comment describing usage when appropriate.

### User Request:
{user_prompt}

### File Path:
{path}

{selection_block}{body}
### Retrieved Code Context:
{context_chunks}
"#
    )
}

impl ShadowWorkspace {
    /// Syntax-check a single staged file when full-project verify is the wrong tool
    /// (e.g. adding a .py script inside a Rust repo).
    pub fn verify_single_file(&self, relative_path: &str) -> Result<(), String> {
        let path = self.shadow.join(relative_path);
        if !path.exists() {
            return Err(format!("missing staged file {relative_path}"));
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        match ext {
            "py" => {
                let output = Command::new("python3")
                    .args(["-m", "py_compile"])
                    .arg(&path)
                    .current_dir(&self.shadow)
                    .output()
                    .map_err(|e| e.to_string())?;
                if output.status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "{}\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    ))
                }
            }
            "js" | "mjs" | "cjs" => {
                let output = Command::new("node")
                    .args(["--check"])
                    .arg(&path)
                    .current_dir(&self.shadow)
                    .output()
                    .map_err(|e| e.to_string())?;
                if output.status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "{}\n{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    ))
                }
            }
            "ts" | "tsx" => {
                if which("tsc") {
                    // Can't easily typecheck one orphan file; fall through to ok.
                    Ok(())
                } else {
                    Ok(())
                }
            }
            "rs" => {
                // Prefer full cargo check when Cargo.toml exists.
                if self.shadow.join("Cargo.toml").exists() {
                    self.verify_build()
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sync_and_stage_rust_fixture() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            r#"[package]
name = "demo"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
        )
        .unwrap();

        let shadow = ShadowWorkspace::new(root).unwrap();
        shadow.sync_from_origin().unwrap();
        assert!(shadow.shadow_dir().join("Cargo.toml").exists());
        shadow
            .stage_file_change(
                "src/lib.rs",
                "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
            )
            .unwrap();
        // cargo check should pass when cargo is available
        if which("cargo") {
            assert!(shadow.verify_build().is_ok());
        }
    }

    #[test]
    fn detect_kinds() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(ShadowWorkspace::detect_kind(dir.path()), ProjectKind::Rust);
    }
}
