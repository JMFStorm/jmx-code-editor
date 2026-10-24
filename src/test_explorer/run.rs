//! Running tests in the background: Vitest / Jest (JSON report) and `dotnet test` (TRX report),
//! and mapping their results back to the discovered tests.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use quick_xml::Reader;
use quick_xml::events::Event;
use serde::Deserialize;

use super::{Framework, Project, Status, TestCase};
use crate::lint::{Diagnostic, roslyn};
use crate::process;

/// A result as a runner reports it, before matching to a discovered test.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub key: Key,
    pub status: Status,
    pub duration: Option<Duration>,
    pub message: Option<String>,
    /// The whole failure text (assertion, diff, stack), capped.
    pub detail: Option<String>,
    /// Data-row label (`accepts a@b.fi`, `Parses(n: 2)`), shown when a test has several rows.
    pub row: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// JS: file and suite path + test title.
    Js { file: PathBuf, path: Vec<String> },
    /// .NET: `Namespace.Class.Method` (parameters removed).
    Dotnet(String),
}

#[derive(Debug, Default)]
pub struct Report {
    pub outcomes: Vec<Outcome>,
    /// Compiler diagnostics from the build `dotnet test` did first, per project folder.
    pub diagnostics: Option<BTreeMap<PathBuf, Vec<Diagnostic>>>,
    pub error: Option<String>,
}

/// What to run in one project.
#[derive(Debug, Clone)]
pub struct Request {
    pub project: Project,
    /// JS: test files to pass to the runner (empty = all).
    pub files: Vec<PathBuf>,
    /// JS: `-t` name pattern (regex). .NET: `--filter` expression.
    pub filter: Option<String>,
    /// Ids of the tests this run covers (marked running), with the hash of their text.
    pub ids: Vec<(String, u64)>,
}

pub struct TestJob {
    rx: Receiver<Report>,
    pub request: Request,
    cancel: Arc<AtomicBool>,
}

impl TestJob {
    pub fn spawn(request: Request) -> Self {
        let (tx, rx) = mpsc::channel();
        let req = request.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::Builder::new()
            .name("jdc-tests".into())
            .spawn(move || {
                let started = Instant::now();
                let report = run(&req, &stop);
                log::info!(
                    "tests in {}: {} results in {:?}{}",
                    req.project.name,
                    report.outcomes.len(),
                    started.elapsed(),
                    report
                        .error
                        .as_ref()
                        .map_or(String::new(), |e| format!(", error: {e}"))
                );
                let _ = tx.send(report);
            })
            .expect("cannot start test thread");
        Self {
            rx,
            request,
            cancel,
        }
    }

    /// Stop the runner process.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn try_result(&self) -> Option<Report> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Report {
                error: Some("test run crashed".into()),
                ..Default::default()
            }),
        }
    }
}

