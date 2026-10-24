//! Finding test methods in C# source: methods marked `[Fact]`, `[Theory]` (xUnit), `[Test]`,
//! `[TestCase]` (NUnit) or `[TestMethod]` (MSTest), with their namespace and class.

use super::{FileTests, Found, FoundSuite, Framework, hash_text};

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str,
    Punct(char),
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    start: usize,
    end: usize,
}

/// Identifiers and punctuation of C# source; strings, chars, comments and preprocessor lines
/// are skipped (strings leave a placeholder).
fn tokenize(src: &str) -> Vec<Token> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let at_line_start = std::mem::replace(&mut line_start, false);
        let start = i;
        if at_line_start && c == b'#' {
            i = src[i..].find('\n').map_or(b.len(), |n| i + n);
        } else if src[i..].starts_with("//") {
            i = src[i..].find('\n').map_or(b.len(), |n| i + n);
        } else if src[i..].starts_with("/*") {
            i = src[i + 2..].find("*/").map_or(b.len(), |n| i + 2 + n + 2);
        } else if let Some(len) = string_len(&src[i..]) {
            i += len;
            out.push(Token {
                tok: Tok::Str,
                start,
                end: i,
            });
        } else if c == b'\'' {
            // Char literal.
            i += 1;
            while i < b.len() && b[i] != b'\'' && b[i] != b'\n' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push(Token {
                tok: Tok::Str,
                start,
                end: i,
            });
        } else if c.is_ascii_alphanumeric() || c == b'_' || c == b'@' || c >= 0x80 {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80) {
                i += 1;
            }
            out.push(Token {
                tok: Tok::Ident(src[start..i].trim_start_matches('@').to_string()),
                start,
                end: i,
            });
        } else {
            let ch = src[i..].chars().next().unwrap_or(' ');
            i += ch.len_utf8();
            out.push(Token {
                tok: Tok::Punct(ch),
                start,
                end: i,
            });
        }
    }
    out
}

/// Length of a string literal at the start of `s` (`"…"`, `@"…"`, `$"…"`, `"""…"""` …).
fn string_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut k = 0;
    let mut verbatim = false;
    while k < b.len() && k < 3 && (b[k] == b'@' || b[k] == b'$') {
        verbatim |= b[k] == b'@';
        k += 1;
    }
    if b.get(k) != Some(&b'"') {
        return None;
    }
    let quotes = b[k..].iter().take_while(|&&q| q == b'"').count();
    if quotes >= 3 {
        let close = "\"".repeat(quotes);
        let body = k + quotes;
        return Some(
            s[body..]
                .find(&close)
                .map_or(s.len(), |n| body + n + quotes),
        );
    }
    if quotes == 2 && !verbatim {
        return Some(k + 2); // ""
    }
    let mut i = k + 1;
    let mut depth = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if !verbatim => i += 1,
            b'"' if verbatim && b.get(i + 1) == Some(&b'"') => i += 1,
            b'"' if depth == 0 => return Some(i + 1),
            b'\n' if !verbatim => return Some(i),
            b'{' => depth += 1,
            b'}' if depth > 0 => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    Some(b.len())
}

