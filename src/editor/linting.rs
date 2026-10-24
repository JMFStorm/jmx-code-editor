//! ESLint runs (on open, on save, whole workspace with Ctrl+Shift+M) and the Problems page in
//! the panel: a list of diagnostics where moving the cursor shows each one in the editor.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ropey::Rope;

use super::page::{PageBox, PageText};
use super::selection::Selection;
use super::{EDITOR, Editor, PANEL};
use crate::lint::eslint::{self, LintJob, Scope};
use crate::lint::{Diagnostic, Severity, Source};
use crate::syntax::Lang;
use crate::syntax::Token;

/// Title of the Problems page.
pub const PROBLEMS_TITLE: &str = "Problems";

/// How often running lint jobs are polled.
pub(super) const LINT_POLL: Duration = Duration::from_millis(16);

/// The Problems page as text, with what each line points at.
struct ProblemsPage {
    text: String,
    rows: Vec<Option<Target>>,
    boxes: Vec<PageBox>,
    chips: [(usize, usize); 3],
    /// The line the chips are on.
    chips_line: usize,
    /// Lines of the message box and the problem's severity.
    detail: Option<(usize, usize, Severity)>,
}

/// The place a list (References, Problems, F4, Go to Definition) just showed in the
/// editor, highlighted there until the text changes or the cursor leaves its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub path: PathBuf,
    pub line: usize,
    /// Char columns of the highlighted range.
    pub start_col: usize,
    pub end_col: usize,
    /// The document's revision when shown.
    pub revision: u64,
}

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

    /// Lint every ESLint project in the workspace (or the current file's project). `show`
    /// (Ctrl+Shift+M) opens the Problems page and reports; automatic runs (workspace opened)
    /// are quiet and only fill in the problems.
    pub(super) fn lint_workspace(&mut self, show: bool) {
        let folders: Vec<PathBuf> = match &self.workspace {
            Some(ws) if !ws.folders.is_empty() => ws.folders.clone(),
            _ => vec![PathBuf::from(self.dir_hint())],
        };
        let mut roots: Vec<PathBuf> = folders.iter().flat_map(|f| eslint::roots_in(f)).collect();
        roots.sort();
        roots.dedup();
        if show {
            self.show_problems();
        }
        if roots.is_empty() {
            if show {
                self.info("No ESLint config (eslint.config.js) found in the workspace");
            }
            return;
        }
        let mut started = 0;
        for root in roots {
            match eslint::find_eslint(&root) {
                Some(bin) => {
                    log::info!("eslint workspace run in {}", root.display());
                    self.lint_jobs
                        .retain(|j| j.scope != Scope::Root(root.clone()));
                    let job = LintJob::root(bin, root);
                    self.lint_jobs.push(if show { job } else { job.quiet() });
                    started += 1;
                }
                None => log::info!("no eslint for {}", root.display()),
            }
        }
        if !show {
            return;
        }
        if started == 0 {
            self.info("ESLint not found: run npm i -D eslint in the project");
        } else {
            self.info("Linting… (ESLint)");
            self.refresh_problems_page();
        }
    }

    /// Lint a file from disk (changed outside the editor and not open).
    pub(super) fn lint_file_on_disk(&mut self, path: &Path) {
        if !eslint::lintable(path) {
            return;
        }
        let Some(root) = eslint::config_root(path) else {
            return;
        };
        let Some(bin) = eslint::find_eslint(&root) else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let path = path.to_path_buf();
        self.lint_jobs
            .retain(|j| j.scope != Scope::File(path.clone()));
        self.lint_jobs
            .push(LintJob::file(bin, root, path, text, 0).quiet());
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
                    if !self.linting() && !job.quiet {
                        self.info(format!(
                            "ESLint: {e} error{}, {w} warning{} ({:.1} s)",
                            plural(e),
                            plural(w),
                            job.elapsed().as_secs_f32()
                        ));
                    }
                }
                (Err(e), Scope::Root(_)) if !job.quiet => self.error(e),
                (Err(e), Scope::Root(_)) => log::warn!("eslint: {e}"),
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
        // F4 walks problems again.
        self.refs.f4 = false;
        let page = self.problems_text();
        self.open_virtual(PROBLEMS_TITLE, &page.text);
        if let Some((p, i)) = self.problems_page() {
            self.panes[p].docs[i].boxes = page.boxes.clone();
        }
        self.problem_rows = page.rows;
        self.problem_chips = page.chips;
        self.problem_chips_line = page.chips_line;
        self.problem_detail = page.detail;
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
        let page = self.problems_text();
        self.problem_chips = page.chips;
        self.problem_chips_line = page.chips_line;
        // Box lines come after their row, so the first match is the problem's own row.
        let new_line = keep
            .and_then(|t| page.rows.iter().position(|r| r.as_ref() == Some(&t)))
            .unwrap_or(line);
        let changed = self.panes[p].docs[i].rope != page.text.as_str();
        let doc = &mut self.panes[p].docs[i];
        if changed {
            doc.rope = Rope::from_str(&page.text);
            crate::perf::bump(&crate::perf::PAGE_REBUILDS);
            doc.syntax.set_lang(Lang::Problems);
        }
        let line = new_line.min(doc.rope.len_lines().saturating_sub(1));
        doc.selection = Selection::point(doc.rope.line_to_char(line));
        doc.boxes = page.boxes;
        self.problem_rows = page.rows;
        self.problem_detail = page.detail;
    }

    /// Page text and, per line, the location it points at.
    fn problems_text(&self) -> ProblemsPage {
        let selected = self.problems_page_selection();
        let (e, w, i) = self.problems.counts3();
        let f = self.problem_filter;
        let busy = if self.linting() || self.tests_building() {
            format!("   {} checking…", spinner())
        } else {
            String::new()
        };
        // Line 1: one chip per severity; `·` instead of the icon when it is hidden.
        let mut show = String::from("Show:  ");
        let mut chips = [(0, 0); 3];
        for (k, (on, icon, label, n)) in [
            (f.errors, '✖', "Errors", e),
            (f.warnings, '⚠', "Warnings", w),
            (f.infos, 'ℹ', "Messages", i),
        ]
        .into_iter()
        .enumerate()
        {
            let start = show.chars().count();
            show.push_str(&format!("[{} {label} {n}]", if on { icon } else { '·' }));
            chips[k] = (start, show.chars().count());
            show.push_str("  ");
        }
        show.push_str(" e / w / m toggle · Enter open · F4 next");
        let mut page: PageText<Target> = PageText::new(self.panel_width());
        page.wrapped(
            &format!(
                "Problems: {e} error{} · {w} warning{} · {i} message{}{busy}",
                plural(e),
                plural(w),
                plural(i)
            ),
            2,
            None,
        );
        let chips_line = page.len();
        page.wrapped(&show, 7, None);
        page.line("", None);
        let mut detail: Option<(usize, usize, Severity)> = None;
        let visible: Vec<(&PathBuf, Vec<&Diagnostic>)> = self
            .problems
            .files()
            .map(|(p, d)| {
                (
                    p,
                    d.iter()
                        .filter(|d| f.allows(d.severity))
                        .collect::<Vec<_>>(),
                )
            })
            .filter(|(_, d)| !d.is_empty())
            .collect();
        if self.problems.is_empty() {
            page.line("No problems found.", None);
        } else if !(f.errors || f.warnings || f.infos) {
            page.line("All problem types are hidden (e / w / m)", None);
        } else if visible.is_empty() {
            page.line("No problems of the shown types.", None);
        }
        for (path, diags) in visible {
            page.wrapped(
                &format!("{}  ({})", self.display_path(path), diags.len()),
                0,
                None,
            );
            for d in diags {
                let mark = match d.severity {
                    Severity::Error => '✖',
                    Severity::Warning => '⚠',
                    Severity::Info => 'ℹ',
                };
                let target = Target {
                    path: path.clone(),
                    line: d.line,
                    col: d.col,
                };
                // Compact row: position, rule and source; the message goes in the box.
                let what = match &d.code {
                    Some(c) => c.clone(),
                    None => d.message.lines().next().unwrap_or("").to_string(),
                };
                let pos = format!("{}:{}", d.line + 1, d.col + 1);
                page.wrapped(
                    &format!("  {mark} {pos}  {what}  · {}", d.source.name()),
                    6 + pos.chars().count(),
                    Some(target.clone()),
                );
                // The selected problem shows its whole message in a box underneath.
                if selected.as_ref() == Some(&target) && detail.is_none() {
                    let accent = match d.severity {
                        Severity::Error => Token::Error,
                        Severity::Warning => Token::Warning,
                        Severity::Info => Token::Function,
                    };
                    let (a, b) =
                        page.boxed(d.message.trim(), 4, Some(target.clone()), Some(accent));
                    detail = Some((a, b, d.severity));
                }
            }
        }
        ProblemsPage {
            text: page.text(),
            rows: page.rows,
            boxes: page.boxes,
            chips,
            chips_line,
            detail,
        }
    }

    /// The problem the Problems page cursor is on (its row or its box).
    fn problems_page_selection(&self) -> Option<Target> {
        let (p, i) = self.problems_page()?;
        let doc = &self.panes[p].docs[i];
        let line = doc.rope.char_to_line(doc.selection.head);
        self.problem_rows.get(line).cloned().flatten()
    }

    /// Up / Down on the Problems page: the previous / next problem, skipping its message box.
    pub(super) fn problems_step(&mut self, delta: isize) {
        let doc = self.doc();
        let line = doc.rope.char_to_line(doc.selection.head) as isize;
        let in_box = |k: usize| {
            self.problem_detail
                .is_some_and(|(a, b, _)| k >= a && k <= b)
        };
        let here = self.problem_rows.get(line as usize).cloned().flatten();
        let mut k = line;
        loop {
            k += delta;
            if k < 0 || k as usize >= self.problem_rows.len() {
                return;
            }
            let k = k as usize;
            // Another problem's row (wrapped rows span several lines).
            if self.problem_rows[k].is_some() && !in_box(k) && self.problem_rows[k] != here {
                break;
            }
        }
        // Its first line.
        let mut k = k as usize;
        while k > 0 && self.problem_rows[k - 1] == self.problem_rows[k] && !in_box(k - 1) {
            k -= 1;
        }
        let doc = self.doc_mut();
        doc.selection = Selection::point(doc.rope.line_to_char(k));
        self.refresh_problems_page();
        self.preview_problem();
    }

    /// `c` on the Problems page: copy the selected problem's message.
    fn copy_problem_message(&mut self) {
        let msg = self.problems_page_selection().and_then(|t| {
            self.problems
                .for_file(&t.path)
                .iter()
                .find(|d| d.line == t.line && d.col == t.col)
                .map(|d| d.message.clone())
        });
        match msg {
            Some(m) => {
                self.set_clipboard(m);
                self.info("Copied the problem message");
            }
            None => self.info("No problem selected"),
        }
    }

    /// e / w / m on the Problems page: show or hide that severity.
    pub(super) fn problems_page_key(&mut self, c: char) {
        if c.eq_ignore_ascii_case(&'c') {
            self.copy_problem_message();
            return;
        }
        let sev = match c.to_ascii_lowercase() {
            'e' => Severity::Error,
            'w' => Severity::Warning,
            'm' | 'i' => Severity::Info,
            _ => return,
        };
        self.problem_filter.toggle(sev);
        self.refresh_problems_page();
    }

    /// A click on the filter line of the Problems page: toggle the chip under it.
    pub(super) fn problems_chip_click(&mut self) -> bool {
        let doc = self.doc();
        let line = doc.rope.char_to_line(doc.selection.head);
        if line != self.problem_chips_line {
            return false;
        }
        let col = doc.selection.head - doc.rope.line_to_char(line);
        let Some(k) = self
            .problem_chips
            .iter()
            .position(|&(a, b)| col >= a && col < b)
        else {
            return false;
        };
        let sev = [Severity::Error, Severity::Warning, Severity::Info][k];
        self.problem_filter.toggle(sev);
        self.refresh_problems_page();
        true
    }

    /// A path relative to the workspace folder that holds it, else the full path.
    pub(super) fn display_path(&self, path: &Path) -> String {
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
        // A problem at this spot marks its own range; anything else the word there.
        let problem_end = self
            .problems
            .for_file(&t.path)
            .iter()
            .find(|d| d.line == t.line && d.col == t.col && d.end_line == d.line)
            .map(|d| d.end_col);
        let pane = &mut self.panes[EDITOR];
        if let Some(doc) = pane.docs.get_mut(pane.active)
            && doc.canonical.as_deref() == Some(t.path.as_path())
        {
            let line = t.line.min(doc.rope.len_lines().saturating_sub(1));
            let start = doc.rope.line_to_char(line);
            let len = super::movement::line_content_end(&doc.rope, line) - start;
            let col = t.col.min(len);
            let end_col = match problem_end {
                Some(e) if e > col => e.min(len),
                _ => match super::symbols::word_at(&doc.rope, start + col) {
                    Some((ws, we, _)) if ws == start + col => we - start,
                    _ => len,
                },
            };
            self.located = Some(Located {
                path: t.path.clone(),
                line,
                start_col: col,
                end_col: end_col.max(col + 1).min(len.max(col + 1)),
                revision: doc.revision,
            });
            doc.selection = Selection::point(start + col);
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
        let filter = self.problem_filter;
        let all: Vec<Target> = self
            .problems
            .all()
            .filter(|(_, d)| filter.allows(d.severity))
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
            // Open the message box on the new problem.
            self.refresh_problems_page();
        }
        self.show_location(&target, !in_panel);
        let msg = self
            .problems
            .all()
            .filter(|(_, d)| filter.allows(d.severity))
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

/// Frame of the busy spinner (a braille dot circling, 10 frames a second).
pub fn spinner() -> char {
    const FRAMES: [char; 8] = ['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    FRAMES[(ms / 100 % 8) as usize]
}

/// Is `c` a spinner frame (braille pattern)?
pub fn is_spinner(c: char) -> bool {
    ('\u{2800}'..='\u{28FF}').contains(&c)
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
        let dir = std::env::temp_dir().join(format!("jdc-problems-{name}-{}", std::process::id()));
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
        assert!(
            text.starts_with("Problems: 1 error · 2 warnings · 0 messages"),
            "{text}"
        );
        // Compact rows: the message only shows in the box of the selected problem.
        assert!(text.contains("  ✖ 2:3  rule  · ESLint"), "{text}");
        assert!(
            !text.contains("bad a1") && ed.problem_detail.is_none(),
            "{text}"
        );
        // Down onto the first problem: the editor shows a.js at line 1, focus stays, and the
        // message opens in a box under the row.
        ed.execute(Command::Move(Motion::Down, false));
        assert_eq!(ed.focus, PANEL);
        let shown = ed.panes[EDITOR].doc().unwrap();
        assert_eq!(shown.canonical.as_deref(), Some(a.as_path()));
        assert_eq!(shown.rope.char_to_line(shown.selection.head), 1);
        let line = |ed: &Editor| ed.doc().rope.char_to_line(ed.doc().selection.head);
        let row = line(&ed);
        assert_eq!(ed.problem_detail, Some((row + 1, row + 1, Severity::Error)));
        assert_eq!(ed.doc().rope.line(row + 1).to_string(), "    │ bad a1\n");
        // Down skips the box to the next problem, and the box follows.
        ed.execute(Command::Move(Motion::Down, false));
        let row = line(&ed);
        assert!(ed.doc().rope.line(row).to_string().contains("4:3"));
        assert_eq!(
            ed.problem_detail,
            Some((row + 1, row + 1, Severity::Warning))
        );
        assert!(!ed.doc().rope.to_string().contains("bad a1"));
        // Down again skips b.js's header: b.js replaces the a.js preview tab.
        ed.execute(Command::Move(Motion::Down, false));
        let titles: Vec<String> = ed.panes[EDITOR].docs.iter().map(|d| d.title()).collect();
        assert!(
            titles.contains(&"b.js".to_string()) && !titles.contains(&"a.js".to_string()),
            "{titles:?}"
        );
        // `c` copies the message (internal clipboard only: tests run in parallel).
        ed.clipboard = None;
        ed.insert_text("c");
        assert_eq!(ed.internal_clip, "warn b0");
        // Enter on a box line opens the problem for real and moves the cursor to the editor.
        let (first, _, _) = ed.problem_detail.unwrap();
        let doc = ed.doc_mut();
        doc.selection = Selection::point(doc.rope.line_to_char(first));
        ed.execute(Command::Newline);
        assert_eq!(ed.focus, EDITOR);
        assert_eq!(ed.doc().canonical.as_deref(), Some(b.as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn long_messages_wrap_and_codeless_rows_show_the_message() {
        let (mut ed, dir, a, _) = setup("wrap");
        let mut long = diag(1, Severity::Error, &"word ".repeat(60));
        long.code = None;
        ed.problems.set_file(Source::Eslint, &a, vec![long]);
        ed.execute(Command::ShowProblems);
        let text = ed.doc().rope.to_string();
        // The row wraps (no `…`): its lines fit the panel, the source ends the last one.
        let width = ed.panel_width();
        let at = ed.problem_rows.iter().position(Option::is_some).unwrap();
        let span = ed.problem_rows[at..]
            .iter()
            .take_while(|r| **r == ed.problem_rows[at])
            .count();
        assert!(span > 1, "{text}");
        let lines: Vec<&str> = text.lines().skip(at).take(span).collect();
        assert!(lines.iter().all(|l| l.chars().count() <= width), "{text}");
        assert!(lines[0].contains("2:3  word word") && lines[span - 1].ends_with("· ESLint"));
        assert!(!text.contains('…'));
        // F4 from the editor lands on it and opens the box, wrapped to the panel.
        ed.execute(Command::FocusLeft);
        ed.execute(Command::NextProblem);
        let (first, last, _) = ed.problem_detail.unwrap();
        assert!(last > first, "message should wrap over several lines");
        let page = &ed.panes[PANEL].doc().unwrap().rope;
        for k in first..=last {
            let l = page.line(k).to_string();
            assert!(
                l.starts_with("    │ ")
                    && l.trim_end().chars().count() <= ed.panes[PANEL].cols.max(26),
                "{l}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn severity_filter_hides_rows_and_steers_f4() {
        let (mut ed, dir, a, b) = setup("filter");
        ed.execute(Command::ShowProblems);
        let rows = |ed: &Editor| ed.problem_rows.iter().filter(|r| r.is_some()).count();
        assert_eq!(rows(&ed), 3);
        // w hides the warnings: only a.js's error is left, and b.js is gone.
        ed.insert_text("w");
        assert_eq!(rows(&ed), 1);
        let text = ed.doc().rope.to_string();
        assert!(text.contains("[· Warnings 2]"), "{text}");
        assert!(
            text.contains("a.js  (1)") && !text.contains("b.js"),
            "{text}"
        );
        // F4 only visits shown problems.
        ed.execute(Command::FocusLeft);
        ed.execute(Command::NextProblem);
        ed.execute(Command::NextProblem);
        assert_eq!(ed.doc().canonical.as_deref(), Some(a.as_path()));
        assert!(ed.message.as_ref().unwrap().text.starts_with("Problem 1/1"));
        // Clicking the Errors chip hides errors too.
        ed.execute(Command::ShowProblems);
        let (s0, _) = ed.problem_chips[0];
        let pos = ed.doc().rope.line_to_char(1) + s0 + 1;
        ed.click(pos, false, 1);
        assert!(!ed.problem_filter.errors);
        let page = |ed: &Editor| ed.doc().rope.to_string();
        assert!(
            page(&ed).contains("No problems of the shown types"),
            "{}",
            page(&ed)
        );
        ed.insert_text("m"); // messages off too: everything is hidden
        assert!(
            page(&ed).contains("All problem types are hidden"),
            "{}",
            page(&ed)
        );
        let _ = b;
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
        // The problem's own range is marked in the editor (diag cols 2..4).
        ed.show_location(
            &Target {
                path: a.clone(),
                line: 1,
                col: 2,
            },
            true,
        );
        let shown = ed.panes[EDITOR].doc().unwrap();
        let loc = ed.located_in(shown).expect("highlighted");
        assert_eq!((loc.start_col, loc.end_col), (2, 4));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
