//! Plain-text, line-based search. Smart case: case-insensitive unless the query has uppercase.

use std::borrow::Cow;

use ropey::Rope;

pub struct Query {
    chars: Vec<char>,
    case_insensitive: bool,
}

impl Query {
    pub fn new(text: &str) -> Option<Self> {
        if text.is_empty() || text.contains('\n') {
            return None;
        }
        let case_insensitive = !text.chars().any(char::is_uppercase);
        Some(Self {
            chars: text.chars().map(|c| fold(c, case_insensitive)).collect(),
            case_insensitive,
        })
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    /// Char offsets of every (non-overlapping) match in `line`.
    pub fn matches_in(&self, line: &str) -> Vec<usize> {
        let hay: Vec<char> = line
            .chars()
            .map(|c| fold(c, self.case_insensitive))
            .collect();
        let n = self.chars.len();
        let mut out = Vec::new();
        let mut i = 0;
        while i + n <= hay.len() {
            if hay[i..i + n] == self.chars[..] {
                out.push(i);
                i += n;
            } else {
                i += 1;
            }
        }
        out
    }

    /// Next match starting at or after `from` (forward) or before `from` (backward), wrapping.
    /// Returns `(start, end)` char indices.
    pub fn find(&self, rope: &Rope, from: usize, forward: bool) -> Option<(usize, usize)> {
        let lines = rope.len_lines();
        let from = from.min(rope.len_chars());
        let start_line = rope.char_to_line(from);
        let off = from - rope.line_to_char(start_line);
        let n = self.len();

        let line_matches = |l: usize| -> Vec<usize> {
            let text: Cow<str> = rope.line(l).into();
            self.matches_in(&text)
        };
        let hit = |l: usize, m: usize| {
            let s = rope.line_to_char(l) + m;
            (s, s + n)
        };

        if forward {
            if let Some(&m) = line_matches(start_line).iter().find(|&&m| m >= off) {
                return Some(hit(start_line, m));
            }
            for k in 1..=lines {
                let l = (start_line + k) % lines;
                let ms = line_matches(l);
                let found = if l == start_line {
                    ms.into_iter().find(|&m| m < off)
                } else {
                    ms.into_iter().next()
                };
                if let Some(m) = found {
                    return Some(hit(l, m));
                }
            }
        } else {
            if let Some(&m) = line_matches(start_line).iter().rev().find(|&&m| m < off) {
                return Some(hit(start_line, m));
            }
            for k in 1..=lines {
                let l = (start_line + lines - k) % lines;
                let ms = line_matches(l);
                let found = if l == start_line {
                    ms.into_iter().rev().find(|&m| m >= off)
                } else {
                    ms.into_iter().next_back()
                };
                if let Some(m) = found {
                    return Some(hit(l, m));
                }
            }
        }
        None
    }
}

/// Case folding that keeps a 1:1 char mapping so offsets stay valid.
fn fold(c: char, ci: bool) -> char {
    if ci {
        c.to_lowercase().next().unwrap_or(c)
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_case_and_wrap() {
        let r = Rope::from_str("Foo foo\nbar FOO\n");
        let q = Query::new("foo").unwrap();
        assert_eq!(q.find(&r, 0, true), Some((0, 3)));
        assert_eq!(q.find(&r, 1, true), Some((4, 7)));
        assert_eq!(q.find(&r, 5, true), Some((12, 15)));
        assert_eq!(q.find(&r, 13, true), Some((0, 3)));
        assert_eq!(q.find(&r, 0, false), Some((12, 15)));
        assert_eq!(q.find(&r, 4, false), Some((0, 3)));
        let q = Query::new("FOO").unwrap();
        assert_eq!(q.find(&r, 0, true), Some((12, 15)));
        assert!(Query::new("zzz").unwrap().find(&r, 0, true).is_none());
    }
}
