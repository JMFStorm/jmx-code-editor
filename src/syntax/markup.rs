//! XML, HTML and Vue single-file components: tags and attributes, with JavaScript inside
//! `<script>`, CSS inside `<style>`, and (Vue) JavaScript in `{{ }}` and directive values.

use super::{Lang, Lexer, Token, css, js};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Embed {
    None,
    Script,
    Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Text,
    /// `<!-- ... -->`
    Comment,
    /// `<![CDATA[ ... ]]>`
    CData,
    /// `<!DOCTYPE ...>` or `<?xml ...?>`
    Decl,
    /// Inside a start or end tag, possibly within a quoted attribute value.
    Tag {
        embed: Embed,
        quote: Option<char>,
    },
    Script,
    Style,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    mode: Mode,
}

fn name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')
}

fn attr_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.' | '@' | '#' | '[' | ']' | '$')
}

pub fn lex(lx: &mut Lexer, st: &mut super::State, lang: Lang) {
    let end = lx.chars.len();
    while lx.i < end {
        let start = lx.i;
        match st.markup.mode {
            Mode::Comment => close_with(lx, st, "-->", Token::Comment),
            Mode::CData => close_with(lx, st, "]]>", Token::String),
            Mode::Decl => close_with(lx, st, ">", Token::Keyword),
            Mode::Script => {
                let stop = lx.find_ci("</script", end);
                js::lex(lx, &mut st.js, stop.unwrap_or(end));
                if let Some(k) = stop {
                    lx.i = k;
                    st.markup.mode = Mode::Text;
                }
            }
            Mode::Style => {
                let stop = lx.find_ci("</style", end);
                css::lex(lx, &mut st.css, stop.unwrap_or(end));
                if let Some(k) = stop {
                    lx.i = k;
                    st.markup.mode = Mode::Text;
                }
            }
            Mode::Tag { embed, quote } => tag(lx, st, embed, quote),
            Mode::Text => {
                if lx.at("<!--") {
                    lx.i += 4;
                    st.markup.mode = Mode::Comment;
                    close_with(lx, st, "-->", Token::Comment);
                    extend_last(lx, start);
                } else if lx.at("<![CDATA[") {
                    lx.i += 9;
                    st.markup.mode = Mode::CData;
                    close_with(lx, st, "]]>", Token::String);
                    extend_last(lx, start);
                } else if lx.at("<!") || lx.at("<?") {
                    lx.i += 2;
                    st.markup.mode = Mode::Decl;
                    close_with(lx, st, ">", Token::Keyword);
                    extend_last(lx, start);
                } else if lx.at("</") && lx.peek_at(2).is_some_and(name_start) {
                    lx.push(start, start + 2, Token::Punct);
                    lx.i += 2;
                    let s = lx.i;
                    lx.eat_while(end, name_char);
                    lx.push(s, lx.i, Token::Tag);
                    st.markup.mode = Mode::Tag {
                        embed: Embed::None,
                        quote: None,
                    };
                } else if lx.at("<") && lx.peek_at(1).is_some_and(name_start) {
                    lx.push(start, start + 1, Token::Punct);
                    lx.i += 1;
                    let s = lx.i;
                    lx.eat_while(end, name_char);
                    lx.push(s, lx.i, Token::Tag);
                    let name: String = lx.chars[s..lx.i].iter().collect();
                    let embed = match name.to_ascii_lowercase().as_str() {
                        _ if lang == Lang::Xml => Embed::None,
                        "script" => Embed::Script,
                        "style" => Embed::Style,
                        _ => Embed::None,
                    };
                    st.markup.mode = Mode::Tag { embed, quote: None };
                } else if lx.at("&") {
                    lx.i += 1;
                    lx.eat_while(end, |c| c.is_alphanumeric() || c == '#');
                    if lx.peek() == Some(';') {
                        lx.i += 1;
                        lx.push(start, lx.i, Token::Entity);
                    }
                } else if lang != Lang::Xml && lx.at("{{") {
                    interpolation(lx);
                } else {
                    lx.i += 1;
                }
            }
        }
    }
}

/// Inside a construct that ends with `close`: color up to and including it.
fn close_with(lx: &mut Lexer, st: &mut super::State, close: &str, token: Token) {
    let start = lx.i;
    let end = lx.chars.len();
    match lx.find(close, end) {
        Some(k) => {
            lx.i = k + close.chars().count();
            st.markup.mode = Mode::Text;
        }
        None => lx.i = end,
    }
    lx.push(start, lx.i, token);
}

/// Stretch the last span back to `start` (to include an opening `<!--` etc.).
fn extend_last(lx: &mut Lexer, start: usize) {
    match lx.spans.last_mut() {
        Some(s) if s.start >= start => s.start = start,
        _ => {}
    }
}

/// Vue / template `{{ expression }}`.
fn interpolation(lx: &mut Lexer) {
    let start = lx.i;
    let end = lx.chars.len();
    lx.push(start, start + 2, Token::Punct);
    lx.i += 2;
    let close = lx.find("}}", end);
    let stop = close.unwrap_or(end);
    js::lex(lx, &mut js::State::default(), stop);
    if let Some(k) = close {
        lx.push(k, k + 2, Token::Punct);
        lx.i = k + 2;
    }
}

