pub mod document;
mod find_files;
mod formatting;
pub mod history;
pub mod hover;
mod linting;
mod testing;
pub use linting::{PROBLEMS_TITLE, is_spinner, spinner};
pub use testing::{TESTS_TITLE, status_glyph};
mod docs_page;
pub mod movement;
mod multi;
pub mod page;
pub mod search;
pub mod selection;
pub mod symbols;
pub use docs_page::DOCS_TITLE;
mod references;
pub use references::REFERENCES_TITLE;
pub mod view;

use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::time::{Duration, Instant};

use crate::app_state::AppState;
use crate::commands::{Command, Motion};
use crate::config::{Config, MAX_FONT_SIZE, MIN_FONT_SIZE};
use crate::help;
use crate::ui::explorer::{Explorer, ExplorerAction};
use crate::ui::find_panel::FindPanel;
use crate::ui::minibuffer::{AfterSave, Minibuffer, PendingAction, PromptKind, expand_tilde};
use crate::ui::picker::{FilePicker, PickResult, PickerKind};
use crate::workspace::{self as ws_file, Workspace};
use document::{Document, Indent};
use history::EditKind;
use movement as mv;
use search::{Query, SearchOptions};
use selection::Selection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub text: String,
    pub kind: MsgKind,
    pub at: Instant,
}

pub const MESSAGE_TIMEOUT: Duration = Duration::from_secs(5);

/// One of the two side-by-side views. Each owns its tabs; a file is open in at most one view.
pub struct Pane {
    pub docs: Vec<Document>,
    pub active: usize,
    /// Viewport size in text cells, updated by the layout on resize/zoom.
    pub rows: usize,
    pub cols: usize,
}

impl Default for Pane {
    fn default() -> Self {
        Self {
            docs: Vec::new(),
            active: 0,
            rows: 30,
            cols: 80,
        }
    }
}

impl Pane {
    /// The active document, if the view has any tabs.
    pub fn doc(&self) -> Option<&Document> {
        self.docs.get(self.active)
    }
}

/// The left view: files and editing.
pub const EDITOR: usize = 0;
/// The right view: internal pages only (help, shortcuts; later problems and settings).
pub const PANEL: usize = 1;

pub const MIN_SPLIT: f32 = 0.15;
pub const MAX_SPLIT: f32 = 0.85;

pub struct Editor {
    /// Left (0) and right (1) views.
    pub panes: [Pane; 2],
    /// View that has the cursor.
    pub focus: usize,
    /// Two views side by side; otherwise only the focused view is shown.
    pub split: bool,
    /// Left view's share of the width.
    pub split_ratio: f32,
    /// Open workspace (workspace mode); `None` is file mode.
    pub workspace: Option<Workspace>,
    pub explorer: Explorer,
    pub config: Config,
    pub minibuffer: Option<Minibuffer>,
    pub picker: Option<FilePicker>,
    /// Number of list rows the picker overlay shows, updated by the app.
    pub picker_rows: usize,
    pub message: Option<Message>,
    pub find_query: String,
    /// Search toggles shared by Find and Find in Files.
    pub search_opts: SearchOptions,
    /// Find in Files panel; kept when closed so the results can be reopened and stepped.
    pub find: FindPanel,
    pub should_quit: bool,
    /// Prettier runs in progress.
    pub format_jobs: Vec<crate::format::FormatJob>,
    /// Lint errors and warnings, for the Problems page and the editor markers.
    pub problems: crate::lint::Problems,
    /// ESLint runs in progress.
    pub lint_jobs: Vec<crate::lint::eslint::LintJob>,
    /// What each line of the Problems page points at.
    pub problem_rows: Vec<Option<linting::Target>>,
    /// Severities the Problems list shows.
    pub problem_filter: crate::lint::ProblemFilter,
    /// Columns of the Errors / Warnings / Messages chips on the Problems page (line 1).
    pub problem_chips: [(usize, usize); 3],
    /// The line of the Problems page the chips are on.
    pub problem_chips_line: usize,
    /// The panel width its pages were last laid out at.
    pub panel_layout_cols: usize,
    /// Page lines of the message box under the selected problem, and its severity.
    pub problem_detail: Option<(usize, usize, crate::lint::Severity)>,
    /// File opened only to preview a problem; the next preview replaces it.
    preview_doc: Option<PathBuf>,
    /// Test explorer, builds and file watching.
    pub tests: testing::TestState,
    /// The editor file last revealed in the Explorer.
    revealed: Option<PathBuf>,
    /// Popup with a test failure or problems (mouse rest, Ctrl+I).
    pub hover: Option<hover::Hover>,
    /// Symbol index, parameter hints and Go Back history.
    pub symbols: symbols::SymbolState,
    /// The compiled find query, by query text and options.
    query_cache: std::cell::RefCell<Option<(String, SearchOptions, Option<Query>)>>,
    /// Highlighting did not reach the visible lines in the last frame's budget.
    pub syntax_pending: bool,
    /// The place a list just showed in the editor (highlighted there).
    pub located: Option<linting::Located>,
    /// Find All References: the last search and its page.
    pub refs: references::RefState,
    /// Open web pages in the browser (off for headless runs and tests).
    pub launch_urls: bool,
    /// The last web page opened (or that would have been).
    pub opened_url: Option<String>,
    /// Where the last workspace is remembered; `None` in headless runs and tests.
    pub state_file: Option<PathBuf>,
    clipboard: Option<arboard::Clipboard>,
    /// Fallback when the system clipboard is unavailable.
    internal_clip: String,
    /// Set when the last copy was a whole line (copy with no selection); pasting it inserts a line.
    line_clip: Option<String>,
}

impl Editor {
    pub fn new(config: Config, paths: &[PathBuf]) -> Self {
        let clipboard = arboard::Clipboard::new()
            .map_err(|e| log::warn!("system clipboard unavailable: {e}"))
            .ok();
        let mut ed = Self {
            panes: [Pane::default(), Pane::default()],
            focus: 0,
            split: true,
            split_ratio: 0.5,
            workspace: None,
            explorer: Explorer::default(),
            config,
            minibuffer: None,
            picker: None,
            picker_rows: 10,
            message: None,
            find_query: String::new(),
            search_opts: SearchOptions::default(),
            find: FindPanel::default(),
            should_quit: false,
            state_file: None,
            format_jobs: Vec::new(),
            problems: crate::lint::Problems::default(),
            lint_jobs: Vec::new(),
            problem_rows: Vec::new(),
            problem_filter: crate::lint::ProblemFilter::default(),
            problem_chips: [(0, 0); 3],
            problem_chips_line: 1,
            panel_layout_cols: 0,
            problem_detail: None,
            preview_doc: None,
            tests: testing::TestState::default(),
            revealed: None,
            hover: None,
            symbols: symbols::SymbolState::default(),
            refs: references::RefState::default(),
            located: None,
            syntax_pending: false,
            query_cache: Default::default(),
            launch_urls: !cfg!(test),
            opened_url: None,
            clipboard,
            internal_clip: String::new(),
            line_clip: None,
        };
        // A workspace file on the command line opens that workspace; other files open in it.
        let (workspaces, files): (Vec<&PathBuf>, Vec<&PathBuf>) =
            paths.iter().partition(|p| ws_file::is_workspace_file(p));
        // Files go to the left view, Help to the right; the cursor starts on the left.
        for p in &files {
            ed.open_path(p);
        }
        if ed.panes[0].docs.is_empty() {
            let doc = Document::new_empty(None, ed.config.tab_width);
            ed.panes[0].docs.push(doc);
        }
        ed.panes[0].active = 0;
        ed.panes[1]
            .docs
            .push(Document::new_virtual(help::HELP_TITLE, &help::help_text()));
        ed.focus = 0;
        if let Some(file) = workspaces.first() {
            ed.open_workspace_now(file);
            // Files given next to the workspace open in it.
            for p in &files {
                ed.open_path(p);
            }
        }
        ed
    }

    pub fn pane(&self) -> &Pane {
        &self.panes[self.focus]
    }

