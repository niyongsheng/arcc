use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use tracing::warn;

use super::types::AuditEvent;

/// Append-only JSON Lines audit log.
///
/// Thread-safe: wraps a `BufWriter<File>` behind a `Mutex`.
/// Each `write` call flushes immediately to minimise data loss risk.
pub struct AuditWriter {
    inner: Mutex<BufWriter<File>>,
}

impl AuditWriter {
    /// Open (or create) an audit log file at `path`.
    pub fn open(path: &Path) -> Result<Self, std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        Ok(Self {
            inner: Mutex::new(BufWriter::new(file)),
        })
    }

    /// Append a single audit event as a JSON line and flush.
    pub fn write(&self, event: &AuditEvent) {
        let line = event.to_line();
        match self.inner.lock() {
            Ok(mut writer) => {
                let _ = writeln!(writer, "{line}");
                let _ = writer.flush();
            }
            Err(e) => {
                warn!(err = %e, "audit writer lock poisoned");
            }
        }
    }
}
