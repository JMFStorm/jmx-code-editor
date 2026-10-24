//! JSON (with comments, as in jsconfig.json and .jsonc).

use super::{Lexer, Token, js};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    in_comment: bool,
}

pub fn lex(lx: &mut Lexer, st: &mut State) {
    let end = lx.chars.len();
    while lx.i < end {
        let start = lx.i;
        if st.in_comment {
            let close = lx.find("*/", end);
            lx.i = close.map_or(end, |k| k + 2);
            lx.push(start, lx.i, Token::Comment);
            st.in_comment = close.is_none();
            continue;
        }
        let c = lx.chars[lx.i];
        match c {
            _ if lx.at("//") => {
                lx.push(start, end, Token::Comment);
                return;
            }
            _ if lx.at("/*") => {
                lx.i += 2;
                st.in_comment = true;
                // Re-enter the loop with the span starting at the `/*`.
                let close = lx.find("*/", end);
                lx.i = close.map_or(end, |k| k + 2);
                lx.push(start, lx.i, Token::Comment);
                st.in_comment = close.is_none();
            }
            '"' => {
                js::string(lx, end, '"');
                // A string followed by `:` is a key.
                if lx.next_non_blank(lx.i) == Some(':') {
                    for s in lx.spans.iter_mut().filter(|s| s.start >= start) {
                        if s.token == Token::String {
                            s.token = Token::Property;
                        }
                    }
                }
            }
            '-' | '0'..='9' => js::number(lx, end),
            c if c.is_ascii_alphabetic() => {
                lx.eat_while(end, |c| c.is_ascii_alphanumeric());
                lx.push(start, lx.i, Token::Literal);
            }
            _ => lx.i += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Lang;
    use super::super::test_util::{token_of, tokens};
    use super::*;

    #[test]
    fn keys_values_and_comments() {
        let src = "{\n  \"name\": \"jmx\", \"n\": -1.5e3,\n  \"ok\": true, // note\n  /* a\n b */ \"x\": null\n}";
        assert_eq!(token_of(Lang::Json, src, "\"name\""), Some(Token::Property));
        assert_eq!(token_of(Lang::Json, src, "\"jmx\""), Some(Token::String));
        assert_eq!(token_of(Lang::Json, src, "-1.5e3"), Some(Token::Number));
        assert_eq!(token_of(Lang::Json, src, "true"), Some(Token::Literal));
        assert_eq!(token_of(Lang::Json, src, "// note"), Some(Token::Comment));
        let t = tokens(Lang::Json, src);
        assert_eq!(t[4][0], (" b */".into(), Token::Comment));
        assert_eq!(t[4][1], ("\"x\"".into(), Token::Property));
    }
}
