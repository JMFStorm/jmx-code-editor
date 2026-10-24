//! JavaScript (and close enough: TypeScript, JSX) lexer.

use super::{Lexer, Token};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// Inside the text of a template literal.
    Template,
    /// Inside `${ ... }` of a template literal, with the `{` depth so far.
    Interp(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// Inside `/* */` (`doc`: a `/** */` doc comment).
    comment: Option<bool>,
    stack: Vec<Ctx>,
    /// A `/` here starts a regex rather than dividing.
    regex_ok: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            comment: None,
            stack: Vec::new(),
            regex_ok: true,
        }
    }
}

const KEYWORDS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "keyof",
    "let",
    "namespace",
    "new",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "set",
    "static",
    "switch",
    "throw",
    "try",
    "type",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const LITERALS: &[&str] = &[
    "true",
    "false",
    "null",
    "undefined",
    "this",
    "super",
    "NaN",
    "Infinity",
];

/// Keywords after which a `/` starts a regex.
const REGEX_AFTER: &[&str] = &[
    "return",
    "typeof",
    "case",
    "do",
    "else",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "instanceof",
    "yield",
    "await",
];

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Lex `lx` from its cursor up to `end` (embedding stops at `</script>`).
pub fn lex(lx: &mut Lexer, st: &mut State, end: usize) {
    while lx.i < end {
        if let Some(doc) = st.comment {
            block_comment(lx, st, end, doc, lx.i);
            continue;
        }
        if st.stack.last() == Some(&Ctx::Template) {
            template_text(lx, st, end);
            continue;
        }
        let c = lx.chars[lx.i];
        let start = lx.i;
        if c.is_whitespace() {
            lx.i += 1;
            continue;
        }
        if lx.at("//") {
            lx.push(start, end, Token::Comment);
            lx.i = end;
            return;
        }
        if lx.at("/*") {
            let doc = lx.at("/**") && !lx.at("/**/");
            lx.i += 2;
            st.comment = Some(doc);
            block_comment(lx, st, end, doc, start);
            continue;
        }
        match c {
            '"' | '\'' => {
                string(lx, end, c);
                st.regex_ok = false;
            }
            '`' => {
                lx.push(start, start + 1, Token::String);
                lx.i += 1;
                st.stack.push(Ctx::Template);
            }
            '0'..='9' => {
                number(lx, end);
                st.regex_ok = false;
            }
            '.' if lx.peek_at(1).is_some_and(|d| d.is_ascii_digit()) => {
                number(lx, end);
                st.regex_ok = false;
            }
            '/' if st.regex_ok => {
                regex(lx, end);
                st.regex_ok = false;
            }
            '{' => {
                if let Some(Ctx::Interp(n)) = st.stack.last_mut() {
                    *n += 1;
                }
                lx.i += 1;
                st.regex_ok = true;
            }
            '}' => {
                match st.stack.last_mut() {
                    Some(Ctx::Interp(0)) => {
                        st.stack.pop();
                        lx.push(start, start + 1, Token::Punct);
                    }
                    Some(Ctx::Interp(n)) => *n -= 1,
                    _ => {}
                }
                lx.i += 1;
                st.regex_ok = true;
            }
            c if ident_start(c) => identifier(lx, st, end),
            '@' if lx.peek_at(1).is_some_and(ident_start) => {
                // Decorator.
                lx.i += 1;
                lx.eat_while(end, ident_char);
                lx.push(start, lx.i, Token::Function);
                st.regex_ok = false;
            }
            ')' | ']' => {
                lx.i += 1;
                st.regex_ok = false;
            }
            _ => {
                lx.i += 1;
                st.regex_ok = true;
            }
        }
    }
}

