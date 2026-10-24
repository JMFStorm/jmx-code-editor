//! Test explorer model: tests found in the workspace (JS specs, Storybook stories, C# test
//! methods), their projects, and their last results.
//!
//! A test's status is tied to the text that produced it: each test keeps a hash of its source
//! (body), and a result only counts while the hash matches the one that ran. Editing a test
//! therefore turns it back to "not run" without any bookkeeping, and undoing the edit brings the
//! result back.

pub mod discover_cs;
pub mod discover_js;
pub mod run;
pub mod watch;

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Framework {
    Vitest,
    Jest,
    Storybook,
    XUnit,
    NUnit,
    MSTest,
}

impl Framework {
    pub fn name(self) -> &'static str {
        match self {
            Framework::Vitest => "Vitest",
            Framework::Jest => "Jest",
            Framework::Storybook => "Storybook",
            Framework::XUnit => "xUnit",
            Framework::NUnit => "NUnit",
            Framework::MSTest => "MSTest",
        }
    }

    pub fn is_dotnet(self) -> bool {
        matches!(
            self,
            Framework::XUnit | Framework::NUnit | Framework::MSTest
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    NotRun,
    Running,
    Passed,
    Failed,
    Skipped,
    /// A story in a project that does not run stories as tests.
    NotRunnable,
}

/// A test as found in the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    /// Enclosing `describe` names (JS) or classes (C#, outermost first).
    pub suite: Vec<String>,
    /// 0-based first and last line of the test's source.
    pub line: usize,
    pub end_line: usize,
    /// Hash of the test's source text.
    pub hash: u64,
    /// C#: `Namespace.Class.Method` as test runners report it.
    pub full_name: Option<String>,
    pub framework: Framework,
}

/// A suite (`describe` block, C# class) as found in the source, for "run at cursor".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundSuite {
    pub path: Vec<String>,
    pub line: usize,
    pub end_line: usize,
    /// C#: `Namespace.Class`.
    pub full_name: Option<String>,
}

/// Everything discovered in one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTests {
    pub tests: Vec<Found>,
    pub suites: Vec<FoundSuite>,
}

/// The outcome of the last run of a test, for the text with hash `hash`.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    pub status: Status,
    pub duration: Option<Duration>,
    /// First line of the failure message.
    pub message: Option<String>,
    pub hash: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TestCase {
    pub id: String,
    pub file: PathBuf,
    pub found: Found,
    pub result: Option<RunResult>,
    pub running: bool,
    /// Hash of the source when the running job started (the result belongs to that text).
    pub running_hash: u64,
    pub runnable: bool,
}

impl TestCase {
    pub fn status(&self) -> Status {
        if self.running {
            return Status::Running;
        }
        if !self.runnable {
            return Status::NotRunnable;
        }
        match &self.result {
            Some(r) if r.hash == self.found.hash => r.status,
            _ => Status::NotRun,
        }
    }

    /// The result, if it still applies to the current text.
    pub fn current_result(&self) -> Option<&RunResult> {
        self.result.as_ref().filter(|r| r.hash == self.found.hash)
    }

    /// `Suite › Sub › name`.
    pub fn display_name(&self) -> String {
        let mut parts = self.found.suite.clone();
        parts.push(self.found.name.clone());
        parts.join(" › ")
    }
}

/// A test project: a JS package (package.json folder) or a C# test project (csproj folder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
    pub name: String,
    /// The runner for its tests (Vitest/Jest, or the .NET test framework).
    pub runner: Option<Framework>,
    /// Vitest runs stories as tests (Storybook's Vitest addon).
    pub stories_runnable: bool,
    /// C#: the project file.
    pub csproj: Option<PathBuf>,
}

/// All tests in the workspace.
#[derive(Debug, Default)]
pub struct TestTree {
    pub projects: Vec<Project>,
    /// Sorted by project, file, line.
    pub tests: Vec<TestCase>,
    /// Suites per file, for running the suite around the cursor.
    pub suites: HashMap<PathBuf, Vec<FoundSuite>>,
}

pub fn hash_text(text: &str) -> u64 {
    // Whitespace-only edits (re-indenting, formatting) do not invalidate results.
    let mut h = DefaultHasher::new();
    for word in text.split_whitespace() {
        word.hash(&mut h);
    }
    h.finish()
}

impl TestTree {
    /// The project a file belongs to, adding it if new.
    pub fn project_for(&mut self, file: &Path) -> Option<usize> {
        let p = detect_project(file)?;
        let root = p.root.clone();
        if let Some(i) = self.projects.iter().position(|q| q.root == root) {
            return Some(i);
        }
        self.projects.push(p);
        self.projects.sort_by_key(|a| a.name.to_lowercase());
        self.projects.iter().position(|q| q.root == root)
    }

