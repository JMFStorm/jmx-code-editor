//! Line-based search shared by Find (Ctrl+F) and Find in Files: literal or regex, with case and
//! whole-word options.

use std::borrow::Cow;

use regex::{Regex, RegexBuilder};
use ropey::Rope;
use serde::{Deserialize, Serialize};

/// Search toggles (Alt+C, Alt+O, Alt+R), shared by every search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

#[derive(Debug, Clone)]
pub struct Query {
    re: Regex,
}

impl Query {
    /// `Ok(None)` for an empty query (or one with a newline), `Err` for an invalid regex.
    pub fn new(text: &str, opts: SearchOptions) -> Result<Option<Self>, String> {
        if text.is_empty() || text.contains('\n') {
            return Ok(None);
        }
        let mut pattern = if opts.regex {
            text.to_string()
        } else {
            regex::escape(text)
        };
        if opts.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        RegexBuilder::new(&pattern)
            .case_insensitive(!opts.case_sensitive)
            .size_limit(1 << 22)
            .build()
            .map(|re| Some(Self { re }))
            .map_err(|e| match e {
                regex::Error::Syntax(s) => {
                    // The last line of the syntax error is the useful part.
                    let msg = s.lines().last().unwrap_or("").trim();
                    format!("Invalid regex: {}", msg.trim_start_matches("error: "))
                }
                other => format!("Invalid regex: {other}"),
            })
    }

    /// Byte ranges of every non-empty match in `line`.
    pub fn byte_matches<'a>(&'a self, line: &'a str) -> impl Iterator<Item = (usize, usize)> + 'a {
        self.re
            .find_iter(line)
            .filter(|m| !m.is_empty())
            .map(|m| (m.start(), m.end()))
    }

    /// Char ranges of every non-empty match in `line`.
    pub fn matches_in(&self, line: &str) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        // Byte -> char offsets, walking the line once.
        let mut chars = 0;
        let mut byte = 0;
        let mut to_char = |b: usize| {
            chars += line[byte..b].chars().count();
            byte = b;
            chars
        };
        for (s, e) in self.byte_matches(line) {
            let cs = to_char(s);
            let ce = to_char(e);
            out.push((cs, ce));
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

        let line_matches = |l: usize| -> Vec<(usize, usize)> {
            let text: Cow<str> = rope.line(l).into();
            self.matches_in(text.trim_end_matches(['\n', '\r']))
        };
        let hit = |l: usize, (s, e): (usize, usize)| {
            let base = rope.line_to_char(l);
            (base + s, base + e)
        };

        if forward {
            if let Some(&m) = line_matches(start_line).iter().find(|m| m.0 >= off) {
                return Some(hit(start_line, m));
            }
            for k in 1..=lines {
                let l = (start_line + k) % lines;
                let ms = line_matches(l);
                let found = if l == start_line {
                    ms.into_iter().find(|m| m.0 < off)
                } else {
                    ms.into_iter().next()
                };
                if let Some(m) = found {
                    return Some(hit(l, m));
                }
            }
        } else {
            if let Some(&m) = line_matches(start_line).iter().rev().find(|m| m.0 < off) {
                return Some(hit(start_line, m));
            }
            for k in 1..=lines {
                let l = (start_line + lines - k) % lines;
                let ms = line_matches(l);
                let found = if l == start_line {
                    ms.into_iter().rev().find(|m| m.0 >= off)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str, case_sensitive: bool, whole_word: bool, regex: bool) -> Query {
        let opts = SearchOptions {
            case_sensitive,
            whole_word,
            regex,
        };
        Query::new(text, opts).unwrap().unwrap()
    }

    #[test]
    fn case_and_wrap() {
        let r = Rope::from_str("Foo foo\nbar FOO\n");
        let ci = q("foo", false, false, false);
        assert_eq!(ci.find(&r, 0, true), Some((0, 3)));
        assert_eq!(ci.find(&r, 1, true), Some((4, 7)));
        assert_eq!(ci.find(&r, 5, true), Some((12, 15)));
        assert_eq!(ci.find(&r, 13, true), Some((0, 3)));
        assert_eq!(ci.find(&r, 0, false), Some((12, 15)));
        assert_eq!(ci.find(&r, 4, false), Some((0, 3)));
        let cs = q("FOO", true, false, false);
        assert_eq!(cs.find(&r, 0, true), Some((12, 15)));
        assert!(q("zzz", false, false, false).find(&r, 0, true).is_none());
    }

    #[test]
    fn whole_word_regex_and_errors() {
        assert_eq!(
            q("cat", false, true, false).matches_in("cat concat cat_x cat"),
            [(0, 3), (17, 20)]
        );
        // Literal mode escapes regex characters.
        assert_eq!(
            q("a.c", false, false, false).matches_in("abc a.c"),
            [(4, 7)]
        );
        assert_eq!(
            q(r"\d+", false, false, true).matches_in("a1 b22"),
            [(1, 2), (4, 6)]
        );
        assert!(
            Query::new(
                "(",
                SearchOptions {
                    regex: true,
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(Query::new("", SearchOptions::default()).unwrap().is_none());
        // Zero-length matches are skipped.
        assert!(q("x*", false, false, true).matches_in("abc").is_empty());
    }

    #[test]
    fn char_offsets_with_multibyte_text() {
        assert_eq!(
            q("öl", false, false, false).matches_in("Äöl ÖL"),
            [(1, 3), (4, 6)]
        );
        let r = Rope::from_str("日本 x\nxx 日本\n");
        assert_eq!(
            q("日本", false, false, false).find(&r, 1, true),
            Some((8, 10))
        );
    }
}
