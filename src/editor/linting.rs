//! ESLint runs (on open, on save, whole workspace with Ctrl+Shift+M) and the Problems page in
//! the panel: a list of diagnostics where moving the cursor shows each one in the editor.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ropey::Rope;

use super::selection::Selection;
use super::{EDITOR, Editor, PANEL};
use crate::lint::eslint::{self, LintJob, Scope};
use crate::lint::{Diagnostic, Severity, Source};
use crate::syntax::Lang;

/// Title of the Problems page.
pub const PROBLEMS_TITLE: &str = "Problems";

/// How often running lint jobs are polled.
pub(super) const LINT_POLL: Duration = Duration::from_millis(16);

/// A location a Problems row points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
}

impl Editor {
    // ------------------------------------------------------------ running ESLint

    /// Lint a document in the background if it is a lintable file in an ESLint project.
    /// Quiet: nothing happens (and nothing is said) without ESLint or a config.
    pub(super) fn lint_doc(&mut self, pane: usize, index: usize) {
        let Some(doc) = self.panes[pane].docs.get(index) else {
            return;
        };
        let Some(path) = doc.canonical.clone() else {
            return;
        };
        if doc.read_only || !eslint::lintable(&path) {
            return;
        }
        let Some(root) = eslint::config_root(&path) else {
            return;
        };
        let Some(bin) = eslint::find_eslint(&root) else {
            return;
        };
        let (text, revision) = (doc.rope.to_string(), doc.revision);
        // A newer request for the same file replaces the older one.
        self.lint_jobs
            .retain(|j| j.scope != Scope::File(path.clone()));
        self.lint_jobs
            .push(LintJob::file(bin, root, path, text, revision));
    }

    /// Ctrl+Shift+M: lint every ESLint project in the workspace (or the current file's
    /// project) and show the Problems page.
    pub(super) fn lint_workspace(&mut self) {
        let folders: Vec<PathBuf> = match &self.workspace {
            Some(ws) if !ws.folders.is_empty() => ws.folders.clone(),
            _ => vec![PathBuf::from(self.dir_hint())],
        };
        let mut roots: Vec<PathBuf> = folders.iter().flat_map(|f| eslint::roots_in(f)).collect();
        roots.sort();
        roots.dedup();
        self.show_problems();
        if roots.is_empty() {
            self.info("No ESLint config (eslint.config.js) found in the workspace");
            return;
        }
        let mut started = 0;
        for root in roots {
            match eslint::find_eslint(&root) {
                Some(bin) => {
                    log::info!("eslint workspace run in {}", root.display());
                    self.lint_jobs
                        .retain(|j| j.scope != Scope::Root(root.clone()));
                    self.lint_jobs.push(LintJob::root(bin, root));
                    started += 1;
                }
                None => log::info!("no eslint for {}", root.display()),
            }
        }
        if started == 0 {
            self.info("ESLint not found: run npm i -D eslint in the project");
        } else {
            self.info("Linting… (ESLint)");
            self.refresh_problems_page();
        }
    }

    pub(super) fn linting(&self) -> bool {
        !self.lint_jobs.is_empty()
    }

    /// Collect finished lint jobs. Returns true if anything changed.
    pub(super) fn poll_lint(&mut self) -> bool {
        let mut changed = false;
        let mut i = 0;
        while i < self.lint_jobs.len() {
            let Some(result) = self.lint_jobs[i].try_result() else {
                i += 1;
                continue;
            };
            let job = self.lint_jobs.remove(i);
            changed = true;
            match (result, &job.scope) {
                (Ok(report), Scope::File(path)) => {
                    // Results for text that has changed since are out of date.
                    let current = self
                        .find_doc(|d| d.canonical.as_deref() == Some(path.as_path()))
                        .map(|(p, i)| self.panes[p].docs[i].revision);
                    if current.is_none_or(|r| r == job.revision) {
                        let mut files = report.files;
                        let diags = files
                            .remove(path)
                            .or_else(|| files.into_values().next())
                            .unwrap_or_default();
                        self.problems.set_file(Source::Eslint, path, diags);
                    }
                }
                (Ok(report), Scope::Root(root)) => {
                    let (e, w) = count(report.files.values().flatten());
                    self.problems.set_under(Source::Eslint, root, report.files);
                    if !self.linting() {
                        self.info(format!(
                            "ESLint: {e} error{}, {w} warning{} ({:.1} s)",
                            plural(e),
                            plural(w),
                            job.elapsed().as_secs_f32()
                        ));
                    }
                }
                (Err(e), Scope::Root(_)) => self.error(e),
                (Err(e), Scope::File(_)) => log::warn!("eslint: {e}"),
            }
        }
        if changed {
            self.refresh_problems_page();
        }
        changed
    }

