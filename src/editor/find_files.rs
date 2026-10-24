//! Find in Files (Ctrl+Shift+F): drives the background grep and the results panel, and the
//! search option toggles shared with Find.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::Editor;
use super::search::Query;
use super::selection::Selection;
use crate::commands::{Command, Motion};
use crate::ui::find_panel::Field;
use crate::ui::minibuffer::PromptKind;
use crate::workspace::grep::{SearchJob, SearchMsg, SearchRequest};

/// Typing pause before the search re-runs.
const DEBOUNCE: Duration = Duration::from_millis(150);
/// How often running jobs are polled.
const POLL: Duration = Duration::from_millis(16);

impl Editor {
    /// Ctrl+Shift+F: open the panel (with the selected text as the query), or close it.
    pub(super) fn toggle_find_in_files(&mut self) {
        if self.find.open {
            self.find.open = false;
            return;
        }
        self.minibuffer = None;
        self.picker = None;
        self.explorer.focused = false;
        self.find.open = true;
        self.find.field = Field::Query;
        let sel = if self.has_doc() {
            self.doc().selected_text()
        } else {
            String::new()
        };
        let changed = !sel.is_empty() && !sel.contains('\n') && sel != self.find.query;
        if changed {
            self.find.query = sel;
        }
        // Keep the previous results unless the query changed or they were never computed.
        if changed || (self.find.results.is_empty() && !self.find.running()) {
            self.restart_search();
        }
    }

    /// Where the search runs: the workspace folders, or the current file's folder.
    fn search_roots(&self) -> Vec<PathBuf> {
        match &self.workspace {
            Some(ws) if !ws.folders.is_empty() => ws.folders.clone(),
            _ => {
                let dir = PathBuf::from(self.dir_hint());
                vec![std::fs::canonicalize(&dir).unwrap_or(dir)]
            }
        }
    }

