//! The test explorer in the editor: discovering tests (workspace scan, live re-parse of edited
//! buffers, file watcher), running them, Roslyn builds, re-running checks when config files
//! change, and the Tests page in the panel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use ropey::Rope;

use super::linting::Target;
use super::selection::Selection;
use super::{EDITOR, Editor};
use crate::lint::eslint::{self, LintJob, Scope};
use crate::lint::roslyn::{self, BuildJob};
use crate::lint::{Severity, Source};
use crate::syntax::Lang;
use crate::test_explorer::run::{self, Request, TestJob};
use crate::test_explorer::watch::FsWatcher;
use crate::test_explorer::{self as tx, FileTests, Framework, Project, RunResult, Status};

pub const TESTS_TITLE: &str = "Tests";

/// Quiet time after an edit before a test file is re-parsed.
const REPARSE_DELAY: Duration = Duration::from_millis(250);
/// Quiet time after a save before a build starts (several saves: one build).
const BUILD_DELAY: Duration = Duration::from_millis(500);
/// How often the file watcher and background jobs are checked.
pub(super) const TEST_POLL: Duration = Duration::from_millis(50);
const WATCH_POLL: Duration = Duration::from_millis(500);

/// A line of the Tests page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestRow {
    Project(PathBuf),
    File(PathBuf),
    Test(String),
}

impl TestRow {
    fn key(&self) -> String {
        match self {
            TestRow::Project(p) => format!("p:{}", p.display()),
            TestRow::File(p) => format!("f:{}", p.display()),
            TestRow::Test(id) => format!("t:{id}"),
        }
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
type ScanResult = Vec<(PathBuf, Project, FileTests)>;

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
    scan: Option<Receiver<ScanResult>>,
    pub watcher: Option<FsWatcher>,
    next_watch: Option<Instant>,
    pub builds: Vec<BuildJob>,
    /// Projects to build, when (debounced).
    build_wanted: HashMap<PathBuf, Instant>,
    /// ESLint config roots to lint again, when.
    lint_wanted: HashMap<PathBuf, Instant>,
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
        self.scan_tests(folders);
    }

    /// Forget all tests (workspace closed).
    pub(super) fn stop_test_explorer(&mut self) {
        for j in &self.tests.jobs {
            j.cancel();
        }
        self.tests = TestState::default();
        self.refresh_tests_page();
    }

