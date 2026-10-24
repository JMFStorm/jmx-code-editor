use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ropey::Rope;

use super::history::{Change, EditKind, History};
use super::movement;
use super::selection::Selection;
use super::view::View;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::Crlf => "CRLF",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indent {
    Spaces(usize),
    Tabs,
}

impl Indent {
    pub fn unit(self) -> String {
        match self {
            Indent::Spaces(n) => " ".repeat(n),
            Indent::Tabs => "\t".to_string(),
        }
    }

    pub fn label(self) -> String {
        match self {
            Indent::Spaces(n) => format!("Spaces: {n}"),
            Indent::Tabs => "Tabs".to_string(),
        }
    }
}

#[derive(Debug)]
pub struct Document {
    pub rope: Rope,
    pub path: Option<PathBuf>,
    pub line_ending: LineEnding,
    pub indent: Indent,
    /// File started with a UTF-8 byte order mark; written back on save.
    pub bom: bool,
    /// File was not valid UTF-8 and was decoded lossily.
    pub lossy: bool,
    pub selection: Selection,
    /// Display column that up/down movement tries to keep.
    pub goal_col: Option<usize>,
    pub history: History,
    pub view: View,
    /// Built-in pages (help, shortcuts) cannot be edited or saved.
    pub read_only: bool,
    /// Tab title for documents that are not backed by a file.
    pub virtual_name: Option<String>,
}

impl Document {
    pub fn new_empty(path: Option<PathBuf>, tab_width: usize) -> Self {
        Self {
            rope: Rope::new(),
            path,
            line_ending: if cfg!(windows) {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            },
            indent: Indent::Spaces(tab_width),
            bom: false,
            lossy: false,
            selection: Selection::default(),
            goal_col: None,
            history: History::default(),
            view: View::default(),
            read_only: false,
            virtual_name: None,
        }
    }

    /// A read-only page with fixed contents, e.g. the help tab.
    pub fn new_virtual(name: &str, text: &str) -> Self {
        let mut doc = Self::new_empty(None, 4);
        doc.rope = Rope::from_str(text);
        doc.read_only = true;
        doc.virtual_name = Some(name.to_string());
        doc
    }