    pub fn pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.focus]
    }

    /// True if the focused view has a document (views can be empty).
    /// The highlighted shown place, if it is in `doc` and still current: same text, and the
    /// cursor still on its line.
    pub fn located_in(&self, doc: &document::Document) -> Option<&linting::Located> {
        let l = self.located.as_ref()?;
        (doc.canonical.as_deref() == Some(l.path.as_path())
            && doc.revision == l.revision
            && doc.rope.char_to_line(doc.selection.head) == l.line)
            .then_some(l)
    }

    pub fn has_doc(&self) -> bool {
        self.pane().doc().is_some()
    }

    /// The focused view's active document. Only call when `has_doc()`.
    pub fn doc(&self) -> &Document {
        let p = self.pane();
        &p.docs[p.active]
    }

    pub fn doc_mut(&mut self) -> &mut Document {
        let p = self.pane_mut();
        let i = p.active;
        &mut p.docs[i]
    }

    pub fn all_docs(&self) -> impl Iterator<Item = &Document> {
        self.panes.iter().flat_map(|p| p.docs.iter())
    }

    pub fn set_split_ratio(&mut self, ratio: f32) {
        self.split_ratio = ratio.clamp(MIN_SPLIT, MAX_SPLIT);
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.set_message(text, MsgKind::Info);
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.set_message(text, MsgKind::Warn);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.set_message(text, MsgKind::Error);
    }

    fn set_message(&mut self, text: impl Into<String>, kind: MsgKind) {
        self.message = Some(Message {
            text: text.into(),
            kind,
            at: Instant::now(),
        });
    }

    /// Drop the status message once it has been shown long enough.
    pub fn expire_message(&mut self) -> bool {
        if self
            .message
            .as_ref()
            .is_some_and(|m| m.at.elapsed() >= MESSAGE_TIMEOUT)
        {
            self.message = None;
            return true;
        }
        false
    }

    pub fn ensure_cursor_visible(&mut self) {
        if !self.has_doc() {
            return;
        }
        let (rows, cols, tab) = (self.pane().rows, self.pane().cols, self.config.tab_width);
        let doc = self.doc_mut();
        let head = doc.selection.head;
        let line = doc.rope.char_to_line(head);
        let col = mv::col_of(&doc.rope, head, tab);
        doc.view.ensure_visible(line, col, rows, cols);
    }

    // ---------------------------------------------------------------- input

    /// Typed text (already filtered of shortcuts by the app).
    pub fn insert_text(&mut self, text: &str) {
        self.hover = None;
        if self.find.open {
            self.find_panel_type(text);
            return;
        }
        if let Some(p) = &mut self.picker {
            p.type_text(text);
            return;
        }
        if let Some(mb) = &mut self.minibuffer {
            if mb.is_confirm() {
                if let Some(c) = text.chars().next() {
                    self.confirm_key(c.to_ascii_lowercase());
                }
                return;
            }
            mb.insert(text);
            self.prompt_changed();
            return;
        }
        if self.on_tests_page() {
            if let Some(c) = text.chars().next() {
                self.tests_page_key(c);
            }
            return;
        }
        if self.on_problems_page() {
            if let Some(c) = text.chars().next() {
                self.problems_page_key(c);
            }
            return;
        }
        if self.on_docs_page() {
            if let Some(c) = text.chars().next() {
                self.docs_page_key(c);
            }
            return;
        }
        if self.explorer.focused {
            // Space opens/toggles, letters jump to the next matching entry.
            match text.chars().next() {
                Some(' ') => self.explorer_activate(),
                Some(c) if !c.is_control() => self.explorer.jump_to_letter(c),
                _ => {}
            }
            return;
        }
        if text.chars().any(|c| c.is_control() && c != '\t') || !self.has_doc() {
            return;
        }
        if self.doc().read_only {
            self.warn("Read-only");
            return;
        }
        if self.multi_cursor() {
            self.multi_insert(text);
            self.track_test_edits();
            self.symbols.signature = None;
            return;
        }
        self.doc_mut().replace_selection(text, EditKind::Insert);
        self.track_test_edits();
        self.ensure_cursor_visible();
        self.update_signature(text.contains(['(', ',']));
    }

    pub fn execute(&mut self, cmd: Command) {
        // Esc drops the highlight of a shown place.
        if cmd == Command::Cancel && self.focus == EDITOR {
            self.located = None;
        }
        // Esc closes the parameter hints first.
        if cmd == Command::Cancel && self.symbols.signature.take().is_some() {
            self.hover = None;
            return;
        }
        self.execute_command(cmd);
        // Parameter hints follow the cursor (and open on request).
        if cmd == Command::SignatureHelp {
            self.update_signature(true);
            if self.symbols.signature.is_none() {
                self.info("No known function call at the cursor");
            }
        } else {
            self.update_signature(false);
        }
    }

    fn execute_command(&mut self, cmd: Command) {
        log::trace!("command {cmd:?}");
        // Any key closes a popup (Ctrl+I opens one).
        self.hover = None;
        if cmd == Command::ShowHover {
            self.show_hover_at_cursor();
            return;
        }
        let overlay = self.find.open || self.picker.is_some() || self.minibuffer.is_some();
        match cmd {
            Command::SignatureHelp => return,
            Command::GoToDefinition if !overlay => return self.go_to_definition(),
            Command::GoBack if !overlay => return self.go_back(),
            Command::OpenDocsInBrowser if !overlay => return self.open_docs_in_browser(),
            Command::FindAllReferences if !overlay => return self.find_all_references(),
            // F4 walks the latest list: references after Shift+F12, else problems.
            Command::NextProblem | Command::PrevProblem
                if !overlay && self.refs.f4 && self.refs.count() > 0 =>
            {
                return self.step_reference(cmd == Command::NextProblem);
            }
            _ => {}
        }
        if self.find.open && self.find_command(cmd) {
            return;
        }
        if self.picker.is_some() && self.picker_command(cmd) {
            return;
        }
        if self.minibuffer.is_some() && self.prompt_command(cmd) {
            return;
        }
        if self.explorer.focused && self.explorer_command(cmd) {
            return;
        }
        use Command::*;
        if !self.has_doc() && needs_doc(cmd) {
            return;
        }
        // The References page: Enter opens the reference, Up / Down preview them.
        if self.on_references_page() {
            match cmd {
                Newline => return self.open_reference(),
                Move(Motion::Up, false) => return self.references_step(-1),
                Move(Motion::Down, false) => return self.references_step(1),
                _ => {}
            }
        }
        // The Docs page: Enter opens the API on the line, Backspace goes back.
        if self.on_docs_page() {
            match cmd {
                Newline => return self.docs_open_row(),
                Backspace => return self.docs_go_back(),
                _ => {}
            }
        }
        // Enter on a Problems row opens that problem; on the Tests page, the test.
        if cmd == Newline && self.on_problems_page() {
            self.open_problem();
            return;
        }
        if cmd == Newline && self.on_tests_page() {
            self.open_test();
            return;
        }
        if cmd == Cancel && self.on_tests_page() && self.cancel_tests() {
            return;
        }
        // Left / Right fold and unfold groups on the Tests page.
        if self.on_tests_page() {
            match cmd {
                Move(Motion::Left, false) => return self.tests_left(),
                Move(Motion::Right, false) => return self.tests_right(),
                Move(Motion::Up, false) => return self.tests_step(-1),
                Move(Motion::Down, false) => return self.tests_step(1),
                _ => {}
            }
        }
        // Up / Down on the Problems page step between problems, past the message box.
        if self.on_problems_page() {
            match cmd {
                Move(Motion::Up, false) => return self.problems_step(-1),
                Move(Motion::Down, false) => return self.problems_step(1),
                _ => {}
            }
        }
        if modifies_text(cmd) && self.doc().read_only {
            self.warn(format!("{} is read-only", self.doc().title()));
            return;
        }
        let keep_scroll = matches!(cmd, ScrollUp | ScrollDown | ZoomIn | ZoomOut | ZoomReset);
        if self.multi_cursor() {
            if cmd == Cancel {
                self.doc_mut().collapse_extra();
                return;
            }
            if !Self::multi_aware(cmd) {
                self.doc_mut().collapse_extra();
            } else if self.multi_edit(cmd) {
                return;
            }
        }
        match cmd {
            Move(m, extend) => self.move_cursor(m, extend),
            SelectAll => {
                let len = self.doc().rope.len_chars();
                let doc = self.doc_mut();
                doc.selection = Selection::new(0, len);
                doc.history.break_coalescing();
            }
            SelectLine => self.select_line(),
            AddNextOccurrence => self.add_next_occurrence(),
            RemoveLastOccurrence => self.remove_last_occurrence(),
            SelectAllOccurrences => self.select_all_occurrences(),
            Cancel => {
                self.message = None;
                let doc = self.doc_mut();
                doc.selection = Selection::point(doc.selection.head);
            }
            Newline => self.newline(),
            Indent => self.indent(),
            Outdent => self.outdent(),
            Backspace => self.backspace(),
            Delete => self.delete_with(mv::next_grapheme),
            DeleteWordLeft => self.delete_with(mv::prev_word_start),
            DeleteWordRight => self.delete_with(mv::next_word_end),
            DeleteLine => self.delete_lines(),
            MoveLineUp => self.move_lines(true),
            MoveLineDown => self.move_lines(false),
            DuplicateLine => self.duplicate_lines(),
            Copy => self.copy(false),
            Cut => self.copy(true),
            Paste => self.paste(),
            Undo => {
                if !self.doc_mut().undo() {
                    self.info("Nothing to undo");
                }
            }
            Redo => {
                if !self.doc_mut().redo() {
                    self.info("Nothing to redo");
                }
            }
            NewFile => {
                let doc = Document::new_empty(None, self.config.tab_width);
                self.focus_view(EDITOR);
                let pane = self.pane_mut();
                pane.docs.push(doc);
                pane.active = pane.docs.len() - 1;
            }
            OpenFile => {
                self.minibuffer = None;
                self.picker = Some(FilePicker::new(PathBuf::from(self.dir_hint())));
            }
            OpenPath => {
                let initial = self.dir_hint();
                self.minibuffer = Some(Minibuffer::new(PromptKind::Open, initial));
            }
            Save => self.save(AfterSave::Nothing),
            SaveAs => self.prompt_save_as(AfterSave::Nothing),
            FormatDocument => self.format_document(false),
            FormatAndSave => self.format_document(true),
            RevertFile => {
                let tab = self.config.tab_width;
                let title = self.doc().title();
                match self.doc_mut().revert(tab) {
                    Ok(true) => {
                        log::info!("reverted {title}");
                        self.info(format!("Reverted {title} (Ctrl+Z brings the changes back)"));
                    }
                    Ok(false) => self.info("No changes to revert"),
                    Err(e) => self.error(format!("{e:#}")),
                }
            }
            CloseFile => {
                let (pane, index) = (self.focus, self.pane().active);
                if self.doc().is_dirty() {
                    self.minibuffer = Some(Minibuffer::new(
                        PromptKind::ConfirmClose { pane, index },
                        String::new(),
                    ));
                } else {
                    self.close_doc(pane, index);
                }
            }
            NextTab => {
                let p = self.pane_mut();
                p.active = (p.active + 1) % p.docs.len();
            }
            PrevTab => {
                let p = self.pane_mut();
                p.active = (p.active + p.docs.len() - 1) % p.docs.len();
            }
            GotoTab(i) => {
                let p = self.pane_mut();
                if i < p.docs.len() {
                    p.active = i;
                }
            }
            Find => self.start_find(),
            FindNext => self.find_step(true),
            FindPrev => self.find_step(false),
            GotoLine => {
                self.minibuffer = Some(Minibuffer::new(PromptKind::GotoLine, String::new()));
            }
            FindInFiles => self.toggle_find_in_files(),
            NextResult => self.step_result(true),
            PrevResult => self.step_result(false),
            ToggleCase | ToggleWholeWord | ToggleRegex => self.toggle_search_option(cmd),
            LintWorkspace => {
                self.lint_workspace(true);
                self.build_workspace();
            }
            ShowTests => self.show_tests(),
            RunTestAtCursor => self.run_test_at_cursor(),
            RunAllTests => self.run_tests(testing::RunWhat::All),
            RunFailedTests => self.run_tests(testing::RunWhat::Failed),
            RefreshTests => {
                self.start_test_explorer();
                self.show_tests();
            }
            ShowProblems => self.show_problems(),
            NextProblem => self.step_problem(true),
            PrevProblem => self.step_problem(false),
            ScrollUp => self.scroll(-1),
            ScrollDown => self.scroll(1),
            ZoomIn => self.zoom(self.config.font_size + 1.0),
            ZoomOut => self.zoom(self.config.font_size - 1.0),
            ZoomReset => self.zoom(self.config.default_font_size),
            Help => {
                self.open_virtual(help::HELP_TITLE, &help::help_text());
                self.refresh_static_pages();
            }
            ShowShortcuts => {
                self.open_virtual(help::SHORTCUTS_TITLE, &help::shortcuts_text());
                self.refresh_static_pages();
            }
            FocusOtherView => self.focus_view(1 - self.focus),
            FocusLeft => self.focus_view(0),
            FocusRight => self.focus_view(1),
            ToggleSplit => {
                self.split = !self.split;
                if !self.split {
                    self.focus_view(EDITOR);
                }
                self.info(if self.split {
                    "Panel shown"
                } else {
                    "Panel hidden"
                });
            }
            ResetSplit => self.split_ratio = 0.5,
            // Menus belong to the session (UI state); nothing to do here.
            // Handled before the match.
            ShowHover | GoToDefinition | GoBack | SignatureHelp | OpenDocsInBrowser
            | FindAllReferences => {}
            FileMenu | WorkspaceMenu | SelectionMenu | PanelMenu => {}
            ClosePanelTab => {
                let index = self.panes[PANEL].active;
                if index < self.panes[PANEL].docs.len() {
                    self.close_doc(PANEL, index);
                }
                if self.panes[PANEL].docs.is_empty() && self.focus == PANEL {
                    self.focus_view(EDITOR);
                }
            }
            QuickOpen => self.quick_open(),
            ToggleExplorer => {
                self.explorer.visible = !self.explorer.visible;
                if self.explorer.visible {
                    self.explorer.rebuild(self.workspace.as_ref());
                } else {
                    self.explorer.focused = false;
                }
            }
            FocusExplorer => {
                self.explorer.visible = true;
                self.explorer.focused = true;
                self.minibuffer = None;
                self.explorer.rebuild(self.workspace.as_ref());
            }
            NewWorkspace => {
                self.minibuffer = Some(Minibuffer::new(PromptKind::WorkspaceName, String::new()));
            }
            OpenWorkspace => {
                let start = self
                    .workspace
                    .as_ref()
                    .and_then(|w| w.file.as_ref())
                    .and_then(|f| f.parent().map(Path::to_path_buf))
                    .unwrap_or_else(|| PathBuf::from(self.dir_hint()));
                self.minibuffer = None;
                self.picker = Some(FilePicker::browse(PickerKind::OpenWorkspace, start));
            }
            SaveWorkspace => self.save_workspace(),
            SaveWorkspaceAs => self.prompt_save_workspace_as(None),
            AddFolder => {
                if self.workspace.is_none() {
                    self.info("Create or open a workspace first (Workspace menu)");
                } else {
                    self.minibuffer = None;
                    let start = PathBuf::from(self.dir_hint());
                    self.picker = Some(FilePicker::browse(PickerKind::ChooseFolder, start));
                }
            }
            RemoveFolder => match &self.workspace {
                Some(ws) if !ws.folders.is_empty() => {
                    self.minibuffer = None;
                    self.picker = Some(FilePicker::remove_folder(&ws.folders));
                }
                _ => self.info("The workspace has no folders"),
            },
            ToggleGitignore => {
                if let Some(ws) = &mut self.workspace {
                    ws.respect_gitignore = !ws.respect_gitignore;
                    let on = ws.respect_gitignore;
                    self.explorer.rebuild(self.workspace.as_ref());
                    self.info(if on {
                        "Respecting .gitignore"
                    } else {
                        "Showing ignored files"
                    });
                }
            }
            CloseWorkspace => {
                if self.workspace.is_some() {
                    self.leave_current(PendingAction::CloseWorkspace);
                }
            }
            Quit => self.quit(),
        }
        if !keep_scroll {
            self.ensure_cursor_visible();
        }
        // Stepping through the Problems / Tests page shows each row in the editor. Paging
        // (PageUp/Down, Home/End) only moves the list.
        let step = matches!(cmd, Move(Motion::Up | Motion::Down, false));
        if step && self.on_problems_page() {
            self.preview_problem();
        }
        if step && self.on_tests_page() {
            self.preview_test();
        }
        // Paging may land on a failure box: keep the box with the selected test.
        if matches!(cmd, Move(..)) && self.on_tests_page() && self.tests_box_moves() {
            self.refresh_tests_page();
        }
        if matches!(cmd, Move(..)) && self.on_problems_page() {
            self.refresh_problems_page();
        }
        self.track_test_edits();
    }

    /// Reveal the editor view's active file in the Explorer when it changed (tab switch, file
    /// opened, tab closed). Called before every layout.
    pub fn follow_active_file(&mut self) {
        let current = self.panes[EDITOR].doc().and_then(|d| d.canonical.clone());
        if current == self.revealed {
            return;
        }
        if let Some(path) = &current {
            self.explorer.reveal(path, self.workspace.as_ref());
        }
        self.revealed = current;
    }

    /// Compute highlighting state down to the last visible line of every shown document.
    /// Bring highlighting up to the visible lines, within a frame's time budget; what is
    /// left sets `syntax_pending` (the next frame continues).
    pub fn update_syntax(&mut self) {
        let deadline = Instant::now() + SYNTAX_BUDGET;
        let mut pending = false;
        for pane in &mut self.panes {
            let rows = pane.rows;
            if let Some(doc) = pane.docs.get_mut(pane.active) {
                let last = doc.view.top_line + rows + 1;
                pending |= !doc.syntax.ensure(&doc.rope, last, Some(deadline));
            }
        }
        self.syntax_pending = pending;
    }

    pub fn focus_view(&mut self, pane: usize) {
        // Going to the panel shows it.
        if pane == PANEL {
            self.split = true;
        }
        if pane != self.focus {
            log::debug!("focus view {pane}");
            self.focus = pane;
            self.minibuffer = None;
            if let Some(doc) = self.panes[pane].docs.get_mut(self.panes[pane].active) {
                doc.history.break_coalescing();
            }
        }
    }

    /// Find a document anywhere: (view, tab index).
    fn find_doc(&self, pred: impl Fn(&Document) -> bool) -> Option<(usize, usize)> {
        self.panes
            .iter()
            .enumerate()
            .find_map(|(p, pane)| pane.docs.iter().position(&pred).map(|i| (p, i)))
    }

    /// Focus the view holding tab `index` and make it active.
    fn show_doc(&mut self, pane: usize, index: usize) {
        self.focus_view(pane);
        self.panes[pane].active = index;
    }

    /// Show a built-in read-only page, reusing its tab if open; new pages open in the right view.
    fn open_virtual(&mut self, name: &str, text: &str) {
        self.minibuffer = None;
        if let Some((p, i)) = self.find_doc(|d| d.virtual_name.as_deref() == Some(name)) {
            self.show_doc(p, i);
            return;
        }
        let right = &mut self.panes[1];
        right.docs.push(Document::new_virtual(name, text));
        let index = right.docs.len() - 1;
        self.show_doc(1, index);
    }

    // ---------------------------------------------------------------- picker

    /// Handle a command while the file picker is open. Returns false to let it run normally.
    fn picker_command(&mut self, cmd: Command) -> bool {
        use Command::*;
        let rows = self.picker_rows.max(1);
        let page = rows.saturating_sub(1).max(1) as isize;
        let Some(p) = &mut self.picker else {
            return false;
        };
        match cmd {
            Cancel => {
                self.picker = None;
                return true;
            }
            // Pressing the picker's own key again closes it.
            OpenFile | QuickOpen | OpenWorkspace | AddFolder | RemoveFolder
                if Some(p.kind) == picker_kind_for(cmd) =>
            {
                self.picker = None;
                return true;
            }
            Move(Motion::Up, _) => p.move_selection(-1),
            Move(Motion::Down, _) => p.move_selection(1),
            Move(Motion::PageUp, _) => p.move_selection(-page),
            Move(Motion::PageDown, _) => p.move_selection(page),
            Move(Motion::Home | Motion::DocStart, _) => p.select_first(),
            Move(Motion::End | Motion::DocEnd, _) => p.select_last(),
            Newline => {
                self.picker_activate();
                return true;
            }
            Backspace | DeleteWordLeft => p.backspace(),
            Paste => {
                if let Some(text) = self.get_clipboard()
                    && let Some(p) = &mut self.picker
                {
                    p.type_text(text.lines().next().unwrap_or(""));
                }
            }
            ZoomIn | ZoomOut | ZoomReset => return false,
            // Anything that leaves the picker (new file, help, quit, ...) closes it first.
            NewFile | OpenFile | OpenPath | Help | ShowShortcuts | Quit | CloseFile | NextTab
            | PrevTab | GotoTab(_) | FocusOtherView | FocusLeft | FocusRight | ToggleSplit
            | QuickOpen | ToggleExplorer | FocusExplorer | NewWorkspace | OpenWorkspace
            | SaveWorkspace | SaveWorkspaceAs | AddFolder | RemoveFolder | ToggleGitignore
            | CloseWorkspace | FindInFiles | NextResult | PrevResult | LintWorkspace
            | ShowProblems | NextProblem | PrevProblem | ShowTests | RunTestAtCursor
            | RunAllTests | RunFailedTests | RefreshTests => {
                self.picker = None;
                return false;
            }
            _ => {}
        }
        if let Some(p) = &mut self.picker {
            p.ensure_visible(rows);
        }
        true
    }

    fn picker_activate(&mut self) {
        let Some(p) = &mut self.picker else {
            return;
        };
        match p.activate() {
            PickResult::Open(path) => {
                log::debug!("picker open {}", path.display());
                self.picker = None;
                self.open_path(&path);
                self.ensure_cursor_visible();
            }
            PickResult::OpenAt(path, line, col) => {
                log::debug!("picker open {}:{line}", path.display());
                self.picker = None;
                self.open_path(&path);
                if self.has_doc() && self.doc().path.is_some() {
                    self.goto_line_col(line, col.unwrap_or(1));
                }
            }
            PickResult::OpenWorkspace(path) => {
                self.picker = None;
                self.leave_current(PendingAction::OpenWorkspace(path));
            }
            PickResult::AddFolder(path) => {
                self.picker = None;
                self.add_folder(&path);
            }
            PickResult::RemoveFolder(path) => {
                self.picker = None;
                self.remove_folder(&path);
            }
            PickResult::None => {
                log::debug!("picker in {}", p.dir.display());
                p.ensure_visible(self.picker_rows);
            }
        }
    }

    /// Mouse click on picker list row `index`; `open` on double-click.
    pub fn picker_click(&mut self, index: usize, open: bool) {
        let Some(p) = &mut self.picker else {
            return;
        };
        if index >= p.len() {
            return;
        }
        p.selected = index;
        if open {
            self.picker_activate();
        }
    }

    pub fn picker_scroll(&mut self, rows: isize) {
        let visible = self.picker_rows;
        if let Some(p) = &mut self.picker {
            p.scroll_by(rows, visible);
        }
    }

    pub fn scroll(&mut self, lines: isize) {
        self.scroll_pane(self.focus, lines);
    }

    /// Scroll any view (the one under the mouse need not have focus).
    pub fn scroll_pane(&mut self, pane: usize, lines: isize) {
        let p = &mut self.panes[pane];
        let i = p.active;
        if let Some(doc) = p.docs.get_mut(i) {
            let total = doc.rope.len_lines();
            doc.view.scroll_lines(lines, total);
        }
    }

    pub fn scroll_pane_horizontal(&mut self, pane: usize, cols: isize) {
        let p = &mut self.panes[pane];
        let i = p.active;
        if let Some(doc) = p.docs.get_mut(i) {
            doc.view.scroll_cols(cols);
        }
    }

    fn zoom(&mut self, size: f32) {
        self.config.font_size = size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.info(format!("Font size {}", self.config.font_size));
    }

    // ---------------------------------------------------------------- mouse

    /// Char position for a cell relative to the text area (row may be negative / past the end
    /// while drag-selecting; it is clamped).
    /// Text position under a cell of any view (the one under the mouse need not have focus).
    pub fn pos_in_pane(&self, pane: usize, row: isize, col: isize) -> Option<(usize, usize)> {
        let doc = self.panes[pane].doc()?;
        let last = doc.rope.len_lines() as isize - 1;
        let line = doc.view.top_line as isize + row;
        if line < 0 || line > last {
            return None;
        }
        let line = line as usize;
        let col = (doc.view.left_col as isize + col).max(0) as usize;
        let pos = mv::pos_at_col(&doc.rope, line, col, self.config.tab_width);
        Some((line, pos - doc.rope.line_to_char(line)))
    }

    pub fn pos_at_cell(&self, row: isize, col: isize) -> usize {
        let doc = self.doc();
        let last = doc.rope.len_lines() as isize - 1;
        let line = (doc.view.top_line as isize + row).clamp(0, last) as usize;
        let col = (doc.view.left_col as isize + col).max(0) as usize;
        mv::pos_at_col(&doc.rope, line, col, self.config.tab_width)
    }

    pub fn click(&mut self, pos: usize, extend: bool, count: u32) {
        let doc = self.doc_mut();
        doc.collapse_extra();
        match count {
            2 => {
                let (s, e) = mv::word_at(&doc.rope, pos);
                doc.selection = Selection::new(s, e);
            }
            3.. => {
                let line = doc.rope.char_to_line(pos);
                let s = doc.rope.line_to_char(line);
                let e = if line + 1 < doc.rope.len_lines() {
                    doc.rope.line_to_char(line + 1)
                } else {
                    doc.rope.len_chars()
                };
                doc.selection = Selection::new(s, e);
            }
            _ => doc.move_to(pos, extend),
        }
        doc.goal_col = None;
        doc.history.break_coalescing();
        self.ensure_cursor_visible();
        if self.on_problems_page() {
            if self.problems_chip_click() {
                // Toggled a severity filter.
            } else if count >= 2 {
                self.open_problem();
            } else {
                // Move the message box to the clicked problem.
                self.refresh_problems_page();
                self.preview_problem();
            }
        }
        if self.on_tests_page() {
            self.tests_click(count >= 2);
        }
        if self.on_references_page() {
            if count >= 2 {
                self.open_reference();
            } else {
                self.preview_reference();
            }
        }
    }

    pub fn drag_to(&mut self, pos: usize) {
        let doc = self.doc_mut();
        doc.selection.head = pos;
        self.ensure_cursor_visible();
    }

    // ---------------------------------------------------------------- motion

    fn move_cursor(&mut self, m: Motion, extend: bool) {
        let tab = self.config.tab_width;
        let page = self.pane().rows.saturating_sub(1).max(1);
        let doc = self.doc_mut();
        if matches!(m, Motion::PageUp | Motion::PageDown) {
            let lines = if m == Motion::PageUp {
                -(page as isize)
            } else {
                page as isize
            };
            let total = doc.rope.len_lines();
            doc.view.scroll_lines(lines, total);
        }
        let (pos, goal) =
            motion_target(&doc.rope, doc.selection, doc.goal_col, m, extend, tab, page);
        // Other cursors move the same way, each keeping its own column.
        let extra: Vec<Selection> = doc
            .extra
            .iter()
            .map(|&s| {
                let (p, _) = motion_target(&doc.rope, s, None, m, extend, tab, page);
                s.moved_to(p, extend)
            })
            .collect();
        doc.move_to(pos, extend);
        doc.goal_col = goal;
        if !extra.is_empty() {
            doc.extra = extra;
            doc.merge_selections();
        }
    }

    fn select_line(&mut self) {
        let doc = self.doc_mut();
        let (first, last) = doc.selected_lines();
        let line_end = |l: usize| {
            if l + 1 < doc.rope.len_lines() {
                doc.rope.line_to_char(l + 1)
            } else {
                doc.rope.len_chars()
            }
        };
        let start = doc.rope.line_to_char(first);
        let mut end = line_end(last);
        // Pressing again extends to the next line.
        if doc.selection.range() == (start, end) && last + 1 < doc.rope.len_lines() {
            end = line_end(last + 1);
        }
        doc.selection = Selection::new(start, end);
        doc.history.break_coalescing();
    }

    // ---------------------------------------------------------------- editing

    fn newline(&mut self) {
        let doc = self.doc_mut();
        let (s, _) = doc.selection.range();
        let line = doc.rope.char_to_line(s);
        let ls = doc.rope.line_to_char(line);
        let indent_end = doc.line_indent_end(line).min(s);
        let indent = doc.rope.slice(ls..indent_end).to_string();
        doc.replace_selection(&format!("\n{indent}"), EditKind::Insert);
    }

    fn backspace(&mut self) {
        let tab = self.config.tab_width;
        let doc = self.doc_mut();
        if !doc.selection.is_empty() {
            doc.replace_selection("", EditKind::Delete);
            return;
        }
        let head = doc.selection.head;
        if head == 0 {
            return;
        }
        // In leading spaces, delete back to the previous tab stop.
        let mut start = mv::prev_grapheme(&doc.rope, head);
        if let Indent::Spaces(n) = doc.indent {
            let line = doc.rope.char_to_line(head);
            let ls = doc.rope.line_to_char(line);
            if head > ls && doc.rope.slice(ls..head).chars().all(|c| c == ' ') {
                let col = head - ls;
                let n = if n == 0 { tab } else { n };
                start = head - ((col - 1) % n + 1);
            }
        }
        doc.transact(
            EditKind::Delete,
            vec![(start, head, String::new())],
            Selection::point(start),
        );
    }

    /// Delete the selection, or from the cursor to `target(rope, head)`.
    fn delete_with(&mut self, target: impl Fn(&ropey::Rope, usize) -> usize) {
        let doc = self.doc_mut();
        if !doc.selection.is_empty() {
            doc.replace_selection("", EditKind::Delete);
            return;
        }
        let head = doc.selection.head;
        let t = target(&doc.rope, head);
        let (s, e) = if t < head { (t, head) } else { (head, t) };
        if s != e {
            doc.transact(
                EditKind::Delete,
                vec![(s, e, String::new())],
                Selection::point(s),
            );
        }
    }

    fn indent(&mut self) {
        let tab = self.config.tab_width;
        let doc = self.doc_mut();
        let (first, last) = doc.selected_lines();
        let unit = doc.indent.unit();
        if doc.selection.is_empty() || first == last && !spans_whole_line(doc) {
            let text = match doc.indent {
                Indent::Tabs => "\t".to_string(),
                Indent::Spaces(n) => {
                    let n = if n == 0 { tab } else { n };
                    let col = mv::col_of(&doc.rope, doc.selection.range().0, tab);
                    " ".repeat(n - col % n)
                }
            };
            doc.replace_selection(&text, EditKind::Other);
            return;
        }
        let ulen = unit.chars().count();
        let rope = &doc.rope;
        let mut added = Vec::new(); // (line_start, chars added) in original coordinates
        let mut edits = Vec::new();
        for line in first..=last {
            let ls = rope.line_to_char(line);
            if mv::line_content_end(rope, line) > ls {
                added.push((ls, ulen));
            }
        }
        for &(ls, _) in added.iter().rev() {
            edits.push((ls, ls, unit.clone()));
        }
        let map = |p: usize| -> usize {
            p + added
                .iter()
                .filter(|(ls, _)| *ls < p || (*ls == p && p != doc.selection.range().0))
                .map(|(_, n)| n)
                .sum::<usize>()
        };
        let after = Selection::new(map(doc.selection.anchor), map(doc.selection.head));
        doc.transact(EditKind::Other, edits, after);
    }

    fn outdent(&mut self) {
        let doc = self.doc_mut();
        let (first, last) = doc.selected_lines();
        let width = match doc.indent {
            Indent::Tabs => 1,
            Indent::Spaces(n) => n.max(1),
        };
        let rope = &doc.rope;
        let mut removed = Vec::new(); // (line_start, chars removed)
        for line in first..=last {
            let ls = rope.line_to_char(line);
            let le = mv::line_content_end(rope, line);
            let n = if ls < le && rope.char(ls) == '\t' {
                1
            } else {
                rope.slice(ls..le)
                    .chars()
                    .take(width)
                    .take_while(|&c| c == ' ')
                    .count()
            };
            if n > 0 {
                removed.push((ls, n));
            }
        }
        if removed.is_empty() {
            return;
        }
        let edits = removed
            .iter()
            .rev()
            .map(|&(ls, n)| (ls, ls + n, String::new()))
            .collect();
        let map = |p: usize| -> usize {
            let mut shift = 0;
            for &(ls, n) in &removed {
                if ls < p {
                    shift += n.min(p - ls);
                }
            }
            p - shift
        };
        let after = Selection::new(map(doc.selection.anchor), map(doc.selection.head));
        doc.transact(EditKind::Other, edits, after);
    }

    fn delete_lines(&mut self) {
        let tab = self.config.tab_width;
        let doc = self.doc_mut();
        let (first, last) = doc.selected_lines();
        let rope = &doc.rope;
        let col = mv::col_of(rope, doc.selection.head, tab);
        let (start, end, new_pos) = if last + 1 < rope.len_lines() {
            let start = rope.line_to_char(first);
            let end = rope.line_to_char(last + 1);
            (
                start,
                end,
                mv::pos_at_col(rope, last + 1, col, tab) - (end - start),
            )
        } else if first > 0 {
            let start = rope.line_to_char(first) - 1;
            (
                start,
                rope.len_chars(),
                mv::pos_at_col(rope, first - 1, col, tab),
            )
        } else {
            (0, rope.len_chars(), 0)
        };
        doc.transact(
            EditKind::Other,
            vec![(start, end, String::new())],
            Selection::point(new_pos),
        );
    }

    fn move_lines(&mut self, up: bool) {
        let doc = self.doc_mut();
        let (first, last) = doc.selected_lines();
        let rope = &doc.rope;
        let line_text = |a: usize, b: usize| {
            rope.slice(rope.line_to_char(a)..mv::line_content_end(rope, b))
                .to_string()
        };
        let block = line_text(first, last);
        let sel = doc.selection;
        let (edit, shift): ((usize, usize, String), isize) = if up {
            if first == 0 {
                return;
            }
            let prev = line_text(first - 1, first - 1);
            let start = rope.line_to_char(first - 1);
            let end = mv::line_content_end(rope, last);
            let shift = -(prev.chars().count() as isize + 1);
            ((start, end, format!("{block}\n{prev}")), shift)
        } else {
            if last + 1 >= rope.len_lines() {
                return;
            }
            let next = line_text(last + 1, last + 1);
            let start = rope.line_to_char(first);
            let end = mv::line_content_end(rope, last + 1);
            let shift = next.chars().count() as isize + 1;
            ((start, end, format!("{next}\n{block}")), shift)
        };
        let after = Selection::new(
            sel.anchor.saturating_add_signed(shift),
            sel.head.saturating_add_signed(shift),
        );
        doc.transact(EditKind::Other, vec![edit], after);
    }

    fn duplicate_lines(&mut self) {
        let doc = self.doc_mut();
        let (first, last) = doc.selected_lines();
        let rope = &doc.rope;
        let end = mv::line_content_end(rope, last);
        let block = rope.slice(rope.line_to_char(first)..end).to_string();
        let shift = block.chars().count() + 1;
        let sel = doc.selection;
        let after = Selection::new(sel.anchor + shift, sel.head + shift);
        doc.transact(
            EditKind::Other,
            vec![(end, end, format!("\n{block}"))],
            after,
        );
    }

    // ---------------------------------------------------------------- clipboard

    fn copy(&mut self, cut: bool) {
        let doc = self.doc();
        let (text, line_mode) = if doc.selection.is_empty() {
            let line = doc.cursor_line();
            let s = doc.rope.line_to_char(line);
            let e = mv::line_content_end(&doc.rope, line);
            (format!("{}\n", doc.rope.slice(s..e)), true)
        } else {
            (doc.selected_text(), false)
        };
        self.set_clipboard(text.clone());
        self.line_clip = line_mode.then_some(text);
        if cut {
            if line_mode {
                self.delete_lines();
            } else {
                self.doc_mut().replace_selection("", EditKind::Other);
            }
        }
    }

    fn paste(&mut self) {
        let Some(text) = self.get_clipboard() else {
            return;
        };
        let text = text.replace("\r\n", "\n");
        if self.minibuffer.is_some() {
            if let Some(mb) = &mut self.minibuffer {
                mb.insert(text.lines().next().unwrap_or(""));
            }
            self.prompt_changed();
            return;
        }
        let line_mode = self.line_clip.as_deref() == Some(text.as_str());
        let doc = self.doc_mut();
        if line_mode && doc.selection.is_empty() {
            // Whole-line paste goes above the current line, cursor keeps its column.
            let head = doc.selection.head;
            let ls = doc.rope.line_to_char(doc.cursor_line());
            let n = text.chars().count();
            doc.transact(
                EditKind::Other,
                vec![(ls, ls, text)],
                Selection::point(head + n),
            );
        } else {
            doc.replace_selection(&text, EditKind::Other);
        }
    }

    fn set_clipboard(&mut self, text: String) {
        if let Some(cb) = &mut self.clipboard
            && let Err(e) = cb.set_text(text.clone())
        {
            log::warn!("clipboard set failed: {e}");
        }
        self.internal_clip = text;
    }

    fn get_clipboard(&mut self) -> Option<String> {
        if let Some(cb) = &mut self.clipboard
            && let Ok(t) = cb.get_text()
        {
            return Some(t);
        }
        (!self.internal_clip.is_empty()).then(|| self.internal_clip.clone())
    }

    // ---------------------------------------------------------------- files

    /// Folder to start Open/Save As in: the focused file's folder, else the other view's file's
    /// folder (e.g. when the cursor is in Help), else the first workspace folder, else the
    /// working directory.
    fn dir_hint(&self) -> String {
        let file_of = |p: usize| self.panes[p].doc().and_then(|d| d.path.as_ref());
        let first_folder = self.workspace.as_ref().and_then(|w| w.folders.first());
        let dir = file_of(self.focus)
            .or_else(|| file_of(1 - self.focus))
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| first_folder.cloned())
            .or_else(|| std::env::current_dir().ok());
        match dir {
            Some(d) => {
                let mut s = d.display().to_string();
                if !s.ends_with(MAIN_SEPARATOR) && !s.ends_with('/') {
                    s.push(MAIN_SEPARATOR);
                }
                s
            }
            None => String::new(),
        }
    }

    /// Open `path` in a new tab, or switch to it if it is already open.
    pub fn open_path(&mut self, path: &Path) {
        if path.is_dir() {
            self.error(format!("{} is a directory", path.display()));
            return;
        }
        let canon = std::fs::canonicalize(path).ok();
        if let Some((p, i)) = self.find_doc(|d| {
            d.path.as_deref() == Some(path)
                || canon.is_some()
                    && d.path.as_ref().and_then(|p| std::fs::canonicalize(p).ok()) == canon
        }) {
            self.show_doc(p, i);
            return;
        }
        match Document::open(path, self.config.tab_width) {
            Ok(doc) => {
                log::info!(
                    "opened {} ({} lines, {}, {}{}{})",
                    path.display(),
                    doc.rope.len_lines(),
                    doc.line_ending.label(),
                    doc.indent.label(),
                    if doc.lossy { ", lossy UTF-8" } else { "" },
                    if path.exists() { "" } else { ", new file" },
                );
                let exists = path.exists();
                let lossy = doc.lossy;
                // Files always open in the editor view.
                self.focus_view(EDITOR);
                // Replace a pristine untitled tab instead of keeping it around.
                let pane = self.pane_mut();
                let reuse = pane.docs.len() == 1
                    && pane.docs[0].path.is_none()
                    && pane.docs[0].virtual_name.is_none()
                    && !pane.docs[0].is_dirty()
                    && pane.docs[0].rope.len_chars() == 0;
                if reuse {
                    pane.docs[0] = doc;
                    pane.active = 0;
                } else {
                    pane.docs.push(doc);
                    pane.active = pane.docs.len() - 1;
                }
                let (p, i) = (self.focus, self.pane().active);
                self.lint_doc(p, i);
                if lossy {
                    self.warn("File is not valid UTF-8; invalid bytes were replaced and will be lost on save");
                } else if !exists {
                    self.info(format!("New file: {}", path.display()));
                }
            }
            Err(e) => {
                log::warn!("open {} failed: {e:#}", path.display());
                self.error(format!("{e:#}"));
            }
        }
    }

    fn save(&mut self, after: AfterSave) {
        if self.doc().path.is_none() {
            self.prompt_save_as(after);
            return;
        }
        match self.doc_mut().save() {
            Ok(()) => {
                let doc = self.doc();
                let msg = format!("Saved {} ({} lines)", doc.title(), doc.rope.len_lines());
                log::info!(
                    "saved {} ({} lines, {})",
                    doc.path
                        .as_ref()
                        .map_or(String::new(), |p| p.display().to_string()),
                    doc.rope.len_lines(),
                    doc.line_ending.label()
                );
                self.info(msg);
                let (p, i) = (self.focus, self.pane().active);
                self.lint_doc(p, i);
                if let Some(path) = self.doc().path.clone() {
                    self.after_save_checks(&path);
                }
                self.after_save(after);
            }
            Err(e) => {
                log::error!("save failed: {e:#}");
                self.error(format!("{e:#}"));
            }
        }
    }

    fn prompt_save_as(&mut self, after: AfterSave) {
        let initial = match &self.doc().path {
            Some(p) => p.display().to_string(),
            None => self.dir_hint(),
        };
        self.minibuffer = Some(Minibuffer::new(PromptKind::SaveAs(after), initial));
    }

    fn after_save(&mut self, after: AfterSave) {
        match after {
            AfterSave::Nothing => {}
            AfterSave::CloseDoc => self.close_doc(self.focus, self.pane().active),
            AfterSave::Continue(action) => self.save_all_then(action),
        }
    }

    /// Close a tab. The view may become empty; it then shows a placeholder.
    fn close_doc(&mut self, pane: usize, i: usize) {
        let p = &mut self.panes[pane];
        log::info!(
            "closed {}{}",
            p.docs[i].title(),
            if p.docs[i].is_dirty() {
                " (discarded changes)"
            } else {
                ""
            }
        );
        p.docs.remove(i);
        if p.active > i || p.active >= p.docs.len() {
            p.active = p.active.saturating_sub(1);
        }
    }

    fn quit(&mut self) {
        self.leave_current(PendingAction::Quit);
    }

    /// Deal with unsaved work before `action` replaces or discards the open tabs:
    /// - a workspace with a file is saved automatically (it keeps unsaved edits),
    /// - a workspace never saved asks whether to save it,
    /// - in file mode, unsaved files ask save all / discard / cancel.
    fn leave_current(&mut self, action: PendingAction) {
        if let Some(ws) = &self.workspace {
            if ws.file.is_some() {
                match self.write_workspace() {
                    Ok(()) => self.continue_pending(action),
                    Err(e) => self.error(format!("Workspace not saved: {e:#}")),
                }
            } else {
                self.minibuffer = Some(Minibuffer::new(
                    PromptKind::ConfirmSaveWorkspace(action),
                    String::new(),
                ));
            }
            return;
        }
        self.check_unsaved_then(action);
    }

    fn check_unsaved_then(&mut self, action: PendingAction) {
        if self.all_docs().any(Document::is_dirty) {
            self.minibuffer = Some(Minibuffer::new(
                PromptKind::ConfirmDiscard(action),
                String::new(),
            ));
        } else {
            self.continue_pending(action);
        }
    }

    fn continue_pending(&mut self, action: PendingAction) {
        match action {
            PendingAction::Quit => self.should_quit = true,
            PendingAction::OpenWorkspace(path) => self.open_workspace_now(&path),
            PendingAction::CloseWorkspace => self.finish_close_workspace(),
        }
    }

    /// Save every dirty document, then run `action`. Untitled ones get a Save As prompt that
    /// resumes afterwards.
    fn save_all_then(&mut self, action: PendingAction) {
        for p in 0..self.panes.len() {
            for i in 0..self.panes[p].docs.len() {
                if !self.panes[p].docs[i].is_dirty() {
                    continue;
                }
                self.show_doc(p, i);
                if self.doc().path.is_none() {
                    self.prompt_save_as(AfterSave::Continue(action));
                    return;
                }
                if let Err(e) = self.doc_mut().save() {
                    log::error!("save failed: {e:#}");
                    self.error(format!("{e:#}"));
                    return;
                }
            }
        }
        self.continue_pending(action);
    }

    // ---------------------------------------------------------------- workspace

    fn new_workspace(&mut self, name: &str) {
        if name.trim().is_empty() {
            return;
        }
        if let Err(e) = self.write_workspace() {
            self.error(format!("Previous workspace not saved: {e:#}"));
            return;
        }
        let ws = Workspace::new(name);
        log::info!("new workspace {}", ws.name);
        self.info(format!(
            "Workspace \"{}\" created. Add a folder: Workspace → Add Folder…",
            ws.name
        ));
        self.workspace = Some(ws);
        self.explorer = Explorer {
            visible: true,
            ..Explorer::default()
        };
        self.explorer.rebuild(self.workspace.as_ref());
    }

    fn add_folder(&mut self, path: &Path) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let Some(ws) = &mut self.workspace else {
            return;
        };
        if let Some(f) = ws
            .folders
            .iter()
            .find(|f| path.starts_with(f) || f.starts_with(&path))
        {
            let msg = if *f == path {
                format!("{} is already in the workspace", path.display())
            } else {
                format!("{} overlaps {}", path.display(), f.display())
            };
            self.warn(msg);
            return;
        }
        ws.folders.push(path.clone());
        log::info!("workspace {}: added folder {}", ws.name, path.display());
        self.explorer.expanded.insert(path.clone());
        self.explorer.visible = true;
        self.explorer.rebuild(self.workspace.as_ref());
        self.info(format!("Added {}", path.display()));
        self.start_test_explorer();
    }

    fn remove_folder(&mut self, path: &Path) {
        let Some(ws) = &mut self.workspace else {
            return;
        };
        let before = ws.folders.len();
        ws.folders.retain(|f| f != path);
        if ws.folders.len() == before {
            return;
        }
        log::info!("workspace {}: removed folder {}", ws.name, path.display());
        self.explorer.forget(path);
        self.explorer.rebuild(self.workspace.as_ref());
        self.info(format!("Removed {} from the workspace", path.display()));
        let tree = &mut self.tests.tree;
        tree.tests.retain(|t| !t.file.starts_with(path));
        tree.projects.retain(|p| !p.root.starts_with(path));
        self.start_test_explorer();
    }

    fn save_workspace(&mut self) {
        let Some(ws) = &self.workspace else {
            return;
        };
        if ws.file.is_none() {
            self.prompt_save_workspace_as(None);
            return;
        }
        match self.write_workspace() {
            Ok(()) => {
                let name = self
                    .workspace
                    .as_ref()
                    .map_or(String::new(), |w| w.name.clone());
                self.info(format!("Saved workspace {name}"));
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn prompt_save_workspace_as(&mut self, then: Option<PendingAction>) {
        let Some(ws) = &self.workspace else {
            return;
        };
        let dir = ws
            .file
            .as_ref()
            .and_then(|f| f.parent().map(Path::to_path_buf))
            .or_else(|| {
                ws.folders
                    .first()
                    .and_then(|f| f.parent().map(Path::to_path_buf))
            })
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let initial = dir.join(ws.default_file_name()).display().to_string();
        self.minibuffer = Some(Minibuffer::new(PromptKind::SaveWorkspaceAs(then), initial));
    }

    fn save_workspace_to(&mut self, input: &str, then: Option<PendingAction>) {
        let mut path = expand_tilde(input);
        if !ws_file::is_workspace_file(&path) {
            let mut name = path.into_os_string();
            name.push(format!(".{}", ws_file::EXTENSION));
            path = PathBuf::from(name);
        }
        let Some(ws) = &mut self.workspace else {
            return;
        };
        let old = ws.file.replace(path.clone());
        match self.write_workspace() {
            Ok(()) => {
                self.remember_workspace();
                self.info(format!("Saved workspace to {}", path.display()));
                if let Some(action) = then {
                    self.continue_pending(action);
                }
            }
            Err(e) => {
                if let Some(ws) = &mut self.workspace {
                    ws.file = old;
                }
                self.error(format!("{e:#}"));
            }
        }
    }

    fn open_workspace_now(&mut self, path: &Path) {
        match self.load_workspace(path) {
            Ok(missing) => {
                let name = self
                    .workspace
                    .as_ref()
                    .map_or(String::new(), |w| w.name.clone());
                log::info!("opened workspace {name} from {}", path.display());
                self.remember_workspace();
                self.start_test_explorer();
                if missing.is_empty() {
                    self.info(format!("Opened workspace {name}"));
                } else {
                    self.warn(format!("Could not reopen: {}", missing.join(", ")));
                }
            }
            Err(e) => {
                log::warn!("open workspace failed: {e:#}");
                self.error(format!("{e:#}"));
            }
        }
    }

    /// Back to file mode with the startup layout.
    fn finish_close_workspace(&mut self) {
        if let Some(ws) = self.workspace.take() {
            log::info!("closed workspace {}", ws.name);
            self.info(format!("Closed workspace {}", ws.name));
        }
        self.remember_workspace();
        self.explorer = Explorer::default();
        self.stop_test_explorer();
        self.find.open = false;
        self.find.clear_results();
        self.panes = [Pane::default(), Pane::default()];
        self.panes[0]
            .docs
            .push(Document::new_empty(None, self.config.tab_width));
        self.panes[1]
            .docs
            .push(Document::new_virtual(help::HELP_TITLE, &help::help_text()));
        self.focus = 0;
        self.split = true;
    }

    /// Use `file` to remember the open workspace. With `restore` (no files on the command
    /// line), reopen the workspace remembered from the last run.
    pub fn attach_state(&mut self, file: PathBuf, restore: bool) {
        self.state_file = Some(file.clone());
        if self.workspace.is_some() {
            self.remember_workspace();
            return;
        }
        if !restore {
            return;
        }
        let Some(last) = AppState::load(&file).last_workspace else {
            return;
        };
        if last.is_file() {
            log::info!("reopening last workspace {}", last.display());
            self.open_workspace_now(&last);
        } else {
            log::warn!("last workspace {} not found", last.display());
            self.warn(format!("Last workspace not found: {}", last.display()));
            self.remember_workspace();
        }
    }

    /// Record the current workspace file (or none) as the one to reopen next time.
    fn remember_workspace(&self) {
        let Some(state_file) = &self.state_file else {
            return;
        };
        let last = self
            .workspace
            .as_ref()
            .and_then(|w| w.file.as_ref())
            .map(|f| std::fs::canonicalize(f).unwrap_or_else(|_| f.clone()));
        let mut state = AppState::load(state_file);
        if state.last_workspace == last {
            return;
        }
        state.last_workspace = last;
        if let Err(e) = state.save(state_file) {
            log::warn!("cannot save {}: {e:#}", state_file.display());
        }
    }

    fn quick_open(&mut self) {
        let Some(ws) = self.workspace.as_ref().filter(|w| !w.folders.is_empty()) else {
            self.info("Quick Open searches workspace folders: add a folder first");
            return;
        };
        let started = Instant::now();
        let (files, truncated) =
            ws_file::files::index(&ws.folders, ws.respect_gitignore, QUICK_OPEN_LIMIT);
        log::debug!("indexed {} files in {:?}", files.len(), started.elapsed());
        self.minibuffer = None;
        self.picker = Some(FilePicker::quick_open(files, truncated));
    }

    // ---------------------------------------------------------------- explorer

    /// Commands while the Explorer has focus. Returns false to let the command run normally.
    fn explorer_command(&mut self, cmd: Command) -> bool {
        use Command::*;
        let page = self.explorer.visible_rows.saturating_sub(1).max(1) as isize;
        let ws = self.workspace.as_ref();
        let ex = &mut self.explorer;
        match cmd {
            Cancel | FocusExplorer => ex.focused = false,
            Move(Motion::Up, _) => ex.move_selection(-1),
            Move(Motion::Down, _) => ex.move_selection(1),
            Move(Motion::PageUp, _) => ex.move_selection(-page),
            Move(Motion::PageDown, _) => ex.move_selection(page),
            Move(Motion::Home | Motion::DocStart, _) => ex.select(0),
            Move(Motion::End | Motion::DocEnd, _) => ex.select(ex.rows.len().saturating_sub(1)),
            Move(Motion::Right, _) => ex.expand_or_enter(ws),
            Move(Motion::Left, _) => ex.collapse_or_parent(ws),
            Newline => self.explorer_activate(),
            Delete => {
                if let Some(row) = ex.selected_row().filter(|r| r.is_root) {
                    let path = row.path.clone();
                    self.minibuffer = Some(Minibuffer::new(
                        PromptKind::ConfirmRemoveFolder(path),
                        String::new(),
                    ));
                }
            }
            // These work without leaving the Explorer.
            ZoomIn | ZoomOut | ZoomReset | ToggleGitignore | AddFolder | RemoveFolder
            | SaveWorkspace | SaveWorkspaceAs | WorkspaceMenu | FileMenu | SelectionMenu
            | PanelMenu | ClosePanelTab | ToggleExplorer | ToggleCase | ToggleWholeWord
            | ToggleRegex => {
                return false;
            }
            // Editing keys have no meaning here.
            Backspace | DeleteWordLeft | DeleteWordRight | Indent | Outdent | Paste | Cut
            | Copy | Undo | Redo | SelectAll | SelectLine | DeleteLine | MoveLineUp
            | MoveLineDown | DuplicateLine | Move(..) | ScrollUp | ScrollDown | Find | FindNext
            | FindPrev | GotoLine => {}
            // Anything else (opening files, switching views, quitting...) moves focus back.
            _ => {
                ex.focused = false;
                return false;
            }
        }
        true
    }

    /// Enter/Space in the Explorer: open the file in the focused view, or toggle a folder.
    fn explorer_activate(&mut self) {
        match self.explorer.activate(self.workspace.as_ref()) {
            ExplorerAction::Open(path) => {
                self.explorer.focused = false;
                self.open_path(&path);
                self.ensure_cursor_visible();
            }
            ExplorerAction::None => {}
        }
    }

    /// Mouse click on Explorer row `index` (relative to the whole list). Opening a file moves
    /// focus to the view; toggling a folder keeps it in the Explorer.
    pub fn explorer_click(&mut self, index: usize) {
        if index >= self.explorer.rows.len() {
            return;
        }
        self.explorer.focused = true;
        self.explorer.select(index);
        self.explorer_activate();
    }

    // ---------------------------------------------------------------- prompts

    /// Handle a command while the minibuffer is open. Returns false to let it run normally.
    fn prompt_command(&mut self, cmd: Command) -> bool {
        use Command::*;
        let Some(mb) = &mut self.minibuffer else {
            return false;
        };
        let is_find = matches!(mb.kind, PromptKind::Find { .. });
        let is_path = matches!(mb.kind, PromptKind::Open | PromptKind::SaveAs(_));
        match cmd {
            Cancel => {
                self.minibuffer = None;
                return true;
            }
            _ if mb.is_confirm() => return !matches!(cmd, ZoomIn | ZoomOut | ZoomReset),
            Newline => {
                self.submit_prompt();
                return true;
            }
            Backspace => mb.backspace(),
            Delete => mb.delete(),
            DeleteWordLeft => mb.delete_word(),
            Move(Motion::Left, _) => mb.left(),
            Move(Motion::Right, _) => mb.right(),
            Move(Motion::Home, _) => mb.home(),
            Move(Motion::End, _) => mb.end(),
            Indent if is_path => {
                let before = mb.input.clone();
                let cands = mb.complete_path();
                if cands.is_empty() && mb.input == before {
                    self.warn("No matches");
                } else if !cands.is_empty() {
                    let shown: Vec<_> = cands.iter().take(12).cloned().collect();
                    let more = cands.len().saturating_sub(shown.len());
                    let mut msg = shown.join("  ");
                    if more > 0 {
                        msg.push_str(&format!("  (+{more} more)"));
                    }
                    self.info(msg);
                }
                return true;
            }
            FindNext | Move(Motion::Down, _) if is_find => {
                self.find_step(true);
                return true;
            }
            FindPrev | Move(Motion::Up, _) if is_find => {
                self.find_step(false);
                return true;
            }
            Paste => {
                self.paste();
                return true;
            }
            Find if is_find => return true,
            // Let these work while a prompt is open.
            ZoomIn | ZoomOut | ZoomReset | Save | FormatAndSave | Quit | ScrollUp | ScrollDown
            | ToggleCase | ToggleWholeWord | ToggleRegex => return false,
            _ => return true,
        }
        if matches!(cmd, Backspace | Delete | DeleteWordLeft) {
            self.prompt_changed();
        }
        true
    }

    /// Input text changed: drive incremental search.
    fn prompt_changed(&mut self) {
        let Some(mb) = &self.minibuffer else {
            return;
        };
        if let PromptKind::Find { origin } = mb.kind {
            self.find_query = mb.input.clone();
            let q = match Query::new(&self.find_query, self.search_opts) {
                Ok(Some(q)) => q,
                Ok(None) => {
                    let doc = self.doc_mut();
                    doc.selection = Selection::point(origin);
                    return;
                }
                Err(e) => {
                    self.doc_mut().selection = Selection::point(origin);
                    self.error(e);
                    return;
                }
            };
            match q.find(&self.doc().rope, origin, true) {
                Some((s, e)) => self.doc_mut().selection = Selection::new(s, e),
                None => self.warn("No matches"),
            }
            self.ensure_cursor_visible();
        }
    }

    fn submit_prompt(&mut self) {
        let Some(mb) = self.minibuffer.take() else {
            return;
        };
        let input = mb.input.trim().to_string();
        match mb.kind {
            PromptKind::Open => {
                if !input.is_empty() {
                    self.open_path(&expand_tilde(&input));
                }
            }
            PromptKind::SaveAs(after) => {
                if input.is_empty() {
                    return;
                }
                let path = expand_tilde(&input);
                if path.is_dir() {
                    self.error(format!("{} is a directory", path.display()));
                    return;
                }
                let old = self.doc_mut().path.replace(path.clone());
                self.save(after);
                if self
                    .message
                    .as_ref()
                    .is_some_and(|m| m.kind == MsgKind::Error)
                {
                    self.doc_mut().path = old;
                } else if self.has_doc() && self.doc().path.as_ref() == Some(&path) {
                    self.doc_mut().canonical = Some(document::canonical_path(&path));
                    // A new name may mean a new language.
                    let lang = crate::syntax::Lang::from_path(&path);
                    if self.doc().syntax.lang != lang {
                        self.doc_mut().syntax.set_lang(lang);
                    }
                }
            }
            PromptKind::Find { .. } => {
                // Enter jumps to the next match and keeps the prompt open.
                self.minibuffer = Some(mb);
                self.find_step(true);
            }
            PromptKind::GotoLine => self.goto_line(&input),
            PromptKind::WorkspaceName => self.new_workspace(&input),
            PromptKind::SaveWorkspaceAs(then) => {
                if !input.is_empty() {
                    self.save_workspace_to(&input, then);
                }
            }
            PromptKind::ConfirmClose { .. }
            | PromptKind::ConfirmDiscard(_)
            | PromptKind::ConfirmSaveWorkspace(_)
            | PromptKind::ConfirmRemoveFolder(_)
            | PromptKind::ConfirmOpenUrl { .. } => self.minibuffer = Some(mb),
        }
        self.ensure_cursor_visible();
    }

    fn confirm_key(&mut self, c: char) {
        let Some(mb) = self.minibuffer.take() else {
            return;
        };
        match (mb.kind.clone(), c) {
            (PromptKind::ConfirmClose { pane, index }, 'y') => {
                self.show_doc(pane, index);
                self.save(AfterSave::CloseDoc);
            }
            (PromptKind::ConfirmClose { pane, index }, 'n') => self.close_doc(pane, index),
            (PromptKind::ConfirmDiscard(action), 's') => self.save_all_then(action),
            (PromptKind::ConfirmDiscard(action), 'd') => self.continue_pending(action),
            (PromptKind::ConfirmSaveWorkspace(action), 'y') => {
                self.prompt_save_workspace_as(Some(action))
            }
            // Not saving the workspace: unsaved files still need a decision.
            (PromptKind::ConfirmSaveWorkspace(action), 'n') => self.check_unsaved_then(action),
            (PromptKind::ConfirmRemoveFolder(path), 'y') => self.remove_folder(&path),
            (PromptKind::ConfirmRemoveFolder(_), 'n') => {}
            (PromptKind::ConfirmOpenUrl { url, .. }, 'y') => self.open_url(&url),
            (PromptKind::ConfirmOpenUrl { .. }, 'n') => {}
            (_, 'c') => {}
            _ => self.minibuffer = Some(mb), // ignore other keys
        }
    }

    fn goto_line(&mut self, input: &str) {
        let mut parts = input.splitn(2, ':');
        let line: Option<usize> = parts.next().and_then(|s| s.trim().parse().ok());
        let col: usize = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(1);
        let Some(line) = line else {
            if !input.is_empty() {
                self.error(format!("Not a line number: {input}"));
            }
            return;
        };
        self.goto_line_col(line, col);
    }

    /// Move the cursor to 1-based `line` and display column `col` (clamped), with the line
    /// placed near the top third of the view.
    fn goto_line_col(&mut self, line: usize, col: usize) {
        let tab = self.config.tab_width;
        let doc = self.doc_mut();
        let line = line.clamp(1, doc.rope.len_lines()) - 1;
        let pos = mv::pos_at_col(&doc.rope, line, col.saturating_sub(1), tab);
        doc.move_to(pos, false);
        // Put the target line near the middle of the screen.
        let rows = self.pane().rows;
        self.doc_mut().view.top_line = line.saturating_sub(rows / 3);
    }

    fn start_find(&mut self) {
        let doc = self.doc();
        let sel = doc.selected_text();
        let initial = if !sel.is_empty() && !sel.contains('\n') {
            sel
        } else {
            self.find_query.clone()
        };
        let origin = doc.selection.range().0;
        self.minibuffer = Some(Minibuffer::new(PromptKind::Find { origin }, initial));
        self.prompt_changed();
    }

    fn find_step(&mut self, forward: bool) {
        let q = match Query::new(&self.find_query, self.search_opts) {
            Ok(Some(q)) => q,
            Ok(None) => {
                self.start_find();
                return;
            }
            Err(e) => {
                self.error(e);
                return;
            }
        };
        let doc = self.doc();
        let (s, e) = doc.selection.range();
        // Forward from the end of the current match (or the cursor), backward from its start.
        let from = if forward && !doc.selection.is_empty() {
            e
        } else {
            s
        };
        match q.find(&doc.rope, from, forward) {
            Some((s, e)) => {
                let doc = self.doc_mut();
                doc.selection = Selection::new(s, e);
                doc.history.break_coalescing();
            }
            None => self.warn(format!("No matches for \"{}\"", self.find_query)),
        }
        self.ensure_cursor_visible();
    }

    /// Current find query for highlighting, when the find prompt is open.
    pub fn active_query(&self) -> Option<Query> {
        match &self.minibuffer {
            Some(Minibuffer {
                kind: PromptKind::Find { .. },
                ..
            }) => {
                // Compiled once per query text and options, not on every frame.
                let mut cache = self.query_cache.borrow_mut();
                match &*cache {
                    Some((text, opts, q))
                        if *text == self.find_query && *opts == self.search_opts =>
                    {
                        q.clone()
                    }
                    _ => {
                        let q = Query::new(&self.find_query, self.search_opts)
                            .ok()
                            .flatten();
                        *cache = Some((self.find_query.clone(), self.search_opts, q.clone()));
                        q
                    }
                }
            }
            _ => None,
        }
    }
}

/// Where `m` moves the head of `sel`, and the goal column for vertical motion.
fn motion_target(
    rope: &ropey::Rope,
    sel: Selection,
    goal_col: Option<usize>,
    m: Motion,
    extend: bool,
    tab: usize,
    page: usize,
) -> (usize, Option<usize>) {
    let head = sel.head;
    let (s, e) = sel.range();
    let collapse = !extend && !sel.is_empty();
    let vertical = |lines: isize| -> (usize, Option<usize>) {
        let line = rope.char_to_line(head) as isize;
        let goal = goal_col.unwrap_or_else(|| mv::col_of(rope, head, tab));
        let target = line + lines;
        let pos = if target < 0 {
            0
        } else if target >= rope.len_lines() as isize {
            rope.len_chars()
        } else {
            mv::pos_at_col(rope, target as usize, goal, tab)
        };
        (pos, Some(goal))
    };
    let pos = match m {
        Motion::Left if collapse => s,
        Motion::Right if collapse => e,
        Motion::Left => mv::prev_grapheme(rope, head),
        Motion::Right => mv::next_grapheme(rope, head),
        Motion::WordLeft => mv::prev_word_start(rope, head),
        Motion::WordRight => mv::next_word_end(rope, head),
        Motion::Home => mv::smart_home(rope, head),
        Motion::End => mv::line_content_end(rope, rope.char_to_line(head)),
        Motion::DocStart => 0,
        Motion::DocEnd => rope.len_chars(),
        Motion::Up => return vertical(-1),
        Motion::Down => return vertical(1),
        Motion::PageUp => return vertical(-(page as isize)),
        Motion::PageDown => return vertical(page as isize),
    };
    (pos, None)
}

/// The picker kind a command opens, so pressing it again closes the picker.
fn picker_kind_for(cmd: Command) -> Option<PickerKind> {
    match cmd {
        Command::OpenFile => Some(PickerKind::OpenFile),
        Command::QuickOpen => Some(PickerKind::QuickOpen),
        Command::OpenWorkspace => Some(PickerKind::OpenWorkspace),
        Command::AddFolder => Some(PickerKind::ChooseFolder),
        Command::RemoveFolder => Some(PickerKind::RemoveFolder),
        _ => None,
    }
}

/// Time per frame for highlighting (the rest continues in the next frames).
const SYNTAX_BUDGET: Duration = Duration::from_millis(6);

/// Files indexed for Quick Open at most.
const QUICK_OPEN_LIMIT: usize = 100_000;

/// Commands that need a document; ignored while the focused view is empty.
fn needs_doc(cmd: Command) -> bool {
    use Command::*;
    !matches!(
        cmd,
        NewFile
            | WorkspaceMenu
            | QuickOpen
            | FindInFiles
            | NextResult
            | PrevResult
            | ToggleCase
            | ToggleWholeWord
            | ToggleRegex
            | ToggleExplorer
            | FocusExplorer
            | NewWorkspace
            | OpenWorkspace
            | SaveWorkspace
            | SaveWorkspaceAs
            | AddFolder
            | RemoveFolder
            | ToggleGitignore
            | CloseWorkspace
            | OpenFile
            | OpenPath
            | Help
            | ShowShortcuts
            | FocusOtherView
            | FocusLeft
            | FocusRight
            | ToggleSplit
            | ResetSplit
            | FileMenu
            | SelectionMenu
            | PanelMenu
            | ClosePanelTab
            | LintWorkspace
            | ShowProblems
            | NextProblem
            | PrevProblem
            | ShowTests
            | RunTestAtCursor
            | RunAllTests
            | RunFailedTests
            | RefreshTests
            | ZoomIn
            | ZoomOut
            | ZoomReset
            | Quit
    )
}

/// Commands that change the text, refused on read-only pages.
fn modifies_text(cmd: Command) -> bool {
    use Command::*;
    matches!(
        cmd,
        Newline
            | Indent
            | Outdent
            | Backspace
            | Delete
            | DeleteWordLeft
            | DeleteWordRight
            | DeleteLine
            | MoveLineUp
            | MoveLineDown
            | DuplicateLine
            | Cut
            | Paste
            | Undo
            | Redo
            | SaveAs
    )
}

/// True if the selection covers whole line(s) (e.g. after Ctrl+L), so Tab indents instead of replacing.
fn spans_whole_line(doc: &Document) -> bool {
    let (s, e) = doc.selection.range();
    let ls = doc.rope.line_to_char(doc.rope.char_to_line(s));
    s == ls && e > s && (e == doc.rope.len_chars() || doc.rope.char(e - 1) == '\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        let mut ed = Editor::new(Config::default(), &[]);
        ed.panes[0].docs[0] = Document::from_bytes(text.as_bytes(), 4);
        ed
    }

    fn titles(ed: &Editor, pane: usize) -> Vec<String> {
        ed.panes[pane].docs.iter().map(Document::title).collect()
    }

    #[test]
    fn startup_has_file_left_and_help_right() {
        let ed = editor("x");
        assert!(ed.split);
        assert_eq!(ed.focus, 0);
        assert_eq!(titles(&ed, 1), [help::HELP_TITLE]);
        assert!(!ed.doc().read_only);
    }

    #[test]
    fn focus_split_and_reset_commands() {
        let mut ed = editor("x");
        ed.execute(Command::FocusOtherView);
        assert_eq!(ed.focus, 1);
        assert!(ed.doc().read_only);
        ed.insert_text("nope");
        assert_eq!(ed.doc().rope.len_chars(), help::help_text().chars().count());
        ed.execute(Command::FocusLeft);
        assert_eq!(ed.focus, 0);
        ed.execute(Command::FocusRight);
        assert_eq!(ed.focus, 1);
        ed.execute(Command::ToggleSplit);
        assert!(!ed.split);
        ed.set_split_ratio(0.99);
        assert_eq!(ed.split_ratio, MAX_SPLIT);
        ed.execute(Command::ResetSplit);
        assert_eq!(ed.split_ratio, 0.5);
    }

    #[test]
    fn help_and_shortcuts_open_on_the_right_and_reuse_tabs() {
        let mut ed = editor("x");
        ed.execute(Command::ShowShortcuts);
        assert_eq!(ed.focus, 1);
        assert_eq!(titles(&ed, 1), [help::HELP_TITLE, help::SHORTCUTS_TITLE]);
        ed.execute(Command::FocusLeft);
        ed.execute(Command::Help);
        assert_eq!((ed.focus, ed.pane().active), (1, 0));
        assert_eq!(ed.panes[1].docs.len(), 2);
        // With the panel hidden, Help shows it again.
        ed.execute(Command::FocusLeft);
        ed.execute(Command::ToggleSplit);
        assert!(!ed.split);
        ed.execute(Command::Help);
        assert_eq!(ed.focus, 1);
        assert!(ed.split);
        // Hiding the panel while it has focus moves the cursor to the editor.
        ed.execute(Command::ToggleSplit);
        assert_eq!(ed.focus, 0);
    }

    #[test]
    fn opening_a_file_from_the_other_view_focuses_it() {
        let dir = std::env::temp_dir().join(format!("jdc-ed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, "hi").unwrap();
        let mut ed = Editor::new(Config::default(), std::slice::from_ref(&file));
        ed.execute(Command::FocusRight);
        ed.open_path(&file);
        assert_eq!(ed.focus, 0);
        assert_eq!(ed.panes[0].docs.len(), 1);
        // Files never open in the panel: new files and untitled tabs go left too.
        ed.execute(Command::FocusRight);
        let other = dir.join("b.txt");
        ed.open_path(&other);
        assert_eq!(ed.focus, 0);
        assert_eq!(titles(&ed, 0), ["a.txt", "b.txt"]);
        assert_eq!(titles(&ed, 1), [help::HELP_TITLE]);
        ed.execute(Command::FocusRight);
        ed.execute(Command::NewFile);
        assert_eq!(ed.focus, 0);
        assert_eq!(ed.panes[0].docs.len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn empty_view_is_safe() {
        let mut ed = editor("x");
        ed.execute(Command::FocusRight);
        ed.execute(Command::CloseFile);
        assert!(!ed.has_doc());
        for cmd in [
            Command::Move(Motion::Down, true),
            Command::Paste,
            Command::Save,
            Command::Find,
            Command::NextTab,
            Command::CloseFile,
            Command::Undo,
            Command::GotoLine,
        ] {
            ed.execute(cmd);
        }
        ed.insert_text("x");
        ed.ensure_cursor_visible();
        ed.scroll_pane(1, 3);
        assert!(!ed.has_doc());
        ed.execute(Command::NewFile);
        assert!(ed.has_doc());
    }

    #[test]
    fn quit_asks_when_either_view_is_dirty() {
        let mut ed = editor("x");
        ed.execute(Command::FocusRight);
        ed.execute(Command::NewFile);
        ed.insert_text("unsaved");
        ed.execute(Command::FocusLeft);
        ed.execute(Command::Quit);
        assert!(!ed.should_quit);
        assert!(matches!(
            ed.minibuffer.as_ref().map(|m| &m.kind),
            Some(PromptKind::ConfirmDiscard(PendingAction::Quit))
        ));
    }

    fn text(ed: &Editor) -> String {
        ed.doc().rope.to_string()
    }

    #[test]
    fn typing_newline_autoindent() {
        let mut ed = editor("    foo");
        ed.execute(Command::Move(Motion::End, false));
        ed.execute(Command::Newline);
        ed.insert_text("bar");
        assert_eq!(text(&ed), "    foo\n    bar");
    }

    #[test]
    fn indent_outdent_lines() {
        let mut ed = editor("a\nb\n\nc");
        ed.execute(Command::SelectAll);
        ed.execute(Command::Indent);
        assert_eq!(text(&ed), "    a\n    b\n\n    c");
        ed.execute(Command::Outdent);
        assert_eq!(text(&ed), "a\nb\n\nc");
        ed.execute(Command::Undo);
        assert_eq!(text(&ed), "    a\n    b\n\n    c");
    }

    #[test]
    fn soft_tab_backspace() {
        let mut ed = editor("");
        ed.execute(Command::Indent);
        ed.execute(Command::Indent);
        assert_eq!(text(&ed), "        ");
        ed.execute(Command::Backspace);
        assert_eq!(text(&ed), "    ");
    }

    #[test]
    fn line_operations() {
        let mut ed = editor("one\ntwo\nthree");
        ed.execute(Command::Move(Motion::Down, false));
        ed.execute(Command::MoveLineUp);
        assert_eq!(text(&ed), "two\none\nthree");
        ed.execute(Command::MoveLineDown);
        ed.execute(Command::MoveLineDown);
        assert_eq!(text(&ed), "one\nthree\ntwo");
        ed.execute(Command::DuplicateLine);
        assert_eq!(text(&ed), "one\nthree\ntwo\ntwo");
        ed.execute(Command::DeleteLine);
        assert_eq!(text(&ed), "one\nthree\ntwo");
        ed.execute(Command::Move(Motion::DocStart, false));
        ed.execute(Command::DeleteLine);
        assert_eq!(text(&ed), "three\ntwo");
    }

    #[test]
    fn vertical_motion_keeps_goal_column() {
        let mut ed = editor("abcdef\nx\nabcdef");
        ed.execute(Command::Move(Motion::End, false));
        ed.execute(Command::Move(Motion::Down, false));
        assert_eq!(ed.doc().selection.head, 8);
        ed.execute(Command::Move(Motion::Down, false));
        assert_eq!(ed.doc().selection.head, 15);
    }

    #[test]
    fn find_prompt_selects_matches() {
        let mut ed = editor("alpha beta alpha");
        ed.execute(Command::Find);
        ed.insert_text("alpha");
        assert_eq!(ed.doc().selection.range(), (0, 5));
        ed.execute(Command::Newline);
        assert_eq!(ed.doc().selection.range(), (11, 16));
        ed.execute(Command::Cancel);
        assert!(ed.minibuffer.is_none());
    }

    #[test]
    fn goto_line() {
        let mut ed = editor("a\nb\nc\nd");
        ed.execute(Command::GotoLine);
        ed.insert_text("3");
        ed.execute(Command::Newline);
        assert_eq!(ed.doc().cursor_line(), 2);
    }
}
