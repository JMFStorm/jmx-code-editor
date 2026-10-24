//! File logging. Every launch starts a fresh `<name>.log`; the previous run's log is kept
//! as `<name>.prev.log` and anything older is deleted.
//!
//! Levels: debug builds log `jdc=debug`, release builds `jdc=info` (debug/trace calls are
//! compiled out of release builds by the `release_max_level_info` feature of `log`).
//! Other crates log at `warn`. Override with `JDC_LOG` / `--log-level`: a bare level applies
//! to the editor itself (`trace`), `target=level` to one crate (`wgpu_hal=info`) and
//! `*=level` to everything else.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Log, Metadata, Record};

pub const DEFAULT_SPEC: &str = if cfg!(debug_assertions) {
    "debug"
} else {
    "info"
};

pub struct LogOptions {
    /// Log folder; `None` uses the per-user default.
    pub dir: Option<PathBuf>,
    /// Base file name, e.g. "jdc" or "jdc-headless".
    pub name: &'static str,
    /// Filter spec; `None` uses `JDC_LOG` or the build default.
    pub spec: Option<String>,
}

/// Per-target level filter: a default level (for other crates) plus `target=level` overrides.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    default: LevelFilter,
    targets: Vec<(String, LevelFilter)>,
}

impl Filter {
    /// Parse comma-separated items: `level` (the editor's own level), `target=level`, and
    /// `*=level` (all other crates; default warn). Bad items are ignored.
    pub fn parse(spec: &str) -> Self {
        let mut f = Filter {
            default: LevelFilter::Warn,
            targets: Vec::new(),
        };
        for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            match item.split_once('=') {
                Some((target, level)) => {
                    if let Ok(level) = level.trim().parse() {
                        match target.trim() {
                            "*" => f.default = level,
                            t => f.targets.push((t.to_string(), level)),
                        }
                    }
                }
                None => {
                    if let Ok(level) = item.parse() {
                        f.targets.push(("jdc".to_string(), level));
                    }
                }
            }
        }
        // Longest (most specific) target first.
        f.targets.sort_by_key(|(t, _)| std::cmp::Reverse(t.len()));
        f
    }

    pub fn level_for(&self, target: &str) -> LevelFilter {
        self.targets
            .iter()
            .find(|(t, _)| {
                target == t || target.starts_with(t.as_str()) && target[t.len()..].starts_with("::")
            })
            .map_or(self.default, |(_, l)| *l)
    }

    pub fn max(&self) -> LevelFilter {
        self.targets
            .iter()
            .map(|(_, l)| *l)
            .fold(self.default, std::cmp::max)
    }
}

struct FileLogger {
    start: Instant,
    filter: Filter,
    file: Mutex<Option<BufWriter<File>>>,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.filter.level_for(metadata.target())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let t = self.start.elapsed().as_secs_f64();
        let line = format!(
            "+{t:9.3}s {:<5} {}: {}\n",
            record.level(),
            record.target(),
            record.args()
        );
        let important = record.level() <= Level::Warn;
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(w) => {
                let _ = w.write_all(line.as_bytes());
                if important {
                    let _ = w.flush();
                }
                // Mirror our own warnings (and anyone's errors) to the console in debug builds;
                // driver chatter from wgpu stays in the file.
                let ours = record.target() == "jdc" || record.target().starts_with("jdc::");
                if cfg!(debug_assertions) && (record.level() == Level::Error || important && ours) {
                    eprint!("{line}");
                }
            }
            None => eprint!("{line}"),
        }
    }

    fn flush(&self) {
        if let Some(w) = self.file.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = w.flush();
        }
    }
}

static LOGGER: OnceLock<FileLogger> = OnceLock::new();
static LOG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Per-user log folder.
pub fn default_dir() -> Option<PathBuf> {
    crate::paths::state_dir().map(|d| d.join("logs"))
}

/// Path of the current log file, once logging is initialized.
pub fn log_path() -> Option<&'static Path> {
    LOG_PATH.get().and_then(|p| p.as_deref())
}

