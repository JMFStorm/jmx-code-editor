//! Coloring of the Problems page: the counts line, file headers and one row per problem
//! (`  ✖ 12:5  rule  · ESLint`), with the selected problem's message in a box (`│ text`).

use super::{Lexer, Token};

pub fn lex(lx: &mut Lexer) {
    let chars = lx.chars;
    let end = chars.len();
    if end == 0 {
        return;
    }
    let first = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(end);
    match chars.get(first) {
        // A problem row.
        Some(&mark @ ('✖' | '⚠' | 'ℹ')) if first > 0 => {
            let token = match mark {
                '✖' => Token::Error,
                '⚠' => Token::Warning,
                _ => Token::Function,
            };
            lx.push(first, first + 1, token);
            // `line:col`
            let pos_start = first + 2;
            let pos_end = (pos_start..end)
                .find(|&k| chars[k].is_whitespace())
                .unwrap_or(end);
            // `line:col` in the severity's color, like the icon.
            lx.push(pos_start, pos_end, token);
            // Trailing `· source`.
            if let Some(dot) = (pos_end..end).rev().find(|&k| chars[k] == '·') {
                lx.push(dot, end, Token::Comment);
            }
        }
        // A message box line: the bar dimmed, the text plain.
        Some('│') => lx.push(first, first + 1, Token::Comment),
        _ if chars[..end].starts_with(&['P', 'r', 'o', 'b', 'l', 'e', 'm', 's', ':']) => {
            lx.push(0, end, Token::Keyword);
        }
        // The filter line: `Show:  [✖ Errors 1]  [· Warnings 66]  …` (· = hidden).
        _ if chars[..end].starts_with(&['S', 'h', 'o', 'w', ':']) => {
            lx.push(0, end, Token::Comment);
            let mut k = 0;
            while k < end {
                if chars[k] == '['
                    && let Some(close) = (k..end).find(|&c| chars[c] == ']')
                {
                    let token = match chars.get(k + 1) {
                        Some('✖') => Token::Error,
                        Some('⚠') => Token::Warning,
                        Some('ℹ') => Token::Function,
                        _ => Token::Comment,
                    };
                    lx.push(k, close + 1, token);
                    k = close + 1;
                } else {
                    k += 1;
                }
            }
        }
        _ if chars.contains(&'·') || chars.starts_with(&['N', 'o', ' ']) => {
            lx.push(0, end, Token::Comment);
        }
        // A file header: `src/App.vue  (2)`.
        _ if first == 0 => lx.push(0, end, Token::Function),
        _ => {}
    }
    lx.i = end;
}

#[cfg(test)]
mod tests {
    use super::super::test_util::tokens;
    use super::super::{Lang, Token};

    #[test]
    fn rows_headers_and_codes() {
        let t = tokens(
            Lang::Problems,
            "Problems: 1 error, 1 warning\nESLint · F4 next\n\nsrc/a.js  (2)\n  ✖ 3:7  no-unused-vars  · ESLint\n  ⚠ 9:1  hmm  · Roslyn\n    │ 'x' · unused",
        );
        assert_eq!(t[0][0].1, Token::Keyword);
        assert_eq!(t[1][0].1, Token::Comment);
        assert_eq!(t[3][0], ("src/a.js  (2)".into(), Token::Function));
        assert_eq!(t[4][0], ("✖".into(), Token::Error));
        assert_eq!(t[4][1], ("3:7".into(), Token::Error));
        assert_eq!(t[5][1], ("9:1".into(), Token::Warning));
        assert_eq!(t[4][2], ("· ESLint".into(), Token::Comment));
        assert_eq!(t[5][0], ("⚠".into(), Token::Warning));
        // Box lines: only the bar is colored.
        assert_eq!(t[6], vec![("│".into(), Token::Comment)]);
    }
}
