//! Cursor motion over the rope. All positions are char indices.
//! Lines are LF-only (ropey is built without CR/unicode line breaks, files are
//! normalized to `\n` on load).

use ropey::{Rope, RopeSlice};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Graphemes longer than this many chars are treated as split; keeps cost bounded on huge lines.
const GRAPHEME_WINDOW: usize = 64;

/// Char index just past the line's content (before its `\n`, if any).
pub fn line_content_end(rope: &Rope, line: usize) -> usize {
    if line + 1 < rope.len_lines() {
        rope.line_to_char(line + 1) - 1
    } else {
        rope.len_chars()
    }
}

pub fn prev_grapheme(rope: &Rope, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let ls = rope.line_to_char(rope.char_to_line(pos));
    if pos == ls {
        return pos - 1; // step back over the newline
    }
    let start = pos.saturating_sub(GRAPHEME_WINDOW).max(ls);
    let s = rope.slice(start..pos).to_string();
    match s.grapheme_indices(true).next_back() {
        Some((bi, _)) => start + s[..bi].chars().count(),
        None => pos - 1,
    }
}

pub fn next_grapheme(rope: &Rope, pos: usize) -> usize {
    let len = rope.len_chars();
    if pos >= len {
        return len;
    }
    if rope.char(pos) == '\n' {
        return pos + 1;
    }
    let end = (pos + GRAPHEME_WINDOW).min(line_content_end(rope, rope.char_to_line(pos)));
    let s = rope.slice(pos..end).to_string();
    match s.graphemes(true).next() {
        Some(g) => pos + g.chars().count(),
        None => pos + 1,
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Class {
    Word,
    Punct,
    Space,
    Newline,
}

fn class(c: char) -> Class {
    if c == '\n' {
        Class::Newline
    } else if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// Ctrl+Right: skip spaces, then to the end of the next word/punctuation run.
pub fn next_word_end(rope: &Rope, pos: usize) -> usize {
    let len = rope.len_chars();
    let mut p = pos;
    if p >= len {
        return len;
    }
    if rope.char(p) == '\n' {
        return p + 1;
    }
    while p < len && class(rope.char(p)) == Class::Space {
        p += 1;
    }
    if p >= len || rope.char(p) == '\n' {
        return p;
    }
    let c = class(rope.char(p));
    while p < len && class(rope.char(p)) == c {
        p += 1;
    }
    p
}

/// Ctrl+Left: skip spaces backwards, then to the start of the previous word/punctuation run.
pub fn prev_word_start(rope: &Rope, pos: usize) -> usize {
    let mut p = pos.min(rope.len_chars());
    if p == 0 {
        return 0;
    }
    if rope.char(p - 1) == '\n' {
        return p - 1;
    }
    while p > 0 && class(rope.char(p - 1)) == Class::Space {
        p -= 1;
    }
    if p == 0 || rope.char(p - 1) == '\n' {
        return p;
    }
    let c = class(rope.char(p - 1));
    while p > 0 && class(rope.char(p - 1)) == c {
        p -= 1;
    }
    p
}

/// Range of the word (or punctuation/space run) under `pos`, for double-click.
pub fn word_at(rope: &Rope, pos: usize) -> (usize, usize) {
    let len = rope.len_chars();
    if len == 0 {
        return (0, 0);
    }
    let probe = if pos < len && rope.char(pos) != '\n' {
        pos
    } else if pos > 0 && rope.char(pos - 1) != '\n' {
        pos - 1
    } else {
        return (pos, pos);
    };
    let c = class(rope.char(probe));
    let mut s = probe;
    while s > 0 && class(rope.char(s - 1)) == c {
        s -= 1;
    }
    let mut e = probe + 1;
    while e < len && class(rope.char(e)) == c {
        e += 1;
    }
    (s, e)
}

/// Home: toggle between first non-blank char and column 0.
pub fn smart_home(rope: &Rope, pos: usize) -> usize {
    let line = rope.char_to_line(pos);
    let ls = rope.line_to_char(line);
    let le = line_content_end(rope, line);
    let mut first = ls;
    while first < le && matches!(rope.char(first), ' ' | '\t') {
        first += 1;
    }
    if pos == first { ls } else { first }
}

/// Display width of `c` when it starts at display column `col`.
pub fn char_width(c: char, col: usize, tab_width: usize) -> usize {
    match c {
        '\t' => tab_width - col % tab_width,
        c if c.is_control() => 1,
        c => c.width().unwrap_or(1),
    }
}

/// Display column of `pos` within its line (tabs expanded, wide chars count 2).
pub fn col_of(rope: &Rope, pos: usize, tab_width: usize) -> usize {
    let line = rope.char_to_line(pos);
    let ls = rope.line_to_char(line);
    width_of(rope.slice(ls..pos), 0, tab_width)
}

/// Display width of a slice that starts at display column `start_col`.
pub fn width_of(slice: RopeSlice, start_col: usize, tab_width: usize) -> usize {
    let mut col = start_col;
    for c in slice.chars() {
        col += char_width(c, col, tab_width);
    }
    col - start_col
}

/// Char index in `line` at display column `target` (or the line end if shorter).
pub fn pos_at_col(rope: &Rope, line: usize, target: usize, tab_width: usize) -> usize {
    let ls = rope.line_to_char(line);
    let le = line_content_end(rope, line);
    let mut col = 0;
    let mut pos = ls;
    for c in rope.slice(ls..le).chars() {
        let w = char_width(c, col, tab_width);
        if col + w > target {
            // Land on whichever side of the char is closer.
            return if target - col > w / 2 { pos + 1 } else { pos };
        }
        col += w;
        pos += 1;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphemes_and_newlines() {
        let r = Rope::from_str("ae\u{301}x\nb");
        assert_eq!(next_grapheme(&r, 1), 3); // e + combining acute
        assert_eq!(prev_grapheme(&r, 3), 1);
        assert_eq!(next_grapheme(&r, 4), 5); // over newline
        assert_eq!(prev_grapheme(&r, 5), 4);
        assert_eq!(next_grapheme(&r, 6), 6);
        assert_eq!(prev_grapheme(&r, 0), 0);
    }

    #[test]
    fn words() {
        let r = Rope::from_str("foo.bar  baz\nqux");
        assert_eq!(next_word_end(&r, 0), 3);
        assert_eq!(next_word_end(&r, 3), 4);
        assert_eq!(next_word_end(&r, 7), 12);
        assert_eq!(next_word_end(&r, 12), 13);
        assert_eq!(prev_word_start(&r, 12), 9);
        assert_eq!(prev_word_start(&r, 9), 4);
        assert_eq!(prev_word_start(&r, 13), 12);
        assert_eq!(word_at(&r, 5), (4, 7));
        assert_eq!(word_at(&r, 12), (9, 12));
    }

    #[test]
    fn columns() {
        let r = Rope::from_str("\tab\n日本x");
        assert_eq!(col_of(&r, 1, 4), 4);
        assert_eq!(col_of(&r, 3, 4), 6);
        assert_eq!(pos_at_col(&r, 0, 5, 4), 2);
        assert_eq!(pos_at_col(&r, 0, 99, 4), 3);
        assert_eq!(col_of(&r, 6, 4), 4); // two wide chars
        assert_eq!(pos_at_col(&r, 1, 2, 4), 5);
        assert_eq!(pos_at_col(&r, 1, 1, 4), 4); // middle of wide char rounds left
    }

    #[test]
    fn home() {
        let r = Rope::from_str("    let x;");
        assert_eq!(smart_home(&r, 8), 4);
        assert_eq!(smart_home(&r, 4), 0);
        assert_eq!(smart_home(&r, 0), 4);
    }
}
