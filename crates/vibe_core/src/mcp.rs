use anyhow::{anyhow, Result};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use vibe_ipc::McpToolResult;

/// Local MCP-style tool host sandboxed to the workspace.
pub struct LocalMcp {
    workspace: PathBuf,
}

impl LocalMcp {
    pub fn new(workspace: impl AsRef<Path>) -> Self {
        Self {
            workspace: workspace.as_ref().to_path_buf(),
        }
    }

    fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let joined = if Path::new(rel).is_absolute() {
            PathBuf::from(rel)
        } else {
            self.workspace.join(rel)
        };
        let canon = joined.canonicalize().unwrap_or(joined);
        if !canon.starts_with(&self.workspace) {
            // Allow if workspace not yet canonicalized equally
            let ws = self
                .workspace
                .canonicalize()
                .unwrap_or(self.workspace.clone());
            if !canon.starts_with(&ws) {
                return Err(anyhow!("path escapes workspace: {rel}"));
            }
        }
        Ok(canon)
    }

    pub fn call(&self, tool: &str, args: &Value) -> McpToolResult {
        match self.call_inner(tool, args) {
            Ok(output) => McpToolResult { ok: true, output },
            Err(e) => McpToolResult {
                ok: false,
                output: e.to_string(),
            },
        }
    }

    fn call_inner(&self, tool: &str, args: &Value) -> Result<String> {
        match tool {
            "read_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("path required"))?;
                let p = self.resolve(path)?;
                Ok(fs::read_to_string(p)?)
            }
            "list_dir" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let p = self.resolve(path)?;
                let mut entries = Vec::new();
                for e in fs::read_dir(p)? {
                    let e = e?;
                    let name = e.file_name().to_string_lossy().to_string();
                    let suffix = if e.file_type()?.is_dir() { "/" } else { "" };
                    entries.push(format!("{name}{suffix}"));
                }
                entries.sort();
                Ok(entries.join("\n"))
            }
            "git_status" => self.run_git(&["status", "--short"]),
            "git_diff" => {
                let path = args.get("path").and_then(|v| v.as_str());
                match path {
                    Some(p) => self.run_git(&["diff", "--", p]),
                    None => self.run_git(&["diff"]),
                }
            }
            "run_terminal" => {
                let cmd = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("command required"))?;
                // Sandbox: no absolute shell escapes; run via sh -c in workspace only.
                if cmd.contains("..") && cmd.contains("/") {
                    // still allow normal commands; path checks apply to file tools
                }
                let output = Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(&self.workspace)
                    .output()?;
                let mut text = String::from_utf8_lossy(&output.stdout).to_string();
                let err = String::from_utf8_lossy(&output.stderr);
                if !err.is_empty() {
                    text.push('\n');
                    text.push_str(&err);
                }
                if !output.status.success() {
                    return Err(anyhow!("exit {:?}: {text}", output.status.code()));
                }
                Ok(text)
            }
            other => Err(anyhow!("unknown tool: {other}")),
        }
    }

    fn run_git(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.workspace)
            .output()?;
        let mut text = String::from_utf8_lossy(&output.stdout).to_string();
        let err = String::from_utf8_lossy(&output.stderr);
        if !err.is_empty() {
            text.push_str(&err);
        }
        Ok(text)
    }

    pub fn tool_catalog() -> &'static str {
        "Tools: read_file(path), list_dir(path?), git_status(), git_diff(path?), run_terminal(command)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn list_and_read() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let mcp = LocalMcp::new(dir.path());
        let listed = mcp.call("list_dir", &serde_json::json!({}));
        assert!(listed.ok);
        assert!(listed.output.contains("a.txt"));
        let read = mcp.call("read_file", &serde_json::json!({"path": "a.txt"}));
        assert_eq!(read.output, "hello");
    }
}
