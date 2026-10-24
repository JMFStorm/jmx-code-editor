//! C# lexer: verbatim, interpolated and raw strings, XML doc comments, preprocessor lines.

use super::{Lexer, Token, js};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `"..."`: ends at the end of the line if unterminated.
    Regular,
    /// `@"..."`: `""` is a quote, may span lines.
    Verbatim,
    /// `"""..."""`: closed by this many quotes, may span lines.
    Raw(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// Inside string text; `interp` for `$` strings.
    Str { kind: Kind, interp: bool },
    /// Inside `{ ... }` of an interpolated string, with the `{` depth so far.
    Code(u16),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    in_comment: bool,
    stack: Vec<Ctx>,
}

const KEYWORDS: &[&str] = &[
    "abstract",
    "and",
    "as",
    "async",
    "await",
    "base",
    "break",
    "case",
    "catch",
    "checked",
    "class",
    "const",
    "continue",
    "default",
    "delegate",
    "do",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "file",
    "finally",
    "fixed",
    "for",
    "foreach",
    "get",
    "global",
    "goto",
    "if",
    "implicit",
    "in",
    "init",
    "interface",
    "internal",
    "is",
    "lock",
    "namespace",
    "new",
    "not",
    "operator",
    "or",
    "out",
    "override",
    "params",
    "partial",
    "private",
    "protected",
    "public",
    "readonly",
    "record",
    "ref",
    "required",
    "return",
    "scoped",
    "sealed",
    "set",
    "sizeof",
    "stackalloc",
    "static",
    "struct",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "unchecked",
    "unsafe",
    "using",
    "value",
    "var",
    "virtual",
    "void",
    "volatile",
    "when",
    "where",
    "while",
    "with",
    "yield",
    "nameof",
    "select",
    "from",
    "orderby",
    "group",
    "join",
    "let",
    "into",
    "on",
    "equals",
    "by",
    "ascending",
    "descending",
    "add",
    "remove",
    "notnull",
    "unmanaged",
    "managed",
    "dynamic",
    // Built-in types.
    "bool",
    "byte",
    "char",
    "decimal",
    "double",
    "float",
    "int",
    "long",
    "nint",
    "nuint",
    "object",
    "sbyte",
    "short",
    "string",
    "uint",
    "ulong",
    "ushort",
];

const LITERALS: &[&str] = &["true", "false", "null"];

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

pub fn lex(lx: &mut Lexer, st: &mut State, end: usize) {
    // Preprocessor directive: the whole line.
    if !st.in_comment
        && st.stack.is_empty()
        && lx.chars.iter().find(|c| !c.is_whitespace()) == Some(&'#')
    {
        lx.push(0, end, Token::Keyword);
        lx.i = end;
        return;
    }
    while lx.i < end {
        if st.in_comment {
            block_comment(lx, st, end, lx.i);
            continue;
        }
        if let Some(Ctx::Str { kind, interp }) = st.stack.last().copied() {
            string_text(lx, st, end, kind, interp);
            continue;
        }
        let start = lx.i;
        let c = lx.chars[lx.i];
        if c.is_whitespace() {
            lx.i += 1;
        } else if lx.at("///") {
            doc_comment(lx, end);
        } else if lx.at("//") {
            lx.push(start, end, Token::Comment);
            lx.i = end;
        } else if lx.at("/*") {
            lx.i += 2;
            st.in_comment = true;
            block_comment(lx, st, end, start);
        } else if let Some((len, kind, interp)) = string_start(lx) {
            lx.push(start, start + len, Token::String);
            lx.i += len;
            st.stack.push(Ctx::Str { kind, interp });
        } else if c == '\'' {
            js::string(lx, end, '\'');
        } else if c.is_ascii_digit()
            || (c == '.' && lx.peek_at(1).is_some_and(|d| d.is_ascii_digit()))
        {
            js::number(lx, end);
        } else if c == '{' {
            if let Some(Ctx::Code(n)) = st.stack.last_mut() {
                *n += 1;
            }
            lx.i += 1;
        } else if c == '}' {
            match st.stack.last_mut() {
                Some(Ctx::Code(0)) => {
                    st.stack.pop();
                    lx.push(start, start + 1, Token::Punct);
                }
                Some(Ctx::Code(n)) => *n -= 1,
                _ => {}
            }
            lx.i += 1;
        } else if c == '@' && lx.peek_at(1).is_some_and(ident_start) {
            // `@class`: a verbatim identifier, not a keyword.
            lx.i += 1;
            lx.eat_while(end, ident_char);
        } else if ident_start(c) {
            identifier(lx, end);
        } else {
            lx.i += 1;
        }
    }
    // A regular string (and anything nested in it) cannot continue on the next line.
    if let Some(k) = st.stack.iter().position(|c| {
        matches!(
            c,
            Ctx::Str {
                kind: Kind::Regular,
                ..
            }
        )
    }) {
        st.stack.truncate(k);
    }
}

