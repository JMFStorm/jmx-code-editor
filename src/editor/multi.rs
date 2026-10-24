//! Multi-cursor editing: adding occurrences of the selection (Shift+Alt+. / Shift+Alt+,,
//! Ctrl+Shift+L) and the edits that apply at every cursor.

use ropey::Rope;

use super::Editor;
use super::history::EditKind;
use super::movement as mv;
use super::selection::Selection;
use crate::commands::Command;

impl Editor {
    /// The focused document has more than one cursor.
    pub fn multi_cursor(&self) -> bool {
        self.has_doc() && !self.doc().extra.is_empty()
    }

    /// Shift+Alt+.: select the word under the cursor, or add the next occurrence of the
    /// selected text (exact match, wrapping) as a new primary selection.
    pub(super) fn add_next_occurrence(&mut self) {
        if self.select_word_if_empty() {
            return;
        }
        let doc = self.doc();
        let needle = doc.selected_text();
        let taken = doc.all_selections();
        let from = doc.selection.range().1;
        let found = occurrences(&doc.rope, &needle)
            .into_iter()
            .filter(|&(s, e)| !taken.iter().any(|t| intersects(t.range(), (s, e))))
            // Next one after the newest selection, else wrap to the first.
            .min_by_key(|&(s, _)| (s < from, s));
        match found {
            Some((s, e)) => {
                let doc = self.doc_mut();
                let old = doc.selection;
                doc.extra.push(old);
                doc.selection = Selection::new(s, e);
                doc.history.break_coalescing();
                self.cursor_count_message();
            }
            None => self.info("No more occurrences"),
        }
    }

    /// Shift+Alt+,: drop the newest selection; the one added before it becomes primary.
    pub(super) fn remove_last_occurrence(&mut self) {
        let doc = self.doc_mut();
        match doc.extra.pop() {
            Some(prev) => {
                doc.selection = prev;
                self.cursor_count_message();
            }
            None => self.info("Only one cursor"),
        }
    }

    /// Ctrl+Shift+L: select every occurrence of the selection (or of the word under the cursor).
    pub(super) fn select_all_occurrences(&mut self) {
        self.select_word_if_empty();
        let doc = self.doc();
        let needle = doc.selected_text();
        if needle.is_empty() {
            return;
        }
        let primary = doc.selection.range();
        let all = occurrences(&doc.rope, &needle);
        let doc = self.doc_mut();
        doc.extra = all
            .into_iter()
            .filter(|&r| r != primary)
            .map(|(s, e)| Selection::new(s, e))
            .collect();
        doc.history.break_coalescing();
        self.cursor_count_message();
    }

    /// With nothing selected, select the word under the cursor. Returns true if it did.
    fn select_word_if_empty(&mut self) -> bool {
        let doc = self.doc_mut();
        if !doc.selection.is_empty() {
            return false;
        }
        let (s, e) = mv::word_at(&doc.rope, doc.selection.head);
        let word = doc.rope.slice(s..e).to_string();
        if word.trim().is_empty() {
            return true;
        }
        doc.selection = Selection::new(s, e);
        doc.history.break_coalescing();
        true
    }

    fn cursor_count_message(&mut self) {
        let n = self.doc().extra.len() + 1;
        if n > 1 {
            self.info(format!("{n} cursors (Esc: back to one)"));
        }
    }

    /// Commands that act at every cursor. Others first collapse to the primary cursor.
    pub(super) fn multi_aware(cmd: Command) -> bool {
        use Command::*;
        matches!(
            cmd,
            Move(..)
                | Newline
                | Indent
                | Backspace
                | Delete
                | DeleteWordLeft
                | DeleteWordRight
                | Copy
                | Cut
                | Paste
                | AddNextOccurrence
                | RemoveLastOccurrence
                | SelectAllOccurrences
                | Save
                | SaveAs
                | ScrollUp
                | ScrollDown
                | ZoomIn
                | ZoomOut
                | ZoomReset
                | Help
                | ShowShortcuts
                | FocusOtherView
                | FocusLeft
                | FocusRight
                | ToggleSplit
                | ResetSplit
                | FileMenu
                | WorkspaceMenu
                | SelectionMenu
                | ToggleExplorer
                | ToggleCase
                | ToggleWholeWord
                | ToggleRegex
                | NextTab
                | PrevTab
                | GotoTab(_)
        )
    }