/// Install the logger and panic hook. Call once, first thing in `main`.
pub fn init(opts: LogOptions) {
    let spec = opts
        .spec
        .or_else(|| std::env::var("JDC_LOG").ok())
        .unwrap_or_else(|| DEFAULT_SPEC.to_string());
    let filter = Filter::parse(&spec);

    let mut warning = None;
    let file = match opts.dir.or_else(default_dir) {
        Some(dir) => match rotate(&dir, opts.name) {
            Ok(path) => match File::create(&path) {
                Ok(f) => {
                    let _ = LOG_PATH.set(Some(path));
                    Some(BufWriter::new(f))
                }
                Err(e) => {
                    warning = Some(format!("cannot create {}: {e}", path.display()));
                    None
                }
            },
            Err(e) => {
                warning = Some(format!("cannot prepare log folder {}: {e}", dir.display()));
                None
            }
        },
        None => {
            warning = Some("no log folder found (HOME/LOCALAPPDATA unset)".to_string());
            None
        }
    };
    // No-op if a path was set above.
    let _ = LOG_PATH.set(None);

    let logger = LOGGER.get_or_init(|| FileLogger {
        start: Instant::now(),
        filter: filter.clone(),
        file: Mutex::new(file),
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(filter.max());
    }
    write_header(logger, &spec);
    if let Some(w) = warning {
        eprintln!("jdc: logging to stderr only: {w}");
    }
    install_panic_hook();
}

fn write_header(logger: &FileLogger, spec: &str) {
    let mut guard = logger.file.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(w) = guard.as_mut() {
        let args: Vec<String> = std::env::args().collect();
        let _ = writeln!(
            w,
            "# jdc {} ({}) started {} on {}/{}\n# args: {}\n# filter: {spec}\n",
            env!("CARGO_PKG_VERSION"),
            if cfg!(debug_assertions) {
                "debug build"
            } else {
                "release build"
            },
            utc_timestamp(SystemTime::now()),
            std::env::consts::OS,
            std::env::consts::ARCH,
            args.join(" "),
        );
        let _ = w.flush();
    }
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(non-string panic payload)".into());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let backtrace = std::backtrace::Backtrace::force_capture();
        log::error!("PANIC at {location}: {msg}\n{backtrace}");
        flush();
        default_hook(info);
    }));
}

pub fn flush() {
    log::logger().flush();
}

/// Move `<name>.log` to `<name>.prev.log`, delete older `<name>.*.log` files, and return
/// the path for the new log.
pub fn rotate(dir: &Path, name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let current = dir.join(format!("{name}.log"));
    let prev = dir.join(format!("{name}.prev.log"));
    if current.exists() {
        // Windows rename does not replace an existing file.
        let _ = fs::remove_file(&prev);
        fs::rename(&current, &prev)?;
    }
    let stale_prefix = format!("{name}.");
    for entry in fs::read_dir(dir)?.flatten() {
        let fname = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if fname.starts_with(&stale_prefix)
            && fname.ends_with(".log")
            && path != prev
            && path != current
        {
            let _ = fs::remove_file(path);
        }
    }
    Ok(current)
}

/// `2026-10-24 05:16:13 UTC`, without pulling in a date crate.
pub fn utc_timestamp(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn filter_parsing() {
        let f = Filter::parse("*=info,jdc=trace,jdc::render=warn,wgpu_hal=error,bogus=nope");
        assert_eq!(f.level_for("jdc"), LevelFilter::Trace);
        assert_eq!(f.level_for("jdc::editor"), LevelFilter::Trace);
        assert_eq!(f.level_for("jdc::render::gpu"), LevelFilter::Warn);
        assert_eq!(f.level_for("jdcfoo"), LevelFilter::Info);
        assert_eq!(f.level_for("wgpu_hal::vulkan"), LevelFilter::Error);
        assert_eq!(f.level_for("naga"), LevelFilter::Info);
        assert_eq!(f.max(), LevelFilter::Trace);
        assert_eq!(Filter::parse("").level_for("x"), LevelFilter::Warn);
        // A bare level is the editor's own level; other crates stay at warn.
        let f = Filter::parse("trace");
        assert_eq!(f.level_for("jdc::editor"), LevelFilter::Trace);
        assert_eq!(f.level_for("wgpu_hal::vulkan"), LevelFilter::Warn);
    }

    #[test]
    fn utc_formatting() {
        assert_eq!(utc_timestamp(UNIX_EPOCH), "1970-01-01 00:00:00 UTC");
        let t = UNIX_EPOCH + Duration::from_secs(1_793_942_173);
        assert_eq!(utc_timestamp(t), "2026-11-06 05:16:13 UTC");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert!(utc_timestamp(leap).starts_with("2000-02-29"));
    }

    #[test]
    fn rotation_keeps_current_and_previous_only() {
        let dir = std::env::temp_dir().join(format!("jdc-logs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for run in 1..=3 {
            let path = rotate(&dir, "jdc").unwrap();
            fs::write(&path, format!("run {run}")).unwrap();
        }
        fs::write(dir.join("jdc.2025-01-01.log"), "stale").unwrap();
        fs::write(dir.join("jdc-headless.log"), "other pair").unwrap();
        fs::write(dir.join("notes.txt"), "unrelated").unwrap();
        let path = rotate(&dir, "jdc").unwrap();
        fs::write(&path, "run 4").unwrap();

        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["jdc-headless.log", "jdc.log", "jdc.prev.log", "notes.txt"]
        );
        assert_eq!(
            fs::read_to_string(dir.join("jdc.prev.log")).unwrap(),
            "run 3"
        );
        assert_eq!(fs::read_to_string(dir.join("jdc.log")).unwrap(), "run 4");
        fs::remove_dir_all(&dir).unwrap();
    }
}