/// `"`, `@"`, `$"`, `$@"`, `@$"`, `"""`, `$"""` at the cursor: (length, kind, interpolated).
fn string_start(lx: &Lexer) -> Option<(usize, Kind, bool)> {
    let mut k = 0;
    let (mut verbatim, mut interp) = (false, false);
    while k < 3 {
        match lx.peek_at(k) {
            Some('@') if !verbatim => verbatim = true,
            Some('$') => interp = true,
            _ => break,
        }
        k += 1;
    }
    if lx.peek_at(k) != Some('"') {
        return None;
    }
    let quotes = lx.chars[lx.i + k..]
        .iter()
        .take_while(|&&c| c == '"')
        .count();
    if quotes >= 3 && !verbatim {
        return Some((k + quotes, Kind::Raw(quotes.min(255) as u8), interp));
    }
    let kind = if verbatim {
        Kind::Verbatim
    } else {
        Kind::Regular
    };
    Some((k + 1, kind, interp))
}

fn string_text(lx: &mut Lexer, st: &mut State, end: usize, kind: Kind, interp: bool) {
    let start = lx.i;
    while lx.i < end {
        let c = lx.chars[lx.i];
        match c {
            '\\' if kind == Kind::Regular => {
                lx.push(start, lx.i, Token::String);
                let e = (lx.i + 2).min(end);
                lx.push(lx.i, e, Token::Escape);
                lx.i = e;
                return;
            }
            '"' => {
                let run = lx.chars[lx.i..end]
                    .iter()
                    .take_while(|&&q| q == '"')
                    .count();
                match kind {
                    Kind::Verbatim if run >= 2 => lx.i += 2,
                    Kind::Raw(n) if run < n as usize => lx.i += run,
                    _ => {
                        let close = match kind {
                            Kind::Raw(n) => n as usize,
                            _ => 1,
                        };
                        lx.i += close;
                        lx.push(start, lx.i, Token::String);
                        st.stack.pop();
                        return;
                    }
                }
            }
            '{' | '}' if interp && lx.peek_at(1) == Some(c) => lx.i += 2,
            '{' if interp => {
                lx.push(start, lx.i, Token::String);
                lx.push(lx.i, lx.i + 1, Token::Punct);
                lx.i += 1;
                st.stack.push(Ctx::Code(0));
                return;
            }
            _ => lx.i += 1,
        }
    }
    lx.push(start, lx.i, Token::String);
}

/// Rest of a `/* */` comment from the cursor; the span starts at `start`.
fn block_comment(lx: &mut Lexer, st: &mut State, end: usize, start: usize) {
    let close = lx.find("*/", end);
    lx.i = close.map_or(end, |k| k + 2);
    lx.push(start, lx.i, Token::Comment);
    st.in_comment = close.is_none();
}

/// `/// <summary>text</summary>`: the comment with its XML tags highlighted.
fn doc_comment(lx: &mut Lexer, end: usize) {
    let start = lx.i;
    lx.push(start, end, Token::Comment);
    let mut k = start + 3;
    while k < end {
        if lx.chars[k] == '<' {
            let s = k;
            while k < end && lx.chars[k] != '>' {
                k += 1;
            }
            if k < end {
                lx.push(s, k + 1, Token::DocTag);
            }
        }
        k += 1;
    }
    lx.i = end;
}

fn identifier(lx: &mut Lexer, end: usize) {
    let start = lx.i;
    lx.eat_while(end, ident_char);
    let word: String = lx.chars[start..lx.i].iter().collect();
    let after_dot = lx.prev_non_blank(start) == Some('.');
    let next = lx.next_non_blank(lx.i);
    let call = next == Some('(') || (next == Some('<') && generic_call(lx, lx.i, end));
    let token = if KEYWORDS.contains(&word.as_str()) && !after_dot {
        Some(Token::Keyword)
    } else if LITERALS.contains(&word.as_str()) {
        Some(Token::Literal)
    } else if call {
        Some(Token::Function)
    } else if after_dot {
        Some(Token::Property)
    } else if word.starts_with(|c: char| c.is_uppercase()) {
        Some(Token::Type)
    } else {
        None
    };
    if let Some(t) = token {
        lx.push(start, lx.i, t);
    }
}

