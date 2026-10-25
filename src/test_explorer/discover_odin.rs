//! Odin test discovery: `@(test)` (or `@test`) procedures, `name :: proc(t: ^testing.T)`.
//! A test's full name is `package.name`, as `odin test` reports it.

use super::{FileTests, Found, Framework, hash_text};

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Punct(char),
}

struct Token {
    tok: Tok,
    /// Byte offsets in the source.
    start: usize,
    end: usize,
}

/// Identifiers and punctuation; comments (nested) and strings are skipped.
fn tokenize(src: &str) -> Vec<Token> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            let mut depth = 0;
            while i < b.len() {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if c == b'"' || c == b'\'' {
            i += 1;
            while i < b.len() && b[i] != c && b[i] != b'\n' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
        } else if c == b'`' {
            i += 1;
            while i < b.len() && b[i] != b'`' {
                i += 1;
            }
            i += 1;
        } else if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80) {
                i += 1;
            }
            out.push(Token {
                tok: Tok::Ident(src[start..i].to_string()),
                start,
                end: i,
            });
        } else {
            out.push(Token {
                tok: Tok::Punct(c as char),
                start: i,
                end: i + 1,
            });
            i += 1;
        }
    }
    out
}

/// The index of the bracket closing the one at `open` (or the last token).
fn matching(toks: &[Token], open: usize) -> usize {
    let (o, c) = match toks[open].tok {
        Tok::Punct('(') => ('(', ')'),
        Tok::Punct('{') => ('{', '}'),
        _ => return open,
    };
    let mut depth = 0;
    for (k, t) in toks.iter().enumerate().skip(open) {
        if t.tok == Tok::Punct(o) {
            depth += 1;
        } else if t.tok == Tok::Punct(c) {
            depth -= 1;
            if depth == 0 {
                return k;
            }
        }
    }
    toks.len() - 1
}

fn ident(toks: &[Token], k: usize) -> Option<&str> {
    match toks.get(k).map(|t| &t.tok) {
        Some(Tok::Ident(s)) => Some(s),
        _ => None,
    }
}

fn punct(toks: &[Token], k: usize, c: char) -> bool {
    toks.get(k).is_some_and(|t| t.tok == Tok::Punct(c))
}

/// Test procedures in an Odin file.
pub fn discover(src: &str) -> FileTests {
    let toks = tokenize(src);
    let mut out = FileTests::default();
    let Some(pkg) = toks
        .iter()
        .position(|t| t.tok == Tok::Ident("package".into()))
        .and_then(|k| ident(&toks, k + 1))
        .map(str::to_string)
    else {
        return out;
    };
    let line_of = |byte: usize| src[..byte.min(src.len())].matches('\n').count();
    let mut k = 0;
    while k < toks.len() {
        if !punct(&toks, k, '@') {
            k += 1;
            continue;
        }
        let at = k;
        // `@test` or `@(test, …)`, possibly several attribute groups in a row.
        let mut is_test = false;
        let mut j = k;
        while punct(&toks, j, '@') {
            if punct(&toks, j + 1, '(') {
                let close = matching(&toks, j + 1);
                is_test |= (j + 2..close).any(|x| ident(&toks, x) == Some("test"));
                j = close + 1;
            } else {
                is_test |= ident(&toks, j + 1) == Some("test");
                j += 2;
            }
        }
        k = j;
        if !is_test {
            continue;
        }
        // `name :: proc(...) {`
        let (Some(name), true, true, Some("proc")) = (
            ident(&toks, j),
            punct(&toks, j + 1, ':'),
            punct(&toks, j + 2, ':'),
            ident(&toks, j + 3),
        ) else {
            continue;
        };
        let mut b = j + 4;
        while b < toks.len() && !punct(&toks, b, '{') {
            b += 1;
        }
        if b >= toks.len() {
            break;
        }
        let end = matching(&toks, b);
        let (start_byte, end_byte) = (toks[at].start, toks[end].end);
        out.tests.push(Found {
            name: name.to_string(),
            suite: Vec::new(),
            line: line_of(start_byte),
            end_line: line_of(end_byte),
            hash: hash_text(&src[start_byte..end_byte]),
            full_name: Some(format!("{pkg}.{name}")),
            framework: Framework::Odin,
        });
        k = end + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_test_procs() {
        let src = r#"package geometry

import "core:testing"

/* @(test) commented :: proc(t: ^testing.T) {} */
@(test)
adds :: proc(t: ^testing.T) {
	testing.expect(t, add(1, 2) == 3, "}")
}

helper :: proc() {}

@test
@(private)
scales :: proc(t: ^testing.T) { s := `@(test) x :: proc() {}` }

@(require_results)
not_a_test :: proc() -> int { return 1 }
"#;
        let found = discover(src);
        let names: Vec<_> = found
            .tests
            .iter()
            .map(|t| (t.full_name.clone().unwrap(), t.line, t.end_line))
            .collect();
        assert_eq!(
            names,
            vec![
                ("geometry.adds".to_string(), 5, 8),
                ("geometry.scales".to_string(), 12, 14),
            ]
        );
        // Whitespace-only edits keep the hash.
        let again = discover(&src.replace("add(1, 2)", "add(1,  2)"));
        assert_eq!(again.tests[0].hash, found.tests[0].hash);
    }
}
