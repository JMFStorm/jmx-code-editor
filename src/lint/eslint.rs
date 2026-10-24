//! ESLint: the project's own installation, run in the background with JSON output.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{Diagnostic, Severity, Source};
use crate::process;

/// Config files that mark the root of an ESLint project.
const CONFIG_FILES: &[&str] = &[
    "eslint.config.js",
    "eslint.config.mjs",
    "eslint.config.cjs",
    "eslint.config.ts",
    "eslint.config.mts",
    "eslint.config.cts",
    ".eslintrc.js",
    ".eslintrc.cjs",
    ".eslintrc.json",
    ".eslintrc.yaml",
    ".eslintrc.yml",
    ".eslintrc",
];

/// File types worth sending to ESLint (its config decides the rest).
pub fn lintable(path: &Path) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" | "vue"
    )
}

/// The folder holding the ESLint config that applies to `path` (walking up).
pub fn config_root(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    start
        .ancestors()
        .find(|d| CONFIG_FILES.iter().any(|f| d.join(f).is_file()))
        .map(Path::to_path_buf)
}

/// Config roots in and above `folder`: its own (or an ancestor's) config, else projects found
/// in subfolders up to three levels down (node_modules and ignored folders skipped).
pub fn roots_in(folder: &Path) -> Vec<PathBuf> {
    if let Some(r) = config_root(folder) {
        return vec![r];
    }
    let mut roots: Vec<PathBuf> = crate::workspace::files::walker(folder, true)
        .max_depth(Some(3))
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter(|e| CONFIG_FILES.iter().any(|f| e.file_name() == *f))
        .filter_map(|e| e.path().parent().map(Path::to_path_buf))
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

pub fn find_eslint(root: &Path) -> Option<PathBuf> {
    process::find_node_tool("eslint", "JDC_ESLINT", root)
}

// ESLint's `--format json` output.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileReport {
    file_path: String,
    #[serde(default)]
    messages: Vec<Message>,
    /// Fixed text (with `--fix-dry-run`), when fixes were made.
    output: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    rule_id: Option<String>,
    severity: u8,
    message: String,
    line: Option<usize>,
    column: Option<usize>,
    end_line: Option<usize>,
    end_column: Option<usize>,
}

/// Parsed results: diagnostics per file (canonical paths) and fixed text per file.
#[derive(Debug, Default)]
pub struct Report {
    pub files: BTreeMap<PathBuf, Vec<Diagnostic>>,
    pub fixed: BTreeMap<PathBuf, String>,
}

/// Parse `eslint --format json` output; relative paths are taken from `root`.
pub fn parse(json: &str, root: &Path) -> Result<Report, String> {
    let reports: Vec<FileReport> = serde_json::from_str(json.trim())
        .map_err(|e| format!("ESLint: unreadable output ({e})"))?;
    let mut out = Report::default();
    for r in reports {
        let path = root.join(&r.file_path);
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let diags: Vec<Diagnostic> = r
            .messages
            .into_iter()
            // "File ignored because of a matching ignore pattern" and friends.
            .filter(|m| {
                !(m.rule_id.is_none() && m.severity == 1 && m.message.starts_with("File ignored"))
            })
            .map(|m| {
                let line = m.line.unwrap_or(1).saturating_sub(1);
                let col = m.column.unwrap_or(1).saturating_sub(1);
                let end_line = m.end_line.map_or(line, |l| l.saturating_sub(1));
                let end_col = m.end_column.map_or(col, |c| c.saturating_sub(1));
                Diagnostic {
                    source: Source::Eslint,
                    severity: if m.severity >= 2 {
                        Severity::Error
                    } else {
                        Severity::Warning
                    },
                    line,
                    col,
                    end_line,
                    end_col,
                    message: m.message.trim().to_string(),
                    code: m.rule_id,
                }
            })
            .collect();
        if let Some(text) = r.output {
            out.fixed.insert(path.clone(), text);
        }
        out.files.insert(path, diags);
    }
    Ok(out)
}

/// How long one file's lint may take.
const FILE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a whole project's lint may take.
const ROOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Run ESLint and parse its report. Exit codes 0 and 1 (problems found) are both success.
fn run_eslint(eslint: &Path, root: &Path, args: &[&OsStr], stdin: &str) -> Result<Report, String> {
    run_eslint_for(eslint, root, args, stdin, FILE_TIMEOUT)
}

fn run_eslint_for(
    eslint: &Path,
    root: &Path,
    args: &[&OsStr],
    stdin: &str,
    timeout: std::time::Duration,
) -> Result<Report, String> {
    let out = process::run_with_timeout(eslint, args, root, stdin, timeout)?;
    match out.code {
        Some(0 | 1) => parse(&out.stdout, root),
        _ => Err(process::first_error_line("ESLint", &out.stderr)
            .unwrap_or_else(|| format!("ESLint failed (exit {:?})", out.code))),
    }
}

/// Lint one file's text (`--stdin`), so unsaved text could be linted too.
pub fn lint_text(eslint: &Path, root: &Path, path: &Path, text: &str) -> Result<Report, String> {
    let args: [&OsStr; 5] = [
        "--format".as_ref(),
        "json".as_ref(),
        "--stdin".as_ref(),
        "--stdin-filename".as_ref(),
        path.as_os_str(),
    ];
    run_eslint(eslint, root, &args, text)
}

