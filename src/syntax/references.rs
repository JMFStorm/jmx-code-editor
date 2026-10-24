//! Coloring of the References page: the title, file headers, and one row per reference
//! (`  • 12:5  code`, `◆` for the declaration).

use super::{Lexer, Token};

pub fn lex(lx: &mut Lexer) {
    let chars = lx.chars;
    let end = chars.len();
    if end == 0 {
        return;
    }
    let first = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(end);
    match chars[first] {
        '\'' if first == 0 => lx.push(0, end, Token::Keyword),
        '·' => lx.push(0, end, Token::Comment),
        '◆' | '•' if first == 2 => {
            let mark = if chars[2] == '◆' {
                Token::Keyword
            } else {
                Token::Comment
            };
            lx.push(2, 3, mark);
            let pos_end = (4..end).find(|&k| chars[k] == ' ').unwrap_or(end);
            lx.push(4, pos_end, Token::Number);
        }
        // A file header: `src/utils/format.js  (3)`.
        _ if first == 0 && !chars.starts_with(&['N', 'o', ' ']) => lx.push(0, end, Token::Function),
        _ => {}
    }
    lx.i = end;
}

#[cfg(test)]
mod tests {
    use super::super::test_util::tokens;
    use super::super::{Lang, Token};

    #[test]
    fn title_headers_and_rows() {
        let t = tokens(
            Lang::References,
            "'save' — 2 references in 1 file\n· Enter open\n\nsrc/a.js  (2)\n  ◆ 3:7  save(x) {}\n  • 9:1  store.save(1);",
        );
        assert_eq!(t[0][0].1, Token::Keyword);
        assert_eq!(t[1][0].1, Token::Comment);
        assert_eq!(t[3][0], ("src/a.js  (2)".into(), Token::Function));
        assert_eq!(t[4][0], ("◆".into(), Token::Keyword));
        assert_eq!(t[4][1], ("3:7".into(), Token::Number));
        assert_eq!(t[5][0], ("•".into(), Token::Comment));
    }
}