    /// Open `path`. A missing file gives an empty document that will be created on save.
    pub fn open(path: &Path, tab_width: usize) -> Result<Self> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::new_empty(Some(path.to_path_buf()), tab_width));
            }
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        let mut doc = Self::from_bytes(&bytes, tab_width);
        doc.path = Some(path.to_path_buf());
        Ok(doc)
    }

    pub fn from_bytes(bytes: &[u8], tab_width: usize) -> Self {
        let (bom, bytes) = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
            Some(rest) => (true, rest),
            None => (false, bytes),
        };
        let (text, lossy) = match std::str::from_utf8(bytes) {
            Ok(s) => (std::borrow::Cow::Borrowed(s), false),
            Err(_) => (String::from_utf8_lossy(bytes), true),
        };
        let (line_ending, text) = if text.contains("\r\n") {
            (
                LineEnding::Crlf,
                std::borrow::Cow::Owned(text.replace("\r\n", "\n")),
            )
        } else {
            (LineEnding::Lf, text)
        };
        let indent = detect_indent(&text, tab_width);
        let mut doc = Self::new_empty(None, tab_width);
        doc.rope = Rope::from_str(&text);
        doc.line_ending = line_ending;
        doc.indent = indent;
        doc.bom = bom;
        doc.lossy = lossy;
        doc
    }

    /// File contents as they will be written: original line endings and BOM restored.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut text = self.rope.to_string();
        if self.line_ending == LineEnding::Crlf {
            text = text.replace('\n', "\r\n");
        }
        let mut out = Vec::with_capacity(text.len() + 3);
        if self.bom {
            out.extend_from_slice(b"\xEF\xBB\xBF");
        }
        out.extend_from_slice(text.as_bytes());
        out
    }

    /// Atomic save: write a temp file next to the target, then rename it over.
    pub fn save(&mut self) -> Result<()> {
        if self.read_only {
            anyhow::bail!("{} is read-only", self.title());
        }
        let path = self.path.clone().context("document has no file name")?;
        // Write through symlinks instead of replacing them.
        let target = fs::canonicalize(&path).unwrap_or(path);
        let name = target
            .file_name()
            .context("invalid file name")?
            .to_string_lossy()
            .into_owned();
        let tmp = target.with_file_name(format!(".{name}.jmx-tmp"));
        fs::write(&tmp, self.to_bytes())
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        if let Ok(meta) = fs::metadata(&target) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        if let Err(e) = fs::rename(&tmp, &target) {
            let _ = fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("cannot replace {}", target.display()));
        }
        self.lossy = false;
        self.history.mark_saved();
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }

    pub fn title(&self) -> String {
        if let Some(name) = &self.virtual_name {
            return name.clone();
        }
        match &self.path {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string()),
            None => "untitled".to_string(),
        }
    }

    pub fn cursor_line(&self) -> usize {
        self.rope.char_to_line(self.selection.head)
    }

    pub fn selected_text(&self) -> String {
        let (s, e) = self.selection.range();
        self.rope.slice(s..e).to_string()
    }

    /// Apply replacements `(start, end, text)` in order as one undo step, then set the selection.
    pub fn transact(
        &mut self,
        kind: EditKind,
        edits: Vec<(usize, usize, String)>,
        after: Selection,
    ) {
        if self.read_only {
            return;
        }
        let before = self.selection;
        let mut changes = Vec::with_capacity(edits.len());
        for (start, end, text) in edits {
            if start == end && text.is_empty() {
                continue;
            }
            let removed = self.rope.slice(start..end).to_string();
            self.rope.remove(start..end);
            self.rope.insert(start, &text);
            changes.push(Change {
                start,
                removed,
                inserted: text,
            });
        }
        if changes.is_empty() {
            return;
        }
        self.history.record(kind, changes, before, after);
        self.selection = after.clamp(self.rope.len_chars());
        self.goal_col = None;
    }

    /// Replace the selection (or insert at the cursor) with `text`.
    pub fn replace_selection(&mut self, text: &str, kind: EditKind) {
        let (s, e) = self.selection.range();
        let after = Selection::point(s + text.chars().count());
        self.transact(kind, vec![(s, e, text.to_string())], after);
    }

    pub fn undo(&mut self) -> bool {
        let Some(t) = self.history.pop_undo() else {
            return false;
        };
        for c in t.changes.iter().rev() {
            self.rope
                .remove(c.start..c.start + c.inserted.chars().count());
            self.rope.insert(c.start, &c.removed);
        }
        self.selection = t.before.clamp(self.rope.len_chars());
        self.goal_col = None;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(t) = self.history.pop_redo() else {
            return false;
        };
        for c in &t.changes {
            self.rope
                .remove(c.start..c.start + c.removed.chars().count());
            self.rope.insert(c.start, &c.inserted);
        }
        self.selection = t.after.clamp(self.rope.len_chars());
        self.goal_col = None;
        true
    }

    /// Move the cursor to `pos`, extending the selection if `extend`.
    pub fn move_to(&mut self, pos: usize, extend: bool) {
        self.selection = self
            .selection
            .moved_to(pos.min(self.rope.len_chars()), extend);
        self.history.break_coalescing();
    }

    /// Line range `[first, last]` touched by the selection. A selection ending at
    /// column 0 of a line does not include that line.
    pub fn selected_lines(&self) -> (usize, usize) {
        let (s, e) = self.selection.range();
        let first = self.rope.char_to_line(s);
        let mut last = self.rope.char_to_line(e);
        if e > s && last > first && e == self.rope.line_to_char(last) {
            last -= 1;
        }
        (first, last)
    }

    pub fn line_indent_end(&self, line: usize) -> usize {
        let ls = self.rope.line_to_char(line);
        let le = movement::line_content_end(&self.rope, line);
        let mut p = ls;
        while p < le && matches!(self.rope.char(p), ' ' | '\t') {
            p += 1;
        }
        p
    }
}