/// A fresh temporary report path.
fn temp_report(ext: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "jdc-tests-{}-{}.{ext}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

pub fn find_dotnet() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("JDC_DOTNET").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let name = if cfg!(windows) {
        "dotnet.exe"
    } else {
        "dotnet"
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn run(req: &Request, cancel: &AtomicBool) -> Report {
    match req.project.runner {
        Some(Framework::Vitest | Framework::Jest) => run_js(req, cancel),
        Some(fw) if fw.is_dotnet() => run_dotnet(req, cancel),
        _ => Report {
            error: Some(format!("No test runner found for {}", req.project.name)),
            ..Default::default()
        },
    }
}

fn run_js(req: &Request, cancel: &AtomicBool) -> Report {
    let vitest = req.project.runner == Some(Framework::Vitest);
    let (tool, env) = if vitest {
        ("vitest", "JDC_VITEST")
    } else {
        ("jest", "JDC_JEST")
    };
    let Some(bin) = process::find_node_tool(tool, env, &req.project.root) else {
        return Report {
            error: Some(format!("{tool} not found (npm i -D {tool})")),
            ..Default::default()
        };
    };
    let out_file = temp_report("json");
    let mut args: Vec<OsString> = if vitest {
        vec!["run".into(), "--reporter=json".into()]
    } else {
        vec!["--json".into()]
    };
    args.push(format!("--outputFile={}", out_file.display()).into());
    args.extend(req.files.iter().map(|f| f.as_os_str().to_owned()));
    if let Some(f) = &req.filter {
        args.push("-t".into());
        args.push(f.into());
    }
    let refs: Vec<&std::ffi::OsStr> = args.iter().map(OsString::as_os_str).collect();
    let out =
        match process::run_cancellable(&bin, &refs, &req.project.root, "", RUN_TIMEOUT, cancel) {
            Ok(o) => o,
            Err(e) => {
                return Report {
                    error: Some(e),
                    ..Default::default()
                };
            }
        };
    let json = std::fs::read_to_string(&out_file).unwrap_or_else(|_| out.stdout.clone());
    let _ = std::fs::remove_file(&out_file);
    match parse_jest_json(&json, &req.project.root) {
        Ok(outcomes) => Report {
            outcomes,
            ..Default::default()
        },
        Err(e) => Report {
            error: Some(
                process::first_error_line(if vitest { "Vitest" } else { "Jest" }, &out.stderr)
                    .unwrap_or(e),
            ),
            ..Default::default()
        },
    }
}

/// Test runs can take a while (builds, many tests).
const RUN_TIMEOUT: Duration = Duration::from_secs(600);

fn run_dotnet(req: &Request, cancel: &AtomicBool) -> Report {
    let Some(bin) = find_dotnet() else {
        return Report {
            error: Some("dotnet not found (install the .NET SDK)".into()),
            ..Default::default()
        };
    };
    let csproj = req
        .project
        .csproj
        .clone()
        .unwrap_or_else(|| req.project.root.clone());
    let trx = temp_report("trx");
    let mut args: Vec<OsString> = vec![
        "test".into(),
        csproj.into_os_string(),
        "--nologo".into(),
        "--logger".into(),
        format!("trx;LogFileName={}", trx.display()).into(),
    ];
    if let Some(f) = &req.filter {
        args.push("--filter".into());
        args.push(f.into());
    }
    let refs: Vec<&std::ffi::OsStr> = args.iter().map(OsString::as_os_str).collect();
    let out =
        match process::run_cancellable(&bin, &refs, &req.project.root, "", RUN_TIMEOUT, cancel) {
            Ok(o) => o,
            Err(e) => {
                return Report {
                    error: Some(e),
                    ..Default::default()
                };
            }
        };
    let diagnostics = Some(roslyn::parse_build_output(&out.stdout, &req.project.root));
    let xml = std::fs::read_to_string(&trx).unwrap_or_default();
    let _ = std::fs::remove_file(&trx);
    let mut report = Report {
        diagnostics,
        ..Default::default()
    };
    if xml.is_empty() {
        let has_errors = report.diagnostics.as_ref().is_some_and(|d| {
            d.values()
                .flatten()
                .any(|d| d.severity == crate::lint::Severity::Error)
        });
        report.error = Some(if has_errors {
            "Build failed: see Problems".into()
        } else {
            roslyn::dotnet_failure(&format!("{}\n{}", out.stdout, out.stderr), out.code)
        });
        return report;
    }
    match parse_trx(&xml) {
        Ok(o) => report.outcomes = o,
        Err(e) => report.error = Some(e),
    }
    report
}

// Jest / Vitest JSON report.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JestReport {
    #[serde(default)]
    test_results: Vec<JestFile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JestFile {
    name: String,
    #[serde(default)]
    assertion_results: Vec<JestTest>,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JestTest {
    #[serde(default)]
    ancestor_titles: Vec<String>,
    title: String,
    status: String,
    duration: Option<f64>,
    #[serde(default)]
    failure_messages: Vec<String>,
}

pub fn parse_jest_json(json: &str, root: &Path) -> Result<Vec<Outcome>, String> {
    let report: JestReport =
        serde_json::from_str(json.trim()).map_err(|e| format!("unreadable test report ({e})"))?;
    let mut out = Vec::new();
    for f in report.test_results {
        let file = root.join(&f.name);
        let file = std::fs::canonicalize(&file).unwrap_or(file);
        // A file that failed to load: every test in it fails with that message.
        if f.assertion_results.is_empty() && !f.message.is_empty() {
            out.push(Outcome {
                key: Key::Js {
                    file: file.clone(),
                    path: Vec::new(),
                },
                status: Status::Failed,
                duration: None,
                message: first_line(&f.message),
                detail: full_text(&f.message),
                row: None,
            });
        }
        for t in f.assertion_results {
            let row = Some(t.title.clone());
            let mut path = t.ancestor_titles;
            path.push(t.title);
            out.push(Outcome {
                key: Key::Js {
                    file: file.clone(),
                    path,
                },
                status: match t.status.as_str() {
                    "passed" => Status::Passed,
                    "failed" => Status::Failed,
                    _ => Status::Skipped,
                },
                duration: t
                    .duration
                    .map(|ms| Duration::from_secs_f64(ms.max(0.0) / 1000.0)),
                message: t.failure_messages.first().and_then(|m| first_line(m)),
                detail: t.failure_messages.first().and_then(|m| full_text(m)),
                row,
            });
        }
    }
    Ok(out)
}

/// Longest failure text kept for display.
const DETAIL_LINES: usize = 60;
const DETAIL_CHARS: usize = 6000;

/// A failure message for display: ANSI colors stripped, trailing blanks removed, capped.
fn full_text(msg: &str) -> Option<String> {
    let clean = strip_ansi(msg);
    let mut lines: Vec<&str> = clean.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    if lines.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i == DETAIL_LINES || out.len() + l.len() > DETAIL_CHARS {
            out.push('…');
            return Some(out);
        }
        out.push_str(l);
        out.push('\n');
    }
    out.pop();
    Some(out)
}

/// First meaningful line of a failure message, without ANSI colors.
fn first_line(msg: &str) -> Option<String> {
    let clean = strip_ansi(msg);
    clean
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(200).collect())
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Results from a TRX file (any .NET test framework).
pub fn parse_trx(xml: &str) -> Result<Vec<Outcome>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    // testId -> full name from the test definitions.
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    struct Pending {
        test_id: String,
        test_name: String,
        outcome: String,
        duration: Option<Duration>,
        message: Option<String>,
        stack: Option<String>,
    }
    let mut results: Vec<Pending> = Vec::new();
    let mut current_def: Option<String> = None;
    let mut in_result = false;
    let mut in_message = false;
    let mut in_stack = false;
    let attr = |e: &quick_xml::events::BytesStart, key: &str| -> Option<String> {
        e.attributes()
            .flatten()
            .find(|a| a.key.as_ref() == key)
            .and_then(|a| {
                a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .ok()
                    .map(|v| v.into_owned())
            })
    };
    loop {
        match reader.read_event() {
            Ok(ev @ (Event::Start(_) | Event::Empty(_))) => {
                let has_children = matches!(ev, Event::Start(_));
                let (Event::Start(e) | Event::Empty(e)) = ev else {
                    unreachable!()
                };
                match e.name().as_ref() {
                    "UnitTest" => current_def = attr(&e, "id"),
                    "TestMethod" => {
                        if let (Some(id), Some(class), Some(name)) =
                            (current_def.clone(), attr(&e, "className"), attr(&e, "name"))
                        {
                            // className may carry the assembly: "Ns.Class, Assembly".
                            let class =
                                class.split(',').next().unwrap_or(&class).trim().to_string();
                            names.insert(id, format!("{class}.{}", strip_params(&name)));
                        }
                    }
                    "UnitTestResult" => {
                        results.push(Pending {
                            test_id: attr(&e, "testId").unwrap_or_default(),
                            test_name: attr(&e, "testName").unwrap_or_default(),
                            outcome: attr(&e, "outcome").unwrap_or_default(),
                            duration: attr(&e, "duration").and_then(|d| parse_duration(&d)),
                            message: None,
                            stack: None,
                        });
                        in_result = has_children;
                    }
                    "Message" if in_result => in_message = true,
                    "StackTrace" if in_result => in_stack = true,
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if in_message || in_stack => {
                let raw = t.into_inner();
                let text = quick_xml::escape::unescape(&raw)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| raw.into_owned());
                if let Some(r) = results.last_mut() {
                    let slot = if in_message {
                        &mut r.message
                    } else {
                        &mut r.stack
                    };
                    slot.get_or_insert_with(String::new).push_str(&text);
                }
            }
            // `&#xA;`, `&amp;` ... arrive as their own events.
            Ok(Event::GeneralRef(r)) if in_message || in_stack => {
                let c = match r.resolve_char_ref() {
                    Ok(Some(c)) => Some(c),
                    _ => match r.into_inner().as_ref() {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    },
                };
                if let (Some(c), Some(res)) = (c, results.last_mut()) {
                    let slot = if in_message {
                        &mut res.message
                    } else {
                        &mut res.stack
                    };
                    slot.get_or_insert_with(String::new).push(c);
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                "Message" => in_message = false,
                "StackTrace" => in_stack = false,
                "UnitTestResult" => in_result = false,
                "UnitTest" => current_def = None,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("unreadable TRX report: {e}")),
            _ => {}
        }
    }
    Ok(results
        .into_iter()
        .map(|r| {
            let full = names
                .get(&r.test_id)
                .cloned()
                .unwrap_or_else(|| strip_params(&r.test_name).to_string());
            // The message, then the first lines of the stack trace.
            let detail = r.message.as_deref().map(|m| {
                let mut text = m.trim_end().to_string();
                if let Some(stack) = &r.stack {
                    let lines: Vec<&str> = stack
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .collect();
                    for l in lines.iter().take(8) {
                        text.push_str("\n  ");
                        text.push_str(l);
                    }
                    if lines.len() > 8 {
                        text.push_str("\n  …");
                    }
                }
                text
            });
            Outcome {
                key: Key::Dotnet(full),
                status: match r.outcome.as_str() {
                    "Passed" => Status::Passed,
                    "Failed" | "Error" | "Timeout" | "Aborted" => Status::Failed,
                    _ => Status::Skipped,
                },
                duration: r.duration,
                message: r.message.as_deref().and_then(first_line),
                detail: detail.as_deref().and_then(full_text),
                row: r.test_name.contains('(').then(|| r.test_name.clone()),
            }
        })
        .collect())
}

/// `Method(x: 1, s: "a")` -> `Method`.
fn strip_params(name: &str) -> &str {
    name.split('(').next().unwrap_or(name).trim()
}

/// TRX durations look like `00:00:01.2345678`.
fn parse_duration(s: &str) -> Option<Duration> {
    let mut parts = s.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    Some(Duration::from_secs_f64(h * 3600.0 + m * 60.0 + sec))
}

/// Every outcome that belongs to `test` (tests check `OutcomeIndex` gives the same).
#[cfg(test)]
pub fn match_outcome<'a>(test: &TestCase, outcomes: &'a [Outcome]) -> Vec<&'a Outcome> {
    let all: Vec<&Outcome> = outcomes
        .iter()
        .filter(|o| outcome_matches(test, o))
        .collect();
    assert_eq!(OutcomeIndex::new(outcomes).matching(test), all);
    all
}

fn outcome_matches(test: &TestCase, o: &Outcome) -> bool {
    match &o.key {
        Key::Dotnet(name) => test.found.full_name.as_deref() == Some(name.as_str()),
        Key::Js { file, path } => {
            if *file != test.file {
                return false;
            }
            // A file-level failure (the file did not load) applies to all its tests.
            if path.is_empty() {
                return true;
            }
            let (title, suite) = path.split_last().expect("non-empty");
            suite == test.found.suite.as_slice() && title_matches(&test.found.name, title)
        }
    }
}

/// A report's outcomes by .NET name and by JS file, so matching every test of a big run
/// looks at its own few outcomes instead of all of them.
pub struct OutcomeIndex<'a> {
    dotnet: HashMap<&'a str, Vec<&'a Outcome>>,
    js: HashMap<&'a Path, Vec<&'a Outcome>>,
}