/// Apply ESLint's automatic fixes to a file's text without writing it: (text, diagnostics left).
pub fn fix_text(
    eslint: &Path,
    root: &Path,
    path: &Path,
    text: &str,
) -> Result<(String, Vec<Diagnostic>), String> {
    let args: [&OsStr; 6] = [
        "--fix-dry-run".as_ref(),
        "--format".as_ref(),
        "json".as_ref(),
        "--stdin".as_ref(),
        "--stdin-filename".as_ref(),
        path.as_os_str(),
    ];
    let mut report = run_eslint(eslint, root, &args, text)?;
    let fixed = report.fixed.pop_first().map(|(_, t)| t);
    let diags = report.files.pop_first().map(|(_, d)| d).unwrap_or_default();
    Ok((fixed.unwrap_or_else(|| text.to_string()), diags))
}

/// What a lint job covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// One file (canonical path), linted from the given text.
    File(PathBuf),
    /// Everything under a config root.
    Root(PathBuf),
}

/// An ESLint run in the background.
pub struct LintJob {
    rx: Receiver<Result<Report, String>>,
    pub scope: Scope,
    /// For file jobs: the document revision that was linted.
    pub revision: u64,
    pub started: Instant,
    /// Automatic run: no message when it finishes.
    pub quiet: bool,
}

impl LintJob {
    pub fn file(
        eslint: PathBuf,
        root: PathBuf,
        path: PathBuf,
        text: String,
        revision: u64,
    ) -> Self {
        let p = path.clone();
        Self::spawn(Scope::File(path), revision, move || {
            lint_text(&eslint, &root, &p, &text)
        })
    }

    pub fn root(eslint: PathBuf, root: PathBuf) -> Self {
        let r = root.clone();
        Self::spawn(Scope::Root(root), 0, move || {
            let args: [&OsStr; 3] = ["--format".as_ref(), "json".as_ref(), ".".as_ref()];
            // A whole monorepo takes minutes, not seconds.
            run_eslint_for(&eslint, &r, &args, "", ROOT_TIMEOUT)
        })
    }

    fn spawn(
        scope: Scope,
        revision: u64,
        work: impl FnOnce() -> Result<Report, String> + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let what = format!("{scope:?}");
        std::thread::Builder::new()
            .name("jdc-eslint".into())
            .spawn(move || {
                let started = Instant::now();
                let result = work();
                log::debug!(
                    "eslint {what} in {:?}: {}",
                    started.elapsed(),
                    result
                        .as_ref()
                        .map_or_else(|e| e.clone(), |r| format!("{} files", r.files.len()))
                );
                let _ = tx.send(result);
            })
            .expect("cannot start eslint thread");
        Self {
            rx,
            scope,
            revision,
            started: Instant::now(),
            quiet: false,
        }
    }

    /// The same job, finishing without a status message.
    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    pub fn try_result(&self) -> Option<Result<Report, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("ESLint job crashed".into())),
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const SAMPLE: &str = r#"[
      {"filePath": "/w/src/a.js", "messages": [
        {"ruleId": "no-unused-vars", "severity": 1, "message": "'x' is defined but never used.",
         "line": 3, "column": 7, "endLine": 3, "endColumn": 8},
        {"ruleId": null, "fatal": true, "severity": 2, "message": "Parsing error: Unexpected token )",
         "line": 9, "column": 2}
      ], "errorCount": 1, "warningCount": 1, "output": "fixed\n"},
      {"filePath": "src/ignored.js", "messages": [
        {"ruleId": null, "severity": 1, "message": "File ignored because of a matching ignore pattern."}
      ]}
    ]"#;

    #[test]
    fn parses_eslint_json() {
        let r = parse(SAMPLE, Path::new("/w")).unwrap();
        let a = &r.files[Path::new("/w/src/a.js")];
        assert_eq!(a.len(), 2);
        assert_eq!((a[0].line, a[0].col, a[0].end_col), (2, 6, 7));
        assert_eq!(a[0].severity, Severity::Warning);
        assert_eq!(a[0].code.as_deref(), Some("no-unused-vars"));
        assert_eq!(a[1].severity, Severity::Error);
        assert_eq!((a[1].line, a[1].end_line), (8, 8));
        assert!(r.files[Path::new("/w/src/ignored.js")].is_empty());
        assert_eq!(r.fixed[Path::new("/w/src/a.js")], "fixed\n");
        assert!(parse("not json", Path::new("/")).is_err());
    }

    #[test]
    fn finds_config_roots() {
        let dir = std::env::temp_dir().join(format!("jdc-eslroot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("web/src/deep")).unwrap();
        fs::create_dir_all(dir.join("api")).unwrap();
        fs::write(dir.join("web/eslint.config.js"), "").unwrap();
        fs::write(dir.join("web/src/deep/a.js"), "").unwrap();
        assert_eq!(
            config_root(&dir.join("web/src/deep/a.js")),
            Some(dir.join("web"))
        );
        assert_eq!(config_root(&dir.join("api")), None);
        assert_eq!(roots_in(&dir), [dir.join("web")]);
        assert_eq!(roots_in(&dir.join("web/src")), [dir.join("web")]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lintable_types() {
        assert!(lintable(Path::new("App.vue")));
        assert!(lintable(Path::new("x.ts")));
        assert!(!lintable(Path::new("a.css")));
    }
}