/// `Name<T, U>(` on this line: a generic method call rather than a comparison.
fn generic_call(lx: &Lexer, from: usize, end: usize) -> bool {
    let mut depth = 0;
    for k in from..end {
        match lx.chars[k] {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return lx.next_non_blank(k + 1) == Some('(');
                }
            }
            c if c.is_alphanumeric() || matches!(c, '_' | ',' | ' ' | '.' | '?' | '[' | ']') => {}
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::super::Lang;
    use super::super::test_util::{token_of, tokens};
    use super::*;

    const L: Lang = Lang::CSharp;

    #[test]
    fn declarations_calls_and_numbers() {
        let src = "public static async Task<int> Sum(List<int> xs, decimal m = 1.5m) => xs.Where(x => x > 0x1F).Count() + Get<string>(1_000UL);";
        assert_eq!(token_of(L, src, "public"), Some(Token::Keyword));
        assert_eq!(token_of(L, src, "Task"), Some(Token::Type));
        assert_eq!(token_of(L, src, "int"), Some(Token::Keyword));
        assert_eq!(token_of(L, src, "Sum"), Some(Token::Function));
        assert_eq!(token_of(L, src, "1.5m"), Some(Token::Number));
        assert_eq!(token_of(L, src, "Where"), Some(Token::Function));
        assert_eq!(token_of(L, src, "0x1F"), Some(Token::Number));
        assert_eq!(token_of(L, src, "Get"), Some(Token::Function));
        assert_eq!(token_of(L, src, "1_000UL"), Some(Token::Number));
        assert_eq!(
            token_of(L, "var n = p.Name; x = null;", "Name"),
            Some(Token::Property)
        );
        assert_eq!(token_of(L, "x = null;", "null"), Some(Token::Literal));
        assert_eq!(token_of(L, "var @class = 1;", "class"), None);
    }

    #[test]
    fn comments_doc_tags_and_directives() {
        let src = "/// <summary>Adds.</summary>\n  #region Helpers\n/* a\n b */ int x; // c";
        let t = tokens(L, src);
        assert_eq!(t[0][0].1, Token::Comment);
        assert!(t[0].contains(&("<summary>".into(), Token::DocTag)));
        assert_eq!(t[1][0], ("  #region Helpers".into(), Token::Keyword));
        assert_eq!(t[2][0].1, Token::Comment);
        assert_eq!(t[3][0], (" b */".into(), Token::Comment));
        assert_eq!(t[3][1], ("int".into(), Token::Keyword));
        assert_eq!(t[3].last().unwrap(), &("// c".into(), Token::Comment));
    }

    #[test]
    fn strings_of_every_kind() {
        let t = tokens(L, r#"var s = $"Hi {name.ToUpper()}!\n" + 'c' + '\t';"#);
        let k: Vec<_> = t[0].iter().map(|(s, k)| (s.as_str(), *k)).collect();
        assert!(k.contains(&("Hi ", Token::String)), "{k:?}");
        assert!(k.contains(&("ToUpper", Token::Function)));
        assert!(k.contains(&("!", Token::String)));
        assert!(k.contains(&("\\n", Token::Escape)));
        assert!(k.contains(&("'c'", Token::String)));

        // Verbatim strings span lines; "" is a quote, backslashes are plain.
        let t = tokens(L, "var p = @\"C:\\dir\n\"\"q\"\" end\"; int y;");
        assert_eq!(t[0].last().unwrap(), &("C:\\dir".into(), Token::String));
        assert_eq!(t[1][0], ("\"\"q\"\" end\"".into(), Token::String));
        assert_eq!(t[1][1], ("int".into(), Token::Keyword));

        // Raw strings: quotes inside, closed by the same number of quotes.
        let t = tokens(L, "var j = \"\"\"\n  {\"a\": 1}\n  \"\"\"; return;");
        assert_eq!(t[1][0], ("  {\"a\": 1}".into(), Token::String));
        assert_eq!(t[2][1], ("return".into(), Token::Keyword));

        // An unterminated regular string does not leak into the next line.
        let t = tokens(L, "var s = \"open\nint z;");
        assert_eq!(t[1][0], ("int".into(), Token::Keyword));
    }
}