impl<'a> OutcomeIndex<'a> {
    pub fn new(outcomes: &'a [Outcome]) -> Self {
        let mut ix = OutcomeIndex {
            dotnet: HashMap::new(),
            js: HashMap::new(),
        };
        for o in outcomes {
            match &o.key {
                Key::Dotnet(name) => ix.dotnet.entry(name.as_str()).or_default().push(o),
                Key::Js { file, .. } => ix.js.entry(file.as_path()).or_default().push(o),
            }
        }
        ix
    }

    /// Same as `match_outcome` over all the outcomes.
    pub fn matching(&self, test: &TestCase) -> Vec<&'a Outcome> {
        let mut out: Vec<&'a Outcome> = Vec::new();
        if let Some(name) = test.found.full_name.as_deref()
            && let Some(list) = self.dotnet.get(name)
        {
            out.extend(list.iter().copied());
        }
        if let Some(list) = self.js.get(test.file.as_path()) {
            out.extend(list.iter().copied().filter(|o| outcome_matches(test, o)));
        }
        out
    }
}

/// `it.each` titles: `accepts %s` matches `accepts a@b.fi`; `${…}` matches anything.
fn title_matches(pattern: &str, title: &str) -> bool {
    if pattern == title {
        return true;
    }
    let has_placeholder =
        pattern.contains('%') || pattern.contains("${…}") || pattern.contains('$');
    if !has_placeholder {
        return false;
    }
    // Split on placeholders and match the literal pieces in order.
    let mut pieces: Vec<String> = vec![String::new()];
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '%' if chars.peek().is_some_and(|d| "sdifjop#$%".contains(*d)) => {
                chars.next();
                pieces.push(String::new());
            }
            '$' if chars.peek() == Some(&'{') => {
                for d in chars.by_ref() {
                    if d == '}' {
                        break;
                    }
                }
                pieces.push(String::new());
            }
            '$' if chars.peek().is_some_and(|d| d.is_alphanumeric()) => {
                while chars
                    .peek()
                    .is_some_and(|d| d.is_alphanumeric() || *d == '.' || *d == '_')
                {
                    chars.next();
                }
                pieces.push(String::new());
            }
            _ => pieces.last_mut().expect("non-empty").push(c),
        }
    }
    let mut rest = title;
    for (i, p) in pieces.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(p.as_str()) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == pieces.len() - 1 {
            return rest.ends_with(p.as_str());
        } else {
            match rest.find(p.as_str()) {
                Some(k) => rest = &rest[k + p.len()..],
                None => return false,
            }
        }
    }
    rest.is_empty()
}