    fn search_scope(roots: &[PathBuf]) -> String {
        let names: Vec<String> = roots
            .iter()
            .map(|r| {
                r.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| r.display().to_string())
            })
            .collect();
        names.join(", ")
    }

    /// Start the search now with the current query, filter and options.
    pub(super) fn restart_search(&mut self) {
        self.find.debounce_at = None;
        self.find.clear_results();
        let roots = self.search_roots();
        self.find.scope = Self::search_scope(&roots);
        let query = match Query::new(&self.find.query, self.search_opts) {
            Ok(Some(q)) => q,
            Ok(None) => return,
            Err(e) => {
                self.find.error = Some(e);
                return;
            }
        };
        // Search open documents with unsaved edits as they are in the editor.
        let unsaved: HashMap<PathBuf, String> = self
            .all_docs()
            .filter(|d| d.is_dirty())
            .filter_map(|d| {
                let p = d.path.as_ref()?;
                let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
                Some((p, d.rope.to_string()))
            })
            .collect();
        let req = SearchRequest {
            roots,
            respect_gitignore: self.workspace.as_ref().is_none_or(|w| w.respect_gitignore),
            include: self.find.include.clone(),
            query,
            unsaved,
        };
        log::debug!(
            "find in files \"{}\" include \"{}\" {:?}",
            self.find.query,
            self.find.include,
            self.search_opts
        );
        match SearchJob::spawn(req) {
            Ok(job) => self.find.job = Some(job),
            Err(e) => self.find.error = Some(e),
        }
    }

    fn schedule_search(&mut self) {
        self.find.debounce_at = Some(Instant::now() + DEBOUNCE);
    }

    /// Collect background results (search, formatting). Returns true if anything visible
    /// changed.
    pub fn poll_jobs(&mut self) -> bool {
        let formatted = self.poll_format();
        let linted = self.poll_lint();
        self.track_test_edits();
        let tested = self.poll_tests();
        self.poll_search() || formatted || linted || tested
    }

    fn poll_search(&mut self) -> bool {
        let mut changed = false;
        if self.find.debounce_at.is_some_and(|t| Instant::now() >= t) {
            self.restart_search();
            changed = true;
        }
        let Some(job) = &mut self.find.job else {
            return changed;
        };
        if !job.running() {
            return changed;
        }
        let mut files = Vec::new();
        let mut done = None;
        for msg in job.drain() {
            match msg {
                SearchMsg::File(f) => files.push(f),
                SearchMsg::Done { capped } => done = Some(capped),
            }
        }
        let elapsed = job.elapsed;
        // Running jobs update their progress counter, so redraw while they run.
        changed = true;
        self.find.add_results(files);
        if let Some(capped) = done {
            self.find.capped = capped;
            self.find.elapsed = elapsed;
        }
        changed
    }

    /// When the app should call `poll_jobs` next, if anything is pending.
    pub fn next_deadline(&self) -> Option<Instant> {
        let poll = if self.find.running() {
            Some(Instant::now() + POLL)
        } else if self.formatting() {
            Some(Instant::now() + super::formatting::FORMAT_POLL)
        } else if self.linting() {
            Some(Instant::now() + super::linting::LINT_POLL)
        } else {
            None
        };
        [self.find.debounce_at, poll, self.tests_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    /// Background work still running or scheduled.
    pub fn jobs_pending(&self) -> bool {
        self.find.running()
            || self.find.debounce_at.is_some()
            || self.formatting()
            || self.linting()
            || self.tests_busy()
    }

    /// Wait (blocking) until background work is done, up to `timeout`. For headless scripts.
    pub fn wait_jobs(&mut self, timeout: Duration) {
        let end = Instant::now() + timeout;
        while self.jobs_pending() && Instant::now() < end {
            if let Some(t) = self.find.debounce_at {
                std::thread::sleep(t.saturating_duration_since(Instant::now()));
            } else {
                std::thread::sleep(Duration::from_millis(2));
            }
            self.poll_jobs();
        }
    }

    /// Typed text while the panel is open.
    pub(super) fn find_panel_type(&mut self, text: &str) {
        self.find
            .field_mut()
            .extend(text.chars().filter(|c| !c.is_control()));
        self.schedule_search();
    }

    /// Commands while the panel is open. Returns false to let the command run normally.
    pub(super) fn find_command(&mut self, cmd: Command) -> bool {
        use Command::*;
        let page = self.find.visible_rows.saturating_sub(1).max(1) as isize;
        let f = &mut self.find;
        match cmd {
            Cancel | FindInFiles => f.open = false,
            Move(Motion::Up, _) => f.move_selection(-1),
            Move(Motion::Down, _) => f.move_selection(1),
            Move(Motion::PageUp, _) => f.move_selection(-page),
            Move(Motion::PageDown, _) => f.move_selection(page),
            Move(Motion::Home | Motion::DocStart, _) => f.select(0),
            Move(Motion::End | Motion::DocEnd, _) => f.select(f.rows.len().saturating_sub(1)),
            Indent | Outdent => {
                f.field = match f.field {
                    Field::Query => Field::Include,
                    Field::Include => Field::Query,
                }
            }
            Newline => self.open_selected_result(true),
            Backspace => {
                if f.field_mut().pop().is_some() {
                    self.schedule_search();
                }
            }
            DeleteWordLeft => {
                let s = f.field_mut();
                let keep = s
                    .trim_end()
                    .rfind([' ', ',', '/', '.'])
                    .map_or(0, |i| i + 1);
                let keep = if keep >= s.len() { 0 } else { keep };
                s.truncate(keep);
                self.schedule_search();
            }
            Paste => {
                if let Some(text) = self.get_clipboard() {
                    self.find_panel_type(text.lines().next().unwrap_or(""));
                }
            }
            ToggleCase | ToggleWholeWord | ToggleRegex | ZoomIn | ZoomOut | ZoomReset | Save
            | FormatAndSave | NextProblem | PrevProblem | Quit | NextResult | PrevResult => {
                return false;
            }
            // Anything that leaves the panel (opening files, switching views, ...) closes it.
            NewFile | OpenFile | OpenPath | Help | ShowShortcuts | CloseFile | NextTab
            | PrevTab | GotoTab(_) | FocusOtherView | FocusLeft | FocusRight | ToggleSplit
            | QuickOpen | ToggleExplorer | FocusExplorer | NewWorkspace | OpenWorkspace
            | SaveWorkspace | SaveWorkspaceAs | AddFolder | RemoveFolder | ToggleGitignore
            | CloseWorkspace | Find | GotoLine | FileMenu | WorkspaceMenu | SelectionMenu
            | PanelMenu | ClosePanelTab | LintWorkspace | ShowProblems => {
                f.open = false;
                return false;
            }
            _ => {}
        }
        true
    }

    /// Open the selected match with the match selected. `close` hides the panel.
    pub(super) fn open_selected_result(&mut self, close: bool) {
        let Some((f, hit)) = self.find.selected_hit() else {
            return;
        };
        let (path, line, start, end) = (f.abs.clone(), hit.line, hit.start, hit.end);
        self.find.visited = true;
        if close {
            self.find.open = false;
        }
        self.open_path(&path);
        if self.has_doc() && self.doc().path.is_some() {
            self.select_in_line(line, start, end);
        }
    }

    /// Mouse click on result row `index`; `open` on double-click.
    pub fn find_click(&mut self, index: usize, open: bool) {
        if index < self.find.rows.len() {
            self.find.select(index);
            if open {
                self.open_selected_result(true);
            }
        }
    }

    pub fn find_focus_field(&mut self, field: Field) {
        self.find.field = field;
    }

    /// F8 / Shift+F8: go to the next / previous result without opening the panel.
    pub(super) fn step_result(&mut self, forward: bool) {
        // A search typed but not yet run (or still running) is finished first.
        if self.find.debounce_at.is_some() {
            self.restart_search();
        }
        if self.find.running() && self.find.match_count == 0 {
            self.wait_jobs(Duration::from_secs(2));
        }
        let first = !self.find.visited
            && self.find.match_number().is_some()
            && self.find.selected_hit().is_some();
        if !first && !self.find.step(forward) {
            self.info("No Find in Files results (Ctrl+Shift+F searches)");
            return;
        }
        self.open_selected_result(false);
        if let Some(n) = self.find.match_number() {
            self.info(format!("Result {n} of {}", self.find.match_count));
        }
    }

    /// Alt+C / Alt+O / Alt+R: flip a search option and re-run whichever search is active.
    pub(super) fn toggle_search_option(&mut self, cmd: Command) {
        let o = &mut self.search_opts;
        let (name, on) = match cmd {
            Command::ToggleCase => {
                o.case_sensitive = !o.case_sensitive;
                ("Match case", o.case_sensitive)
            }
            Command::ToggleWholeWord => {
                o.whole_word = !o.whole_word;
                ("Whole word", o.whole_word)
            }
            _ => {
                o.regex = !o.regex;
                ("Regex", o.regex)
            }
        };
        let finding = matches!(
            self.minibuffer.as_ref().map(|m| &m.kind),
            Some(PromptKind::Find { .. })
        );
        if self.find.open {
            self.restart_search();
        } else if finding {
            self.prompt_changed();
        } else {
            self.info(format!("{name} {}", if on { "on" } else { "off" }));
        }
    }

    /// Select chars `start..end` of 0-based `line` (clamped), centering the line.
    fn select_in_line(&mut self, line: usize, start: usize, end: usize) {
        let rows = self.pane().rows;
        let doc = self.doc_mut();
        let line = line.min(doc.rope.len_lines().saturating_sub(1));
        let ls = doc.rope.line_to_char(line);
        let len = super::movement::line_content_end(&doc.rope, line) - ls;
        doc.selection = Selection::new(ls + start.min(len), ls + end.min(len));
        doc.history.break_coalescing();
        doc.view.top_line = line.saturating_sub(rows / 3);
        doc.view.left_col = 0;
        self.ensure_cursor_visible();
    }
}
