//! Layout of the panel pages (Help, Shortcuts, Problems, Tests, Docs, References): every
//! line is word-wrapped to the panel's width when the page is built, and long texts go in
//! boxes. Pages are rebuilt when the panel's width changes, so nothing runs off the edge.

use crate::syntax::Token;

/// A box drawn behind lines `first..=last` of a page; `accent` colors its left bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageBox {
    pub first: usize,
    pub last: usize,
    pub accent: Option<Token>,
}

/// A page being built: its lines, what each line points at, and its boxes.
pub struct PageText<R> {
    pub width: usize,
    pub lines: Vec<String>,
    pub rows: Vec<Option<R>>,
    pub boxes: Vec<PageBox>,
}

impl<R: Clone> PageText<R> {
    pub fn new(width: usize) -> Self {
        Self {
            width: width.max(20),
            lines: Vec::new(),
            rows: Vec::new(),
            boxes: Vec::new(),
        }
    }

    /// A line as it is.
    pub fn line(&mut self, text: impl Into<String>, row: Option<R>) {
        self.lines.push(text.into());
        self.rows.push(row);
    }

    /// `text` (with its own leading indent) wrapped to the width; continuation lines are
    /// indented by `hang` and point at the same `row`.
    pub fn wrapped(&mut self, text: &str, hang: usize, row: Option<R>) {
        for l in wrap_hanging(text, self.width, hang) {
            self.line(l, row.clone());
        }
    }

    /// `text` wrapped inside a box at `indent` (lines `│ …`); returns its line range.
    pub fn boxed(
        &mut self,
        text: &str,
        indent: usize,
        row: Option<R>,
        accent: Option<Token>,
    ) -> (usize, usize) {
        let first = self.lines.len();
        let inner = self.width.saturating_sub(indent + 4).max(16);
        let pad = " ".repeat(indent);
        for l in wrap_text(text.trim_end(), inner) {
            self.line(format!("{pad}│ {l}"), row.clone());
        }
        let last = self.lines.len().saturating_sub(1).max(first);
        self.boxes.push(PageBox {
            first,
            last,
            accent,
        });
        (first, last)
    }

