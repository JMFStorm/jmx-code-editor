//! Finding tests in JavaScript/TypeScript source: `describe` / `it` / `test` calls in spec files,
//! and exported stories in Storybook files. Static and tolerant: it works on half-typed code.

use std::path::Path;

use super::{FileTests, Found, FoundSuite, Framework, hash_text};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    Ident(String),
    /// A string or template literal's text.
    Str(String),
    Punct(char),
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub tok: Tok,
    /// Byte offset of the token start and end in the source.
    pub start: usize,
    pub end: usize,
}

/// Split JS source into identifiers, string literals and punctuation, skipping comments.
/// Template literals become one string (their `${}` parts are skipped).
pub(crate) fn tokenize(src: &str) -> Vec<Token> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if src[i..].starts_with("//") {
            i = src[i..].find('\n').map_or(b.len(), |n| i + n);
        } else if src[i..].starts_with("/*") {
            i = src[i + 2..].find("*/").map_or(b.len(), |n| i + 2 + n + 2);
        } else if c == b'\'' || c == b'"' {
            let start = i;
            i += 1;
            let mut text = String::new();
            while i < b.len() && b[i] != c && b[i] != b'\n' {
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 1;
                }
                let ch = src[i..].chars().next().unwrap_or(' ');
                text.push(ch);
                i += ch.len_utf8();
            }
            i = (i + 1).min(b.len());
            out.push(Token {
                tok: Tok::Str(text),
                start,
                end: i,
            });
        } else if c == b'`' {
            let start = i;
            i += 1;
            let mut text = String::new();
            let mut depth = 0usize;
            while i < b.len() {
                if depth == 0 && b[i] == b'`' {
                    i += 1;
                    break;
                }
                if b[i] == b'\\' && i + 1 < b.len() {
                    i += 2;
                    continue;
                }
                if src[i..].starts_with("${") {
                    depth += 1;
                    text.push_str("${…}");
                    i += 2;
                    continue;
                }
                if depth > 0 {
                    match b[i] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                    continue;
                }
                let ch = src[i..].chars().next().unwrap_or(' ');
                text.push(ch);
                i += ch.len_utf8();
            }
            out.push(Token {
                tok: Tok::Str(text),
                start,
                end: i,
            });
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80 {
            let start = i;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] >= 0x80)
            {
                i += 1;
            }
            out.push(Token {
                tok: Tok::Ident(src[start..i].to_string()),
                start,
                end: i,
            });
        } else {
            let ch = src[i..].chars().next().unwrap_or(' ');
            out.push(Token {
                tok: Tok::Punct(ch),
                start: i,
                end: i + ch.len_utf8(),
            });
            i += ch.len_utf8();
        }
    }
    out
}

/// Index of the token closing the bracket opened at `open` (or the last token).
pub(crate) fn matching(toks: &[Token], open: usize) -> usize {
    let (o, c) = match toks[open].tok {
        Tok::Punct('(') => ('(', ')'),
        Tok::Punct('{') => ('{', '}'),
        Tok::Punct('[') => ('[', ']'),
        _ => return open,
    };
    let mut depth = 0;
    for (k, t) in toks.iter().enumerate().skip(open) {
        match t.tok {
            Tok::Punct(p) if p == o => depth += 1,
            Tok::Punct(p) if p == c => {
                depth -= 1;
                if depth == 0 {
                    return k;
                }
            }
            _ => {}
        }
    }
    toks.len() - 1
}