/// The rest of a block comment from the cursor; the span starts at `start` (the `/*`, or the
/// line start when the comment continues from an earlier line).
fn block_comment(lx: &mut Lexer, st: &mut State, end: usize, doc: bool, start: usize) {
    let close = lx.find("*/", end);
    let stop = close.map_or(end, |k| k + 2);
    lx.push(start, stop, Token::Comment);
    if doc {
        // `@tag` and `{Type}` inside doc comments.
        let mut k = start;
        while k < stop {
            let c = lx.chars[k];
            if c == '@' && (k == 0 || !ident_char(lx.chars[k - 1])) {
                let s = k;
                k += 1;
                while k < stop && ident_char(lx.chars[k]) {
                    k += 1;
                }
                if k > s + 1 {
                    lx.push(s, k, Token::DocTag);
                }
            } else if c == '{' {
                let s = k;
                while k < stop && lx.chars[k] != '}' {
                    k += 1;
                }
                if k < stop {
                    lx.push(s, k + 1, Token::Type);
                }
                k += 1;
            } else {
                k += 1;
            }
        }
    }
    lx.i = stop;
    if close.is_some() {
        st.comment = None;
        st.regex_ok = true;
    }
}

fn template_text(lx: &mut Lexer, st: &mut State, end: usize) {
    let start = lx.i;
    while lx.i < end {
        match lx.chars[lx.i] {
            '\\' => {
                lx.push(start, lx.i, Token::String);
                let e = (lx.i + 2).min(end);
                lx.push(lx.i, e, Token::Escape);
                lx.i = e;
                return template_text(lx, st, end);
            }
            '`' => {
                lx.i += 1;
                lx.push(start, lx.i, Token::String);
                st.stack.pop();
                st.regex_ok = false;
                return;
            }
            '$' if lx.peek_at(1) == Some('{') => {
                lx.push(start, lx.i, Token::String);
                lx.push(lx.i, lx.i + 2, Token::Punct);
                lx.i += 2;
                st.stack.push(Ctx::Interp(0));
                st.regex_ok = true;
                return;
            }
            _ => lx.i += 1,
        }
    }
    lx.push(start, lx.i, Token::String);
}

/// A quoted string; unterminated strings end at the end of the line.
pub fn string(lx: &mut Lexer, end: usize, quote: char) {
    let start = lx.i;
    lx.i += 1;
    let mut piece = start;
    while lx.i < end {
        match lx.chars[lx.i] {
            '\\' => {
                lx.push(piece, lx.i, Token::String);
                let e = (lx.i + 2).min(end);
                lx.push(lx.i, e, Token::Escape);
                lx.i = e;
                piece = e;
            }
            c if c == quote => {
                lx.i += 1;
                break;
            }
            _ => lx.i += 1,
        }
    }
    lx.push(piece, lx.i, Token::String);
}

pub fn number(lx: &mut Lexer, end: usize) {
    let start = lx.i;
    // The first char (a digit, `.` or a JSON `-`) always belongs to the number.
    lx.i += 1;
    while lx.i < end {
        let c = lx.chars[lx.i];
        let exp_sign = (c == '+' || c == '-')
            && matches!(lx.chars[lx.i - 1], 'e' | 'E')
            && !lx.chars[start..lx.i].iter().any(|d| matches!(d, 'x' | 'X'));
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || exp_sign {
            lx.i += 1;
        } else {
            break;
        }
    }
    lx.push(start, lx.i, Token::Number);
}

fn regex(lx: &mut Lexer, end: usize) {
    let start = lx.i;
    lx.i += 1;
    let mut class = false;
    while lx.i < end {
        match lx.chars[lx.i] {
            '\\' => lx.i += 1,
            '[' => class = true,
            ']' => class = false,
            '/' if !class => {
                lx.i += 1;
                lx.eat_while(end, |c| c.is_ascii_alphabetic());
                lx.push(start, lx.i, Token::Regex);
                return;
            }
            _ => {}
        }
        lx.i += 1;
    }
    // No closing slash on this line: it was a division after all.
    lx.i = start + 1;
}