    /// Run an editing command at every cursor. Returns false if it is not a multi-cursor edit.
    pub(super) fn multi_edit(&mut self, cmd: Command) -> bool {
        use Command::*;
        let unit = self.doc().indent.unit();
        match cmd {
            Newline => self.edit_each(EditKind::Insert, |rope, s| {
                let (start, end) = s.range();
                let line = rope.char_to_line(start);
                let ls = rope.line_to_char(line);
                let indent: String = rope
                    .slice(ls..start)
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                (start, end, format!("\n{indent}"))
            }),
            // Tab with selections indents lines: done for the primary selection only.
            Indent if self.doc().all_selections().iter().any(|s| !s.is_empty()) => {
                self.doc_mut().collapse_extra();
                return false;
            }
            Indent => self.edit_each(EditKind::Insert, |_, s| {
                let (start, end) = s.range();
                (start, end, unit.clone())
            }),
            Backspace => self.delete_each(mv::prev_grapheme),
            Delete => self.delete_each(mv::next_grapheme),
            DeleteWordLeft => self.delete_each(mv::prev_word_start),
            DeleteWordRight => self.delete_each(mv::next_word_end),
            Copy | Cut => {
                let doc = self.doc();
                let texts: Vec<String> = doc
                    .all_selections()
                    .iter()
                    .map(|s| {
                        let (a, b) = s.range();
                        doc.rope.slice(a..b).to_string()
                    })
                    .collect();
                self.set_clipboard(texts.join("\n"));
                self.line_clip = None;
                if cmd == Cut {
                    self.edit_each(EditKind::Other, |_, s| {
                        let (a, b) = s.range();
                        (a, b, String::new())
                    });
                }
            }
            Paste => {
                let Some(text) = self.get_clipboard() else {
                    return true;
                };
                let text = text.replace("\r\n", "\n");
                let n = self.doc().extra.len() + 1;
                let lines: Vec<&str> = text
                    .strip_suffix('\n')
                    .unwrap_or(&text)
                    .split('\n')
                    .collect();
                // One line per cursor when the counts match (e.g. copied from n cursors).
                let parts: Vec<String> = if lines.len() == n {
                    lines.iter().map(|l| l.to_string()).collect()
                } else {
                    vec![text.clone(); n]
                };
                let mut i = 0;
                self.edit_each(EditKind::Other, |_, s| {
                    let (a, b) = s.range();
                    let part = parts[i.min(parts.len() - 1)].clone();
                    i += 1;
                    (a, b, part)
                });
            }
            _ => return false,
        }
        true
    }

    /// Typed text at every cursor.
    pub(super) fn multi_insert(&mut self, text: &str) {
        self.edit_each(EditKind::Insert, |_, s| {
            let (a, b) = s.range();
            (a, b, text.to_string())
        });
    }

    /// Delete each selection, or from each cursor to `target(rope, head)`.
    fn delete_each(&mut self, target: impl Fn(&Rope, usize) -> usize) {
        self.edit_each(EditKind::Delete, |rope, s| {
            let (a, b) = s.range();
            if a != b {
                return (a, b, String::new());
            }
            let t = target(rope, s.head);
            (t.min(s.head), t.max(s.head), String::new())
        });
    }

    /// Replace a range per selection (in document order) as one undo step. Each cursor ends up
    /// after its inserted text.
    fn edit_each(
        &mut self,
        kind: EditKind,
        mut f: impl FnMut(&Rope, Selection) -> (usize, usize, String),
    ) {
        let doc = self.doc_mut();
        let primary = doc.selection;
        let sels = doc.all_selections();
        let mut edits: Vec<(usize, usize, String)> = Vec::with_capacity(sels.len());
        let mut prev_end = 0;
        for s in &sels {
            let (mut a, b, text) = f(&doc.rope, *s);
            // Edits must not overlap (e.g. Backspace at neighbouring cursors).
            a = a.max(prev_end);
            let b = b.max(a);
            prev_end = b;
            edits.push((a, b, text));
        }
        // New cursor positions, front to back with the length change so far.
        let mut delta: isize = 0;
        let mut after = Vec::with_capacity(edits.len());
        for (a, b, text) in &edits {
            let n = text.chars().count() as isize;
            let pos = (*a as isize + delta + n) as usize;
            after.push(Selection::point(pos));
            delta += n - (*b - *a) as isize;
        }
        let pi = sels.iter().position(|s| *s == primary).unwrap_or(0);
        // Apply back to front so earlier positions stay valid.
        edits.reverse();
        doc.transact(kind, edits, after[pi]);
        if doc.selection == after[pi] {
            after.remove(pi);
            doc.extra = after;
            doc.merge_selections();
        }
        self.ensure_cursor_visible();
    }
}