/// Matched outcomes combined into one result.
#[derive(Debug, Clone, PartialEq)]
pub struct Combined {
    pub status: Status,
    pub duration: Option<Duration>,
    pub message: Option<String>,
    pub detail: Option<String>,
}

/// Combine matched outcomes into one result. With several data rows, the detail lists each
/// failing row under its own heading.
pub fn combine(matched: &[&Outcome]) -> Option<Combined> {
    if matched.is_empty() {
        return None;
    }
    let failed = matched.iter().find(|o| o.status == Status::Failed);
    let status = if failed.is_some() {
        Status::Failed
    } else if matched.iter().any(|o| o.status == Status::Passed) {
        Status::Passed
    } else {
        Status::Skipped
    };
    let duration = matched
        .iter()
        .filter_map(|o| o.duration)
        .reduce(|a, b| a + b);
    let message = failed.and_then(|o| o.message.clone());
    let failing: Vec<&&Outcome> = matched
        .iter()
        .filter(|o| o.status == Status::Failed)
        .collect();
    let detail = if matched.len() > 1 && !failing.is_empty() {
        let parts: Vec<String> = failing
            .iter()
            .map(|o| {
                let head = o.row.clone().unwrap_or_else(|| "row".into());
                let body = o
                    .detail
                    .clone()
                    .or_else(|| o.message.clone())
                    .unwrap_or_default();
                format!("── {head} ──\n{body}")
            })
            .collect();
        full_text(&parts.join("\n"))
    } else {
        failed.and_then(|o| o.detail.clone().or_else(|| o.message.clone()))
    };
    Some(Combined {
        status,
        duration,
        message,
        detail,
    })
}

