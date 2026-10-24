pub mod document;
pub mod history;
pub mod movement;
pub mod search;
pub mod selection;
pub mod view;

use std::path::{MAIN_SEPARATOR, Path, PathBuf};
use std::time::{Duration, Instant};

use crate::commands::{Command, Motion};
use crate::config::{Config, MAX_FONT_SIZE, MIN_FONT_SIZE};
use crate::help;
use crate::ui::minibuffer::{AfterSave, Minibuffer, PromptKind, expand_tilde};
use crate::ui::picker::{FilePicker, PickResult};
use document::{Document, Indent};
use history::EditKind;
use movement as mv;
use search::Query;
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

pub struct Editor {
    pub docs: Vec<Document>,
    pub active: usize,
    pub config: Config,
    pub minibuffer: Option<Minibuffer>,
    pub picker: Option<FilePicker>,
    /// Number of list rows the picker overlay shows, updated by the app.
    pub picker_rows: usize,
    pub message: Option<Message>,
    pub find_query: String,
    /// Viewport size in text cells, updated by the app on resize/zoom.
    pub rows: usize,
    pub cols: usize,
    pub should_quit: bool,
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
            docs: Vec::new(),
            active: 0,
            config,
            minibuffer: None,
            picker: None,
            picker_rows: 10,
            message: None,
            find_query: String::new(),
            rows: 30,
            cols: 80,
            should_quit: false,
            clipboard,
            internal_clip: String::new(),
            line_clip: None,
        };
        for p in paths {
            ed.open_path(p);
        }
        if ed.docs.is_empty() {
            ed.docs.push(Document::new_empty(None, ed.config.tab_width));
        }
        ed.active = 0;
        ed
    }

    pub fn doc(&self) -> &Document {
        &self.docs[self.active]
    }

    pub fn doc_mut(&mut self) -> &mut Document {
        &mut self.docs[self.active]
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
        let (rows, cols, tab) = (self.rows, self.cols, self.config.tab_width);
        let doc = self.doc_mut();
        let head = doc.selection.head;
        let line = doc.rope.char_to_line(head);
        let col = mv::col_of(&doc.rope, head, tab);
        doc.view.ensure_visible(line, col, rows, cols);
    }

    // ---------------------------------------------------------------- input

    /// Typed text (already filtered of shortcuts by the app).
    pub fn insert_text(&mut self, text: &str) {
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
        if text.chars().any(|c| c.is_control() && c != '\t') {
            return;
        }
        if self.doc().read_only {
            self.warn("Read-only");
            return;
        }
        self.doc_mut().replace_selection(text, EditKind::Insert);
        self.ensure_cursor_visible();
    }

    pub fn execute(&mut self, cmd: Command) {
        if self.picker.is_some() && self.picker_command(cmd) {
            return;
        }
        if self.minibuffer.is_some() && self.prompt_command(cmd) {
            return;
        }
        use Command::*;
        if self.doc().read_only && modifies_text(cmd) {
            self.warn(format!("{} is read-only", self.doc().title()));
            return;
        }
        let keep_scroll = matches!(cmd, ScrollUp | ScrollDown | ZoomIn | ZoomOut | ZoomReset);
        match cmd {
            Move(m, extend) => self.move_cursor(m, extend),
            SelectAll => {
                let len = self.doc().rope.len_chars();
                let doc = self.doc_mut();
                doc.selection = Selection::new(0, len);
                doc.history.break_coalescing();
            }
            SelectLine => self.select_line(),
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
                self.docs
                    .push(Document::new_empty(None, self.config.tab_width));
                self.active = self.docs.len() - 1;
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
            CloseFile => {
                if self.doc().is_dirty() {
                    self.minibuffer = Some(Minibuffer::new(
                        PromptKind::ConfirmClose(self.active),
                        String::new(),
                    ));
                } else {
                    self.close_doc(self.active);
                }
            }
            NextTab => self.active = (self.active + 1) % self.docs.len(),
            PrevTab => self.active = (self.active + self.docs.len() - 1) % self.docs.len(),
            GotoTab(i) => {
                if i < self.docs.len() {
                    self.active = i;
                }
            }
            Find => self.start_find(),
            FindNext => self.find_step(true),
            FindPrev => self.find_step(false),
            GotoLine => {
                self.minibuffer = Some(Minibuffer::new(PromptKind::GotoLine, String::new()));
            }
            ScrollUp => self.scroll(-1),
            ScrollDown => self.scroll(1),
            ZoomIn => self.zoom(self.config.font_size + 1.0),
            ZoomOut => self.zoom(self.config.font_size - 1.0),
            ZoomReset => self.zoom(self.config.default_font_size),
            Help => self.open_virtual(help::HELP_TITLE, &help::help_text()),
            ShowShortcuts => self.open_virtual(help::SHORTCUTS_TITLE, &help::shortcuts_text()),
            Quit => self.quit(),
        }
        if !keep_scroll {
            self.ensure_cursor_visible();
        }
    }

    /// Show a built-in read-only page, reusing its tab if already open.
    fn open_virtual(&mut self, name: &str, text: &str) {
        self.minibuffer = None;
        if let Some(i) = self
            .docs
            .iter()
            .position(|d| d.virtual_name.as_deref() == Some(name))
        {
            self.active = i;
            return;
        }
        self.docs.push(Document::new_virtual(name, text));
        self.active = self.docs.len() - 1;
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
            Cancel | OpenFile => {
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
            NewFile | OpenPath | Help | ShowShortcuts | Quit | CloseFile | NextTab | PrevTab
            | GotoTab(_) => {
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
                self.picker = None;
                self.open_path(&path);
                self.ensure_cursor_visible();
            }
            PickResult::None => p.ensure_visible(self.picker_rows),
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
        let doc = self.doc_mut();
        let total = doc.rope.len_lines();
        doc.view.scroll_lines(lines, total);
    }

    pub fn scroll_horizontal(&mut self, cols: isize) {
        self.doc_mut().view.scroll_cols(cols);
    }

    fn zoom(&mut self, size: f32) {
        self.config.font_size = size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        self.info(format!("Font size {}", self.config.font_size));
    }

    // ---------------------------------------------------------------- mouse

    /// Char position for a cell relative to the text area (row may be negative / past the end
    /// while drag-selecting; it is clamped).
    pub fn pos_at_cell(&self, row: isize, col: isize) -> usize {
        let doc = self.doc();
        let last = doc.rope.len_lines() as isize - 1;
        let line = (doc.view.top_line as isize + row).clamp(0, last) as usize;
        let col = (doc.view.left_col as isize + col).max(0) as usize;
        mv::pos_at_col(&doc.rope, line, col, self.config.tab_width)
    }

    pub fn click(&mut self, pos: usize, extend: bool, count: u32) {
        let doc = self.doc_mut();
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
    }

    pub fn drag_to(&mut self, pos: usize) {
        let doc = self.doc_mut();
        doc.selection.head = pos;
        self.ensure_cursor_visible();
    }

    // ---------------------------------------------------------------- motion

    fn move_cursor(&mut self, m: Motion, extend: bool) {
        let tab = self.config.tab_width;
        let page = self.rows.saturating_sub(1).max(1);
        let doc = self.doc_mut();
        let rope = &doc.rope;
        let sel = doc.selection;
        let head = sel.head;
        let (s, e) = sel.range();
        let collapse = !extend && !sel.is_empty();

        let vertical = |doc: &Document, lines: isize| -> (usize, usize) {
            let rope = &doc.rope;
            let line = rope.char_to_line(head) as isize;
            let goal = doc.goal_col.unwrap_or_else(|| mv::col_of(rope, head, tab));
            let target = line + lines;
            let pos = if target < 0 {
                0
            } else if target >= rope.len_lines() as isize {
                rope.len_chars()
            } else {
                mv::pos_at_col(rope, target as usize, goal, tab)
            };
            (pos, goal)
        };

        let mut goal = None;
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
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
                let lines = match m {
                    Motion::Up => -1,
                    Motion::Down => 1,
                    Motion::PageUp => -(page as isize),
                    _ => page as isize,
                };
                if matches!(m, Motion::PageUp | Motion::PageDown) {
                    let total = doc.rope.len_lines();
                    doc.view.scroll_lines(lines, total);
                }
                let (pos, g) = vertical(doc, lines);
                goal = Some(g);
                pos
            }
        };
        doc.move_to(pos, extend);
        doc.goal_col = goal;
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

    fn dir_hint(&self) -> String {
        let dir = self
            .doc()
            .path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .filter(|p| !p.as_os_str().is_empty())
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
        if let Some(i) = self.docs.iter().position(|d| {
            d.path.as_deref() == Some(path)
                || canon.is_some()
                    && d.path.as_ref().and_then(|p| std::fs::canonicalize(p).ok()) == canon
        }) {
            self.active = i;
            return;
        }
        match Document::open(path, self.config.tab_width) {
            Ok(doc) => {
                let exists = path.exists();
                let lossy = doc.lossy;
                // Replace a pristine untitled tab instead of keeping it around.
                let reuse = self.docs.len() == 1
                    && self.docs[0].path.is_none()
                    && !self.docs[0].is_dirty()
                    && self.docs[0].rope.len_chars() == 0;
                if reuse {
                    self.docs[0] = doc;
                    self.active = 0;
                } else {
                    self.docs.push(doc);
                    self.active = self.docs.len() - 1;
                }
                if lossy {
                    self.warn("File is not valid UTF-8; invalid bytes were replaced and will be lost on save");
                } else if !exists {
                    self.info(format!("New file: {}", path.display()));
                }
            }
            Err(e) => self.error(format!("{e:#}")),
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
                self.info(msg);
                self.after_save(after);
            }
            Err(e) => self.error(format!("{e:#}")),
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
            AfterSave::CloseDoc => self.close_doc(self.active),
            AfterSave::ContinueQuit => self.continue_quit(),
        }
    }

    fn close_doc(&mut self, i: usize) {
        self.docs.remove(i);
        if self.docs.is_empty() {
            self.docs
                .push(Document::new_empty(None, self.config.tab_width));
        }
        if self.active > i || self.active >= self.docs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    fn quit(&mut self) {
        if self.docs.iter().any(Document::is_dirty) {
            self.minibuffer = Some(Minibuffer::new(PromptKind::ConfirmQuit, String::new()));
        } else {
            self.should_quit = true;
        }
    }

    /// Save every dirty document; untitled ones get a Save As prompt that resumes quitting.
    fn continue_quit(&mut self) {
        for i in 0..self.docs.len() {
            if !self.docs[i].is_dirty() {
                continue;
            }
            self.active = i;
            if self.docs[i].path.is_none() {
                self.prompt_save_as(AfterSave::ContinueQuit);
                return;
            }
            if let Err(e) = self.docs[i].save() {
                self.error(format!("{e:#}"));
                return;
            }
        }
        self.should_quit = true;
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
            ZoomIn | ZoomOut | ZoomReset | Save | Quit | ScrollUp | ScrollDown => return false,
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
            let Some(q) = Query::new(&self.find_query) else {
                let doc = self.doc_mut();
                doc.selection = Selection::point(origin);
                return;
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
                let old = self.doc_mut().path.replace(path);
                self.save(after);
                if self
                    .message
                    .as_ref()
                    .is_some_and(|m| m.kind == MsgKind::Error)
                {
                    self.doc_mut().path = old;
                }
            }
            PromptKind::Find { .. } => {
                // Enter jumps to the next match and keeps the prompt open.
                self.minibuffer = Some(mb);
                self.find_step(true);
            }
            PromptKind::GotoLine => self.goto_line(&input),
            PromptKind::ConfirmClose(_) | PromptKind::ConfirmQuit => self.minibuffer = Some(mb),
        }
        self.ensure_cursor_visible();
    }

    fn confirm_key(&mut self, c: char) {
        let Some(mb) = self.minibuffer.take() else {
            return;
        };
        match (mb.kind.clone(), c) {
            (PromptKind::ConfirmClose(i), 'y') => {
                self.active = i;
                self.save(AfterSave::CloseDoc);
            }
            (PromptKind::ConfirmClose(i), 'n') => self.close_doc(i),
            (PromptKind::ConfirmQuit, 's') => self.continue_quit(),
            (PromptKind::ConfirmQuit, 'd') => self.should_quit = true,
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
        let tab = self.config.tab_width;
        let doc = self.doc_mut();
        let line = line.clamp(1, doc.rope.len_lines()) - 1;
        let pos = mv::pos_at_col(&doc.rope, line, col.saturating_sub(1), tab);
        doc.move_to(pos, false);
        // Put the target line near the middle of the screen.
        let rows = self.rows;
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
        let Some(q) = Query::new(&self.find_query) else {
            self.start_find();
            return;
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
            }) => Query::new(&self.find_query),
            _ => None,
        }
    }
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
        ed.docs[0] = Document::from_bytes(text.as_bytes(), 4);
        ed
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
