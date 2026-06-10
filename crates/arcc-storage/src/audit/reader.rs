//! Read recent entries from the append-only JSON Lines audit log.
//!
//! Reads from the end of the file to efficiently retrieve the last N lines,
//! avoiding a full scan of large audit files.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use super::types::AuditEvent;

/// Read the most recent `count` audit events from a JSONL file.
///
/// Returns them in chronological order (oldest-first among the returned set).
/// Lines that fail to parse as `AuditEvent` are silently skipped.
pub fn read_recent(path: &Path, count: usize) -> Result<Vec<AuditEvent>, std::io::Error> {
    if count == 0 {
        return Ok(Vec::new());
    }

    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();

    if file_len == 0 {
        return Ok(Vec::new());
    }

    // --- Seek backwards to find an estimate of where the last N lines start ---
    // We read backwards in chunks, collecting lines until we have enough.
    // A conservative average line length of 200 bytes per audit line.
    let avg_line_len: u64 = 200;
    let estimate = (count as u64) * avg_line_len;
    let seek_back = estimate.min(file_len);

    file.seek(SeekFrom::End(-(seek_back as i64)))?;

    // Read everything from that point, then take the last N lines.
    let mut buf = Vec::with_capacity(seek_back as usize + 1);
    file.read_to_end(&mut buf)?;

    let mut lines: Vec<&[u8]> = buf
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .collect();

    // If we didn't get enough lines, fall back to reading the whole file.
    if lines.len() < count {
        return read_all(path, count);
    }

    let tail = lines.split_off(lines.len().saturating_sub(count));
    let mut events: Vec<AuditEvent> = Vec::with_capacity(tail.len());
    for line in tail {
        if let Ok(event) = serde_json::from_slice::<AuditEvent>(line) {
            events.push(event);
        }
    }

    Ok(events)
}

/// Read the entire audit file and return the last N parseable events.
fn read_all(path: &Path, count: usize) -> Result<Vec<AuditEvent>, std::io::Error> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut events: Vec<AuditEvent> = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<AuditEvent>(&line) {
            events.push(event);
        }
    }

    let len = events.len();
    if len > count {
        Ok(events.split_off(len - count))
    } else {
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::types::*;
    use std::io::Write;

    #[test]
    fn test_read_recent_empty_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("audit_test_empty.jsonl");
        let _ = std::fs::write(&path, "");
        let result = read_recent(&path, 5).unwrap();
        assert!(result.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_read_recent_fewer_than_count() {
        let dir = std::env::temp_dir();
        let path = dir.join("audit_test_few.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..3 {
            let event = AuditEvent::CommandExec {
                ts: "2026-01-01T00:00:00Z".into(),
                session: format!("s{i}"),
                cmd: format!("cmd{i}"),
                risk: RiskLevel::Low,
                approved_by: Approval::Auto,
                result: ExecResult::Ok,
                elapsed_ms: 10,
            };
            writeln!(f, "{}", event.to_line()).unwrap();
        }
        drop(f);

        let result = read_recent(&path, 10).unwrap();
        assert_eq!(result.len(), 3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_read_recent_exact_count() {
        let dir = std::env::temp_dir();
        let path = dir.join("audit_test_exact.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..5 {
            let event = AuditEvent::CommandExec {
                ts: "2026-01-01T00:00:00Z".into(),
                session: format!("s{i}"),
                cmd: format!("cmd{i}"),
                risk: RiskLevel::Low,
                approved_by: Approval::Auto,
                result: ExecResult::Ok,
                elapsed_ms: 10,
            };
            writeln!(f, "{}", event.to_line()).unwrap();
        }
        drop(f);

        let result = read_recent(&path, 5).unwrap();
        assert_eq!(result.len(), 5);
        let _ = std::fs::remove_file(&path);
    }
}