    /// Diagnostics from a format run (ESLint fixes) for a document.
    pub(super) fn set_doc_diagnostics(&mut self, path: &Path, diags: Vec<Diagnostic>) {
        self.problems.set_file(Source::Eslint, path, diags);
        self.refresh_problems_page();
    }

    // ------------------------------------------------------------ Problems page

    /// Open (or switch to) the Problems page in the panel.
    pub(super) fn show_problems(&mut self) {
        let (text, rows) = self.problems_text();
        self.open_virtual(PROBLEMS_TITLE, &text);
        self.problem_rows = rows;
        if let Some((p, i)) = self.problems_page() {
            self.panes[p].docs[i].syntax.set_lang(Lang::Problems);
        }
    }

    fn problems_page(&self) -> Option<(usize, usize)> {
        self.find_doc(|d| d.virtual_name.as_deref() == Some(PROBLEMS_TITLE))
    }

    /// The focused document is the Problems page.
    pub(super) fn on_problems_page(&self) -> bool {
        self.has_doc() && self.doc().virtual_name.as_deref() == Some(PROBLEMS_TITLE)
    }

    /// Rebuild the Problems page text after the diagnostics changed, keeping the cursor on the
    /// same problem when it is still listed.
    pub(crate) fn refresh_problems_page(&mut self) {
        let Some((p, i)) = self.problems_page() else {
            return;
        };
        let doc = &self.panes[p].docs[i];
        let line = doc.rope.char_to_line(doc.selection.head);
        let keep = self.problem_rows.get(line).cloned().flatten();
        let (text, rows) = self.problems_text();
        let new_line = keep
            .and_then(|t| rows.iter().position(|r| r.as_ref() == Some(&t)))
            .unwrap_or(line);
        let doc = &mut self.panes[p].docs[i];
        doc.rope = Rope::from_str(&text);
        doc.syntax.set_lang(Lang::Problems);
        let line = new_line.min(doc.rope.len_lines().saturating_sub(1));
        doc.selection = Selection::point(doc.rope.line_to_char(line));
        self.problem_rows = rows;
    }