/// Guess indentation style (tabs vs. spaces, and the space width) from the first lines.
fn detect_indent(text: &str, tab_width: usize) -> Indent {
    let mut tabs = 0;
    let mut spaces = 0;
    let mut min_width = usize::MAX;
    for line in text.lines().take(1000) {
        if line.starts_with('\t') {
            tabs += 1;
        } else if line.starts_with("  ") {
            spaces += 1;
            // Smallest indent of at least 2 spaces; skips " * " JSDoc continuation lines.
            let n = line.len() - line.trim_start_matches(' ').len();
            if n < line.len() {
                min_width = min_width.min(n);
            }
        }
    }
    if tabs > spaces {
        Indent::Tabs
    } else if matches!(min_width, 2 | 3 | 4 | 8) {
        Indent::Spaces(min_width)
    } else {
        Indent::Spaces(tab_width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_rejects_edits_and_save() {
        let mut doc = Document::new_virtual("Help", "text");
        doc.replace_selection("x", EditKind::Insert);
        assert_eq!(doc.rope.to_string(), "text");
        assert!(!doc.is_dirty());
        assert!(doc.save().is_err());
        assert_eq!(doc.title(), "Help");
    }

    #[test]
    fn crlf_and_bom_round_trip() {
        let src = b"\xEF\xBB\xBFone\r\ntwo\r\n";
        let doc = Document::from_bytes(src, 4);
        assert_eq!(doc.line_ending, LineEnding::Crlf);
        assert!(doc.bom);
        assert_eq!(doc.rope.to_string(), "one\ntwo\n");
        assert_eq!(doc.to_bytes(), src);
    }

    #[test]
    fn lf_round_trip_and_lossy() {
        let doc = Document::from_bytes(b"a\nb", 4);
        assert_eq!(doc.line_ending, LineEnding::Lf);
        assert_eq!(doc.to_bytes(), b"a\nb");
        let bad = Document::from_bytes(b"a\xFFb", 4);
        assert!(bad.lossy);
    }

    #[test]
    fn indent_detection() {
        assert_eq!(Document::from_bytes(b"x\n\ty\n\tz", 4).indent, Indent::Tabs);
        assert_eq!(
            Document::from_bytes(b"x\n  y\n    z", 4).indent,
            Indent::Spaces(2)
        );
        assert_eq!(
            Document::from_bytes(b"x\n    y\n * doc", 4).indent,
            Indent::Spaces(4)
        );
        assert_eq!(Document::from_bytes(b"x\ny", 4).indent, Indent::Spaces(4));
    }

    #[test]
    fn edits_undo_redo_and_dirty() {
        let mut doc = Document::from_bytes(b"hello", 4);
        doc.selection = Selection::point(5);
        for c in [" ", "w", "o"] {
            doc.replace_selection(c, EditKind::Insert);
        }
        assert_eq!(doc.rope.to_string(), "hello wo");
        assert!(doc.is_dirty());
        // Typing coalesced into one step.
        assert!(doc.undo());
        assert_eq!(doc.rope.to_string(), "hello");
        assert!(!doc.is_dirty());
        assert!(doc.redo());
        assert_eq!(doc.rope.to_string(), "hello wo");
        assert_eq!(doc.selection, Selection::point(8));
    }

    #[test]
    fn backspace_coalesces_and_multi_change_undo() {
        let mut doc = Document::from_bytes(b"abcdef", 4);
        doc.selection = Selection::point(6);
        for _ in 0..3 {
            let p = doc.selection.head;
            doc.transact(
                EditKind::Delete,
                vec![(p - 1, p, String::new())],
                Selection::point(p - 1),
            );
        }
        assert_eq!(doc.rope.to_string(), "abc");
        doc.undo();
        assert_eq!(doc.rope.to_string(), "abcdef");

        let mut doc = Document::from_bytes(b"a\nb\n", 4);
        doc.transact(
            EditKind::Other,
            vec![(2, 2, "  ".into()), (0, 0, "  ".into())],
            Selection::point(0),
        );
        assert_eq!(doc.rope.to_string(), "  a\n  b\n");
        doc.undo();
        assert_eq!(doc.rope.to_string(), "a\nb\n");
    }

    #[test]
    fn save_marker_survives_undo() {
        let dir = std::env::temp_dir().join(format!("jmx-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        fs::write(&path, "x\r\n").unwrap();
        let mut doc = Document::open(&path, 4).unwrap();
        doc.selection = Selection::point(1);
        doc.replace_selection("y", EditKind::Insert);
        doc.save().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"xy\r\n");
        assert!(!doc.is_dirty());
        // Typing after save must start a new undo step.
        doc.replace_selection("z", EditKind::Insert);
        assert!(doc.is_dirty());
        doc.undo();
        assert!(!doc.is_dirty());
        doc.undo();
        assert!(doc.is_dirty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
