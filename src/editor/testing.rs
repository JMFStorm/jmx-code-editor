//! The test explorer in the editor: discovering tests (workspace scan, live re-parse of edited
//! buffers, file watcher), running them, Roslyn builds, re-running checks when config files
//! change, and the Tests page in the panel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use ropey::Rope;

use super::linting::Target;
use super::page::{PageBox, PageText};
use super::selection::Selection;
use super::{EDITOR, Editor};
use crate::lint::eslint::{self, LintJob, Scope};
use crate::lint::roslyn::{self, BuildJob};
use crate::lint::{Severity, Source};
use crate::syntax::Lang;
use crate::syntax::Token;
use crate::test_explorer::run::{self, Request, TestJob};
use crate::test_explorer::watch::FsWatcher;
use crate::test_explorer::{self as tx, FileTests, Framework, Project, RunResult, Status};

/// More files changed outside at once than this: lint the workspace once, not file by file.
const BULK_CHANGES: usize = 20;
/// Pages longer than this rebuild for their spinners only every `BIG_PAGE_SPIN`.
const BIG_PAGE_LINES: usize = 3000;
const BIG_PAGE_SPIN: Duration = Duration::from_secs(1);
pub const TESTS_TITLE: &str = "Tests";

/// Quiet time after an edit before a test file is re-parsed.
const REPARSE_DELAY: Duration = Duration::from_millis(250);
/// Quiet time after a save before a build starts (several saves: one build).
const BUILD_DELAY: Duration = Duration::from_millis(500);
/// How often the file watcher and background jobs are checked.
pub(super) const TEST_POLL: Duration = Duration::from_millis(50);
const WATCH_POLL: Duration = Duration::from_millis(500);
/// Spinner frame time.
const SPIN: Duration = Duration::from_millis(100);

/// A line of the Tests page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestRow {
    Project(PathBuf),
    File(PathBuf),
    /// A `describe` block or C# class: file and suite path.
    Suite(PathBuf, Vec<String>),
    Test(String),
    /// A line of the failure box under the selected test.
    Detail(String),
}

impl TestRow {
    fn key(&self) -> String {
        match self {
            TestRow::Project(p) => format!("p:{}", p.display()),
            TestRow::File(p) => format!("f:{}", p.display()),
            TestRow::Suite(p, path) => format!("s:{}::{}", p.display(), path.join("\u{1f}")),
            TestRow::Test(id) => format!("t:{id}"),
            TestRow::Detail(id) => format!("d:{id}"),
        }
    }

    fn is_group(&self) -> bool {
        !matches!(self, TestRow::Test(_) | TestRow::Detail(_))
    }
}

/// The status shown for a group: failed if any test failed, running if any runs, passed if
/// all ran and passed, otherwise not run.
fn summary(tests: &[&tx::TestCase]) -> Status {
    let st: Vec<Status> = tests.iter().map(|t| t.status()).collect();
    if st.contains(&Status::Failed) {
        Status::Failed
    } else if st.contains(&Status::Running) {
        Status::Running
    } else if st.contains(&Status::Passed)
        && st
            .iter()
            .all(|s| matches!(s, Status::Passed | Status::Skipped | Status::NotRunnable))
    {
        Status::Passed
    } else {
        Status::NotRun
    }
}

/// Which tests to run.
#[derive(Debug, Clone)]
pub enum RunWhat {
    All,
    Failed,
    Tests(Vec<String>),
    /// A `describe` block or C# class: (file, suite path, C# full name).
    Suite(PathBuf, Vec<String>, Option<String>),
}

/// Files found by the background workspace scan.
/// Files found by a scan, with the folders that were scanned.
type ScanResult = (Vec<PathBuf>, Vec<(PathBuf, Project, FileTests)>);

/// State of the test explorer and the build/watch machinery, owned by the editor.
#[derive(Default)]
pub struct TestState {
    pub tree: tx::TestTree,
    pub jobs: Vec<TestJob>,
    pub rows: Vec<Option<TestRow>>,
    pub collapsed: std::collections::HashSet<String>,
    /// Re-parse these files at these times (debounced edits and outside changes).
    reparse: HashMap<PathBuf, Instant>,
    /// Text revision of each open document when last looked at.
    seen: HashMap<PathBuf, u64>,
    scans: Vec<Receiver<ScanResult>>,
    pub watcher: Option<FsWatcher>,
    next_watch: Option<Instant>,
    pub builds: Vec<BuildJob>,
    /// Projects to build, when (debounced).
    build_wanted: HashMap<PathBuf, Instant>,
    /// ESLint config roots to lint again, when.
    lint_wanted: HashMap<PathBuf, Instant>,
    /// The last run problem of each project (shown on its row until its next run).
    pub run_errors: HashMap<PathBuf, String>,
    /// Results of the projects in the current run, summed up when the last one finishes.
    batch: Vec<(String, bool)>,
    batch_started: Option<Instant>,
    /// Last spinner frame drawn while something runs.
    last_spin: Option<Instant>,
    /// Last time big pages were rebuilt for their spinner glyphs.
    last_page_spin: Option<Instant>,
    /// Test project roots to re-scan, when (runner or framework config changed).
    rescan_wanted: HashMap<PathBuf, Instant>,
}

impl Editor {
    // ------------------------------------------------------------ discovery

    /// Start (or restart) test discovery and file watching for the workspace folders.
    pub(super) fn start_test_explorer(&mut self) {
        let folders: Vec<PathBuf> = self
            .workspace
            .as_ref()
            .map(|w| w.folders.clone())
            .unwrap_or_default();
        self.tests.watcher = if folders.is_empty() {
            None
        } else {
            FsWatcher::new(&folders)
        };
        self.tests.next_watch = self
            .tests
            .watcher
            .as_ref()
            .map(|_| Instant::now() + WATCH_POLL);
        // Check the whole project right away: ESLint everywhere, and a build of every C#
        // project (when dotnet is installed), quietly in the background.
        if !folders.is_empty() {
            self.lint_workspace(false);
            self.build_workspace();
        }
        self.start_symbols();
        self.scan_tests(folders);
    }

    /// Forget all tests (workspace closed).
    pub(super) fn stop_test_explorer(&mut self) {
        for j in &self.tests.jobs {
            j.cancel();
        }
        self.tests = TestState::default();
        self.stop_symbols();
        self.refresh_tests_page();
    }

    /// Walk `folders` in the background and discover every test file.
    fn scan_tests(&mut self, folders: Vec<PathBuf>) {
        if folders.is_empty() {
            return;
        }
        let (tx_, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("jdc-test-scan".into())
            .spawn(move || {
                let started = Instant::now();
                let mut projects: HashMap<PathBuf, Option<Project>> = HashMap::new();
                let mut out = Vec::new();
                for folder in &folders {
                    // Gitignored files are never tests, whatever the Explorer shows.
                    for entry in crate::workspace::files::source_walker(folder)
                        .build()
                        .flatten()
                    {
                        let path = entry.into_path();
                        if !tx::may_have_tests(&path) {
                            continue;
                        }
                        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
                        let project = projects
                            .entry(dir)
                            .or_insert_with(|| tx::detect_project(&path))
                            .clone();
                        let Some(project) = project else {
                            continue;
                        };
                        let Ok(text) = std::fs::read_to_string(&path) else {
                            continue;
                        };
                        let path = std::fs::canonicalize(&path).unwrap_or(path);
                        if let Some(found) = tx::discover(&path, &text, &project)
                            && !found.tests.is_empty()
                        {
                            out.push((path, project, found));
                        }
                    }
                }
                log::info!("test scan: {} files in {:?}", out.len(), started.elapsed());
                let _ = tx_.send((folders, out));
            })
            .expect("cannot start test scan");
        self.tests.scans.push(rx);
    }

    /// Re-parse one file: from its buffer when open, else from disk.
    fn reparse_tests(&mut self, path: &Path) {
        if self.ignored_source(path) {
            self.tests.tree.remove_file(path);
            return;
        }
        let text = match self.find_doc(|d| d.canonical.as_deref() == Some(path)) {
            Some((p, i)) => self.panes[p].docs[i].rope.to_string(),
            None => match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(_) => {
                    self.tests.tree.remove_file(path);
                    return;
                }
            },
        };
        let Some(pi) = self.tests.tree.project_for(path) else {
            self.tests.tree.remove_file(path);
            return;
        };
        let project = self.tests.tree.projects[pi].clone();
        match tx::discover(path, &text, &project) {
            Some(found) => self.tests.tree.replace_file(path, found),
            None => self.tests.tree.remove_file(path),
        }
    }

    /// A file that never holds tests: gitignored (or in node_modules) within the workspace.
    fn ignored_source(&self, path: &Path) -> bool {
        let folders = self
            .workspace
            .as_ref()
            .map_or(&[][..], |w| w.folders.as_slice());
        crate::workspace::files::is_gitignored(path, folders)
    }

    /// Note text changes in open test files (and C# files) so they are re-parsed shortly.
    /// Called after every command and when background results arrive.
    pub(super) fn track_test_edits(&mut self) {
        let now = Instant::now();
        let mut changed: Vec<PathBuf> = Vec::new();
        for doc in self.panes.iter().flat_map(|p| p.docs.iter()) {
            let Some(path) = &doc.canonical else {
                continue;
            };
            if !tx::may_have_tests(path) {
                continue;
            }
            if self.tests.seen.get(path) != Some(&doc.revision) {
                // Just opened, unedited, after the workspace scan: the scan already parsed
                // this very text (re-parsing would re-sort every test and rebuild the page).
                let fresh = !self.tests.seen.contains_key(path)
                    && !doc.is_dirty()
                    && self.tests.scans.is_empty()
                    && self.workspace.is_some();
                if !fresh {
                    changed.push(path.clone());
                }
                self.tests.seen.insert(path.clone(), doc.revision);
            }
        }
        for p in changed {
            self.tests.reparse.insert(p, now + REPARSE_DELAY);
        }
    }

