use anyhow::Result;
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing_subscriber::{fmt, EnvFilter};

pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

pub fn workspace_log_path(workspace: &Path) -> PathBuf {
    workspace.join(".vibe").join("logs").join("vibe.log")
}

pub fn append_local_log(workspace: &Path, message: &str) -> Result<()> {
    let path = workspace_log_path(workspace);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "[{}] {}", Utc::now().to_rfc3339(), message)?;
    Ok(())
}