    /// A hint line (`· a · b`), wrapped with every line starting `·`.
    pub fn hint(&mut self, text: &str) {
        let first = self.len();
        self.wrapped(&format!("· {text}"), 2, None);
        for l in &mut self.lines[first + 1..] {
            let rest = l
                .trim_start()
                .trim_start_matches('·')
                .trim_start()
                .to_string();
            *l = format!("· {rest}");
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

impl super::Editor {
    /// The width panel pages are laid out at (a sensible default before the first layout).
    pub fn panel_width(&self) -> usize {
        match self.panes[super::PANEL].cols {
            0 => 100,
            c => c.saturating_sub(1),
        }
    }

    /// The panel's width changed (window, divider, zoom): lay its pages out again.
    pub fn panel_resized(&mut self) {
        let cols = self.panes[super::PANEL].cols;
        if cols == self.panel_layout_cols || cols == 0 {
            return;
        }
        self.panel_layout_cols = cols;
        self.refresh_static_pages();
        self.refresh_problems_page();
        self.refresh_tests_page();
        self.refresh_docs_page_kept();
        self.refresh_references_page();
    }

    /// Help and Shortcuts, wrapped to the panel (the cursor stays on the same text).
    pub(super) fn refresh_static_pages(&mut self) {
        let width = self.panel_width();
        for p in 0..self.panes.len() {
            for i in 0..self.panes[p].docs.len() {
                let source = match self.panes[p].docs[i].virtual_name.as_deref() {
                    Some(crate::help::HELP_TITLE) => crate::help::help_text(),
                    Some(crate::help::SHORTCUTS_TITLE) => crate::help::shortcuts_text(),
                    _ => continue,
                };
                let text = reflow(&source, width);
                let doc = &mut self.panes[p].docs[i];
                if doc.rope == text.as_str() {
                    continue;
                }
                let new = ropey::Rope::from_str(&text);
                let head = same_spot(&doc.rope, doc.selection.head, &new);
                let top = doc.rope.line_to_char(
                    doc.view
                        .top_line
                        .min(doc.rope.len_lines().saturating_sub(1)),
                );
                let top = new.char_to_line(same_spot(&doc.rope, top, &new));
                doc.rope = new;
                doc.selection = super::selection::Selection::point(head);
                doc.view.top_line = top;
                doc.view.left_col = 0;
            }
        }
    }
}

/// Split `text` into lines of at most `width` chars, breaking at spaces where possible.
/// Continuation lines keep the original line's indentation.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let indent = line
            .chars()
            .take_while(|c| *c == ' ')
            .count()
            .min(width / 2);
        if line.is_empty() {
            out.push(String::new());
        } else {
            out.extend(wrap_hanging(line, width, indent));
        }
    }
    out
}

/// One line wrapped to `width`; continuation lines are indented by `hang`. Breaks at the
/// last space, unless that leaves a very short line (a long path is better split mid-path
/// than moved whole to the next line).
pub fn wrap_hanging(line: &str, width: usize, hang: usize) -> Vec<String> {
    let width = width.max(8);
    let hang = hang.min(width / 2);
    let chars: Vec<char> = line.trim_end().chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    let mut first = true;
    while start < chars.len() || first {
        let avail = if first { width } else { width - hang };
        let mut end = (start + avail).min(chars.len());
        if end < chars.len() {
            let piece = &chars[start..end];
            if let Some(sp) = piece.iter().rposition(|c| *c == ' ')
                && sp > avail / 2
            {
                end = start + sp + 1;
            } else if let Some(soft) = piece
                .iter()
                .rposition(|c| matches!(c, '.' | '/' | '\\' | '_' | ',' | '(' | '-'))
                && soft > avail / 3
            {
                // A long name or path: after a separator, not mid-word.
                end = start + soft + 1;
            }
        }
        let piece: String = chars[start..end].iter().collect();
        out.push(if first {
            piece.trim_end().to_string()
        } else {
            format!("{}{}", " ".repeat(hang), piece.trim())
        });
        first = false;
        start = end;
        // Spaces at a break are dropped.
        while start < chars.len() && chars[start] == ' ' {
            start += 1;
        }
    }
    out
}

fn is_underline(line: &str) -> bool {
    let t = line.trim_end();
    t.len() >= 3 && (t.chars().all(|c| c == '=') || t.chars().all(|c| c == '-'))
}

/// A table row: `  keys   description` (two or more spaces before the description). Returns
/// the description's column.
fn table_column(line: &str) -> Option<usize> {
    let indent = line.chars().take_while(|c| *c == ' ').count();
    if indent == 0 {
        return None;
    }
    let chars: Vec<char> = line.chars().collect();
    let mut k = indent;
    while k + 2 < chars.len() {
        if chars[k] == ' ' && chars[k + 1] == ' ' {
            let mut d = k;
            while d < chars.len() && chars[d] == ' ' {
                d += 1;
            }
            return (d < chars.len()).then_some(d);
        }
        k += 1;
    }
    None
}

/// Re-wrap plain help text to `width`: paragraphs are joined and wrapped, headings kept
/// (underlines fitted), table rows wrapped under their description column.
pub fn reflow(text: &str, width: usize) -> String {
    let width = width.max(20);
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        let next = lines.get(i + 1).copied().unwrap_or("");
        if line.is_empty() {
            out.push(String::new());
            i += 1;
        } else if is_underline(next) && !line.starts_with(' ') {
            // A heading and its underline.
            let head = wrap_hanging(line, width, 0);
            let len = head.iter().map(|h| h.chars().count()).max().unwrap_or(3);
            let mark = next.trim_end().chars().next().unwrap_or('-');
            out.extend(head);
            out.push(mark.to_string().repeat(len.min(width)));
            i += 2;
        } else if !line.starts_with(' ') {
            // A paragraph: up to a blank, indented or heading line.
            let mut para = line.to_string();
            i += 1;
            while i < lines.len() {
                let l = lines[i].trim_end();
                let after = lines.get(i + 1).copied().unwrap_or("");
                if l.is_empty() || l.starts_with(' ') || is_underline(after) || is_underline(l) {
                    break;
                }
                para.push(' ');
                para.push_str(l);
                i += 1;
            }
            out.extend(wrap_hanging(&para, width, 0));
        } else if let Some(col) = table_column(line) {
            // A table row, with continuation lines indented to its description column.
            let head: String = line.chars().take(col).collect();
            let mut desc: String = line.chars().skip(col).collect();
            i += 1;
            while i < lines.len() {
                let l = lines[i];
                let ind = l.chars().take_while(|c| *c == ' ').count();
                if l.trim().is_empty() || ind != col {
                    break;
                }
                desc.push(' ');
                desc.push_str(l.trim());
                i += 1;
            }
            if col <= width / 2 {
                out.extend(wrap_hanging(&format!("{head}{desc}"), width, col));
            } else {
                out.push(head.trim_end().to_string());
                out.extend(wrap_hanging(&format!("    {desc}"), width, 4));
            }
        } else {
            let indent = line.chars().take_while(|c| *c == ' ').count();
            out.extend(wrap_hanging(line, width, indent));
            i += 1;
        }
    }
    let mut s = out.join("\n");
    if text.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Where the same text sits after re-wrapping: the char position in `new` after as many
/// non-space chars as come before `pos` in `old` (heading underlines, whose length follows
/// the wrapping, are not counted).
pub fn same_spot(old: &ropey::Rope, pos: usize, new: &ropey::Rope) -> usize {
    let pos = pos.min(old.len_chars());
    let pos_line = old.char_to_line(pos);
    let mut solid = 0;
    for (n, line) in old.lines().enumerate().take(pos_line + 1) {
        let text: String = line.chars().collect();
        if is_underline(text.trim_end()) {
            continue;
        }
        let upto = if n == pos_line {
            pos - old.line_to_char(n)
        } else {
            text.chars().count()
        };
        solid += text
            .chars()
            .take(upto)
            .filter(|c| !c.is_whitespace())
            .count();
    }
    let mut seen = 0;
    for (n, line) in new.lines().enumerate() {
        let text: String = line.chars().collect();
        if is_underline(text.trim_end()) {
            continue;
        }
        for (k, c) in text.chars().enumerate() {
            if c.is_whitespace() {
                continue;
            }
            if seen == solid {
                return new.line_to_char(n) + k;
            }
            seen += 1;
        }
    }
    new.len_chars()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widest(s: &str) -> usize {
        s.lines().map(|l| l.chars().count()).max().unwrap_or(0)
    }

    #[test]
    fn wraps_with_hanging_indent() {
        assert_eq!(wrap_text("alpha beta gamma", 11), ["alpha beta", "gamma"]);
        assert_eq!(
            wrap_text("    at /a/very/long/path/file.js:1:2", 16),
            ["    at /a/very/", "    long/path/", "    file.js:1:2"]
        );
        assert_eq!(wrap_text("x\n\ny", 10), ["x", "", "y"]);
        assert_eq!(
            wrap_hanging("  12:5  const total = formatCurrency(sum);", 30, 8),
            ["  12:5  const total =", "        formatCurrency(sum);"]
        );
        assert_eq!(wrap_hanging("", 10, 2), [""]);
        // Long names break after a separator.
        assert_eq!(
            wrap_hanging("System.ComponentModel.DataAnnotations", 24, 0),
            ["System.ComponentModel.", "DataAnnotations"]
        );
        assert_eq!(
            wrap_hanging("  ○ GetAllAsync_ReturnsSeededProducts", 26, 4),
            ["  ○ GetAllAsync_", "    ReturnsSeededProducts"]
        );
        let mut p: PageText<u8> = PageText::new(24);
        p.hint("w: Microsoft Learn · Enter: open · Backspace: back");
        assert!(
            p.lines
                .iter()
                .all(|l| l.starts_with("· ") && !l.starts_with("· ·")),
            "{:?}",
            p.lines
        );
    }

    #[test]
    fn reflows_paragraphs_headings_and_tables() {
        let help = "Title here\n==========\n\nThe left view is the editor: every file opens there. The right\nview is a panel for pages.\n\n  F6               move the cursor between the editor and the panel\n  Ctrl+R           revert: discard all unsaved changes to the file\n                   (one undo step, so Ctrl+Z brings them back)\n    indented note that is rather long and needs wrapping too\n";
        let out = reflow(help, 40);
        assert!(widest(&out) <= 40, "{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "Title here");
        assert_eq!(lines[1], "==========");
        // The paragraph is joined, then wrapped.
        assert_eq!(lines[3], "The left view is the editor: every file");
        assert_eq!(lines[4], "opens there. The right view is a panel");
        assert_eq!(lines[5], "for pages.");
        // The continuation line joins its row.
        let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("to the file (one undo step"), "{out}");
        assert!(!out.contains("\n                   (one undo"), "{out}");
        // Narrow: the description column (19) is past half the width, so the description
        // goes on its own lines.
        let narrow = reflow(help, 34);
        assert!(widest(&narrow) <= 34, "{narrow}");
        assert!(narrow.contains("\n  F6\n    move the cursor"), "{narrow}");
        // Wide enough: the description stays beside the keys.
        let wide = reflow(help, 60);
        assert!(wide.contains("  F6               move the cursor between the editor and"));
        assert!(wide.contains("\n                   the panel\n"), "{wide}");
        assert!(widest(&wide) <= 60);
    }

    #[test]
    fn page_rows_and_boxes() {
        let mut p: PageText<u32> = PageText::new(24);
        p.line("Header", None);
        p.wrapped("  12:5  const total = formatCurrency(sum);", 8, Some(7));
        let (a, b) = p.boxed(
            "A rather long message that wraps inside its box.",
            4,
            Some(7),
            None,
        );
        assert!(p.rows[1..].iter().all(|r| *r == Some(7)));
        assert_eq!(b, p.len() - 1);
        assert!(p.lines[a].starts_with("    │ ") && !p.lines[a - 1].contains('│'));
        assert!(
            p.text().lines().all(|l| l.chars().count() <= 24),
            "{}",
            p.text()
        );
        assert_eq!(p.boxes[0].first, a);
    }

    #[test]
    fn same_spot_follows_text() {
        let old = ropey::Rope::from_str("one two\nthree four");
        let new = ropey::Rope::from_str("one\ntwo three\nfour");
        // `four` in the old text (char 14) is `four` in the new one.
        assert_eq!(same_spot(&old, 14, &new), 14);
        assert_eq!(new.char(same_spot(&old, 8, &new)), 't');
    }

    #[test]
    fn panel_pages_rewrap_when_the_width_changes() {
        use crate::editor::{Editor, PANEL};
        let mut ed = Editor::new(crate::config::Config::default(), &[]);
        ed.execute(crate::commands::Command::Help);
        let help = |ed: &Editor| ed.panes[PANEL].doc().unwrap().rope.to_string();
        // Cursor on a word in the middle of the page.
        let text = help(&ed);
        let at = text.find("Ctrl+1 / Ctrl+2").unwrap();
        let pos = ed.panes[PANEL].doc().unwrap().rope.byte_to_char(at);
        ed.panes[PANEL].docs[0].selection = crate::editor::selection::Selection::point(pos);
        for cols in [40, 70, 33] {
            ed.panes[PANEL].cols = cols;
            ed.panel_resized();
            let text = help(&ed);
            assert!(widest(&text) < cols, "{cols}: {text}");
            let doc = ed.panes[PANEL].doc().unwrap();
            let rest: String = doc
                .rope
                .slice(doc.selection.head..)
                .chars()
                .take(5)
                .collect();
            assert_eq!(rest, "Ctrl+", "cursor follows its text at {cols}");
        }
    }
}
