use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{error, info, warn};

/// Watches a TOML config file for changes and notifies via a `watch` channel.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
}

impl ConfigWatcher {
    /// Start watching `path`. Every time the file is written, the latest
    /// contents are sent through `tx`.
    pub fn spawn(
        path: PathBuf,
        tx: watch::Sender<Option<String>>,
    ) -> Result<Self, notify::Error> {
        let path_clone = path.clone();

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| match res {
                Ok(event) => {
                    if !matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Create(_)
                    ) {
                        return;
                    }
                    if !event.paths.iter().any(|p| p == &path_clone) {
                        return;
                    }
                    match std::fs::read_to_string(&path_clone) {
                        Ok(contents) => {
                            let _ = tx.send(Some(contents));
                            info!(path = %path_clone.display(), "config file changed, reloaded");
                        }
                        Err(e) => {
                            error!(path = %path_clone.display(), err = %e, "failed to read config on change");
                        }
                    }
                }
                Err(e) => {
                    warn!(err = %e, "config watcher error");
                }
            },
            Config::default(),
        )?;

        watcher.watch(&path, RecursiveMode::NonRecursive)?;
        info!(path = %path.display(), "config watcher started");

        Ok(Self { _watcher: watcher })
    }

    /// Convenience: spawn a watcher wrapped in an `Arc` so it can be shared
    /// across tasks. Returns the `watch::Receiver` for consumers.
    pub fn spawn_arc(
        path: PathBuf,
    ) -> Result<(Arc<Self>, watch::Receiver<Option<String>>), notify::Error> {
        let (tx, rx) = watch::channel(None);
        let watcher = Self::spawn(path, tx)?;
        Ok((Arc::new(watcher), rx))
    }
}
