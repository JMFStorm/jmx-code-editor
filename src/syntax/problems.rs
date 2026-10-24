//! Coloring of the Problems page: the counts line, file headers and one row per problem
//! (`  ✖ 12:5  message  [rule]`).

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
                _ => Token::Comment,
            };
            lx.push(first, first + 1, token);
            // `line:col`
            let pos_start = first + 2;
            let pos_end = (pos_start..end)
                .find(|&k| chars[k].is_whitespace())
                .unwrap_or(end);
            lx.push(pos_start, pos_end, Token::Number);
            // Trailing `[rule]`.
            if chars[end - 1] == ']'
                && let Some(open) = (pos_end..end).rev().find(|&k| chars[k] == '[')
            {
                lx.push(open, end, Token::Comment);
            }
        }
        _ if chars[..end].starts_with(&['P', 'r', 'o', 'b', 'l', 'e', 'm', 's', ':']) => {
            lx.push(0, end, Token::Keyword);
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
            "Problems: 1 error, 1 warning\nESLint · F4 next\n\nsrc/a.js  (2)\n  ✖ 3:7  'x' unused  [no-unused-vars]\n  ⚠ 9:1  hmm",
        );
        assert_eq!(t[0][0].1, Token::Keyword);
        assert_eq!(t[1][0].1, Token::Comment);
        assert_eq!(t[3][0], ("src/a.js  (2)".into(), Token::Function));
        assert_eq!(t[4][0], ("✖".into(), Token::Error));
        assert_eq!(t[4][1], ("3:7".into(), Token::Number));
        assert_eq!(t[4][2], ("[no-unused-vars]".into(), Token::Comment));
        assert_eq!(t[5][0], ("⚠".into(), Token::Warning));
    }
}