/// A regex for `-t` matching a test or suite by its full name (suite names + title, joined by
/// spaces as Vitest and Jest do). Placeholders in `.each` titles match anything.
pub fn name_pattern(path: &[String], whole: bool) -> String {
    let joined = path.join(" ");
    let mut out = String::from("^");
    let mut chars = joined.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '%' if chars.peek().is_some_and(|d| "sdifjop#".contains(*d)) => {
                chars.next();
                out.push_str(".*");
            }
            '$' if chars.peek() == Some(&'{') => {
                for d in chars.by_ref() {
                    if d == '}' {
                        break;
                    }
                }
                out.push_str(".*");
            }
            c if "\\.+*?()|[]{}^$".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    if whole {
        out.push('$');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_explorer::{Found, hash_text};

    fn case(file: &str, suite: &[&str], name: &str, full: Option<&str>) -> TestCase {
        TestCase {
            id: name.into(),
            file: PathBuf::from(file),
            found: Found {
                name: name.into(),
                suite: suite.iter().map(|s| s.to_string()).collect(),
                line: 0,
                end_line: 0,
                hash: hash_text(name),
                full_name: full.map(String::from),
                framework: Framework::Vitest,
            },
            result: None,
            running: false,
            running_hash: 0,
            runnable: true,
        }
    }

    #[test]
    fn parses_jest_style_json_and_matches_each_rows() {
        let json = r#"{"numTotalTests":3,"testResults":[{"name":"/w/tests/a.spec.js","status":"failed","message":"",
          "assertionResults":[
            {"ancestorTitles":["isEmail"],"title":"accepts a@b.fi","status":"passed","duration":2.5,"failureMessages":[]},
            {"ancestorTitles":["isEmail"],"title":"accepts c@d.fi","status":"failed","duration":1,
             "failureMessages":["\u001b[31mAssertionError: expected false to be true\u001b[39m\n    at x"]},
            {"ancestorTitles":[],"title":"top","status":"skipped","failureMessages":[]}]}]}"#;
        let o = parse_jest_json(json, Path::new("/w")).unwrap();
        assert_eq!(o.len(), 3);
        let t = case("/w/tests/a.spec.js", &["isEmail"], "accepts %s", None);
        let m = match_outcome(&t, &o);
        assert_eq!(m.len(), 2);
        let c = combine(&m).unwrap();
        assert_eq!(c.status, Status::Failed);
        assert_eq!(c.duration, Some(Duration::from_secs_f64(0.0035)));
        assert_eq!(
            c.message.unwrap(),
            "AssertionError: expected false to be true"
        );
        // Two rows, one failed: the detail names the failing row and keeps the stack.
        let d = c.detail.unwrap();
        assert!(
            d.starts_with("── accepts c@d.fi ──\nAssertionError: expected false to be true"),
            "{d}"
        );
        assert!(d.contains("at x"), "{d}");
        let top = case("/w/tests/a.spec.js", &[], "top", None);
        assert_eq!(
            combine(&match_outcome(&top, &o)).unwrap().status,
            Status::Skipped
        );
        assert!(parse_jest_json("nope", Path::new("/")).is_err());
    }

    #[test]
    fn parses_trx_with_theory_rows() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<TestRun xmlns="http://microsoft.com/schemas/VisualStudio/TeamTest/2010">
  <Results>
    <UnitTestResult testId="1" testName="Inventory.Tests.ProductServiceTests.GetAll" outcome="Passed" duration="00:00:00.0120000" />
    <UnitTestResult testId="2" testName="Inventory.Tests.ProductServiceTests.Parses(n: 1)" outcome="Passed" duration="00:00:00.0010000" />
    <UnitTestResult testId="3" testName="Inventory.Tests.ProductServiceTests.Parses(n: 2)" outcome="Failed" duration="00:00:00.0010000">
      <Output><ErrorInfo><Message>Assert.True() Failure&#xA;Expected: True</Message><StackTrace>   at Inventory.Tests.ProductServiceTests.Parses(Int32 n) in /w/T.cs:line 9</StackTrace></ErrorInfo></Output>
    </UnitTestResult>
  </Results>
  <TestDefinitions>
    <UnitTest name="GetAll" id="1"><TestMethod className="Inventory.Tests.ProductServiceTests, Inventory.Tests" name="GetAll" /></UnitTest>
    <UnitTest name="Parses(n: 1)" id="2"><TestMethod className="Inventory.Tests.ProductServiceTests" name="Parses" /></UnitTest>
  </TestDefinitions>