fn line_of(src: &str, byte: usize) -> usize {
    src[..byte.min(src.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
}

/// `describe` / `it` / `test` calls in a spec file.
pub fn specs(src: &str, framework: Framework) -> FileTests {
    let toks = tokenize(src);
    // (is_suite, name, start byte, end byte)
    let mut calls: Vec<(bool, String, usize, usize)> = Vec::new();
    let mut k = 0;
    while k < toks.len() {
        let Tok::Ident(word) = &toks[k].tok else {
            k += 1;
            continue;
        };
        let is_suite = matches!(word.as_str(), "describe" | "suite");
        let is_test = matches!(word.as_str(), "it" | "test" | "bench");
        // Not a method call on something else (`foo.it(`).
        let after_dot = k > 0 && toks[k - 1].tok == Tok::Punct('.');
        if !(is_suite || is_test) || after_dot {
            k += 1;
            continue;
        }
        let start = toks[k].start;
        let mut j = k + 1;
        // Modifiers: `.only`, `.skip`, `.each(...)`, `.each\`table\``, `.concurrent` ...
        while j + 1 < toks.len() && toks[j].tok == Tok::Punct('.') {
            let Tok::Ident(m) = &toks[j + 1].tok else {
                break;
            };
            j += 2;
            if m == "each" || m == "for" {
                match toks.get(j).map(|t| &t.tok) {
                    Some(Tok::Punct('(')) => j = matching(&toks, j) + 1,
                    Some(Tok::Str(_)) => j += 1, // tagged template table
                    _ => {}
                }
            }
        }
        if toks.get(j).map(|t| &t.tok) != Some(&Tok::Punct('(')) {
            k += 1;
            continue;
        }
        let close = matching(&toks, j);
        // The name: a string, or an identifier / expression as written.
        let name = match toks.get(j + 1).map(|t| &t.tok) {
            Some(Tok::Str(s)) => s.clone(),
            Some(Tok::Ident(s)) => s.clone(),
            _ => {
                k += 1;
                continue;
            }
        };
        calls.push((is_suite, name, start, toks[close].end));
        k = j + 1; // keep scanning inside (nested describes and tests)
    }
    let mut out = FileTests::default();
    let suites: Vec<&(bool, String, usize, usize)> = calls.iter().filter(|c| c.0).collect();
    let path_at = |pos: usize, own_start: usize| -> Vec<String> {
        suites
            .iter()
            .filter(|s| s.2 < pos && pos < s.3 && s.2 != own_start)
            .map(|s| s.1.clone())
            .collect()
    };
    for (is_suite, name, start, end) in &calls {
        let (line, end_line) = (line_of(src, *start), line_of(src, *end));
        let suite = path_at(*start, *start);
        if *is_suite {
            let mut path = suite;
            path.push(name.clone());
            out.suites.push(FoundSuite {
                path,
                line,
                end_line,
                full_name: None,
            });
        } else {
            out.tests.push(Found {
                name: name.clone(),
                suite,
                line,
                end_line,
                hash: hash_text(&src[*start..*end]),
                full_name: None,
                framework,
            });
        }
    }
    out
}

/// Stories in a Storybook file: every `export const Name` (the default export is the meta).
pub fn stories(path: &Path, src: &str) -> FileTests {
    let toks = tokenize(src);
    // The component title from `title: '...'`, else the file name.
    let title = toks
        .windows(3)
        .find_map(|w| match (&w[0].tok, &w[1].tok, &w[2].tok) {
            (Tok::Ident(t), Tok::Punct(':'), Tok::Str(s)) if t == "title" => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            let name = path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            name.split(".stories").next().unwrap_or(&name).to_string()
        });
    // Top-level `export` statements (brace depth 0).
    let mut exports: Vec<(usize, Option<String>)> = Vec::new();
    let mut depth = 0i32;
    for (k, t) in toks.iter().enumerate() {
        match &t.tok {
            Tok::Punct('{' | '(' | '[') => depth += 1,
            Tok::Punct('}' | ')' | ']') => depth -= 1,
            Tok::Ident(w) if w == "export" && depth == 0 => {
                let name = match (
                    toks.get(k + 1).map(|t| &t.tok),
                    toks.get(k + 2).map(|t| &t.tok),
                ) {
                    (Some(Tok::Ident(kw)), Some(Tok::Ident(n)))
                        if matches!(kw.as_str(), "const" | "let" | "var" | "function") =>
                    {
                        Some(n.clone())
                    }
                    _ => None,
                };
                exports.push((k, name));
            }
            _ => {}
        }
    }
    let mut out = FileTests::default();
    for (n, (k, name)) in exports.iter().enumerate() {
        let Some(name) = name else {
            continue;
        };
        let start = toks[*k].start;
        let end = exports
            .get(n + 1)
            .map_or(src.len(), |(next, _)| toks[*next].start);
        let text = src[start..end].trim_end();
        out.tests.push(Found {
            name: name.clone(),
            suite: vec![title.clone()],
            line: line_of(src, start),
            end_line: line_of(src, start + text.len()),
            hash: hash_text(text),
            full_name: None,
            framework: Framework::Storybook,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"import { describe, it, expect } from 'vitest';

describe('isEmail', () => {
  it.each(['a@b.fi', 'c@d.fi'])('accepts %s', (email) => {
    expect(isEmail(email)).toBe(true);
  });

  // it('commented out', () => {});
  it(`handles ${'template'} names`, () => {
    expect(x).toBe('it("not a test")');
  });

  describe('nested', () => {
    test.skip('deep', async () => {
      await thing();
    });
  });
});

it('top level', () => {});
"#;

    #[test]
    fn finds_specs_with_suites_and_lines() {
        let ft = specs(SPEC, Framework::Vitest);
        let got: Vec<(String, Vec<String>, usize, usize)> = ft
            .tests
            .iter()
            .map(|t| (t.name.clone(), t.suite.clone(), t.line, t.end_line))
            .collect();
        assert_eq!(
            got,
            [
                ("accepts %s".into(), vec!["isEmail".into()], 3, 5),
                ("handles ${…} names".into(), vec!["isEmail".into()], 8, 10),
                (
                    "deep".into(),
                    vec!["isEmail".into(), "nested".into()],
                    13,
                    15
                ),
                ("top level".into(), vec![], 19, 19),
            ]
        );
        let suites: Vec<_> = ft
            .suites
            .iter()
            .map(|s| (s.path.join(" › "), s.line, s.end_line))
            .collect();
        assert_eq!(
            suites,
            [
                ("isEmail".into(), 2, 17),
                ("isEmail › nested".into(), 12, 16)
            ]
        );
    }

    #[test]
    fn hashes_change_only_with_the_test() {
        let a = specs(SPEC, Framework::Vitest);
        let edited = SPEC.replace("toBe(true)", "toBe(false)");
        let b = specs(&edited, Framework::Vitest);
        assert_ne!(a.tests[0].hash, b.tests[0].hash);
        assert_eq!(a.tests[1].hash, b.tests[1].hash);
        // Half-typed code still yields the complete tests.
        let partial = format!("{SPEC}\nit('new one', () => {{\n  expect(");
        let c = specs(&partial, Framework::Vitest);
        assert_eq!(c.tests.last().unwrap().name, "new one");
    }

    #[test]
    fn finds_stories() {
        let src = "import StatCard from './StatCard.vue';\n\nexport default {\n  title: 'Dashboard/StatCard',\n  component: StatCard,\n};\n\nexport const Default = {\n  args: { value: 1 },\n};\n\nexport const Loading = { args: { loading: true } };\n";
        let ft = stories(Path::new("src/StatCard.stories.js"), src);
        let names: Vec<_> = ft
            .tests
            .iter()
            .map(|t| (t.name.as_str(), t.suite[0].as_str(), t.line))
            .collect();
        assert_eq!(
            names,
            [
                ("Default", "Dashboard/StatCard", 7),
                ("Loading", "Dashboard/StatCard", 11)
            ]
        );
        let untitled = stories(Path::new("Button.stories.ts"), "export const Primary = {};");
        assert_eq!(untitled.tests[0].suite, ["Button"]);
    }
}
