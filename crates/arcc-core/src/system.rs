//! System-level platform introspection helpers.
//!
//! Consolidates duplicated OS-specific code for memory info,
//! shell path detection, and other platform queries.
//! All functions return zero / default values on failure —
//! never panic.

/// Result of a physical-memory query.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryInfo {
    pub total_bytes: u64,
    pub used_bytes: u64,
}

impl MemoryInfo {
    /// Total memory in mebibytes.
    pub fn total_mb(&self) -> u64 {
        self.total_bytes / 1_048_576
    }

    /// Used memory in mebibytes.
    pub fn used_mb(&self) -> u64 {
        self.used_bytes / 1_048_576
    }

    /// Memory usage as a percentage (0.0 – 100.0).
    pub fn usage_pct(&self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.used_bytes as f64 / self.total_bytes as f64 * 100.0).clamp(0.0, 100.0)
        }
    }
}

/// Query total and used physical memory.
///
/// On macOS: runs `sysctl hw.memsize` and `vm_stat`.
/// On Linux: reads `/proc/meminfo`.
/// Returns `MemoryInfo { 0, 0 }` on any failure or unsupported OS.
pub fn memory_info() -> MemoryInfo {
    #[cfg(target_os = "macos")]
    {
        let total = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok());

        let vm = std::process::Command::new("vm_stat")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok());

        if let (Some(total_bytes), Some(ref vm_out)) = (total, vm) {
            let page_size = vm_out
                .lines()
                .next()
                .and_then(|l| l.split("page size of ").nth(1))
                .and_then(|s| s.split(" bytes").next())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(16384);

            let active = parse_vm_val(vm_out, "Pages active:");
            let wired = parse_vm_val(vm_out, "Pages wired down:");
            let compressed = parse_vm_val(vm_out, "Pages occupied by compressor:");
            let used_bytes = (active + wired + compressed) * page_size;

            return MemoryInfo {
                total_bytes,
                used_bytes,
            };
        }
    }

    #[cfg(target_os = "linux")]
    if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
        let total_kb = parse_proc_val(&content, "MemTotal:");
        let avail_kb = parse_proc_val(&content, "MemAvailable:");
        if let (Some(t), Some(a)) = (total_kb, avail_kb) {
            return MemoryInfo {
                total_bytes: t * 1024,
                used_bytes: (t - a) * 1024,
            };
        }
    }

    MemoryInfo::default()
}

/// Returns `("sh", "-c")` on Unix, `("cmd.exe", "/C")` on Windows.
pub fn shell_and_arg() -> (&'static str, &'static str) {
    if cfg!(target_os = "windows") {
        ("cmd.exe", "/C")
    } else {
        ("sh", "-c")
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Parse a numeric value from `vm_stat` output by key.
/// Example: `parse_vm_val(output, "Pages active:")` returns the number.
#[allow(dead_code)]
fn parse_vm_val(output: &str, key: &str) -> u64 {
    output
        .lines()
        .find(|l| l.contains(key))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|s| s.trim().trim_end_matches('.').parse::<u64>().ok())
        .unwrap_or(0)
}

/// Parse a numeric value (in kB) from `/proc/meminfo` by key.
/// Example: `parse_proc_val(&content, "MemTotal:")` returns the kB value.
#[allow(dead_code)]
fn parse_proc_val(content: &str, key: &str) -> Option<u64> {
    content
        .lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u64>().ok())
}