/// Char ranges of every non-overlapping exact occurrence of `needle`.
fn occurrences(rope: &Rope, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let text = rope.to_string();
    let n = needle.chars().count();
    text.match_indices(needle)
        .map(|(b, _)| {
            let s = rope.byte_to_char(b);
            (s, s + n)
        })
        .collect()
}

/// Two ranges share text (or are the same empty spot).
fn intersects(a: (usize, usize), b: (usize, usize)) -> bool {
    a == b || (a.0 < b.1 && b.0 < a.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Motion;
    use crate::config::Config;

    fn editor(text: &str) -> Editor {
        let mut ed = Editor::new(Config::default(), &[]);
        ed.doc_mut().rope = Rope::from_str(text);
        ed
    }

    fn text(ed: &Editor) -> String {
        ed.doc().rope.to_string()
    }

    fn ranges(ed: &Editor) -> Vec<(usize, usize)> {
        ed.doc()
            .all_selections()
            .iter()
            .map(|s| s.range())
            .collect()
    }

    #[test]
    fn add_and_remove_occurrences() {
        let mut ed = editor("foo bar foo baz foo");
        ed.execute(Command::AddNextOccurrence); // selects the word
        assert_eq!(ranges(&ed), [(0, 3)]);
        ed.execute(Command::AddNextOccurrence);
        ed.execute(Command::AddNextOccurrence);
        assert_eq!(ranges(&ed), [(0, 3), (8, 11), (16, 19)]);
        assert_eq!(ed.doc().selection.range(), (16, 19)); // newest is primary
        ed.execute(Command::AddNextOccurrence);
        assert_eq!(ed.doc().extra.len(), 2); // nothing left to add
        ed.execute(Command::RemoveLastOccurrence);
        assert_eq!(ranges(&ed), [(0, 3), (8, 11)]);
        assert_eq!(ed.doc().selection.range(), (8, 11));
        ed.execute(Command::Cancel);
        assert!(!ed.multi_cursor());
    }

    #[test]
    fn wraps_from_the_middle() {
        let mut ed = editor("ab ab ab");
        ed.click(4, false, 1);
        ed.execute(Command::AddNextOccurrence); // second "ab"
        ed.execute(Command::AddNextOccurrence); // third
        ed.execute(Command::AddNextOccurrence); // wraps to the first
        assert_eq!(ranges(&ed), [(0, 2), (3, 5), (6, 8)]);
    }

    #[test]
    fn typing_and_deleting_at_every_cursor() {
        let mut ed = editor("foo bar foo baz foo");
        ed.execute(Command::SelectAllOccurrences);
        assert_eq!(ed.doc().extra.len(), 2);
        ed.insert_text("X");
        assert_eq!(text(&ed), "X bar X baz X");
        ed.insert_text("yz");
        ed.execute(Command::Backspace);
        assert_eq!(text(&ed), "Xy bar Xy baz Xy");
        ed.execute(Command::Move(Motion::Home, false));
        assert_eq!(ranges(&ed), [(0, 0)]); // all cursors met at the line start
        ed.execute(Command::Undo);
        assert!(!ed.multi_cursor());
    }

    #[test]
    fn copy_and_paste_one_line_per_cursor() {
        let mut ed = editor("a1\nb2\nc3\n");
        ed.click(1, false, 1);
        // Cursors after the letter on each line.
        ed.doc_mut().extra = vec![Selection::point(4), Selection::point(7)];
        ed.execute(Command::Move(Motion::End, true));
        ed.execute(Command::Copy);
        assert_eq!(ed.get_clipboard().unwrap(), "1\n2\n3");
        ed.execute(Command::Move(Motion::Left, false));
        ed.execute(Command::Paste);
        assert_eq!(text(&ed), "a11\nb22\nc33\n");
        ed.execute(Command::Newline);
        assert_eq!(text(&ed), "a1\n1\nb2\n2\nc3\n3\n");
    }

    #[test]
    fn other_commands_collapse_first() {
        let mut ed = editor("x x x");
        ed.execute(Command::SelectAllOccurrences);
        assert!(ed.multi_cursor());
        ed.execute(Command::DuplicateLine);
        assert!(!ed.multi_cursor());
        assert_eq!(text(&ed), "x x x\nx x x");
    }
}
