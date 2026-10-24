//! Coloring of the Docs page: the API's full name, section titles, the signature, parameter
//! names and the rows that open other APIs (`▸`).

use super::{Lexer, Token};

const SECTIONS: &[&str] = &[
    "Type parameters",
    "Parameters",
    "Returns",
    "Exceptions",
    "Remarks",
    "Overloads",
    "Members",
    "Other matches for this name",
];

pub fn lex(lx: &mut Lexer) {
    let chars = lx.chars;
    let end = chars.len();
    if end == 0 {
        return;
    }
    let text: String = chars.iter().collect();
    let first = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(end);
    // `name — text`: the part before the dash.
    let dash = text.find(" — ").map(|b| text[..b].chars().count());
    if chars.get(first) == Some(&'│') {
        // A box line: its bar.
        lx.push(first, first + 1, Token::Comment);
    } else if chars[0] == '·' {
        lx.push(0, end, Token::Comment);
    } else if SECTIONS.iter().any(|s| text.starts_with(s)) {
        lx.push(0, end, Token::Type);
    } else if first == 0 && !text.contains(' ') && text.contains('.') {
        // The full name.
        lx.push(0, end, Token::Keyword);
    } else if first == 2 && matches!(chars[2], '▸' | '●') {
        lx.push(
            2,
            3,
            if chars[2] == '●' {
                Token::Running
            } else {
                Token::Comment
            },
        );
        lx.push(4, dash.unwrap_or(end), Token::Function);
    } else if first == 2 {
        match dash {
            // A parameter / exception: its name.
            Some(d) => lx.push(2, d, Token::Property),
            // The signature.
            None => lx.push(2, end, Token::Function),
        }
    }
    lx.i = end;
}

#[cfg(test)]
mod tests {
    use super::super::test_util::tokens;
    use super::super::{Lang, Token};

    #[test]
    fn titles_signature_and_rows() {
        let t = tokens(
            Lang::Docs,
            "System.String.Join\n· .NET API docs\n\n  String.Join(string separator)\n\nParameters\n  separator — The text.\n  ▸ String.Join(char c)",
        );
        assert_eq!(t[0][0].1, Token::Keyword);
        assert_eq!(t[1][0].1, Token::Comment);
        assert_eq!(
            t[3][0],
            ("String.Join(string separator)".into(), Token::Function)
        );
        assert_eq!(t[5][0].1, Token::Type);
        assert_eq!(t[6][0], ("separator".into(), Token::Property));
        assert_eq!(t[7][0], ("▸".into(), Token::Comment));
        assert_eq!(t[7][1], ("String.Join(char c)".into(), Token::Function));
    }
}