    pub fn project_of(&self, file: &Path) -> Option<&Project> {
        self.projects
            .iter()
            .filter(|p| file.starts_with(&p.root))
            .max_by_key(|p| p.root.components().count())
    }

    /// Merge what was found in `file`: new tests appear (not run), missing ones go, and kept
    /// ones keep their last result (which only shows while their text is unchanged).
    pub fn replace_file(&mut self, file: &Path, found: FileTests) {
        let runnable_stories = self.project_of(file).is_some_and(|p| p.stories_runnable);
        let mut old: HashMap<String, TestCase> = self
            .tests
            .iter()
            .filter(|t| t.file == file)
            .map(|t| (t.id.clone(), t.clone()))
            .collect();
        self.tests.retain(|t| t.file != file);
        let mut seen: HashMap<String, usize> = HashMap::new();
        for f in found.tests {
            let base = match &f.full_name {
                Some(n) => n.clone(),
                None => {
                    let mut parts = f.suite.clone();
                    parts.push(f.name.clone());
                    format!("{}::{}", file.display(), parts.join(" › "))
                }
            };
            // Same name twice in a file: number the later ones.
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            let id = if *n > 1 { format!("{base} #{n}") } else { base };
            let runnable = f.framework != Framework::Storybook || runnable_stories;
            let prev = old.remove(&id);
            self.tests.push(TestCase {
                id,
                file: file.to_path_buf(),
                result: prev.as_ref().and_then(|p| p.result.clone()),
                running: prev.as_ref().is_some_and(|p| p.running),
                running_hash: prev.as_ref().map_or(0, |p| p.running_hash),
                runnable,
                found: f,
            });
        }
        if found.suites.is_empty() {
            self.suites.remove(file);
        } else {
            self.suites.insert(file.to_path_buf(), found.suites);
        }
        self.sort();
    }

    pub fn remove_file(&mut self, file: &Path) {
        self.tests.retain(|t| t.file != file);
        self.suites.remove(file);
    }

    fn sort(&mut self) {
        let projects = &self.projects;
        let rank = |f: &Path| {
            projects
                .iter()
                .position(|p| f.starts_with(&p.root))
                .unwrap_or(usize::MAX)
        };
        self.tests.sort_by(|a, b| {
            (rank(&a.file), &a.file, a.found.line).cmp(&(rank(&b.file), &b.file, b.found.line))
        });
    }

    /// (passed, failed, not run, running).
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        self.tests
            .iter()
            .fold((0, 0, 0, 0), |(p, f, n, r), t| match t.status() {
                Status::Passed => (p + 1, f, n, r),
                Status::Failed => (p, f + 1, n, r),
                Status::Running => (p, f, n, r + 1),
                Status::NotRun => (p, f, n + 1, r),
                Status::Skipped | Status::NotRunnable => (p, f, n, r),
            })
    }

    /// The innermost test whose source spans `line` in `file`.
    pub fn test_at(&self, file: &Path, line: usize) -> Option<&TestCase> {
        self.tests
            .iter()
            .filter(|t| t.file == file && t.found.line <= line && t.found.end_line >= line)
            .max_by_key(|t| t.found.line)
    }

    /// The innermost suite spanning `line` in `file`.
    pub fn suite_at(&self, file: &Path, line: usize) -> Option<&FoundSuite> {
        self.suites
            .get(file)?
            .iter()
            .filter(|s| s.line <= line && s.end_line >= line)
            .max_by_key(|s| s.line)
    }
}

/// The kind of test file `path` is, by name (C# files are checked against their project).
pub fn js_test_kind(path: &Path) -> Option<Framework> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let (stem, ext) = name.rsplit_once('.')?;
    if !matches!(
        ext,
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx"
    ) {
        return None;
    }
    if stem.ends_with(".stories") {
        return Some(Framework::Storybook);
    }
    let in_tests_dir = path.components().any(|c| c.as_os_str() == "__tests__");
    // The runner (Vitest or Jest) is decided by the project; Vitest is a placeholder here.
    (stem.ends_with(".spec") || stem.ends_with(".test") || in_tests_dir)
        .then_some(Framework::Vitest)
}

pub fn is_cs(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cs"))
}