    /// Collect background test work. Returns true if anything changed.
    pub(super) fn poll_tests(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        // Workspace scan results.
        let mut done: Vec<ScanResult> = Vec::new();
        self.tests.scans.retain(|rx| match rx.try_recv() {
            Ok(r) => {
                done.push(r);
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        for (roots, found) in done {
            // Tests under the scanned folders that the scan did not find are gone (deleted,
            // or now gitignored); open documents are re-parsed from their buffers.
            let found_files: std::collections::HashSet<PathBuf> =
                found.iter().map(|(p, _, _)| p.clone()).collect();
            let stale: Vec<PathBuf> = self
                .tests
                .tree
                .tests
                .iter()
                .map(|t| t.file.clone())
                .filter(|f| roots.iter().any(|r| f.starts_with(r)) && !found_files.contains(f))
                .collect();
            for f in stale {
                if self
                    .find_doc(|d| d.canonical.as_deref() == Some(f.as_path()))
                    .is_some()
                {
                    self.tests.reparse.insert(f, now);
                } else {
                    self.tests.tree.remove_file(&f);
                }
            }
            let mut merged = Vec::new();
            for (path, project, ft) in found {
                if !self
                    .tests
                    .tree
                    .projects
                    .iter()
                    .any(|p| p.root == project.root)
                {
                    self.tests.tree.projects.push(project);
                }
                // Open documents are parsed from their buffers instead.
                if self
                    .find_doc(|d| d.canonical.as_deref() == Some(path.as_path()))
                    .is_some()
                {
                    self.tests.reparse.insert(path, now);
                } else {
                    merged.push((path, ft));
                }
            }
            self.tests.tree.replace_files(merged);
            crate::perf::trim_memory();
            self.tests
                .tree
                .projects
                .sort_by_key(|a| a.name.to_lowercase());
            changed = true;
        }
        // Outside changes.
        if self.tests.next_watch.is_some_and(|t| now >= t) {
            self.tests.next_watch = Some(now + WATCH_POLL);
            let changes = self
                .tests
                .watcher
                .as_ref()
                .map(|w| w.drain())
                .unwrap_or_default();
            // A burst (git checkout, a code generator): one lint of the workspace instead of a
            // process per file.
            let bulk = changes.len() > BULK_CHANGES;
            for c in &changes {
                changed |= self.outside_change(&c.path, c.removed, bulk);
            }
            if bulk {
                log::info!(
                    "{} files changed outside: one workspace lint",
                    changes.len()
                );
                self.lint_workspace(false);
            }
        }
        // Debounced re-parses.
        let due: Vec<PathBuf> = self
            .tests
            .reparse
            .iter()
            .filter(|(_, t)| now >= **t)
            .map(|(p, _)| p.clone())
            .collect();
        for p in due {
            self.tests.reparse.remove(&p);
            self.reparse_tests(&p);
            changed = true;
        }
        changed |= self.poll_test_jobs();
        changed |= self.poll_checks(now);
        // Keep the spinners turning while tests, lints or builds run.
        let busy = !self.tests.jobs.is_empty() || self.linting() || self.tests_building();
        if busy
            && self
                .tests
                .last_spin
                .is_none_or(|t| now.duration_since(t) >= SPIN)
        {
            self.tests.last_spin = Some(now);
            // The spinners in tab titles and the status bar turn on every redraw; the pages'
            // own spinner glyphs are redrawn by rebuilding them, which on a big page costs tens
            // of milliseconds: those turn once a second.
            let big = |l: Option<usize>| l.is_some_and(|n| n > BIG_PAGE_LINES);
            let page_due = self
                .tests
                .last_page_spin
                .is_none_or(|t| now.duration_since(t) >= BIG_PAGE_SPIN);
            let tests_lines = self
                .tests_page()
                .map(|(p, i)| self.panes[p].docs[i].rope.len_lines());
            let problems_lines = self
                .find_doc(|d| d.virtual_name.as_deref() == Some(super::PROBLEMS_TITLE))
                .map(|(p, i)| self.panes[p].docs[i].rope.len_lines());
            if page_due || !big(problems_lines) {
                self.refresh_problems_page();
            }
            // Changed data always shows at once.
            if changed || page_due || !big(tests_lines) {
                self.refresh_tests_page();
            }
            if page_due {
                self.tests.last_page_spin = Some(now);
            }
            return true;
        } else if !busy {
            self.tests.last_spin = None;
        }
        if changed {
            self.refresh_tests_page();
        }
        changed
    }

    /// A file changed outside the editor (watcher).
    fn outside_change(&mut self, path: &Path, removed: bool, bulk: bool) -> bool {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let open = self.find_doc(|d| d.canonical.as_deref() == Some(path.as_path()));
        let mut changed = false;
        self.symbols_file_changed(&path, removed);
        if tx::may_have_tests(&path) {
            if removed {
                self.tests.tree.remove_file(&path);
                changed = true;
            } else if open.is_none() {
                self.tests
                    .reparse
                    .insert(path.clone(), Instant::now() + REPARSE_DELAY);
            }
        }
        if !removed && open.is_none() && !bulk {
            self.lint_file_on_disk(&path);
        }
        if !removed {
            self.config_changed(&path);
            // C# sources changed by other tools: rebuild for fresh compiler diagnostics.
            if tx::is_cs(&path)
                && open.is_none()
                && let Some(csproj) = roslyn::project_of(&path)
            {
                self.want_build(csproj);
            }
        }
        changed
    }

    // ------------------------------------------------------------ builds and config changes

    /// After a successful save: rebuild C# projects, re-lint for config files.
    pub(super) fn after_save_checks(&mut self, path: &Path) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if tx::is_cs(&path)
            && let Some(csproj) = roslyn::project_of(&path)
        {
            self.want_build(csproj);
        }
        self.config_changed(&path);
    }

    fn want_build(&mut self, csproj: PathBuf) {
        self.tests
            .build_wanted
            .insert(csproj, Instant::now() + BUILD_DELAY);
    }

    /// A configuration file was saved or changed: re-run the checks that depend on it.
    pub(super) fn config_changed(&mut self, path: &Path) {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_lowercase()) else {
            return;
        };
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let due = Instant::now() + BUILD_DELAY;
        let eslint_config = name.starts_with("eslint.config.")
            || name.starts_with(".eslintrc")
            || name == ".eslintignore";
        let js_config =
            name == "package.json" || name == "tsconfig.json" || name == "jsconfig.json";
        let runner_config = name.starts_with("vitest.config.")
            || name.starts_with("vite.config.")
            || name.starts_with("jest.config.");
        let cs_project = name.ends_with(".csproj");
        if name == crate::test_explorer::odin_config::FILE_NAME {
            // Odin test flags: the next run uses them; old run errors may be fixed.
            self.tests
                .run_errors
                .retain(|root, _| !root.starts_with(&dir));
        }
        let ignore_file = name == ".gitignore" || name == ".ignore";
        if ignore_file {
            // What counts as a test (or project) may have changed below this folder.
            self.tests.rescan_wanted.insert(dir.clone(), due);
        }
        let cs_config = name == "directory.build.props"
            || name == "directory.build.targets"
            || name == "directory.packages.props"
            || name == ".editorconfig"
            || name == "global.json"
            || name == ".globalconfig"
            || name.ends_with(".ruleset");
        let mut what = Vec::new();
        if (eslint_config || js_config)
            && let Some(root) = eslint::config_root(&dir)
        {
            self.tests.lint_wanted.insert(root, due);
            what.push("re-linting");
        }
        if js_config || runner_config || cs_project {
            self.tests.rescan_wanted.insert(dir.clone(), due);
        }
        if cs_project {
            self.tests.build_wanted.insert(path.to_path_buf(), due);
            what.push("rebuilding");
        } else if cs_config {
            let projects = roslyn::projects_in(&dir);
            if !projects.is_empty() {
                what.push("rebuilding");
            }
            for p in projects {
                self.tests.build_wanted.insert(p, due);
            }
        }
        if !what.is_empty() {
            log::info!("{} changed: {}", path.display(), what.join(", "));
            self.info(format!("{name} changed: {}…", what.join(" and ")));
        }
    }

    /// Start due builds, lints and rescans; collect finished builds.
    fn poll_checks(&mut self, now: Instant) -> bool {
        let mut changed = false;
        // Builds: one at a time (projects that reference each other share outputs); the
        // next wanted build starts when the running one is done.
        let due: Vec<PathBuf> = if self.tests.builds.is_empty() {
            let mut ready: Vec<&PathBuf> = self
                .tests
                .build_wanted
                .iter()
                .filter(|(_, t)| now >= **t)
                .map(|(p, _)| p)
                .collect();
            ready.sort();
            ready.into_iter().take(1).cloned().collect()
        } else {
            Vec::new()
        };
        for csproj in due {
            self.tests.build_wanted.remove(&csproj);
            match run::find_dotnet() {
                Some(dotnet) => {
                    log::info!("roslyn build {}", csproj.display());
                    self.tests.builds.push(BuildJob::spawn(dotnet, csproj));
                }
                None => log::info!("no dotnet; skipping build of {}", csproj.display()),
            }
        }
        let mut i = 0;
        while i < self.tests.builds.len() {
            let Some(result) = self.tests.builds[i].try_result() else {
                i += 1;
                continue;
            };
            let job = self.tests.builds.remove(i);
            changed = true;
            match result {
                Ok(files) => {
                    self.apply_roslyn(&job.root(), files);
                }
                Err(e) => self.error(format!("dotnet build: {e}")),
            }
        }
        // Lints of whole ESLint projects (config changed).
        let due: Vec<PathBuf> = self
            .tests
            .lint_wanted
            .iter()
            .filter(|(_, t)| now >= **t)
            .map(|(p, _)| p.clone())
            .collect();
        for root in due {
            self.tests.lint_wanted.remove(&root);
            if let Some(bin) = eslint::find_eslint(&root) {
                self.lint_jobs
                    .retain(|j| j.scope != Scope::Root(root.clone()));
                self.lint_jobs.push(LintJob::root(bin, root));
                changed = true;
            }
        }
        // Re-scans of projects whose runner or framework config changed.
        let due: Vec<PathBuf> = self
            .tests
            .rescan_wanted
            .iter()
            .filter(|(_, t)| now >= **t)
            .map(|(p, _)| p.clone())
            .collect();
        for dir in due {
            self.tests.rescan_wanted.remove(&dir);
            self.tests.tree.projects.retain(|p| p.root != dir);
            self.scan_tests(vec![dir]);
            changed = true;
        }
        changed
    }

    /// Compiler diagnostics of a build of the project in `root` (and files it compiles from
    /// elsewhere).
    fn apply_roslyn(
        &mut self,
        root: &Path,
        files: std::collections::BTreeMap<PathBuf, Vec<crate::lint::Diagnostic>>,
    ) {
        let (outside, inside): (Vec<_>, Vec<_>) =
            files.into_iter().partition(|(p, _)| !p.starts_with(root));
        let (e, w) =
            inside
                .iter()
                .chain(&outside)
                .flat_map(|(_, d)| d)
                .fold((0, 0), |(e, w), d| match d.severity {
                    Severity::Error => (e + 1, w),
                    _ => (e, w + 1),
                });
        self.problems
            .set_under(Source::Roslyn, root, inside.into_iter().collect());
        for (p, d) in outside {
            self.problems.set_file(Source::Roslyn, &p, d);
        }
        if e > 0 {
            self.warn(format!(
                "Build: {e} error(s), {w} warning(s) (Problems: Ctrl+Shift+M)"
            ));
        }
        self.refresh_problems_page();
    }

    /// Ctrl+Shift+M also builds every C# project in the workspace.
    pub(super) fn build_workspace(&mut self) {
        let folders: Vec<PathBuf> = self
            .workspace
            .as_ref()
            .map(|w| w.folders.clone())
            .unwrap_or_default();
        let now = Instant::now();
        for csproj in folders.iter().flat_map(|f| roslyn::projects_in(f)) {
            self.tests.build_wanted.insert(csproj, now);
        }
    }

    // ------------------------------------------------------------ running

    pub(super) fn run_tests(&mut self, what: RunWhat) {
        let tree = &self.tests.tree;
        // The tests to run, grouped by project root.
        let selected: Vec<&tx::TestCase> = match &what {
            RunWhat::All => tree.tests.iter().filter(|t| t.runnable).collect(),
            RunWhat::Failed => tree
                .tests
                .iter()
                .filter(|t| t.status() == Status::Failed)
                .collect(),
            RunWhat::Tests(ids) => {
                let ids: std::collections::HashSet<&String> = ids.iter().collect();
                tree.tests
                    .iter()
                    .filter(|t| ids.contains(&t.id) && t.runnable)
                    .collect()
            }
            RunWhat::Suite(file, path, _) => tree
                .tests
                .iter()
                .filter(|t| &t.file == file && t.found.suite.starts_with(path) && t.runnable)
                .collect(),
        };
        if selected.is_empty() {
            let msg = match what {
                RunWhat::Failed => "No failed tests",
                _ if tree.tests.is_empty() => "No tests found",
                _ => "Nothing runnable selected",
            };
            self.info(msg);
            return;
        }
        let mut by_project: Vec<(Project, Vec<&tx::TestCase>)> = Vec::new();
        for t in selected {
            let Some(p) = tree.project_of(&t.file) else {
                continue;
            };
            match by_project.iter_mut().find(|(q, _)| q.root == p.root) {
                Some((_, list)) => list.push(t),
                None => by_project.push((p.clone(), vec![t])),
            }
        }
        let mut requests = Vec::new();
        let mut busy: Vec<String> = Vec::new();
        let mut no_runner: Vec<(PathBuf, String)> = Vec::new();
        for (project, tests) in by_project {
            if self
                .tests
                .jobs
                .iter()
                .any(|j| j.request.project.root == project.root)
            {
                busy.push(project.name.clone());
                continue;
            }
            if project.runner.is_none() {
                no_runner.push((project.root.clone(), project.name.clone()));
                continue;
            }
            let all_of_project = matches!(what, RunWhat::All);
            let dotnet = project.runner.is_some_and(Framework::is_dotnet);
            let mut files: Vec<PathBuf> = Vec::new();
            let mut seen_files = std::collections::HashSet::new();
            for t in &tests {
                if seen_files.insert(&t.file) {
                    files.push(t.file.clone());
                }
            }
            let odin = project.runner == Some(Framework::Odin);
            let filter = if odin {
                // `ODIN_TEST_NAMES`: package.name, comma separated.
                match &what {
                    RunWhat::All => None,
                    _ => Some(
                        tests
                            .iter()
                            .filter_map(|t| t.found.full_name.as_ref())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(","),
                    ),
                }
            } else if dotnet {
                match &what {
                    RunWhat::All => None,
                    RunWhat::Suite(_, _, Some(full)) => Some(format!("FullyQualifiedName~{full}.")),
                    _ => Some(
                        tests
                            .iter()
                            .filter_map(|t| t.found.full_name.as_ref())
                            .map(|n| format!("FullyQualifiedName={n}"))
                            .collect::<Vec<_>>()
                            .join("|"),
                    ),
                }
            } else {
                match &what {
                    RunWhat::Tests(_) if tests.len() == 1 => {
                        let t = tests[0];
                        let mut path = t.found.suite.clone();
                        path.push(t.found.name.clone());
                        Some(run::name_pattern(&path, true))
                    }
                    RunWhat::Suite(_, path, _) => Some(run::name_pattern(path, false)),
                    _ => None,
                }
            };
            if all_of_project || dotnet || odin {
                files.clear();
            }
            let ids: Vec<(String, u64)> =
                tests.iter().map(|t| (t.id.clone(), t.found.hash)).collect();
            requests.push(Request {
                project,
                files,
                filter,
                ids,
            });
        }
        if self.tests.jobs.is_empty() {
            self.tests.batch.clear();
            self.tests.batch_started = Some(Instant::now());
        }
        for (root, name) in no_runner {
            let e = "no test runner (vitest or jest in package.json)".to_string();
            self.tests.batch.push((format!("{name}: {e}"), true));
            self.tests.run_errors.insert(root, e);
        }
        if !busy.is_empty() {
            self.info(format!("Tests are already running in {}", busy.join(", ")));
        }
        let count: usize = requests.iter().map(|r| r.ids.len()).sum();
        if requests.is_empty() && !self.tests.batch.is_empty() {
            self.finish_batch();
        }
        for req in requests {
            self.tests.run_errors.remove(&req.project.root);
            let wanted: HashMap<&String, u64> = req.ids.iter().map(|(id, h)| (id, *h)).collect();
            for t in self.tests.tree.tests.iter_mut() {
                if let Some(hash) = wanted.get(&t.id) {
                    t.running = true;
                    t.running_hash = *hash;
                }
            }
            log::info!(
                "running {} test(s) in {} (filter {:?})",
                req.ids.len(),
                req.project.name,
                req.filter
            );
            self.tests.jobs.push(TestJob::spawn(req));
        }
        if count > 0 {
            self.info(format!(
                "Running {count} test{}…",
                if count == 1 { "" } else { "s" }
            ));
        }
        self.refresh_tests_page();
    }

    fn poll_test_jobs(&mut self) -> bool {
        let mut changed = false;
        let mut i = 0;
        while i < self.tests.jobs.len() {
            let Some(report) = self.tests.jobs[i].try_result() else {
                i += 1;
                continue;
            };
            let job = self.tests.jobs.remove(i);
            changed = true;
            let root = job.request.project.root.clone();
            if let Some(diags) = report.diagnostics {
                self.apply_roslyn(&root, diags);
            }
            let ran: std::collections::HashSet<&str> =
                job.request.ids.iter().map(|(id, _)| id.as_str()).collect();
            let (mut passed, mut failed) = (0, 0);
            let outcomes = run::OutcomeIndex::new(&report.outcomes);
            for t in self.tests.tree.tests.iter_mut() {
                if !t.file.starts_with(&root) {
                    continue;
                }
                let was_running = ran.contains(t.id.as_str());
                if let Some(run::Combined {
                    status,
                    duration,
                    message,
                    detail,
                }) = run::combine(&outcomes.matching(t))
                    // Runners list filtered-out tests as skipped: only tests that were asked
                    // for (or that actually ran) get a new result.
                    .filter(|c| was_running || c.status != Status::Skipped)
                {
                    // The result belongs to the text that ran.
                    let hash = if was_running {
                        t.running_hash
                    } else {
                        t.found.hash
                    };
                    t.result = Some(RunResult {
                        status,
                        duration,
                        message,
                        detail,
                        hash,
                    });
                    match status {
                        Status::Passed => passed += 1,
                        Status::Failed => failed += 1,
                        _ => {}
                    }
                }
                if was_running {
                    t.running = false;
                }
            }
            let name = job.request.project.name.clone();
            match report.error {
                Some(e) => {
                    // The first line is the summary; the rest shows on the Tests page.
                    let first = e.lines().next().unwrap_or_default();
                    self.tests.batch.push((format!("{name}: {first}"), true));
                    self.tests.run_errors.insert(root, e);
                }
                None if failed > 0 => self
                    .tests
                    .batch
                    .push((format!("{name}: {failed} failed, {passed} passed"), true)),
                None => self
                    .tests
                    .batch
                    .push((format!("{name}: {passed} passed"), false)),
            }
            if self.tests.jobs.is_empty() {
                self.finish_batch();
            }
        }
        changed
    }

    /// The whole run is over: one message for all its projects.
    fn finish_batch(&mut self) {
        let parts = std::mem::take(&mut self.tests.batch);
        let secs = self
            .tests
            .batch_started
            .take()
            .map_or(0.0, |t| t.elapsed().as_secs_f32());
        let bad = parts.iter().any(|(_, bad)| *bad);
        let text = parts
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>()
            .join(" · ");
        let msg = format!("Tests: {text} ({secs:.1} s)");
        if bad {
            self.error(msg);
        } else {
            self.info(msg);
        }
    }

    /// Esc on the Tests page: stop running tests.
    pub(super) fn cancel_tests(&mut self) -> bool {
        if self.tests.jobs.is_empty() {
            return false;
        }
        for j in &self.tests.jobs {
            j.cancel();
        }
        self.info("Stopping tests…");
        true
    }

    /// F5: the test (or suite) at the cursor; on the Tests page, the selected row.
    pub(super) fn run_test_at_cursor(&mut self) {
        if self.on_tests_page() {
            match self.tests_cursor_row() {
                Some(TestRow::Test(id) | TestRow::Detail(id)) => {
                    self.run_tests(RunWhat::Tests(vec![id]))
                }
                Some(TestRow::Suite(f, path)) => {
                    let full = self
                        .tests
                        .tree
                        .suites
                        .get(&f)
                        .and_then(|s| s.iter().find(|s| s.path == path))
                        .and_then(|s| s.full_name.clone());
                    self.run_tests(RunWhat::Suite(f, path, full));
                }
                Some(TestRow::File(f)) => {
                    self.run_tests(RunWhat::Tests(self.ids_in(|t| t.file == f)))
                }
                Some(TestRow::Project(r)) => {
                    self.run_tests(RunWhat::Tests(self.ids_in(|t| t.file.starts_with(&r))))
                }
                None => self.run_tests(RunWhat::All),
            }
            return;
        }
        let Some(doc) = self.panes[EDITOR].doc() else {
            return;
        };
        let Some(path) = doc.canonical.clone() else {
            return;
        };
        let line = doc.rope.char_to_line(doc.selection.head);
        // Make sure the tree reflects the buffer right now.
        if self.tests.reparse.remove(&path).is_some() {
            self.reparse_tests(&path);
        }
        let tree = &self.tests.tree;
        if let Some(t) = tree.test_at(&path, line) {
            let id = t.id.clone();
            self.run_tests(RunWhat::Tests(vec![id]));
        } else if let Some(s) = tree.suite_at(&path, line) {
            let what = RunWhat::Suite(path.clone(), s.path.clone(), s.full_name.clone());
            self.run_tests(what);
        } else if tree.tests.iter().any(|t| t.file == path) {
            let ids = self.ids_in(|t| t.file == path);
            self.run_tests(RunWhat::Tests(ids));
        } else {
            self.info("No test at the cursor (Shift+F5 runs all)");
        }
    }

    fn ids_in(&self, f: impl Fn(&tx::TestCase) -> bool) -> Vec<String> {
        self.tests
            .tree
            .tests
            .iter()
            .filter(|t| f(t))
            .map(|t| t.id.clone())
            .collect()
    }

    /// Test results on the editor's cursor line (failure message for the status bar).
    pub fn cursor_test_failure(&self) -> Option<String> {
        let doc = self.panes[EDITOR].doc()?;
        let path = doc.canonical.as_ref()?;
        let line = doc.rope.char_to_line(doc.selection.head);
        let t = self
            .tests
            .tree
            .tests
            .iter()
            .filter(|t| &t.file == path && t.found.line <= line && line <= t.found.end_line)
            .max_by_key(|t| t.found.line)?;
        let r = t.current_result()?;
        (r.status == Status::Failed).then(|| {
            format!(
                "Test failed: {}",
                r.message.clone().unwrap_or_else(|| t.found.name.clone())
            )
        })
    }

    /// Tests starting on each line of a file, for the editor gutter.
    pub fn test_marks(&self, path: &Path) -> Vec<(usize, Status)> {
        self.tests
            .tree
            .tests
            .iter()
            .filter(|t| t.file == path)
            .map(|t| (t.found.line, t.status()))
            .collect()
    }

    /// A C# build is running or about to start.
    pub(super) fn tests_building(&self) -> bool {
        !self.tests.builds.is_empty() || !self.tests.build_wanted.is_empty()
    }

    pub(super) fn tests_busy(&self) -> bool {
        !self.tests.jobs.is_empty()
            || !self.tests.builds.is_empty()
            || !self.tests.scans.is_empty()
            || !self.tests.reparse.is_empty()
            || !self.tests.build_wanted.is_empty()
            || !self.tests.lint_wanted.is_empty()
            || !self.tests.rescan_wanted.is_empty()
    }

    /// When background test work needs a look.
    pub(super) fn tests_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        let busy = (!self.tests.jobs.is_empty()
            || !self.tests.builds.is_empty()
            || !self.tests.scans.is_empty())
        .then(|| now + TEST_POLL);
        [
            busy,
            self.tests.reparse.values().min().copied(),
            self.tests.build_wanted.values().min().copied(),
            self.tests.lint_wanted.values().min().copied(),
            self.tests.rescan_wanted.values().min().copied(),
            self.tests.next_watch,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    // ------------------------------------------------------------ Tests page

    pub(super) fn show_tests(&mut self) {
        // In file mode, at least the open test files are listed.
        if self.workspace.is_none() {
            let open: Vec<PathBuf> = self
                .panes
                .iter()
                .flat_map(|p| p.docs.iter())
                .filter_map(|d| d.canonical.clone())
                .filter(|p| tx::may_have_tests(p))
                .collect();
            for p in open {
                self.reparse_tests(&p);
            }
        }
        let (text, rows, boxes) = self.tests_text();
        self.open_virtual(TESTS_TITLE, &text);
        self.tests.rows = rows;
        if let Some((p, i)) = self.tests_page() {
            self.panes[p].docs[i].syntax.set_lang(Lang::Tests);
            self.panes[p].docs[i].boxes = boxes;
        }
        self.refresh_tests_page();
    }

    fn tests_page(&self) -> Option<(usize, usize)> {
        self.find_doc(|d| d.virtual_name.as_deref() == Some(TESTS_TITLE))
    }

    pub(super) fn on_tests_page(&self) -> bool {
        self.has_doc() && self.doc().virtual_name.as_deref() == Some(TESTS_TITLE)
    }

    pub(crate) fn refresh_tests_page(&mut self) {
        let Some((p, i)) = self.tests_page() else {
            return;
        };
        let doc = &self.panes[p].docs[i];
        let line = doc.rope.char_to_line(doc.selection.head);
        let keep = self
            .tests
            .rows
            .get(line)
            .cloned()
            .flatten()
            .map(|r| match r {
                TestRow::Detail(id) => TestRow::Test(id),
                other => other,
            });
        let (text, rows, boxes) = self.tests_text();
        if self.panes[p].docs[i].rope == text.as_str() {
            self.tests.rows = rows;
            self.panes[p].docs[i].boxes = boxes;
            return;
        }
        let new_line = keep
            .and_then(|k| rows.iter().position(|r| r.as_ref() == Some(&k)))
            .unwrap_or(line);
        let doc = &mut self.panes[p].docs[i];
        doc.rope = Rope::from_str(&text);
        crate::perf::bump(&crate::perf::PAGE_REBUILDS);
        doc.syntax.set_lang(Lang::Tests);
        doc.boxes = boxes;
        let line = new_line.min(doc.rope.len_lines().saturating_sub(1));
        doc.selection = Selection::point(doc.rope.line_to_char(line));
        self.tests.rows = rows;
    }

    fn tests_text(&self) -> (String, Vec<Option<TestRow>>, Vec<PageBox>) {
        let tree = &self.tests.tree;
        let (passed, failed, not_run, running) = tree.counts();
        let mut lines: Vec<(String, Option<TestRow>)> = Vec::new();
        // Lines of the boxes with a project's run error (the output that tells why).
        let mut error_lines: std::collections::HashSet<usize> = Default::default();
        let mut head = format!("Tests: {passed} passed · {failed} failed · {not_run} not run");
        if running > 0 {
            head.push_str(&format!(
                " · {} {running} running",
                super::linting::spinner()
            ));
        }
        if !self.tests.scans.is_empty() {
            head.push_str("   (finding tests…)");
        }
        lines.push((head, None));
        lines.push((
            "F5 run · Shift+F5 all · Ctrl+F5 failed · r run · Space/←→ fold · -/+ all · Enter open"
                .into(),
            None,
        ));
        lines.push((String::new(), None));
        if tree.tests.is_empty() {
            lines.push((
                if self.workspace.is_some() {
                    "No tests found (spec/test files, stories, C# test projects)."
                } else {
                    "Open a workspace to find its tests."
                }
                .into(),
                None,
            ));
        }
        let arrow = |row: &TestRow| {
            if self.tests.collapsed.contains(&row.key()) {
                '▸'
            } else {
                '▾'
            }
        };
        // Group once: tests by project (one project lookup per file), then by file.
        let selected = self.tests_selected_id();
        let mut owner: HashMap<&Path, Option<usize>> = HashMap::new();
        let mut by_project: Vec<Vec<&tx::TestCase>> = vec![Vec::new(); tree.projects.len()];
        for t in &tree.tests {
            let pi = *owner.entry(t.file.as_path()).or_insert_with(|| {
                tree.project_of(&t.file)
                    .and_then(|p| tree.projects.iter().position(|q| q.root == p.root))
            });
            if let Some(pi) = pi {
                by_project[pi].push(t);
            }
        }
        for (pi, project) in tree.projects.iter().enumerate() {
            let tests = std::mem::take(&mut by_project[pi]);
            if tests.is_empty() {
                continue;
            }
            let row = TestRow::Project(project.root.clone());
            let ok = tests
                .iter()
                .filter(|t| t.status() == Status::Passed)
                .count();
            let runner = project.runner.map_or("no runner", Framework::name);
            let error = self.tests.run_errors.get(&project.root);
            let problem = error
                .and_then(|e| e.lines().next())
                .map(|e| format!("  — {e}"))
                .unwrap_or_default();
            lines.push((
                format!(
                    "{} {} {}  ({runner})  {ok}/{} passed{problem}",
                    arrow(&row),
                    status_glyph(summary(&tests)),
                    project.name,
                    tests.len()
                ),
                Some(row.clone()),
            ));
            // The rest of a run error (compiler / linker output) in a box under the row.
            if let Some(rest) = error.and_then(|e| e.split_once('\n')).map(|(_, r)| r) {
                let width = self.panel_width().saturating_sub(6).max(16);
                for l in wrap_text(rest, width) {
                    error_lines.insert(lines.len());
                    lines.push((format!("  │ {l}"), Some(row.clone())));
                }
            }
            if self.tests.collapsed.contains(&row.key()) {
                continue;
            }
            let mut files: Vec<(&PathBuf, Vec<&tx::TestCase>)> = Vec::new();
            let mut file_at: HashMap<&PathBuf, usize> = HashMap::new();
            for t in &tests {
                let k = *file_at.entry(&t.file).or_insert_with(|| {
                    files.push((&t.file, Vec::new()));
                    files.len() - 1
                });
                files[k].1.push(t);
            }
            for (file, in_file) in files {
                let row = TestRow::File(file.clone());
                let rel = file
                    .strip_prefix(&project.root)
                    .unwrap_or(file)
                    .to_string_lossy()
                    .replace('\\', "/");
                lines.push((
                    format!(
                        "  {} {} {rel}  ({})",
                        arrow(&row),
                        status_glyph(summary(&in_file)),
                        in_file.len()
                    ),
                    Some(row.clone()),
                ));
                if !self.tests.collapsed.contains(&row.key()) {
                    self.suite_lines(file, &[], &in_file, 4, &mut lines, selected.as_deref());
                }
            }
        }
        // Laid out at the panel's width: rows wrap under their name; failure boxes.
        let mut page: PageText<TestRow> = PageText::new(self.panel_width());
        let mut k = 0;
        while k < lines.len() {
            if error_lines.contains(&k) {
                let first = page.len();
                while error_lines.contains(&k) {
                    page.line(lines[k].0.clone(), lines[k].1.clone());
                    k += 1;
                }
                page.boxes.push(PageBox {
                    first,
                    last: page.len() - 1,
                    accent: Some(Token::Error),
                });
                continue;
            }
            if matches!(lines[k].1, Some(TestRow::Detail(_))) {
                let first = page.len();
                while k < lines.len() && matches!(lines[k].1, Some(TestRow::Detail(_))) {
                    page.line(lines[k].0.clone(), lines[k].1.clone());
                    k += 1;
                }
                page.boxes.push(PageBox {
                    first,
                    last: page.len() - 1,
                    accent: Some(Token::Error),
                });
                continue;
            }
            let (l, r) = &lines[k];
            let hang = l.chars().position(|c| c.is_alphanumeric()).unwrap_or(0);
            page.wrapped(l, hang, r.clone());
            k += 1;
        }
        (page.text(), page.rows, page.boxes)
    }

    /// Rows for the tests of `file` under suite `path`: direct tests and nested suites, in
    /// source order, indented by `indent`.
    fn suite_lines(
        &self,
        file: &Path,
        path: &[String],
        tests: &[&tx::TestCase],
        indent: usize,
        lines: &mut Vec<(String, Option<TestRow>)>,
        selected: Option<&str>,
    ) {
        let pad = " ".repeat(indent);
        // Items at this level: a test directly here, or the first test of a child suite.
        let mut seen_suites: Vec<String> = Vec::new();
        for t in tests {
            let suite = &t.found.suite;
            if suite.as_slice() == path {
                let status = t.status();
                let mut s = format!("{pad}  {} {}", status_glyph(status), t.found.name);
                // A failure's message shows in the box when its row is selected.
                match (status, t.current_result()) {
                    (Status::Passed, Some(r)) => {
                        if let Some(d) = r.duration {
                            s.push_str(&format!("   {} ms", d.as_millis()));
                        }
                    }
                    (Status::Running, _) => s.push_str("   running…"),
                    (Status::NotRunnable, _) => s.push_str("   (not runnable)"),
                    _ => {}
                }
                lines.push((s, Some(TestRow::Test(t.id.clone()))));
                // The selected failed test shows its whole message in a box underneath.
                if selected == Some(t.id.as_str())
                    && let Some(r) = t.current_result()
                    && matches!(r.status, Status::Failed | Status::Skipped)
                    && let Some(text) = r.detail.as_ref().or(r.message.as_ref())
                {
                    let indent = indent + 4;
                    let width = self.panel_width().saturating_sub(indent + 4).max(16);
                    let root = self.tests.tree.project_of(&t.file).map(|p| p.root.clone());
                    for l in wrap_text(&tidy_failure(text, root.as_deref()), width) {
                        lines.push((
                            format!("{}│ {l}", " ".repeat(indent)),
                            Some(TestRow::Detail(t.id.clone())),
                        ));
                    }
                }
            } else if suite.len() > path.len() && suite.starts_with(path) {
                let name = suite[path.len()].clone();
                if seen_suites.contains(&name) {
                    continue;
                }
                seen_suites.push(name.clone());
                let mut sub = path.to_vec();
                sub.push(name.clone());
                let inside: Vec<&tx::TestCase> = tests
                    .iter()
                    .filter(|t| t.found.suite.starts_with(&sub))
                    .copied()
                    .collect();
                let row = TestRow::Suite(file.to_path_buf(), sub.clone());
                let folded = self.tests.collapsed.contains(&row.key());
                lines.push((
                    format!(
                        "{pad}{} {} {name}  ({})",
                        if folded { '▸' } else { '▾' },
                        status_glyph(summary(&inside)),
                        inside.len()
                    ),
                    Some(row),
                ));
                if !folded {
                    self.suite_lines(file, &sub, &inside, indent + 2, lines, selected);
                }
            }
        }
    }

    fn tests_cursor_row(&self) -> Option<TestRow> {
        let doc = self.doc();
        let line = doc.rope.char_to_line(doc.selection.head);
        self.tests.rows.get(line).cloned().flatten()
    }

    fn row_target(&self, row: &TestRow) -> Option<Target> {
        let tree = &self.tests.tree;
        let (path, line) = match row {
            TestRow::Test(id) | TestRow::Detail(id) => {
                let t = tree.tests.iter().find(|t| &t.id == id)?;
                (t.file.clone(), t.found.line)
            }
            TestRow::Suite(f, path) => {
                let line = tree
                    .suites
                    .get(f)
                    .and_then(|s| s.iter().find(|s| &s.path == path))
                    .map(|s| s.line)
                    .or_else(|| {
                        tree.tests
                            .iter()
                            .filter(|t| &t.file == f && t.found.suite.starts_with(path))
                            .map(|t| t.found.line)
                            .min()
                    })?;
                (f.clone(), line)
            }
            TestRow::File(f) => (f.clone(), 0),
            TestRow::Project(_) => return None,
        };
        Some(Target { path, line, col: 0 })
    }

    /// Cursor moved on the Tests page: show the test in the editor.
    pub(super) fn preview_test(&mut self) {
        if let Some(t) = self.tests_cursor_row().and_then(|r| self.row_target(&r)) {
            self.show_location(&t, false);
        }
    }

    /// Enter / double-click on the Tests page: open a test, suite or file; fold a project.
    pub(super) fn open_test(&mut self) {
        match self.tests_cursor_row() {
            Some(TestRow::Project(_)) => self.toggle_test_fold(),
            Some(row) => {
                if let Some(t) = self.row_target(&row) {
                    self.preview_doc = None;
                    self.show_location(&t, true);
                }
            }
            None => {}
        }
    }

    /// A single click on the Tests page: on a group's arrow it folds the group, elsewhere it
    /// shows the row in the editor.
    /// The test whose row (or failure box) the Tests page cursor is on.
    /// After the cursor moved on the Tests page: does the failure box have to move? (Only
    /// then is the page rebuilt; on a big workspace that takes tens of milliseconds.)
    pub(super) fn tests_box_moves(&self) -> bool {
        let shown = self.tests.rows.iter().find_map(|r| match r {
            Some(TestRow::Detail(id)) => Some(id.as_str()),
            _ => None,
        });
        let selected = self.tests_selected_id();
        let wanted = selected.as_deref().filter(|id| {
            self.tests.tree.tests.iter().any(|t| {
                t.id == *id
                    && t.current_result().is_some_and(|r| {
                        matches!(r.status, Status::Failed | Status::Skipped)
                            && (r.detail.is_some() || r.message.is_some())
                    })
            })
        });
        shown != wanted
    }

    fn tests_selected_id(&self) -> Option<String> {
        let (p, i) = self.tests_page()?;
        let doc = &self.panes[p].docs[i];
        let line = doc.rope.char_to_line(doc.selection.head);
        match self.tests.rows.get(line).cloned().flatten()? {
            TestRow::Test(id) | TestRow::Detail(id) => Some(id),
            _ => None,
        }
    }

    /// Page lines of the failure box.
    #[cfg(test)]
    pub fn tests_detail_lines(&self) -> Option<(usize, usize)> {
        let is_detail = |r: &Option<TestRow>| matches!(r, Some(TestRow::Detail(_)));
        let first = self.tests.rows.iter().position(is_detail)?;
        let last = self.tests.rows.iter().rposition(is_detail)?;
        Some((first, last))
    }

    /// Up/Down on the Tests page: the previous / next row, skipping the failure box.
    pub(super) fn tests_step(&mut self, delta: isize) {
        let doc = self.doc();
        let line = doc.rope.char_to_line(doc.selection.head) as isize;
        let rows = &self.tests.rows;
        let here = rows.get(line as usize).cloned().flatten();
        let mut k = line;
        loop {
            k += delta;
            if k < 0 || k as usize >= rows.len() {
                return;
            }
            match &rows[k as usize] {
                Some(TestRow::Detail(_)) | None => continue,
                // A wrapped row's other lines.
                r if *r == here => continue,
                Some(_) => break,
            }
        }
        // The row's first line.
        let mut k = k as usize;
        while k > 0 && rows[k - 1] == rows[k] {
            k -= 1;
        }
        let doc = self.doc_mut();
        doc.selection = Selection::point(doc.rope.line_to_char(k));
        if self.tests_box_moves() {
            self.refresh_tests_page();
        }
        self.preview_test();
    }

    /// Copy the selected test's whole failure message.
    fn copy_test_failure(&mut self) {
        let text = self.tests_selected_id().and_then(|id| {
            let t = self.tests.tree.tests.iter().find(|t| t.id == id)?;
            let r = t.current_result()?;
            r.detail.clone().or_else(|| r.message.clone())
        });
        match text {
            Some(t) => {
                self.set_clipboard(t);
                self.info("Copied the failure message");
            }
            None => self.info("No failure message here"),
        }
    }

    pub(super) fn tests_click(&mut self, double: bool) {
        let doc = self.doc();
        let line = doc.rope.char_to_line(doc.selection.head);
        let col = doc.selection.head - doc.rope.line_to_char(line);
        let text: String = doc.rope.line(line).chars().collect();
        let arrow_col = text.chars().position(|c| c == '▾' || c == '▸');
        let row = self.tests.rows.get(line).cloned().flatten();
        let on_group = row.as_ref().is_some_and(TestRow::is_group);
        if on_group && (arrow_col.is_some_and(|a| col <= a + 1) || double) {
            self.toggle_test_fold();
        } else if double {
            self.open_test();
        } else {
            // A click inside a failure box keeps that test selected.
            self.refresh_tests_page();
            self.preview_test();
        }
    }

    /// The group a row folds: itself for groups, the innermost suite or file for a test.
    fn fold_target(&self, row: TestRow) -> Option<TestRow> {
        match row {
            TestRow::Test(id) | TestRow::Detail(id) => {
                let t = self.tests.tree.tests.iter().find(|t| t.id == id)?;
                Some(if t.found.suite.is_empty() {
                    TestRow::File(t.file.clone())
                } else {
                    TestRow::Suite(t.file.clone(), t.found.suite.clone())
                })
            }
            other => Some(other),
        }
    }

    pub(super) fn toggle_test_fold(&mut self) {
        let Some(row) = self.tests_cursor_row().and_then(|r| self.fold_target(r)) else {
            return;
        };
        let key = row.key();
        if !self.tests.collapsed.remove(&key) {
            self.tests.collapsed.insert(key);
        }
        self.rebuild_tests_page_at(&row);
    }

    /// Regenerate the page and put the cursor on `row`.
    fn rebuild_tests_page_at(&mut self, row: &TestRow) {
        let Some((p, i)) = self.tests_page() else {
            return;
        };
        let (text, rows, boxes) = self.tests_text();
        let line = rows
            .iter()
            .position(|r| r.as_ref() == Some(row))
            .unwrap_or(0);
        let doc = &mut self.panes[p].docs[i];
        doc.rope = Rope::from_str(&text);
        crate::perf::bump(&crate::perf::PAGE_REBUILDS);
        doc.syntax.set_lang(Lang::Tests);
        doc.boxes = boxes;
        doc.selection = Selection::point(doc.rope.line_to_char(line));
        self.tests.rows = rows;
    }

    /// Left on the Tests page: collapse the group, or go to the parent group.
    pub(super) fn tests_left(&mut self) {
        let Some(row) = self.tests_cursor_row() else {
            return;
        };
        if row.is_group() && !self.tests.collapsed.contains(&row.key()) {
            self.tests.collapsed.insert(row.key());
            self.rebuild_tests_page_at(&row);
            return;
        }
        // The nearest row above with less indentation is the parent.
        let doc = self.doc();
        let line = doc.rope.char_to_line(doc.selection.head);
        let indent =
            |l: usize| -> usize { doc.rope.line(l).chars().take_while(|c| *c == ' ').count() };
        let own = indent(line);
        if let Some(parent) = (0..line)
            .rev()
            .find(|&l| self.tests.rows.get(l).cloned().flatten().is_some() && indent(l) < own)
        {
            let pos = doc.rope.line_to_char(parent);
            self.doc_mut().selection = Selection::point(pos);
        }
    }

    /// Right on the Tests page: expand the group, or step to the next row.
    pub(super) fn tests_right(&mut self) {
        let Some(row) = self.tests_cursor_row() else {
            return;
        };
        if row.is_group() && self.tests.collapsed.remove(&row.key()) {
            self.rebuild_tests_page_at(&row);
            return;
        }
        let doc = self.doc();
        let line = doc.rope.char_to_line(doc.selection.head);
        if line + 1 < doc.rope.len_lines() {
            let pos = doc.rope.line_to_char(line + 1);
            self.doc_mut().selection = Selection::point(pos);
            self.preview_test();
        }
    }

    /// `-`: fold every file and suite (projects stay open); `+`: unfold everything.
    fn set_all_folded(&mut self, fold: bool) {
        let keep = self.tests_cursor_row();
        self.tests.collapsed.clear();
        if fold {
            let tree = &self.tests.tree;
            let mut keys: Vec<String> = Vec::new();
            for t in &tree.tests {
                keys.push(TestRow::File(t.file.clone()).key());
                for n in 1..=t.found.suite.len() {
                    keys.push(TestRow::Suite(t.file.clone(), t.found.suite[..n].to_vec()).key());
                }
            }
            self.tests.collapsed.extend(keys);
        }
        // Stay on the same group (or its file when it got folded away).
        let target = match keep {
            Some(TestRow::Test(_) | TestRow::Suite(..)) if fold => match keep.clone() {
                Some(TestRow::Test(id)) => self
                    .tests
                    .tree
                    .tests
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| TestRow::File(t.file.clone())),
                Some(TestRow::Suite(f, _)) => Some(TestRow::File(f)),
                other => other,
            },
            other => other,
        };
        match target {
            Some(row) => self.rebuild_tests_page_at(&row),
            None => self.refresh_tests_page(),
        }
    }

    /// Typed keys on the Tests page.
    pub(super) fn tests_page_key(&mut self, c: char) {
        match c {
            'r' | 'R' => self.run_test_at_cursor(),
            'c' | 'C' => self.copy_test_failure(),
            'a' | 'A' => self.run_tests(RunWhat::All),
            'f' | 'F' => self.run_tests(RunWhat::Failed),
            ' ' => self.toggle_test_fold(),
            '-' => self.set_all_folded(true),
            '+' | '=' => self.set_all_folded(false),
            _ => {}
        }
    }
}

/// A failure message made easier to read: paths relative to the project, stack frames from
/// dependencies and the runtime dropped.
pub(super) fn tidy_failure(text: &str, root: Option<&Path>) -> String {
    let prefix = root.map(|r| format!("{}{}", r.display(), std::path::MAIN_SEPARATOR));
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            let frame = t.starts_with("at ");
            !(frame
                && (t.contains("node_modules")
                    || t.contains("node:internal")
                    || t.contains("System.Runtime")))
        })
        .map(|l| match &prefix {
            Some(p) => l.replace(p.as_str(), ""),
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) use super::page::wrap_text;

pub fn status_glyph(s: Status) -> char {
    match s {
        Status::Passed => '✔',
        Status::Failed => '✖',
        Status::NotRun => '○',
        Status::Running => super::linting::spinner(),
        Status::Skipped => '⊘',
        Status::NotRunnable => '◇',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Command;
    use crate::config::Config;
    use crate::workspace::Workspace;
    use std::fs;

    const SPEC: &str = "import { describe, it, expect } from 'vitest';\n\ndescribe('s', () => {\n  it('one', () => {\n    expect(1).toBe(1);\n  });\n});\n";

    /// A JS project with one spec file, opened as a workspace.
    fn project(name: &str) -> (Editor, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("jdc-tx-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("tests")).unwrap();
        let dir = fs::canonicalize(dir).unwrap();
        fs::write(
            dir.join("package.json"),
            r#"{"devDependencies":{"vitest":"^2"}}"#,
        )
        .unwrap();
        let spec = dir.join("tests/a.spec.js");
        fs::write(&spec, SPEC).unwrap();
        let mut ed = Editor::new(Config::default(), &[]);
        // A wide panel: rows stay on one line each.
        ed.panes[crate::editor::PANEL].cols = 200;
        let mut ws = Workspace::new(name);
        ws.folders.push(dir.clone());
        ed.workspace = Some(ws);
        ed.start_test_explorer();
        ed.wait_jobs(Duration::from_secs(10));
        (ed, dir, spec)
    }

    fn names(ed: &Editor) -> Vec<(String, Status)> {
        ed.tests
            .tree
            .tests
            .iter()
            .map(|t| (t.found.name.clone(), t.status()))
            .collect()
    }

    #[test]
    fn edits_add_tests_and_reset_edited_ones() {
        let (mut ed, dir, spec) = project("sync");
        assert_eq!(names(&ed), [("one".to_string(), Status::NotRun)]);
        // Pretend it passed.
        let t = &mut ed.tests.tree.tests[0];
        t.result = Some(RunResult {
            status: Status::Passed,
            duration: None,
            message: None,
            detail: None,
            hash: t.found.hash,
        });
        ed.open_path(&spec);
        // Type a new test after the describe block.
        let end = ed.doc().rope.len_chars();
        ed.doc_mut().selection = Selection::point(end);
        ed.doc_mut().replace_selection(
            "\nit('two', () => {});\n",
            crate::editor::history::EditKind::Insert,
        );
        ed.track_test_edits();
        ed.wait_jobs(Duration::from_secs(5));
        assert_eq!(
            names(&ed),
            [
                ("one".to_string(), Status::Passed),
                ("two".to_string(), Status::NotRun)
            ]
        );
        // Edit the body of the passed test: back to not run; undo brings the result back.
        let text = ed.doc().rope.to_string();
        let at = text.find("toBe(1)").unwrap() + 5;
        ed.doc_mut().selection = Selection::point(at);
        ed.doc_mut()
            .replace_selection("2", crate::editor::history::EditKind::Insert);
        ed.track_test_edits();
        ed.wait_jobs(Duration::from_secs(5));
        assert_eq!(names(&ed)[0].1, Status::NotRun);
        ed.execute(Command::Undo);
        ed.wait_jobs(Duration::from_secs(5));
        assert_eq!(names(&ed)[0].1, Status::Passed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_files_queue_checks() {
        let (mut ed, dir, _) = project("config");
        fs::write(dir.join("eslint.config.js"), "export default [];").unwrap();
        fs::create_dir_all(dir.join("api")).unwrap();
        fs::write(dir.join("api/Api.csproj"), "<Project/>").unwrap();
        ed.config_changed(&dir.join("eslint.config.js"));
        assert!(ed.tests.lint_wanted.contains_key(&dir));
        ed.config_changed(&dir.join("api/Api.csproj"));
        assert!(
            ed.tests
                .build_wanted
                .contains_key(&dir.join("api/Api.csproj"))
        );
        ed.tests.build_wanted.clear();
        ed.config_changed(&dir.join("Directory.Build.props"));
        assert_eq!(
            ed.tests.build_wanted.keys().collect::<Vec<_>>(),
            [&dir.join("api/Api.csproj")]
        );
        // Several quick saves: still one pending entry per project.
        ed.config_changed(&dir.join(".editorconfig"));
        assert_eq!(ed.tests.build_wanted.len(), 1);
        ed.tests.build_wanted.clear();
        ed.tests.lint_wanted.clear();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runs_with_a_project_local_vitest() {
        use std::os::unix::fs::PermissionsExt;
        let (mut ed, dir, spec) = project("run");
        fs::write(
            &spec,
            SPEC.replace("});\n});\n", "});\n  it('two', () => {});\n});\n"),
        )
        .unwrap();
        ed.reparse_tests(&spec);
        fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
        let fake = dir.join("node_modules/.bin/vitest");
        let report = format!(
            r#"{{"testResults":[{{"name":"{}","assertionResults":[{{"ancestorTitles":["s"],"title":"one","status":"passed","duration":2}},{{"ancestorTitles":["s"],"title":"two","status":"failed","failureMessages":["expected 1 to be 2"]}}]}}]}}"#,
            spec.display()
        );
        fs::write(
            &fake,
            format!(
                "#!/bin/sh\nsleep 0.3\nfor a in \"$@\"; do case \"$a\" in --outputFile=*) out=\"${{a#--outputFile=}}\";; esac; done\nprintf '%s' '{report}' > \"$out\"\nexit 1\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        ed.run_tests(RunWhat::All);
        assert_eq!(ed.tests.tree.counts().3, 2); // running
        ed.wait_jobs(Duration::from_secs(10));
        assert_eq!(
            names(&ed),
            [
                ("one".to_string(), Status::Passed),
                ("two".to_string(), Status::Failed)
            ]
        );
        let failed = &ed.tests.tree.tests[1];
        assert_eq!(
            failed.current_result().unwrap().message.as_deref(),
            Some("expected 1 to be 2")
        );

        // Editing a test while it runs: its result is for the old text, so it shows not run.
        ed.open_path(&spec);
        ed.run_tests(RunWhat::Failed);
        let text = ed.doc().rope.to_string();
        let at = text.find("it('two', () => {").unwrap() + "it('two', () => {".len();
        ed.doc_mut().selection = Selection::point(at);
        ed.doc_mut()
            .replace_selection(" x(); ", crate::editor::history::EditKind::Insert);
        ed.track_test_edits();
        ed.wait_jobs(Duration::from_secs(10));
        assert_eq!(names(&ed)[1].1, Status::NotRun);
        fs::remove_dir_all(&dir).unwrap();
    }

    fn page_lines(ed: &Editor) -> Vec<String> {
        let (p, i) = ed.tests_page().unwrap();
        ed.panes[p].docs[i]
            .rope
            .to_string()
            .lines()
            .map(String::from)
            .collect()
    }

    fn cursor_to(ed: &mut Editor, needle: &str) {
        let line = page_lines(ed)
            .iter()
            .position(|l| l.contains(needle))
            .unwrap();
        let doc = ed.doc_mut();
        doc.selection = Selection::point(doc.rope.line_to_char(line));
    }

    #[test]
    fn suites_nest_and_fold() {
        let (mut ed, dir, spec) = project("fold");
        fs::write(
            &spec,
            "describe('outer', () => {\n  it('a', () => {});\n  describe('inner', () => {\n    it('b', () => {});\n  });\n});\nit('top', () => {});\n",
        )
        .unwrap();
        ed.reparse_tests(&spec);
        ed.execute(Command::ShowTests);
        let lines = page_lines(&ed);
        let body: Vec<&str> = lines[3..].iter().map(|l| l.as_str()).collect();
        assert_eq!(
            body,
            [
                "▾ ○ jdc-tx-fold-".to_string()
                    + &std::process::id().to_string()
                    + "  (Vitest)  0/3 passed",
                "  ▾ ○ tests/a.spec.js  (3)".into(),
                "    ▾ ○ outer  (2)".into(),
                "        ○ a".into(),
                "      ▾ ○ inner  (1)".into(),
                "          ○ b".into(),
                "      ○ top".into(),
            ]
        );
        // A failure inside shows on the folded suite.
        let b = ed
            .tests
            .tree
            .tests
            .iter_mut()
            .find(|t| t.found.name == "b")
            .unwrap();
        b.result = Some(RunResult {
            status: Status::Failed,
            duration: None,
            message: Some("nope".into()),
            detail: Some("nope\n  at b (a.spec.js:4)".into()),
            hash: b.found.hash,
        });
        cursor_to(&mut ed, "▾ ○ outer");
        ed.insert_text(" "); // Space folds
        let lines = page_lines(&ed);
        assert!(
            lines.contains(&"    ▸ ✖ outer  (2)".to_string()),
            "{lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.ends_with(" b") || l.contains("inner"))
        );
        // Right unfolds; Left on a test jumps to its suite, Left again folds it.
        ed.execute(Command::Move(crate::commands::Motion::Right, false));
        cursor_to(&mut ed, "✖ b");
        ed.execute(Command::Move(crate::commands::Motion::Left, false));
        assert_eq!(
            ed.tests_cursor_row(),
            Some(TestRow::Suite(
                spec.clone(),
                vec!["outer".into(), "inner".into()]
            ))
        );
        ed.execute(Command::Move(crate::commands::Motion::Left, false));
        assert!(page_lines(&ed).iter().any(|l| l.contains("▸ ✖ inner")));
        // - folds every file, + unfolds all.
        ed.insert_text("-");
        assert_eq!(page_lines(&ed).len(), 5);
        ed.insert_text("+");
        assert_eq!(page_lines(&ed).len(), 10);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn page_keys_do_not_move_the_editor() {
        let (mut ed, dir, spec) = project("page");
        ed.open_path(&spec);
        let before = ed.panes[EDITOR].doc().unwrap().selection;
        ed.execute(Command::ShowTests);
        ed.execute(Command::Move(crate::commands::Motion::PageDown, false));
        ed.execute(Command::Move(crate::commands::Motion::DocEnd, false));
        assert_eq!(ed.panes[EDITOR].doc().unwrap().selection, before);
        // Up/Down still show the selected test.
        ed.execute(Command::Move(crate::commands::Motion::Up, false));
        ed.execute(Command::Move(crate::commands::Motion::Down, false));
        let doc = ed.panes[EDITOR].doc().unwrap();
        assert_eq!(doc.rope.char_to_line(doc.selection.head), 3); // it('one')
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn explorer_reveals_the_active_file() {
        let (mut ed, dir, spec) = project("reveal");
        let other = dir.join("package.json");
        ed.open_path(&spec);
        ed.follow_active_file();
        let names: Vec<String> = ed.explorer.rows.iter().map(|r| r.name.clone()).collect();
        assert!(names.contains(&"a.spec.js".to_string()), "{names:?}");
        assert_eq!(ed.explorer.selected_row().unwrap().path, spec);
        ed.open_path(&other);
        ed.follow_active_file();
        // The tests folder collapsed again; package.json is selected.
        assert!(!ed.explorer.rows.iter().any(|r| r.name == "a.spec.js"));
        assert_eq!(ed.explorer.selected_row().unwrap().path, other);
        ed.execute(Command::PrevTab);
        ed.follow_active_file();
        assert_eq!(ed.explorer.selected_row().unwrap().path, spec);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gitignored_files_are_never_tests() {
        let dir = std::env::temp_dir().join(format!("jdc-tx-ignored-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for d in ["tests", "generated", "node_modules/pkg/test"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        let dir = fs::canonicalize(dir).unwrap();
        fs::write(
            dir.join("package.json"),
            r#"{"devDependencies":{"vitest":"^2"}}"#,
        )
        .unwrap();
        fs::write(dir.join(".gitignore"), "generated/\n").unwrap();
        fs::write(
            dir.join("node_modules/pkg/package.json"),
            r#"{"devDependencies":{"jest":"1"}}"#,
        )
        .unwrap();
        for f in [
            "tests/a.spec.js",
            "generated/g.spec.js",
            "node_modules/pkg/test/p.spec.js",
        ] {
            fs::write(dir.join(f), SPEC).unwrap();
        }
        let mut ed = Editor::new(Config::default(), &[]);
        let mut ws = Workspace::new("ign");
        ws.folders.push(dir.clone());
        ws.respect_gitignore = false; // the Explorer shows everything; tests still skip
        ed.workspace = Some(ws);
        ed.start_test_explorer();
        ed.wait_jobs(Duration::from_secs(10));
        let files = |ed: &Editor| -> Vec<PathBuf> {
            let mut f: Vec<PathBuf> = ed.tests.tree.tests.iter().map(|t| t.file.clone()).collect();
            f.dedup();
            f
        };
        assert_eq!(files(&ed), [dir.join("tests/a.spec.js")]);
        assert_eq!(ed.tests.tree.projects.len(), 1);
        // Opening an ignored spec by hand adds nothing.
        ed.open_path(&dir.join("generated/g.spec.js"));
        ed.track_test_edits();
        ed.wait_jobs(Duration::from_secs(5));
        assert_eq!(files(&ed), [dir.join("tests/a.spec.js")]);
        // Ignoring tests/ in .gitignore (saved): its tests go away.
        fs::write(dir.join(".gitignore"), "generated/\ntests/\n").unwrap();
        ed.config_changed(&dir.join(".gitignore"));
        ed.wait_jobs(Duration::from_secs(10));
        assert!(files(&ed).is_empty(), "{:?}", files(&ed));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn run_problems_show_on_the_project_row() {
        let (mut ed, dir, _) = project("noprun");
        // No vitest/jest anywhere: the project has no runner.
        fs::write(dir.join("package.json"), "{}").unwrap();
        ed.tests.tree.projects[0].runner = None;
        ed.run_tests(RunWhat::All);
        assert!(ed.message.as_ref().unwrap().text.contains("no test runner"));
        ed.execute(Command::ShowTests);
        let lines = page_lines(&ed);
        assert!(
            lines[3].contains("(no runner)  0/1 passed  — no test runner"),
            "{lines:?}"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failure_box_follows_the_selection() {
        let (mut ed, dir, spec) = project("detailbox");
        fs::write(
            &spec,
            "describe('s', () => {\n  it('one', () => {});\n  it('two', () => {});\n});\n",
        )
        .unwrap();
        ed.reparse_tests(&spec);
        let one = ed
            .tests
            .tree
            .tests
            .iter_mut()
            .find(|t| t.found.name == "one")
            .unwrap();
        one.result = Some(RunResult {
            status: Status::Failed,
            duration: None,
            message: Some("expected 1 to be 2".into()),
            detail: Some(format!(
                "AssertionError: expected 1 to be 2\n    at {}/tests/a.spec.js:2:5\n    at x (node_modules/vitest/dist/index.js:1:1)",
                dir.display()
            )),
            hash: one.found.hash,
        });
        ed.execute(Command::ShowTests);
        let before = page_lines(&ed).len();
        assert!(ed.tests_detail_lines().is_none());
        cursor_to(&mut ed, "✖ one");
        ed.refresh_tests_page();
        let lines = page_lines(&ed);
        let (a, b) = ed.tests_detail_lines().unwrap();
        assert_eq!(b - a + 1, 2, "{lines:?}"); // message + project-relative frame, no node_modules
        assert!(
            lines[a]
                .trim_start()
                .starts_with("│ AssertionError: expected 1 to be 2")
        );
        assert!(
            lines[b].ends_with("│     at tests/a.spec.js:2:5"),
            "{:?}",
            lines[b]
        );
        // Down skips the box to the next test; the box closes.
        ed.execute(Command::Move(crate::commands::Motion::Down, false));
        assert_eq!(
            ed.tests_cursor_row(),
            Some(TestRow::Test(ed.tests.tree.tests[1].id.clone()))
        );
        assert!(ed.tests_detail_lines().is_none());
        assert_eq!(page_lines(&ed).len(), before);
        // Up comes back to the failed test and the box opens again under it.
        ed.execute(Command::Move(crate::commands::Motion::Up, false));
        assert!(ed.tests_detail_lines().is_some());
        assert!(matches!(ed.tests_cursor_row(), Some(TestRow::Test(_))));
        // The tab title counts the failure.
        let doc = ed.panes[1].doc().unwrap();
        assert_eq!(
            crate::editor::hover::tab_title(&ed, doc).text,
            "✖ Tests (1)"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wrapping_keeps_words_and_indent() {
        assert_eq!(wrap_text("alpha beta gamma", 11), ["alpha beta", "gamma"]);
        assert_eq!(
            wrap_text("    at /a/very/long/path/file.js:1:2", 16),
            ["    at /a/very/", "    long/path/", "    file.js:1:2"]
        );
        assert_eq!(wrap_text("x\n\ny", 10), ["x", "", "y"]);
    }

    #[cfg(unix)]
    #[test]
    fn opening_a_workspace_lints_the_whole_project() {
        use crate::lint::eslint::Scope;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("jdc-tx-autolint-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        let dir = fs::canonicalize(dir).unwrap();
        fs::write(dir.join("eslint.config.js"), "export default [];").unwrap();
        fs::write(dir.join("src/a.js"), "let x = 1;\n").unwrap();
        // ESLint stand-in: one warning for src/a.js, for any run.
        let report = format!(
            r#"[{{"filePath":"{}","messages":[{{"ruleId":"no-unused-vars","severity":1,"message":"unused","line":1,"column":5}}]}}]"#,
            dir.join("src/a.js").display()
        );
        let fake = dir.join("node_modules/.bin/eslint");
        fs::write(
            &fake,
            format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{report}'\nexit 1\n"),
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let mut ed = Editor::new(Config::default(), &[]);
        let mut ws = Workspace::new("auto");
        ws.folders.push(dir.clone());
        ed.workspace = Some(ws);
        ed.start_test_explorer();
        assert!(
            ed.lint_jobs
                .iter()
                .any(|j| j.scope == Scope::Root(dir.clone()) && j.quiet)
        );
        ed.wait_jobs(Duration::from_secs(10));
        assert_eq!(ed.problems.counts(), (0, 1));
        assert!(ed.message.is_none(), "{:?}", ed.message); // quiet: no status message
        // A file changed outside the editor is linted from disk.
        fs::write(dir.join("src/a.js"), "let y = 2;\n").unwrap();
        ed.outside_change(&dir.join("src/a.js"), false, false);
        assert!(
            ed.lint_jobs
                .iter()
                .any(|j| j.scope == Scope::File(dir.join("src/a.js")))
        );
        ed.wait_jobs(Duration::from_secs(10));
        fs::remove_dir_all(&dir).unwrap();
    }
}
