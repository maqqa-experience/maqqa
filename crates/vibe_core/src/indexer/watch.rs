use anyhow::Result;
use notify_debouncer_mini::{new_debouncer, DebounceEventResult};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Background watcher that pushes changed paths into a shared queue (Send + Sync).
pub struct WorkspaceWatcher {
    pending: Arc<Mutex<Vec<PathBuf>>>,
}

impl WorkspaceWatcher {
    pub fn start(workspace: &Path) -> Result<Self> {
        let pending: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let pending_cb = pending.clone();
        let root = workspace.to_path_buf();

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();

        thread::Builder::new()
            .name("vibe-watch".into())
            .spawn(move || {
                let debouncer = new_debouncer(
                    Duration::from_millis(400),
                    move |res: DebounceEventResult| match res {
                        Ok(events) => {
                            let mut q = pending_cb.lock();
                            for e in events {
                                let path = e.path;
                                let skip = path.components().any(|c| {
                                    matches!(
                                        c.as_os_str().to_str(),
                                        Some("node_modules" | "target" | ".git" | ".vibe" | "dist")
                                    )
                                });
                                if !skip {
                                    q.push(path);
                                }
                            }
                        }
                        Err(_) => {}
                    },
                );

                match debouncer {
                    Ok(mut d) => {
                        if let Err(e) = d.watcher().watch(&root, notify::RecursiveMode::Recursive) {
                            let _ = ready_tx.send(Err(e.into()));
                            return;
                        }
                        let _ = ready_tx.send(Ok(()));
                        // Keep debouncer alive for the thread lifetime.
                        loop {
                            thread::sleep(Duration::from_secs(3600));
                            let _ = &d;
                        }
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.into()));
                    }
                }
            })?;

        ready_rx.recv()??;
        Ok(Self { pending })
    }

    pub fn drain(&self) -> Vec<PathBuf> {
        let mut q = self.pending.lock();
        std::mem::take(&mut *q)
    }
}