fn tag(lx: &mut Lexer, st: &mut super::State, embed: Embed, quote: Option<char>) {
    let end = lx.chars.len();
    if let Some(q) = quote {
        // A quoted value continued from an earlier line.
        let start = lx.i;
        match lx.find(&q.to_string(), end) {
            Some(k) => {
                lx.i = k + 1;
                st.markup.mode = Mode::Tag { embed, quote: None };
            }
            None => lx.i = end,
        }
        lx.push(start, lx.i, Token::String);
        return;
    }
    let mut expr_attr = false;
    while lx.i < end {
        let start = lx.i;
        let c = lx.chars[lx.i];
        if c.is_whitespace() {
            lx.i += 1;
        } else if lx.at("/>") {
            lx.push(start, start + 2, Token::Punct);
            lx.i += 2;
            st.markup.mode = Mode::Text;
            return;
        } else if c == '>' {
            lx.push(start, start + 1, Token::Punct);
            lx.i += 1;
            st.markup.mode = match embed {
                Embed::Script => {
                    st.js = js::State::default();
                    Mode::Script
                }
                Embed::Style => {
                    st.css = css::State::default();
                    Mode::Style
                }
                Embed::None => Mode::Text,
            };
            return;
        } else if c == '"' || c == '\'' {
            match lx.chars[lx.i + 1..end].iter().position(|&d| d == c) {
                Some(n) => {
                    let close = lx.i + 1 + n;
                    if expr_attr {
                        // Vue directive values (`:x`, `@click`, `v-if`) are JavaScript.
                        lx.push(start, start + 1, Token::String);
                        lx.i += 1;
                        js::lex(lx, &mut js::State::default(), close);
                        lx.push(close, close + 1, Token::String);
                    } else {
                        lx.push(start, close + 1, Token::String);
                    }
                    lx.i = close + 1;
                }
                None => {
                    lx.push(start, end, Token::String);
                    lx.i = end;
                    st.markup.mode = Mode::Tag {
                        embed,
                        quote: Some(c),
                    };
                    return;
                }
            }
            expr_attr = false;
        } else if attr_char(c) {
            lx.eat_while(end, attr_char);
            lx.push(start, lx.i, Token::Attribute);
            let name: String = lx.chars[start..lx.i].iter().collect();
            expr_attr = name.starts_with([':', '@', '#']) || name.starts_with("v-");
        } else {
            lx.i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::{token_of, tokens};
    use super::*;

    #[test]
    fn xml_tags_attributes_comments() {
        let src = "<?xml version=\"1.0\"?>\n<svg width=\"10\" xmlns:x=\"u\">\n  <!-- a\n  b --> <g/>&amp;<![CDATA[x<y]]>\n</svg>";
        let t = tokens(Lang::Xml, src);
        assert_eq!(t[0][0].1, Token::Keyword);
        assert_eq!(token_of(Lang::Xml, src, "svg"), Some(Token::Tag));
        assert_eq!(token_of(Lang::Xml, src, "width"), Some(Token::Attribute));
        assert_eq!(token_of(Lang::Xml, src, "xmlns:x"), Some(Token::Attribute));
        assert_eq!(token_of(Lang::Xml, src, "\"10\""), Some(Token::String));
        assert_eq!(t[2][0], ("<!-- a".into(), Token::Comment));
        assert_eq!(t[3][0], ("  b -->".into(), Token::Comment));
        assert_eq!(token_of(Lang::Xml, src, "g"), Some(Token::Tag));
        assert_eq!(token_of(Lang::Xml, src, "&amp;"), Some(Token::Entity));
        assert_eq!(
            token_of(Lang::Xml, src, "<![CDATA[x<y]]>"),
            Some(Token::String)
        );
    }

    #[test]
    fn html_with_script_and_style() {
        let src = "<!DOCTYPE html>\n<style>\n  body { color: red; }\n</style>\n<script>\n  const a = '</p>';\n</script>\n<p class=x>hi</p>";
        assert_eq!(
            token_of(Lang::Html, src, "<!DOCTYPE html>"),
            Some(Token::Keyword)
        );
        assert_eq!(token_of(Lang::Html, src, "color"), Some(Token::Property));
        assert_eq!(token_of(Lang::Html, src, "const"), Some(Token::Keyword));
        assert_eq!(token_of(Lang::Html, src, "'</p>'"), Some(Token::String));
        let t = tokens(Lang::Html, src);
        assert_eq!(t[6][1], ("script".into(), Token::Tag)); // back to markup
        assert_eq!(token_of(Lang::Html, src, "class"), Some(Token::Attribute));
    }

    #[test]
    fn vue_sections_directives_and_interpolation() {
        let src = "<template>\n  <div\n    :title=\"count + 1\"\n    @click=\"inc()\" class=\"a\">{{ user.name }}</div>\n</template>\n<script setup>\nimport { ref } from 'vue';\n</script>\n<style scoped>\n.a { margin: 0; }\n</style>";
        let t = tokens(Lang::Vue, src);
        assert!(
            t[2].contains(&(":title".into(), Token::Attribute)),
            "{:?}",
            t[2]
        );
        assert!(t[2].contains(&("1".into(), Token::Number)));
        assert!(t[3].contains(&("inc".into(), Token::Function)));
        assert!(t[3].contains(&("\"a\"".into(), Token::String)));
        assert!(t[3].contains(&("name".into(), Token::Property)));
        assert!(t[3].contains(&("{{".into(), Token::Punct)));
        assert_eq!(t[6][0], ("import".into(), Token::Keyword));
        assert_eq!(t[6].last().unwrap(), &("'vue'".into(), Token::String));
        assert_eq!(t[9][0], (".a".into(), Token::Type));
        assert_eq!(t[10][1], ("style".into(), Token::Tag));
    }
}