    /// Page text and, per line, the location it points at.
    fn problems_text(&self) -> (String, Vec<Option<Target>>) {
        let (e, w) = self.problems.counts();
        let mut lines: Vec<(String, Option<Target>)> = vec![
            (
                format!(
                    "Problems: {e} error{}, {w} warning{}{}",
                    plural(e),
                    plural(w),
                    if self.linting() {
                        "   (linting…)"
                    } else {
                        ""
                    }
                ),
                None,
            ),
            (
                "ESLint · Ctrl+Shift+M lint again · Up/Down show · Enter open · F4 next".into(),
                None,
            ),
            (String::new(), None),
        ];
        if self.problems.is_empty() {
            lines.push(("No problems found.".into(), None));
        }
        for (path, diags) in self.problems.files() {
            lines.push((
                format!("{}  ({})", self.display_path(path), diags.len()),
                None,
            ));
            for d in diags {
                let mark = match d.severity {
                    Severity::Error => '✖',
                    Severity::Warning => '⚠',
                    Severity::Info => 'ℹ',
                };
                let code = d
                    .code
                    .as_ref()
                    .map(|c| format!("  [{c}]"))
                    .unwrap_or_default();
                let target = Target {
                    path: path.clone(),
                    line: d.line,
                    col: d.col,
                };
                lines.push((
                    format!("  {mark} {}:{}  {}{code}", d.line + 1, d.col + 1, d.message),
                    Some(target),
                ));
            }
        }
        let text = lines
            .iter()
            .map(|(l, _)| l.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        (text, lines.into_iter().map(|(_, t)| t).collect())
    }

    /// A path relative to the workspace folder that holds it, else the full path.
    fn display_path(&self, path: &Path) -> String {
        let folders = self
            .workspace
            .as_ref()
            .map_or(&[][..], |w| w.folders.as_slice());
        let multi = folders.len() > 1;
        if let Some(f) = folders.iter().find(|f| path.starts_with(f)) {
            return crate::workspace::files::rel_path(f, path, multi);
        }
        // File mode: relative to the folder all listed files share.
        let mut common: Option<PathBuf> = None;
        for (p, _) in self.problems.files() {
            let dir = p.parent().unwrap_or(p).to_path_buf();
            common = Some(match common {
                None => dir,
                Some(c) => c
                    .ancestors()
                    .find(|a| dir.starts_with(a))
                    .map_or_else(PathBuf::new, Path::to_path_buf),
            });
        }
        match common {
            Some(c) if !c.as_os_str().is_empty() => {
                crate::workspace::files::rel_path(&c, path, false)
            }
            _ => path.display().to_string(),
        }
    }

    /// The location of the Problems page row under its cursor.
    fn problems_cursor_target(&self) -> Option<Target> {
        let doc = self.doc();
        let line = doc.rope.char_to_line(doc.selection.head);
        self.problem_rows.get(line).cloned().flatten()
    }

    /// After a cursor move on the Problems page: show that problem in the editor, keeping the
    /// cursor in the panel.
    pub(super) fn preview_problem(&mut self) {
        if let Some(t) = self.problems_cursor_target() {
            self.show_location(&t, false);
        }
    }

    /// Enter or double-click on the Problems page: open the problem and go there.
    pub(super) fn open_problem(&mut self) {
        if let Some(t) = self.problems_cursor_target() {
            self.preview_doc = None;
            self.show_location(&t, true);
        }
    }

    /// Show `t` in the editor view, centered. Files opened just for a preview replace the
    /// previous preview tab unless it was edited.
    pub(super) fn show_location(&mut self, t: &Target, focus_editor: bool) {
        let open = self.find_doc(|d| d.canonical.as_deref() == Some(t.path.as_path()));
        if open.is_none()
            && let Some(prev) = self.preview_doc.take()
            && let Some((p, i)) = self.find_doc(|d| d.canonical.as_deref() == Some(prev.as_path()))
            && !self.panes[p].docs[i].is_dirty()
        {
            self.panes[p].docs.remove(i);
            let pane = &mut self.panes[p];
            pane.active = pane.active.min(pane.docs.len().saturating_sub(1));
        }
        self.open_path(&t.path);
        if open.is_none() && !focus_editor {
            self.preview_doc = Some(t.path.clone());
        }
        let rows = self.panes[EDITOR].rows;
        let pane = &mut self.panes[EDITOR];
        if let Some(doc) = pane.docs.get_mut(pane.active)
            && doc.canonical.as_deref() == Some(t.path.as_path())
        {
            let line = t.line.min(doc.rope.len_lines().saturating_sub(1));
            let start = doc.rope.line_to_char(line);
            let len = super::movement::line_content_end(&doc.rope, line) - start;
            doc.selection = Selection::point(start + t.col.min(len));
            doc.collapse_extra();
            doc.history.break_coalescing();
            doc.view.top_line = line.saturating_sub(rows / 3);
            doc.view.left_col = 0;
        }
        if focus_editor {
            self.focus_view(EDITOR);
            self.ensure_cursor_visible();
        } else {
            self.focus_view(EDITOR);
            self.ensure_cursor_visible();
            self.focus_view(PANEL);
        }
    }

    /// F4 / Shift+F4: the next or previous problem after the cursor, across files, wrapping.
    pub(super) fn step_problem(&mut self, forward: bool) {
        let all: Vec<Target> = self
            .problems
            .all()
            .map(|(p, d)| Target {
                path: p.clone(),
                line: d.line,
                col: d.col,
            })
            .collect();
        if all.is_empty() {
            self.info("No problems (Ctrl+Shift+M lints the workspace)");
            return;
        }
        let in_panel = self.on_problems_page();
        // Where we are: the selected row on the Problems page, else the editor cursor.
        let here = if in_panel {
            self.problems_cursor_target()
        } else {
            self.panes[EDITOR].doc().and_then(|d| {
                let path = d.canonical.clone()?;
                let line = d.rope.char_to_line(d.selection.head);
                let col = d.selection.head - d.rope.line_to_char(line);
                Some(Target { path, line, col })
            })
        };
        let key = |t: &Target| (t.path.clone(), t.line, t.col);
        let index = match &here {
            Some(h) if forward => all.iter().position(|t| key(t) > key(h)).unwrap_or(0),
            Some(h) => all
                .iter()
                .rposition(|t| key(t) < key(h))
                .unwrap_or(all.len() - 1),
            None if forward => 0,
            None => all.len() - 1,
        };
        let target = all[index].clone();
        // Keep the Problems page in step.
        if let Some((p, i)) = self.problems_page()
            && let Some(row) = self
                .problem_rows
                .iter()
                .position(|r| r.as_ref() == Some(&target))
        {
            let doc = &mut self.panes[p].docs[i];
            doc.selection = Selection::point(doc.rope.line_to_char(row));
        }
        self.show_location(&target, !in_panel);
        let msg = self
            .problems
            .all()
            .nth(index)
            .map(|(_, d)| d.message.clone())
            .unwrap_or_default();
        self.info(format!("Problem {}/{}: {msg}", index + 1, all.len()));
    }

    /// The diagnostic on the editor's cursor line, for the status bar.
    pub fn cursor_diagnostic(&self) -> Option<&Diagnostic> {
        let doc = self.panes[EDITOR].doc()?;
        let path = doc.canonical.as_ref()?;
        let line = doc.rope.char_to_line(doc.selection.head);
        self.problems
            .for_file(path)
            .iter()
            .filter(|d| d.line == line)
            .min_by_key(|d| d.severity)
    }
}

fn count<'a>(diags: impl Iterator<Item = &'a Diagnostic>) -> (usize, usize) {
    diags.fold((0, 0), |(e, w), d| match d.severity {
        Severity::Error => (e + 1, w),
        Severity::Warning => (e, w + 1),
        Severity::Info => (e, w),
    })
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, Motion};
    use crate::config::Config;

    fn diag(line: usize, severity: Severity, msg: &str) -> Diagnostic {
        Diagnostic {
            source: Source::Eslint,
            severity,
            line,
            col: 2,
            end_line: line,
            end_col: 4,
            message: msg.into(),
            code: Some("rule".into()),
        }
    }

    /// Each test gets its own folder: tests run in parallel.
    fn setup(name: &str) -> (Editor, PathBuf, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("jmx-problems-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        let a = dir.join("a.js");
        let b = dir.join("b.js");
        std::fs::write(&a, "line0\nline1\nline2\nline3\n").unwrap();
        std::fs::write(&b, "b0\nb1\n").unwrap();
        let mut ed = Editor::new(Config::default(), &[]);
        ed.problems.set_file(
            Source::Eslint,
            &a,
            vec![
                diag(1, Severity::Error, "bad a1"),
                diag(3, Severity::Warning, "warn a3"),
            ],
        );
        ed.problems.set_file(
            Source::Eslint,
            &b,
            vec![diag(0, Severity::Warning, "warn b0")],
        );
        (ed, dir, a, b)
    }

    #[test]
    fn page_lists_problems_and_previews_them() {
        let (mut ed, dir, a, b) = setup("page");
        ed.execute(Command::ShowProblems);
        assert_eq!(ed.focus, PANEL);
        let text = ed.doc().rope.to_string();
        assert!(text.starts_with("Problems: 1 error, 2 warnings"), "{text}");
        assert!(text.contains("  ✖ 2:3  bad a1  [rule]"), "{text}");
        // Cursor onto the first problem row: the editor shows a.js at line 1, focus stays.
        let row = ed.problem_rows.iter().position(Option::is_some).unwrap();
        for _ in 0..row {
            ed.execute(Command::Move(Motion::Down, false));
        }
        assert_eq!(ed.focus, PANEL);
        let shown = ed.panes[EDITOR].doc().unwrap();
        assert_eq!(shown.canonical.as_deref(), Some(a.as_path()));
        assert_eq!(shown.rope.char_to_line(shown.selection.head), 1);
        // Down twice more: b.js replaces the a.js preview tab.
        ed.execute(Command::Move(Motion::Down, false));
        ed.execute(Command::Move(Motion::Down, false)); // file header of b.js: no change
        ed.execute(Command::Move(Motion::Down, false));
        let titles: Vec<String> = ed.panes[EDITOR].docs.iter().map(|d| d.title()).collect();
        assert!(
            titles.contains(&"b.js".to_string()) && !titles.contains(&"a.js".to_string()),
            "{titles:?}"
        );
        // Enter opens it for real and moves the cursor to the editor.
        ed.execute(Command::Newline);
        assert_eq!(ed.focus, EDITOR);
        assert_eq!(ed.doc().canonical.as_deref(), Some(b.as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn f4_walks_problems_across_files() {
        let (mut ed, dir, a, b) = setup("f4");
        ed.execute(Command::NextProblem);
        assert_eq!(ed.doc().canonical.as_deref(), Some(a.as_path()));
        assert_eq!(ed.doc().rope.char_to_line(ed.doc().selection.head), 1);
        assert!(
            ed.message
                .as_ref()
                .unwrap()
                .text
                .starts_with("Problem 1/3: bad a1")
        );
        ed.execute(Command::NextProblem);
        assert_eq!(ed.doc().rope.char_to_line(ed.doc().selection.head), 3);
        ed.execute(Command::NextProblem);
        assert_eq!(ed.doc().canonical.as_deref(), Some(b.as_path()));
        ed.execute(Command::NextProblem); // wraps
        assert_eq!(ed.doc().canonical.as_deref(), Some(a.as_path()));
        ed.execute(Command::PrevProblem); // wraps back
        assert_eq!(ed.doc().canonical.as_deref(), Some(b.as_path()));
        assert_eq!(ed.cursor_diagnostic().unwrap().message, "warn b0");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
