//! Measurement for the stress suite: counters of expensive work (frames built, lines lexed,
//! pages rebuilt, symbol scans, processes started) and the process's memory use. Read by
//! the headless `perf` command; counting costs one relaxed atomic add.

use std::sync::atomic::{AtomicU64, Ordering};

pub static FRAMES: AtomicU64 = AtomicU64::new(0);
pub static LINES_LEXED: AtomicU64 = AtomicU64::new(0);
pub static PAGE_REBUILDS: AtomicU64 = AtomicU64::new(0);
pub static SYMBOL_SCANS: AtomicU64 = AtomicU64::new(0);
pub static PROCESSES: AtomicU64 = AtomicU64::new(0);

pub fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

/// The counters as JSON fields.
pub fn counters_json() -> String {
    let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
    format!(
        "\"frames\":{},\"lines_lexed\":{},\"page_rebuilds\":{},\"symbol_scans\":{},\"processes\":{}",
        g(&FRAMES),
        g(&LINES_LEXED),
        g(&PAGE_REBUILDS),
        g(&SYMBOL_SCANS),
        g(&PROCESSES)
    )
}

/// Return memory freed by big background jobs (symbol scans, docs loading) to the system:
/// glibc keeps it in its per-thread arenas otherwise, and the editor looks hundreds of
/// megabytes bigger than it is.
pub fn trim_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim has no preconditions; it only releases unused heap pages.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Bytes the program's heap has in use (glibc; 0 elsewhere): JDC's own data, as opposed to
/// RSS, which also counts the GPU driver, fonts and code.
pub fn heap_in_use() -> u64 {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: mallinfo2 only reads allocator statistics.
    unsafe {
        let m = libc::mallinfo2();
        return (m.uordblks + m.hblkhd) as u64;
    }
    #[allow(unreachable_code)]
    0
}

/// Resident and peak resident memory in KiB (Linux; `None` elsewhere).
pub fn memory_kib() -> Option<(u64, u64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| -> Option<u64> {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    Some((field("VmRSS:")?, field("VmHWM:")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_memory() {
        let before = SYMBOL_SCANS.load(Ordering::Relaxed);
        bump(&SYMBOL_SCANS);
        assert!(SYMBOL_SCANS.load(Ordering::Relaxed) > before);
        assert!(counters_json().contains("\"symbol_scans\":"));
        if cfg!(target_os = "linux") {
            let (rss, peak) = memory_kib().unwrap();
            assert!(rss > 0 && peak >= rss);
        }
    }
}