    /// Walk `folders` in the background and discover every test file.
    fn scan_tests(&mut self, folders: Vec<PathBuf>) {
        if folders.is_empty() {
            return;
        }
        let gitignore = self.workspace.as_ref().is_none_or(|w| w.respect_gitignore);
        let (tx_, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("jmx-test-scan".into())
            .spawn(move || {
                let started = Instant::now();
                let mut projects: HashMap<PathBuf, Option<Project>> = HashMap::new();
                let mut out: ScanResult = Vec::new();
                for folder in &folders {
                    for entry in crate::workspace::files::walker(folder, gitignore)
                        .build()
                        .flatten()
                    {
                        let path = entry.into_path();
                        if !(tx::is_cs(&path) || tx::js_test_kind(&path).is_some()) {
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
                let _ = tx_.send(out);
            })
            .expect("cannot start test scan");
        self.tests.scan = Some(rx);
    }

    /// Re-parse one file: from its buffer when open, else from disk.
    fn reparse_tests(&mut self, path: &Path) {
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

    /// Note text changes in open test files (and C# files) so they are re-parsed shortly.
    /// Called after every command and when background results arrive.
    pub(super) fn track_test_edits(&mut self) {
        let now = Instant::now();
        let mut changed: Vec<PathBuf> = Vec::new();
        for doc in self.panes.iter().flat_map(|p| p.docs.iter()) {
            let Some(path) = &doc.canonical else {
                continue;
            };
            if !(tx::is_cs(path) || tx::js_test_kind(path).is_some()) {
                continue;
            }
            if self.tests.seen.get(path) != Some(&doc.revision) {
                changed.push(path.clone());
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
        if let Some(rx) = &self.tests.scan
            && let Ok(found) = rx.try_recv()
        {
            self.tests.scan = None;
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
                    self.tests.tree.replace_file(&path, ft);
                }
            }
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
            for c in changes {
                changed |= self.outside_change(&c.path, c.removed);
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
        if changed {
            self.refresh_tests_page();
        }
        changed
    }

    /// A file changed outside the editor (watcher).
    fn outside_change(&mut self, path: &Path, removed: bool) -> bool {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let open = self.find_doc(|d| d.canonical.as_deref() == Some(path.as_path()));
        let mut changed = false;
        if tx::is_cs(&path) || tx::js_test_kind(&path).is_some() {
            if removed {
                self.tests.tree.remove_file(&path);
                changed = true;
            } else if open.is_none() {
                self.tests
                    .reparse
                    .insert(path.clone(), Instant::now() + REPARSE_DELAY);
            }
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
        // Builds: one per project at a time; a wanted build waits for a running one.
        let due: Vec<PathBuf> = self
            .tests
            .build_wanted
            .iter()
            .filter(|(p, t)| now >= **t && !self.tests.builds.iter().any(|b| &b.csproj == *p))
            .map(|(p, _)| p.clone())
            .collect();
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
            RunWhat::Tests(ids) => tree
                .tests
                .iter()
                .filter(|t| ids.contains(&t.id) && t.runnable)
                .collect(),
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
            let all_of_project = matches!(what, RunWhat::All);
            let dotnet = project.runner.is_some_and(Framework::is_dotnet);
            let mut files: Vec<PathBuf> = Vec::new();
            for t in &tests {
                if !files.contains(&t.file) {
                    files.push(t.file.clone());
                }
            }
            let filter = if dotnet {
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
            if all_of_project || dotnet {
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
        if !busy.is_empty() {
            self.info(format!("Tests are already running in {}", busy.join(", ")));
        }
        let count: usize = requests.iter().map(|r| r.ids.len()).sum();
        for req in requests {
            for (id, hash) in &req.ids {
                if let Some(t) = self.tests.tree.tests.iter_mut().find(|t| &t.id == id) {
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
            for t in self.tests.tree.tests.iter_mut() {
                if !t.file.starts_with(&root) {
                    continue;
                }
                let was_running = ran.contains(t.id.as_str());
                if let Some((status, duration, message)) =
                    run::combine(&run::match_outcome(t, &report.outcomes))
                        // Runners list filtered-out tests as skipped: only tests that were asked
                        // for (or that actually ran) get a new result.
                        .filter(|(status, _, _)| was_running || *status != Status::Skipped)
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
            let secs = job.started.elapsed().as_secs_f32();
            match report.error {
                Some(e) => self.error(e),
                None if failed > 0 => self.error(format!(
                    "Tests: {failed} failed, {passed} passed ({secs:.1} s)"
                )),
                None => self.info(format!("Tests: {passed} passed ({secs:.1} s)")),
            }
        }
        changed
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
                Some(TestRow::Test(id)) => self.run_tests(RunWhat::Tests(vec![id])),
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
            .find(|t| &t.file == path && t.found.line == line)?;
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

    pub(super) fn tests_busy(&self) -> bool {
        !self.tests.jobs.is_empty()
            || !self.tests.builds.is_empty()
            || self.tests.scan.is_some()
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
            || self.tests.scan.is_some())
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
                .filter(|p| tx::is_cs(p) || tx::js_test_kind(p).is_some())
                .collect();
            for p in open {
                self.reparse_tests(&p);
            }
        }
        let (text, rows) = self.tests_text();
        self.open_virtual(TESTS_TITLE, &text);
        self.tests.rows = rows;
        if let Some((p, i)) = self.tests_page() {
            self.panes[p].docs[i].syntax.set_lang(Lang::Tests);
        }
        self.refresh_tests_page();
    }

    fn tests_page(&self) -> Option<(usize, usize)> {
        self.find_doc(|d| d.virtual_name.as_deref() == Some(TESTS_TITLE))
    }

    pub(super) fn on_tests_page(&self) -> bool {
        self.has_doc() && self.doc().virtual_name.as_deref() == Some(TESTS_TITLE)
    }

    pub(super) fn refresh_tests_page(&mut self) {
        let Some((p, i)) = self.tests_page() else {
            return;
        };
        let doc = &self.panes[p].docs[i];
        let line = doc.rope.char_to_line(doc.selection.head);
        let keep = self.tests.rows.get(line).cloned().flatten();
        let (text, rows) = self.tests_text();
        if self.panes[p].docs[i].rope == text.as_str() {
            self.tests.rows = rows;
            return;
        }
        let new_line = keep
            .and_then(|k| rows.iter().position(|r| r.as_ref() == Some(&k)))
            .unwrap_or(line);
        let doc = &mut self.panes[p].docs[i];
        doc.rope = Rope::from_str(&text);
        doc.syntax.set_lang(Lang::Tests);
        let line = new_line.min(doc.rope.len_lines().saturating_sub(1));
        doc.selection = Selection::point(doc.rope.line_to_char(line));
        self.tests.rows = rows;
    }

    fn tests_text(&self) -> (String, Vec<Option<TestRow>>) {
        let tree = &self.tests.tree;
        let (passed, failed, not_run, running) = tree.counts();
        let mut lines: Vec<(String, Option<TestRow>)> = Vec::new();
        let mut head = format!("Tests: {passed} passed · {failed} failed · {not_run} not run");
        if running > 0 {
            head.push_str(&format!(" · {running} running"));
        }
        if self.tests.scan.is_some() {
            head.push_str("   (finding tests…)");
        }
        lines.push((head, None));
        lines.push((
            "F5 run · Shift+F5 all · Ctrl+F5 failed · r run · a all · f failed · Space fold · Enter open"
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
        for project in &tree.projects {
            let tests: Vec<&tx::TestCase> = tree
                .tests
                .iter()
                .filter(|t| {
                    tree.project_of(&t.file)
                        .is_some_and(|p| p.root == project.root)
                })
                .collect();
            if tests.is_empty() {
                continue;
            }
            let row = TestRow::Project(project.root.clone());
            let folded = self.tests.collapsed.contains(&row.key());
            let ok = tests
                .iter()
                .filter(|t| t.status() == Status::Passed)
                .count();
            let runner = project.runner.map_or("no runner", Framework::name);
            lines.push((
                format!(
                    "{} {}  ({runner})  {ok}/{} passed",
                    if folded { '▸' } else { '▾' },
                    project.name,
                    tests.len()
                ),
                Some(row),
            ));
            if folded {
                continue;
            }
            let mut files: Vec<&PathBuf> = Vec::new();
            for t in &tests {
                if !files.contains(&&t.file) {
                    files.push(&t.file);
                }
            }
            for file in files {
                let in_file: Vec<&&tx::TestCase> =
                    tests.iter().filter(|t| &t.file == file).collect();
                let row = TestRow::File(file.clone());
                let folded = self.tests.collapsed.contains(&row.key());
                let rel = file
                    .strip_prefix(&project.root)
                    .unwrap_or(file)
                    .to_string_lossy()
                    .replace('\\', "/");
                let failed_here = in_file.iter().any(|t| t.status() == Status::Failed);
                lines.push((
                    format!(
                        "  {} {rel}  ({}){}",
                        if folded { '▸' } else { '▾' },
                        in_file.len(),
                        if failed_here && folded { "  ✖" } else { "" }
                    ),
                    Some(row),
                ));
                if folded {
                    continue;
                }
                for t in in_file {
                    let status = t.status();
                    let glyph = status_glyph(status);
                    let mut s = format!("      {glyph} {}", t.display_name());
                    match (status, t.current_result()) {
                        (Status::Failed, Some(r)) => {
                            if let Some(m) = &r.message {
                                s.push_str(&format!("   — {m}"));
                            }
                        }
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
                }
            }
        }
        let text = lines
            .iter()
            .map(|(l, _)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        (text, lines.into_iter().map(|(_, r)| r).collect())
    }

    fn tests_cursor_row(&self) -> Option<TestRow> {
        let doc = self.doc();
        let line = doc.rope.char_to_line(doc.selection.head);
        self.tests.rows.get(line).cloned().flatten()
    }

    fn row_target(&self, row: &TestRow) -> Option<Target> {
        let tree = &self.tests.tree;
        let (path, line) = match row {
            TestRow::Test(id) => {
                let t = tree.tests.iter().find(|t| &t.id == id)?;
                (t.file.clone(), t.found.line)
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

    /// Enter / double-click on the Tests page.
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

    pub(super) fn toggle_test_fold(&mut self) {
        let Some(row) = self.tests_cursor_row() else {
            return;
        };
        let row = match row {
            TestRow::Test(id) => {
                // Folding from a test folds its file.
                match self.tests.tree.tests.iter().find(|t| t.id == id) {
                    Some(t) => TestRow::File(t.file.clone()),
                    None => return,
                }
            }
            other => other,
        };
        let key = row.key();
        if !self.tests.collapsed.remove(&key) {
            self.tests.collapsed.insert(key);
        }
        // Keep the cursor on the folded group.
        if let Some((p, i)) = self.tests_page() {
            let (text, rows) = self.tests_text();
            let line = rows
                .iter()
                .position(|r| r.as_ref() == Some(&row))
                .unwrap_or(0);
            let doc = &mut self.panes[p].docs[i];
            doc.rope = Rope::from_str(&text);
            doc.syntax.set_lang(Lang::Tests);
            doc.selection = Selection::point(doc.rope.line_to_char(line));
            self.tests.rows = rows;
        }
    }

    /// Typed keys on the Tests page.
    pub(super) fn tests_page_key(&mut self, c: char) {
        match c {
            'r' | 'R' => self.run_test_at_cursor(),
            'a' | 'A' => self.run_tests(RunWhat::All),
            'f' | 'F' => self.run_tests(RunWhat::Failed),
            ' ' => self.toggle_test_fold(),
            _ => {}
        }
    }
}

pub fn status_glyph(s: Status) -> char {
    match s {
        Status::Passed => '✔',
        Status::Failed => '✖',
        Status::NotRun => '○',
        Status::Running => '●',
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
        let dir = std::env::temp_dir().join(format!("jmx-tx-{name}-{}", std::process::id()));
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
}