</TestRun>"#;
        let o = parse_trx(xml).unwrap();
        assert_eq!(o.len(), 3);
        let t = case(
            "/x.cs",
            &[],
            "Parses",
            Some("Inventory.Tests.ProductServiceTests.Parses"),
        );
        let c = combine(&match_outcome(&t, &o)).unwrap();
        assert_eq!(c.status, Status::Failed);
        assert_eq!(c.message.unwrap(), "Assert.True() Failure");
        let d = c.detail.unwrap();
        assert!(d.starts_with("── Inventory.Tests.ProductServiceTests.Parses(n: 2) ──\nAssert.True() Failure\nExpected: True"), "{d}");
        assert!(
            d.contains("  at Inventory.Tests.ProductServiceTests.Parses"),
            "{d}"
        );
        let g = case(
            "/x.cs",
            &[],
            "GetAll",
            Some("Inventory.Tests.ProductServiceTests.GetAll"),
        );
        let c = combine(&match_outcome(&g, &o)).unwrap();
        assert_eq!(
            (c.status, c.duration),
            (Status::Passed, Some(Duration::from_millis(12)))
        );
    }

    #[test]
    fn name_patterns_and_title_matching() {
        assert_eq!(
            name_pattern(&["isEmail".into(), "accepts %s".into()], true),
            "^isEmail accepts .*$"
        );
        assert_eq!(name_pattern(&["a.b (c)".into()], false), "^a\\.b \\(c\\)");
        assert!(title_matches("adds %i + %i", "adds 1 + 2"));
        assert!(title_matches(
            "handles ${…} names",
            "handles template names"
        ));
        assert!(title_matches("$a + $b", "1 + 2"));
        assert!(!title_matches("exact", "exactly"));
        assert!(!title_matches("adds %i", "subtracts 1"));
    }
}
