//! Odin lexer: nested block comments, raw strings, `#directives`, `@(attributes)` and the
//! `name :: proc` / `name :: struct` declaration forms.

use super::{Lexer, Token, js};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    /// Depth of nested `/* */` comments open at the line start.
    comment: u16,
    /// Inside a `` `raw` `` string.
    raw: bool,
}

const KEYWORDS: &[&str] = &[
    "align_of",
    "asm",
    "auto_cast",
    "bit_field",
    "bit_set",
    "break",
    "case",
    "cast",
    "context",
    "continue",
    "defer",
    "distinct",
    "do",
    "dynamic",
    "else",
    "enum",
    "fallthrough",
    "for",
    "foreign",
    "if",
    "import",
    "in",
    "map",
    "matrix",
    "not_in",
    "offset_of",
    "or_break",
    "or_continue",
    "or_else",
    "or_return",
    "package",
    "proc",
    "return",
    "size_of",
    "struct",
    "switch",
    "transmute",
    "type_of",
    "typeid_of",
    "union",
    "using",
    "when",
    "where",
];

const LITERALS: &[&str] = &["true", "false", "nil"];

const TYPES: &[&str] = &[
    "any",
    "bool",
    "b8",
    "b16",
    "b32",
    "b64",
    "byte",
    "complex32",
    "complex64",
    "complex128",
    "cstring",
    "f16",
    "f32",
    "f64",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "int",
    "quaternion64",
    "quaternion128",
    "quaternion256",
    "rawptr",
    "rune",
    "string",
    "typeid",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "uint",
    "uintptr",
];

