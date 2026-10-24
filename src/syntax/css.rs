//! CSS lexer: selectors outside blocks, `property: value` inside them.

use super::{Lexer, Token, js};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    in_comment: bool,
    /// `{` nesting depth.
    depth: u16,
    /// After `property:` and before `;` or `}`.
    in_value: bool,
}

fn name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

/// Lex up to `end` (embedding stops at `</style>`).
pub fn lex(lx: &mut Lexer, st: &mut State, end: usize) {
    while lx.i < end {
        let start = lx.i;
        if st.in_comment {
            let close = lx.find("*/", end);
            lx.i = close.map_or(end, |k| k + 2);
            lx.push(start, lx.i, Token::Comment);
            st.in_comment = close.is_none();
            continue;
        }
        if lx.at("/*") {
            lx.i += 2;
            st.in_comment = true;
            let close = lx.find("*/", end);
            lx.i = close.map_or(end, |k| k + 2);
            lx.push(start, lx.i, Token::Comment);
            st.in_comment = close.is_none();
            continue;
        }
        let c = lx.chars[lx.i];
        match c {
            '"' | '\'' => js::string(lx, end, c),
            '{' => {
                st.depth += 1;
                st.in_value = false;
                lx.i += 1;
            }
            '}' => {
                st.depth = st.depth.saturating_sub(1);
                st.in_value = false;
                lx.i += 1;
            }
            ';' => {
                st.in_value = false;
                lx.i += 1;
            }
            '@' => {
                lx.i += 1;
                lx.eat_while(end, name_char);
                lx.push(start, lx.i, Token::Keyword);
            }
            '!' if st.in_value => {
                lx.i += 1;
                lx.eat_while(end, name_char);
                lx.push(start, lx.i, Token::Keyword);
            }
            '#' if st.in_value => {
                lx.i += 1;
                lx.eat_while(end, |c| c.is_ascii_hexdigit());
                lx.push(start, lx.i, Token::Number);
            }
            '#' | '.' if !st.in_value && lx.peek_at(1).is_some_and(name_char) => {
                lx.i += 1;
                lx.eat_while(end, name_char);
                lx.push(start, lx.i, Token::Type);
            }
            ':' if !st.in_value && (st.depth == 0 || selector_ahead(lx, end)) => {
                // Pseudo-class or pseudo-element in a selector.
                lx.i += 1;
                if lx.peek() == Some(':') {
                    lx.i += 1;
                }
                lx.eat_while(end, name_char);
                lx.push(start, lx.i, Token::Keyword);
            }
            ':' => {
                st.in_value = true;
                lx.i += 1;
            }
            '0'..='9' => {
                lx.eat_while(end, |c| c.is_ascii_digit() || c == '.');
                lx.eat_while(end, |c| c.is_ascii_alphabetic() || c == '%');
                lx.push(start, lx.i, Token::Number);
            }
            '-' | '.' if st.in_value && lx.peek_at(1).is_some_and(|d| d.is_ascii_digit()) => {
                lx.i += 1;
                lx.eat_while(end, |c| c.is_ascii_digit() || c == '.');
                lx.eat_while(end, |c| c.is_ascii_alphabetic() || c == '%');
                lx.push(start, lx.i, Token::Number);
            }
            c if name_char(c) => {
                lx.eat_while(end, name_char);
                let token = if st.in_value {
                    (lx.peek() == Some('(')).then_some(Token::Function)
                } else if st.depth > 0
                    && lx.next_non_blank(lx.i) == Some(':')
                    && !selector_ahead(lx, end)
                {
                    Some(Token::Property)
                } else {
                    Some(Token::Tag)
                };
                if let Some(t) = token {
                    lx.push(start, lx.i, t);
                }
            }
            _ => lx.i += 1,
        }
    }
}

/// Inside a block, `a:hover {` is a nested selector rather than a property: a `{` comes before
/// any `;` on the rest of the line.
fn selector_ahead(lx: &Lexer, end: usize) -> bool {
    lx.chars[lx.i..end]
        .iter()
        .find(|c| matches!(c, '{' | ';' | '}'))
        .is_some_and(|c| *c == '{')
}

#[cfg(test)]
mod tests {
    use super::super::Lang;
    use super::super::test_util::{token_of, tokens};
    use super::*;

    const L: Lang = Lang::Css;

    #[test]
    fn selectors_properties_values() {
        let src = "/* card */\n.card > h1:hover, #main {\n  color: #fff;\n  margin: -4px 1.5em !important;\n  background: url(\"a.png\");\n}\n@media (max-width: 600px) { a { top: 0 } }";
        assert_eq!(token_of(L, src, "/* card */"), Some(Token::Comment));
        assert_eq!(token_of(L, src, ".card"), Some(Token::Type));
        assert_eq!(token_of(L, src, "h1"), Some(Token::Tag));
        assert_eq!(token_of(L, src, ":hover"), Some(Token::Keyword));
        assert_eq!(token_of(L, src, "#main"), Some(Token::Type));
        assert_eq!(token_of(L, src, "color"), Some(Token::Property));
        assert_eq!(token_of(L, src, "#fff"), Some(Token::Number));
        assert_eq!(token_of(L, src, "-4px"), Some(Token::Number));
        assert_eq!(token_of(L, src, "!important"), Some(Token::Keyword));
        assert_eq!(token_of(L, src, "url"), Some(Token::Function));
        assert_eq!(token_of(L, src, "\"a.png\""), Some(Token::String));
        assert_eq!(token_of(L, src, "@media"), Some(Token::Keyword));
        assert_eq!(token_of(L, src, "top"), Some(Token::Property));
        let t = tokens(L, "a {\n  b: c;\n}");
        assert_eq!(t[1][0], ("b".into(), Token::Property));
    }
}