fn line_of(src: &str, byte: usize) -> usize {
    src[..byte.min(src.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
}

const TEST_ATTRS: &[&str] = &[
    "Fact",
    "Theory",
    "Test",
    "TestCase",
    "TestCaseSource",
    "TestMethod",
    "DataTestMethod",
];

#[derive(Debug)]
enum Scope {
    Namespace(String),
    /// A class (or record/struct) and where it starts.
    Class(String, usize),
    Other,
}

/// Test methods in a C# file.
pub fn discover(src: &str, framework: Framework) -> FileTests {
    let toks = tokenize(src);
    let mut out = FileTests::default();
    let mut scopes: Vec<Scope> = Vec::new();
    // A scope header seen but its `{` not yet: namespace or class name.
    let mut pending: Option<Scope> = None;
    let mut file_namespace: Option<String> = None;
    // Attributes collected before the next member: (names, start byte of the first).
    let mut attrs: Vec<String> = Vec::new();
    let mut attrs_start: Option<usize> = None;
    let mut k = 0;
    let namespace = |scopes: &[Scope], file_ns: &Option<String>| -> String {
        let mut parts: Vec<&str> = file_ns.iter().map(String::as_str).collect();
        parts.extend(scopes.iter().filter_map(|s| match s {
            Scope::Namespace(n) => Some(n.as_str()),
            _ => None,
        }));
        parts.join(".")
    };
    let classes = |scopes: &[Scope]| -> Vec<String> {
        scopes
            .iter()
            .filter_map(|s| match s {
                Scope::Class(n, _) => Some(n.clone()),
                _ => None,
            })
            .collect()
    };
    while k < toks.len() {
        match &toks[k].tok {
            Tok::Ident(w) if w == "namespace" => {
                // Dotted name up to `{` or `;`.
                let mut name = String::new();
                let mut j = k + 1;
                while let Some(t) = toks.get(j) {
                    match &t.tok {
                        Tok::Ident(p) => name.push_str(p),
                        Tok::Punct('.') => name.push('.'),
                        _ => break,
                    }
                    j += 1;
                }
                if toks.get(j).map(|t| &t.tok) == Some(&Tok::Punct(';')) {
                    file_namespace = Some(name);
                } else {
                    pending = Some(Scope::Namespace(name));
                }
                k = j;
                continue;
            }
            Tok::Ident(w) if matches!(w.as_str(), "class" | "record" | "struct" | "interface") => {
                if let Some(Tok::Ident(name)) = toks.get(k + 1).map(|t| &t.tok) {
                    let start = attrs_start.unwrap_or(toks[k].start);
                    pending = Some(Scope::Class(name.clone(), start));
                }
                attrs.clear();
                attrs_start = None;
            }
            Tok::Punct('[') => {
                // An attribute list: `[Fact]`, `[Theory, Trait("a", "b")]`, `[InlineData(1)]`.
                let close = matching(&toks, k);
                let mut j = k + 1;
                while j < close {
                    if let Tok::Ident(name) = &toks[j].tok {
                        let name = name.strip_suffix("Attribute").unwrap_or(name);
                        attrs.push(name.to_string());
                        // Skip the arguments.
                        if toks.get(j + 1).map(|t| &t.tok) == Some(&Tok::Punct('(')) {
                            j = matching(&toks, j + 1);
                        }
                    }
                    j += 1;
                }
                attrs_start.get_or_insert(toks[k].start);
                k = close + 1;
                continue;
            }
            Tok::Punct('(') if attrs.iter().any(|a| TEST_ATTRS.contains(&a.as_str())) => {
                // A test method: its name is the identifier before `(`.
                let name = match toks.get(k.wrapping_sub(1)).map(|t| &t.tok) {
                    Some(Tok::Ident(n)) => n.clone(),
                    _ => String::new(),
                };
                let params_end = matching(&toks, k);
                // Body: `{ … }` or `=> …;`.
                let mut j = params_end + 1;
                while j < toks.len() && !matches!(toks[j].tok, Tok::Punct('{' | ';' | '=')) {
                    j += 1;
                }
                let end = match toks.get(j).map(|t| &t.tok) {
                    Some(Tok::Punct('{')) => matching(&toks, j),
                    _ => (j..toks.len())
                        .find(|&x| toks[x].tok == Tok::Punct(';'))
                        .unwrap_or(toks.len() - 1),
                };
                let start = attrs_start.unwrap_or(toks[k].start);
                let ns = namespace(&scopes, &file_namespace);
                let cls = classes(&scopes);
                let full = [ns.clone(), cls.join("+"), name.clone()]
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(".");
                let fw = attrs
                    .iter()
                    .find_map(|a| match a.as_str() {
                        "Fact" | "Theory" => Some(Framework::XUnit),
                        "Test" | "TestCase" | "TestCaseSource" => Some(Framework::NUnit),
                        "TestMethod" | "DataTestMethod" => Some(Framework::MSTest),
                        _ => None,
                    })
                    .unwrap_or(framework);
                let end_byte = toks[end.min(toks.len() - 1)].end;
                out.tests.push(Found {
                    name,
                    suite: cls,
                    line: line_of(src, start),
                    end_line: line_of(src, end_byte),
                    hash: hash_text(&src[start..end_byte]),
                    full_name: Some(full),
                    framework: fw,
                });
                attrs.clear();
                attrs_start = None;
                k = end + 1;
                continue;
            }
            Tok::Punct('{') => {
                scopes.push(pending.take().unwrap_or(Scope::Other));
                attrs.clear();
                attrs_start = None;
            }
            Tok::Punct('}') => {
                if let Some(Scope::Class(name, start)) = scopes.last() {
                    let (name, start) = (name.clone(), *start);
                    let cls_path = classes(&scopes);
                    let ns = namespace(&scopes, &file_namespace);
                    if out.tests.iter().any(|t| t.suite == cls_path) {
                        let full = [ns, cls_path.join("+")]
                            .into_iter()
                            .filter(|p| !p.is_empty())
                            .collect::<Vec<_>>()
                            .join(".");
                        let _ = name;
                        out.suites.push(FoundSuite {
                            path: cls_path,
                            line: line_of(src, start),
                            end_line: line_of(src, toks[k].end),
                            full_name: Some(full),
                        });
                    }
                }
                scopes.pop();
                attrs.clear();
                attrs_start = None;
            }
            Tok::Punct(';') => {
                attrs.clear();
                attrs_start = None;
            }
            _ => {}
        }
        k += 1;
    }
    out.suites.sort_by_key(|s| s.line);
    out
}

fn matching(toks: &[Token], open: usize) -> usize {
    let (o, c) = match toks.get(open).map(|t| &t.tok) {
        Some(Tok::Punct('(')) => ('(', ')'),
        Some(Tok::Punct('{')) => ('{', '}'),
        Some(Tok::Punct('[')) => ('[', ']'),
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
    toks.len().saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"using Xunit;

namespace Inventory.Tests;

public sealed class ProductServiceTests : IDisposable
{
    private readonly string _s = "[Fact] not an attribute";

    [Fact]
    public async Task GetAll_ReturnsSeeded()
    {
        var x = new[] { 1, 2 };
        Assert.Equal(2, x.Length);
    }

    [Theory]
    [InlineData(1, "a")]
    [InlineData(2, "b}")]
    public void Parses(int n, string s) => Assert.True(n > 0);

    // [Fact] public void Commented() {}

    public class Nested
    {
        [Fact(Skip = "later")]
        public void Inner() { }
    }

    private static int Helper() => 1;
}
"#;

    #[test]
    fn finds_xunit_tests_with_full_names() {
        let ft = discover(SRC, Framework::XUnit);
        let got: Vec<(String, Option<String>, usize, usize)> = ft
            .tests
            .iter()
            .map(|t| (t.name.clone(), t.full_name.clone(), t.line, t.end_line))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "GetAll_ReturnsSeeded".into(),
                    Some("Inventory.Tests.ProductServiceTests.GetAll_ReturnsSeeded".into()),
                    8,
                    13
                ),
                (
                    "Parses".into(),
                    Some("Inventory.Tests.ProductServiceTests.Parses".into()),
                    15,
                    18
                ),
                (
                    "Inner".into(),
                    Some("Inventory.Tests.ProductServiceTests+Nested.Inner".into()),
                    24,
                    25
                ),
            ]
        );
        let suites: Vec<_> = ft
            .suites
            .iter()
            .map(|s| (s.full_name.clone().unwrap(), s.line))
            .collect();
        assert_eq!(
            suites,
            [
                ("Inventory.Tests.ProductServiceTests".into(), 4),
                ("Inventory.Tests.ProductServiceTests+Nested".into(), 22),
            ]
        );
    }

    #[test]
    fn nunit_mstest_and_block_namespaces() {
        let src = "namespace A.B {\n  [TestFixture]\n  public class T {\n    [Test]\n    public void One() {}\n    [TestCase(1)]\n    public void Two(int x) {}\n  }\n  [TestClass]\n  public class M {\n    [TestMethod]\n    public void Three() {}\n  }\n}\n";
        let ft = discover(src, Framework::NUnit);
        let got: Vec<_> = ft
            .tests
            .iter()
            .map(|t| (t.full_name.clone().unwrap(), t.framework))
            .collect();
        assert_eq!(
            got,
            [
                ("A.B.T.One".into(), Framework::NUnit),
                ("A.B.T.Two".into(), Framework::NUnit),
                ("A.B.M.Three".into(), Framework::MSTest),
            ]
        );
    }

    #[test]
    fn body_edits_change_the_hash() {
        let a = discover(SRC, Framework::XUnit);
        let b = discover(
            &SRC.replace("Assert.Equal(2", "Assert.Equal(3"),
            Framework::XUnit,
        );
        assert_ne!(a.tests[0].hash, b.tests[0].hash);
        assert_eq!(a.tests[1].hash, b.tests[1].hash);
    }
}