/// Find the project of a test file: the nearest csproj folder for C#, the nearest package.json
/// folder for JS. C# projects only count if they reference a test framework.
pub fn detect_project(file: &Path) -> Option<Project> {
    let dir = file.parent()?;
    if is_cs(file) {
        for d in dir.ancestors() {
            let Ok(rd) = std::fs::read_dir(d) else {
                continue;
            };
            let csproj = rd.flatten().map(|e| e.path()).find(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("csproj"))
            })?;
            let text = std::fs::read_to_string(&csproj)
                .unwrap_or_default()
                .to_lowercase();
            let runner = if text.contains("xunit") {
                Framework::XUnit
            } else if text.contains("nunit") {
                Framework::NUnit
            } else if text.contains("mstest") {
                Framework::MSTest
            } else {
                return None;
            };
            return Some(Project {
                root: d.to_path_buf(),
                name: csproj
                    .file_stem()
                    .map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
                runner: Some(runner),
                stories_runnable: false,
                csproj: Some(csproj),
            });
        }
        return None;
    }
    let root = dir.ancestors().find(|d| d.join("package.json").is_file())?;
    let pkg = std::fs::read_to_string(root.join("package.json")).unwrap_or_default();
    let has_bin = |n: &str| root.join("node_modules/.bin").join(n).exists();
    let runner = if pkg.contains("\"vitest\"") || has_bin("vitest") {
        Some(Framework::Vitest)
    } else if pkg.contains("\"jest\"") || has_bin("jest") {
        Some(Framework::Jest)
    } else {
        None
    };
    let stories_runnable = runner == Some(Framework::Vitest)
        && [
            "vitest.config.js",
            "vitest.config.ts",
            "vitest.config.mjs",
            "vite.config.js",
            "vite.config.ts",
            "vite.config.mjs",
        ]
        .iter()
        .any(|f| std::fs::read_to_string(root.join(f)).is_ok_and(|t| t.contains("storybook")));
    Some(Project {
        root: root.to_path_buf(),
        name: root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        runner,
        stories_runnable,
        csproj: None,
    })
}

/// Discover the tests in a file's text, if it is a test file.
pub fn discover(path: &Path, text: &str, project: &Project) -> Option<FileTests> {
    if is_cs(path) {
        let fw = project.runner.filter(|r| r.is_dotnet())?;
        return Some(discover_cs::discover(text, fw));
    }
    match js_test_kind(path)? {
        Framework::Storybook => Some(discover_js::stories(path, text)),
        _ => Some(discover_js::specs(
            text,
            project.runner.unwrap_or(Framework::Vitest),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(name: &str, line: usize, body: &str) -> Found {
        Found {
            name: name.into(),
            suite: vec!["S".into()],
            line,
            end_line: line + 2,
            hash: hash_text(body),
            full_name: None,
            framework: Framework::Vitest,
        }
    }

    fn pass(t: &mut TestTree, name: &str) {
        let tc = t.tests.iter_mut().find(|x| x.found.name == name).unwrap();
        tc.result = Some(RunResult {
            status: Status::Passed,
            duration: None,
            message: None,
            hash: tc.found.hash,
        });
    }

    fn status(t: &TestTree, name: &str) -> Status {
        t.tests
            .iter()
            .find(|x| x.found.name == name)
            .unwrap()
            .status()
    }

    #[test]
    fn edits_reset_status_and_undo_restores_it() {
        let file = Path::new("/w/a.spec.js");
        let mut t = TestTree::default();
        let ft = |a: &str, b: &str| FileTests {
            tests: vec![found("a", 1, a), found("b", 5, b)],
            suites: vec![],
        };
        t.replace_file(file, ft("expect(1)", "expect(2)"));
        assert_eq!(status(&t, "a"), Status::NotRun);
        pass(&mut t, "a");
        pass(&mut t, "b");
        assert_eq!(t.counts(), (2, 0, 0, 0));

        // Edit test b: back to not run; a keeps its result. Re-indenting does not count.
        t.replace_file(file, ft("expect(1)", "expect(3)"));
        assert_eq!(status(&t, "a"), Status::Passed);
        assert_eq!(status(&t, "b"), Status::NotRun);
        t.replace_file(file, ft("  expect(1)  ", "expect(2)")); // undo + whitespace
        assert_eq!(status(&t, "b"), Status::Passed);

        // A new test appears; a removed one goes.
        let ft2 = FileTests {
            tests: vec![found("a", 1, "expect(1)"), found("c", 9, "x")],
            suites: vec![],
        };
        t.replace_file(file, ft2);
        let names: Vec<_> = t.tests.iter().map(|x| x.found.name.as_str()).collect();
        assert_eq!(names, ["a", "c"]);
        assert_eq!(status(&t, "c"), Status::NotRun);
        assert_eq!(t.test_at(file, 10).unwrap().found.name, "c");
    }

    #[test]
    fn test_file_kinds() {
        assert_eq!(
            js_test_kind(Path::new("a/format.spec.js")),
            Some(Framework::Vitest)
        );
        assert_eq!(
            js_test_kind(Path::new("x.test.ts")),
            Some(Framework::Vitest)
        );
        assert_eq!(
            js_test_kind(Path::new("__tests__/x.js")),
            Some(Framework::Vitest)
        );
        assert_eq!(
            js_test_kind(Path::new("Button.stories.tsx")),
            Some(Framework::Storybook)
        );
        assert_eq!(js_test_kind(Path::new("format.js")), None);
    }
}