fn identifier(lx: &mut Lexer, st: &mut State, end: usize) {
    let start = lx.i;
    lx.eat_while(end, ident_char);
    let word: String = lx.chars[start..lx.i].iter().collect();
    let after_dot = lx.prev_non_blank(start) == Some('.')
        && !(start >= 3 && lx.chars[start - 3..start] == ['.', '.', '.']);
    let next = lx.next_non_blank(lx.i);
    let token = if after_dot {
        if next == Some('(') {
            Some(Token::Function)
        } else {
            Some(Token::Property)
        }
    } else if KEYWORDS.contains(&word.as_str()) {
        Some(Token::Keyword)
    } else if LITERALS.contains(&word.as_str()) {
        Some(Token::Literal)
    } else if next == Some('(') {
        Some(Token::Function)
    } else if word.starts_with(|c: char| c.is_uppercase()) {
        Some(Token::Type)
    } else if next == Some(':')
        && lx
            .prev_non_blank(start)
            .is_none_or(|c| c == '{' || c == ',')
    {
        // Object literal key.
        Some(Token::Property)
    } else {
        None
    };
    if let Some(t) = token {
        lx.push(start, lx.i, t);
    }
    st.regex_ok = !after_dot && REGEX_AFTER.contains(&word.as_str());
}

#[cfg(test)]
mod tests {
    use super::super::Lang;
    use super::super::test_util::{token_of, tokens};
    use super::*;

    const L: Lang = Lang::JavaScript;

    #[test]
    fn keywords_strings_numbers_functions() {
        let src = "export function add(a, b = 2.5e-3) { return a.x + 'it\\'s' + Foo.bar(); }";
        assert_eq!(token_of(L, src, "export"), Some(Token::Keyword));
        assert_eq!(token_of(L, src, "add"), Some(Token::Function));
        assert_eq!(token_of(L, src, "2.5e-3"), Some(Token::Number));
        assert_eq!(token_of(L, src, "x"), Some(Token::Property));
        assert_eq!(token_of(L, src, "'it"), Some(Token::String));
        assert_eq!(token_of(L, src, "\\'"), Some(Token::Escape));
        assert_eq!(token_of(L, src, "Foo"), Some(Token::Type));
        assert_eq!(token_of(L, src, "bar"), Some(Token::Function));
        assert_eq!(
            token_of(L, "const o = { key: null };", "key"),
            Some(Token::Property)
        );
        assert_eq!(token_of(L, "x = true", "true"), Some(Token::Literal));
    }

    #[test]
    fn regex_versus_division() {
        assert_eq!(
            token_of(L, "const r = /a\\/[/]b/gi;", "/a\\/[/]b/gi"),
            Some(Token::Regex)
        );
        let t = tokens(L, "const half = total / 2 / 1;");
        assert!(t[0].iter().all(|(_, k)| *k != Token::Regex), "{t:?}");
        assert_eq!(
            token_of(L, "return /x+/.test(s)", "/x+/"),
            Some(Token::Regex)
        );
    }

    #[test]
    fn template_literals_nest() {
        let t = tokens(L, "const s = `a ${b + `c ${d}`} e`;");
        let kinds: Vec<_> = t[0].iter().map(|(s, k)| (s.as_str(), *k)).collect();
        assert!(kinds.contains(&("a ", Token::String)), "{kinds:?}");
        assert!(kinds.contains(&("c ", Token::String)));
        assert!(kinds.contains(&("${", Token::Punct)));
        assert!(kinds.contains(&(" e`", Token::String)));
        // A template spanning lines keeps its state.
        let t = tokens(L, "let s = `one\ntwo ${x}\nthree`; let y");
        assert_eq!(t[1][0], ("two ".into(), Token::String));
        assert_eq!(t[2][0], ("three`".into(), Token::String));
        assert_eq!(t[2][1], ("let".into(), Token::Keyword));
    }

    #[test]
    fn comments_and_doc_tags_across_lines() {
        let src = "/**\n * Sum.\n * @param {number[]} xs - items\n */\nconst z = 1; // done";
        let t = tokens(L, src);
        assert_eq!(t[1][0].1, Token::Comment);
        assert!(
            t[2].contains(&("@param".into(), Token::DocTag)),
            "{:?}",
            t[2]
        );
        assert!(t[2].contains(&("{number[]}".into(), Token::Type)));
        assert_eq!(t[3][0], (" */".into(), Token::Comment));
        assert_eq!(t[4][0], ("const".into(), Token::Keyword));
        assert_eq!(t[4].last().unwrap(), &("// done".into(), Token::Comment));
    }
}