/// Words that make `name :: word` a type declaration.
const TYPE_DECLS: &[&str] = &[
    "struct",
    "enum",
    "union",
    "bit_set",
    "bit_field",
    "distinct",
];

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A built-in type, endian-specific ones (`u32le`, `f64be`) included.
fn builtin_type(word: &str) -> bool {
    let base = word
        .strip_suffix("le")
        .or_else(|| word.strip_suffix("be"))
        .filter(|b| b.len() > 1 && b.ends_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(word);
    TYPES.contains(&base)
}

pub fn lex(lx: &mut Lexer, st: &mut State, end: usize) {
    // `@(...)` on this line: its names are attribute names (depth of its parentheses).
    let mut attr = 0usize;
    while lx.i < end {
        if st.comment > 0 {
            block_comment(lx, st, end, lx.i);
            continue;
        }
        if st.raw {
            raw_string(lx, st, end, lx.i);
            continue;
        }
        let start = lx.i;
        let c = lx.chars[lx.i];
        if c.is_whitespace() {
            lx.i += 1;
        } else if lx.at("//") {
            lx.push(start, end, Token::Comment);
            lx.i = end;
        } else if lx.at("/*") {
            lx.i += 2;
            st.comment = 1;
            block_comment(lx, st, end, start);
        } else if c == '`' {
            lx.i += 1;
            st.raw = true;
            raw_string(lx, st, end, start);
        } else if c == '"' || c == '\'' {
            js::string(lx, end, c);
        } else if c.is_ascii_digit() {
            number(lx, end);
        } else if lx.at("---") {
            lx.push(start, start + 3, Token::Literal);
            lx.i += 3;
        } else if c == '#' && lx.peek_at(1).is_some_and(ident_start) {
            lx.i += 1;
            lx.eat_while(end, ident_char);
            lx.push(start, lx.i, Token::Attribute);
        } else if c == '@' {
            lx.i += 1;
            if lx.peek() == Some('(') {
                attr = 1;
                lx.i += 1;
            } else {
                lx.eat_while(end, ident_char);
            }
            lx.push(start, lx.i, Token::Attribute);
        } else if c == '$' && lx.peek_at(1).is_some_and(ident_start) {
            // A polymorphic parameter: `$T`.
            lx.i += 1;
            lx.eat_while(end, ident_char);
            lx.push(start, lx.i, Token::Type);
        } else if ident_start(c) {
            if attr > 0 {
                lx.eat_while(end, ident_char);
                lx.push(start, lx.i, Token::Attribute);
            } else {
                identifier(lx, end);
            }
        } else {
            if attr > 0 {
                match c {
                    '(' => attr += 1,
                    ')' => {
                        attr -= 1;
                        if attr == 0 {
                            lx.push(start, start + 1, Token::Attribute);
                        }
                    }
                    _ => {}
                }
            }
            lx.i += 1;
        }
    }
}

/// Rest of a (nested) `/* */` comment from the cursor; the span starts at `start`.
fn block_comment(lx: &mut Lexer, st: &mut State, end: usize, start: usize) {
    while lx.i < end && st.comment > 0 {
        if lx.at("/*") {
            st.comment = st.comment.saturating_add(1);
            lx.i += 2;
        } else if lx.at("*/") {
            st.comment -= 1;
            lx.i += 2;
        } else {
            lx.i += 1;
        }
    }
    lx.i = lx.i.min(end);
    lx.push(start, lx.i, Token::Comment);
}

/// Rest of a raw string from the cursor (no escapes; may span lines).
fn raw_string(lx: &mut Lexer, st: &mut State, end: usize, start: usize) {
    let close = (lx.i..end).find(|&k| lx.chars[k] == '`');
    lx.i = close.map_or(end, |k| k + 1);
    lx.push(start, lx.i, Token::String);
    st.raw = close.is_none();
}

/// Like `js::number`, but `1..<5` and `0..=n` stop before the range operator.
fn number(lx: &mut Lexer, end: usize) {
    let start = lx.i;
    lx.i += 1;
    while lx.i < end {
        let c = lx.chars[lx.i];
        let exp_sign = (c == '+' || c == '-')
            && matches!(lx.chars[lx.i - 1], 'e' | 'E')
            && !lx.chars[start..lx.i]
                .iter()
                .any(|d| matches!(d, 'x' | 'X' | 'h'));
        let dot = c == '.' && lx.peek_at(1) != Some('.');
        if c.is_ascii_alphanumeric() || c == '_' || dot || exp_sign {
            lx.i += 1;
        } else {
            break;
        }
    }
    lx.push(start, lx.i, Token::Number);
}

fn identifier(lx: &mut Lexer, end: usize) {
    let start = lx.i;
    lx.eat_while(end, ident_char);
    let word: String = lx.chars[start..lx.i].iter().collect();
    let after_dot = lx.prev_non_blank(start) == Some('.');
    let next = lx.next_non_blank(lx.i);
    let token = if KEYWORDS.contains(&word.as_str()) && !after_dot {
        Some(Token::Keyword)
    } else if LITERALS.contains(&word.as_str()) {
        Some(Token::Literal)
    } else if builtin_type(&word) && !after_dot {
        Some(Token::Type)
    } else if let Some(decl) = declared(lx, lx.i, end) {
        Some(decl)
    } else if next == Some('(') {
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

/// `name :: proc` (Function) or `name :: struct` / `enum` / … (Type), from just after the
/// name. Calling conventions (`proc "c"`) and directives (`#force_inline proc`) are skipped.
fn declared(lx: &Lexer, from: usize, end: usize) -> Option<Token> {
    let skip_blank = |mut k: usize| {
        while k < end && lx.chars[k].is_whitespace() {
            k += 1;
        }
        k
    };
    let mut k = skip_blank(from);
    if !(lx.chars[k..end].starts_with(&[':', ':'])) {
        return None;
    }
    k = skip_blank(k + 2);
    loop {
        if k < end && lx.chars[k] == '#' {
            k += 1;
            while k < end && ident_char(lx.chars[k]) {
                k += 1;
            }
            k = skip_blank(k);
            continue;
        }
        break;
    }
    let w = k;
    while k < end && ident_char(lx.chars[k]) {
        k += 1;
    }
    let word: String = lx.chars[w..k].iter().collect();
    if word == "proc" {
        Some(Token::Function)
    } else if TYPE_DECLS.contains(&word.as_str()) {
        Some(Token::Type)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::Lang;
    use super::super::test_util::{token_of, tokens};
    use super::*;

    const L: Lang = Lang::Odin;

    #[test]
    fn declarations_calls_and_types() {
        let src = "add :: proc(a, b: int) -> f32le { return fmt.println(x.y, 0x1F, 1_000, 2i) }";
        assert_eq!(token_of(L, src, "add"), Some(Token::Function));
        assert_eq!(token_of(L, src, "proc"), Some(Token::Keyword));
        assert_eq!(token_of(L, src, "int"), Some(Token::Type));
        assert_eq!(token_of(L, src, "f32le"), Some(Token::Type));
        assert_eq!(token_of(L, src, "return"), Some(Token::Keyword));
        assert_eq!(token_of(L, src, "println"), Some(Token::Function));
        assert_eq!(token_of(L, src, "y"), Some(Token::Property));
        assert_eq!(token_of(L, src, "0x1F"), Some(Token::Number));
        assert_eq!(token_of(L, src, "1_000"), Some(Token::Number));
        assert_eq!(token_of(L, src, "2i"), Some(Token::Number));
        assert_eq!(
            token_of(L, "Vec :: struct { x: f32 }", "Vec"),
            Some(Token::Type)
        );
        assert_eq!(token_of(L, "Id :: distinct u32", "Id"), Some(Token::Type));
        assert_eq!(
            token_of(L, "f :: #force_inline proc \"c\" () {}", "f"),
            Some(Token::Function)
        );
        assert_eq!(token_of(L, "p: ^Player = nil", "Player"), Some(Token::Type));
        assert_eq!(token_of(L, "p: ^Player = nil", "nil"), Some(Token::Literal));
        assert_eq!(token_of(L, "x: int = ---", "---"), Some(Token::Literal));
        assert_eq!(
            token_of(L, "make :: proc($T: typeid) {}", "$T"),
            Some(Token::Type)
        );
        // Ranges are not part of the number.
        let t = tokens(L, "for i in 0..<10 {}");
        assert!(t[0].contains(&("0".into(), Token::Number)), "{t:?}");
        assert!(t[0].contains(&("10".into(), Token::Number)));
    }

    #[test]
    fn directives_and_attributes() {
        let t = tokens(L, "@(test, link_name=\"x\")\n#assert(true) @private");
        assert_eq!(t[0][0], ("@(".into(), Token::Attribute));
        assert!(t[0].contains(&("test".into(), Token::Attribute)));
        assert!(t[0].contains(&("link_name".into(), Token::Attribute)));
        assert!(t[0].contains(&("\"x\"".into(), Token::String)));
        assert_eq!(t[0].last().unwrap(), &(")".into(), Token::Attribute));
        assert_eq!(t[1][0], ("#assert".into(), Token::Attribute));
        assert!(t[1].contains(&("@private".into(), Token::Attribute)));
    }

    #[test]
    fn comments_and_strings() {
        // Nested block comments only end at the outer `*/`.
        let t = tokens(L, "/* a /* b */ c\n d */ x :: 1 // e");
        assert_eq!(t[0][0].1, Token::Comment);
        assert_eq!(t[1][0], (" d */".into(), Token::Comment));
        assert_eq!(t[1].last().unwrap(), &("// e".into(), Token::Comment));
        // Raw strings span lines and have no escapes.
        let t = tokens(
            L,
            "s := `C:\\dir\nend` + \"q\\n\" + 'r'\nimport \"core:fmt\"",
        );
        assert_eq!(t[0].last().unwrap(), &("`C:\\dir".into(), Token::String));
        assert_eq!(t[1][0], ("end`".into(), Token::String));
        assert!(t[1].contains(&("\\n".into(), Token::Escape)));
        assert!(t[1].contains(&("'r'".into(), Token::String)));
        assert_eq!(t[2][0], ("import".into(), Token::Keyword));
    }
}
