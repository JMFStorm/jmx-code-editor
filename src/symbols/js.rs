//! Declarations in JavaScript / TypeScript (and the `<script>` blocks of `.vue` files):
//! functions, function-valued consts, classes and their methods, object-literal methods
//! (Vue's options API), with their JSDoc comments; and the file's imports.

use std::path::{Path, PathBuf};

use super::{Doc, DocParam, FileSymbols, Import, Kind, Symbol, line_col, one_line, split_params};
use crate::test_explorer::discover_js::{Tok, Token, matching, tokenize};

/// Words that may come before a declaration's name (and belong to it).
const MODIFIERS: &[&str] = &[
    "export",
    "default",
    "async",
    "static",
    "get",
    "set",
    "declare",
    "abstract",
    "public",
    "private",
    "protected",
    "readonly",
    "override",
];

/// Names that look like `name(…) {` but are statements.
const KEYWORDS: &[&str] = &[
    "if", "for", "while", "switch", "catch", "function", "return", "with", "else", "do", "try",
];

#[derive(Debug, Clone, PartialEq)]
enum Ctx {
    Class(String),
    /// An object literal, named by its variable or key when it has one.
    Object(Option<String>),
    Block,
}

pub fn scan(path: &Path, text: &str) -> FileSymbols {
    let is_vue = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("vue"));
    let src = if is_vue {
        script_only(text)
    } else {
        text.to_string()
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default")
        .to_string();
    let toks = tokenize(&src);
    let mut out = FileSymbols::default();
    let mut stack: Vec<Ctx> = Vec::new();
    let mut pending_class: Option<String> = None;
    let mut pending_object: Option<Option<String>> = None;
    let ident = |k: usize| match toks.get(k).map(|t| &t.tok) {
        Some(Tok::Ident(s)) => Some(s.as_str()),
        _ => None,
    };
    let punct =
        |k: usize, c: char| matches!(toks.get(k).map(|t| &t.tok), Some(Tok::Punct(p)) if *p == c);
    let mut k = 0;
    while k < toks.len() {
        let in_block = stack.contains(&Ctx::Block);
        // The outermost class / object: the container of members.
        let container = stack.iter().find_map(|c| match c {
            Ctx::Class(n) => Some(n.clone()),
            Ctx::Object(n) => Some(n.clone().unwrap_or_else(|| stem.clone())),
            Ctx::Block => None,
        });
        let top = stack.last().cloned();
        match &toks[k].tok {
            Tok::Punct('{') => {
                let ctx = if let Some(name) = pending_class.take() {
                    Ctx::Class(name)
                } else if let Some(name) = pending_object.take() {
                    Ctx::Object(name)
                } else if k == 0
                    || toks[k - 1].tok == Tok::Punct('=') && !punct(k.saturating_sub(2), '=')
                    || matches!(&toks[k - 1].tok, Tok::Punct('(' | ',' | ':' | '[' | '?'))
                    || matches!(ident(k - 1), Some("return" | "default"))
                {
                    let name = if punct(k - 1, ':') {
                        ident(k.wrapping_sub(2)).map(str::to_string)
                    } else {
                        None
                    };
                    Ctx::Object(name)
                } else {
                    Ctx::Block
                };
                stack.push(ctx);
            }
            Tok::Punct('}') => {
                stack.pop();
            }
            Tok::Punct(';') => {
                pending_class = None;
                pending_object = None;
            }
            Tok::Ident(word) => match word.as_str() {
                "import" if stack.is_empty() && !punct(k + 1, '(') && !punct(k + 1, '.') => {
                    k = parse_import(path, &toks, k, &mut out);
                    continue;
                }
                "export" if ident(k + 1) == Some("default") => {
                    if let Some(name) = ident(k + 2)
                        && !matches!(name, "function" | "class" | "async")
                    {
                        out.default_export = Some(name.to_string());
                    }
                }
                "class" if !punct(k.wrapping_sub(1), '.') => {
                    if let Some(name) = ident(k + 1) {
                        let end = (k..toks.len())
                            .find(|&j| punct(j, '{') || punct(j, ';'))
                            .unwrap_or(toks.len() - 1);
                        let start = decl_start(&toks, k);
                        out.symbols.push(Symbol {
                            name: name.to_string(),
                            kind: Kind::Class,
                            container: None,
                            signature: one_line(&src[toks[k].start..toks[end].start]),
                            params: vec![],
                            doc: doc_before(&src, &toks, start),
                            line: 0,
                            col: 0,
                        });
                        set_pos(&src, &toks[k + 1], out.symbols.last_mut());
                        note_default(&toks, start, name, &mut out);
                        pending_class = Some(name.to_string());
                    }
                }
                "function" => {
                    let name_at = if punct(k + 1, '*') { k + 2 } else { k + 1 };
                    if let Some(name) = ident(name_at)
                        && punct(name_at + 1, '(')
                    {
                        let start = decl_start(&toks, k);
                        let sym = callable(
                            &src,
                            &toks,
                            name,
                            Kind::Function,
                            container.clone().filter(|_| !in_block),
                            name_at,
                            name_at + 1,
                            start,
                        );
                        note_default(&toks, start, name, &mut out);
                        out.symbols.push(sym);
                    }
                }
                "const" | "let" | "var" if !in_block => {
                    if let Some(name) = ident(k + 1)
                        && punct(k + 2, '=')
                        && !punct(k + 3, '=')
                    {
                        let start = decl_start(&toks, k);
                        let v = if ident(k + 3) == Some("async") {
                            k + 4
                        } else {
                            k + 3
                        };
                        if let Some(open) = fn_value_params(&toks, v) {
                            out.symbols.push(callable(
                                &src,
                                &toks,
                                name,
                                Kind::Function,
                                container.clone(),
                                k + 1,
                                open,
                                start,
                            ));
                        } else {
                            if punct(v, '{') {
                                pending_object = Some(Some(name.to_string()));
                            }
                            let doc = doc_before(&src, &toks, start);
                            out.symbols.push(Symbol {
                                name: name.to_string(),
                                kind: Kind::Const,
                                container: container.clone(),
                                signature: format!("{word} {name}"),
                                params: vec![],
                                doc,
                                line: 0,
                                col: 0,
                            });
                            set_pos(&src, &toks[k + 1], out.symbols.last_mut());
                        }
                    }
                }
                name if matches!(top, Some(Ctx::Class(_) | Ctx::Object(_)))
                    && !in_block
                    && !KEYWORDS.contains(&name)
                    && !punct(k.wrapping_sub(1), '.')
                    // A value (`key: async (…) => …`), not a key.
                    && !punct(k.wrapping_sub(1), ':') =>
                {
                    let in_class = matches!(top, Some(Ctx::Class(_)));
                    let start = decl_start(&toks, k);
                    if punct(k + 1, '(') {
                        // `name(…) {` (TS: `name(…): Type {`).
                        let close = matching(&toks, k + 1);
                        let body = (close + 1..toks.len().min(close + 40))
                            .find(|&j| punct(j, '{') || punct(j, ';') || punct(j, ','));
                        if body.is_some_and(|j| punct(j, '{')) && !punct(close + 1, '=') {
                            let kind = if in_class && name == "constructor" {
                                Kind::Constructor
                            } else {
                                Kind::Method
                            };
                            out.symbols.push(callable(
                                &src,
                                &toks,
                                name,
                                kind,
                                container.clone(),
                                k,
                                k + 1,
                                start,
                            ));
                        }
                    } else if punct(k + 1, ':') || in_class && punct(k + 1, '=') {
                        // `name: function (…)`, `name: (…) =>`, class field `name = (…) =>`.
                        let v = if ident(k + 2) == Some("async") {
                            k + 3
                        } else {
                            k + 2
                        };
                        if let Some(open) = fn_value_params(&toks, v) {
                            out.symbols.push(callable(
                                &src,
                                &toks,
                                name,
                                Kind::Method,
                                container.clone(),
                                k,
                                open,
                                start,
                            ));
                        } else if punct(k + 1, ':') && punct(k + 2, '{') {
                            pending_object = Some(Some(name.to_string()));
                        } else if in_class {
                            let doc = doc_before(&src, &toks, start);
                            if !doc.is_empty() {
                                out.symbols.push(Symbol {
                                    name: name.to_string(),
                                    kind: Kind::Property,
                                    container: container.clone(),
                                    signature: name.to_string(),
                                    params: vec![],
                                    doc,
                                    line: 0,
                                    col: 0,
                                });
                                set_pos(&src, &toks[k], out.symbols.last_mut());
                            }
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
        k += 1;
    }
    out
}

/// A function symbol: `name` at token `name_at`, parameters in the parens opened at `open`
/// (or a single bare parameter), its doc before token `start`.
#[allow(clippy::too_many_arguments)]
fn callable(
    src: &str,
    toks: &[Token],
    name: &str,
    kind: Kind,
    container: Option<String>,
    name_at: usize,
    open: usize,
    start: usize,
) -> Symbol {
    let params_text = if toks[open].tok == Tok::Punct('(') {
        let close = matching(toks, open);
        &src[toks[open].end..toks[close].start.max(toks[open].end)]
    } else {
        // `x => …`
        &src[toks[open].start..toks[open].end]
    };
    let params = split_params(params_text);
    let (line, col) = line_col(src, toks[name_at].start);
    Symbol {
        name: name.to_string(),
        kind,
        container,
        signature: format!("{name}({})", params.join(", ")),
        params,
        doc: doc_before(src, toks, start),
        line,
        col,
    }
}

/// When the value at token `v` is a function, the token of its parameter list: `function (`,
/// `(…) =>`, `x =>`.
fn fn_value_params(toks: &[Token], v: usize) -> Option<usize> {
    let is =
        |k: usize, c: char| matches!(toks.get(k).map(|t| &t.tok), Some(Tok::Punct(p)) if *p == c);
    match toks.get(v).map(|t| &t.tok)? {
        Tok::Ident(w) if w == "function" => {
            let mut k = v + 1;
            if is(k, '*') {
                k += 1;
            }
            if matches!(toks.get(k).map(|t| &t.tok), Some(Tok::Ident(_))) {
                k += 1;
            }
            is(k, '(').then_some(k)
        }
        Tok::Punct('(') => {
            let close = matching(toks, v);
            // `(…) =>` or TS `(…): Type =>`.
            let arrow =
                (close + 1..toks.len().min(close + 30)).find(|&j| is(j, '=') && is(j + 1, '>'))?;
            let between_ok = arrow == close + 1 || is(close + 1, ':');
            between_ok.then_some(v)
        }
        Tok::Ident(_) if is(v + 1, '=') && is(v + 2, '>') => Some(v),
        _ => None,
    }
}

/// The first token of the declaration whose keyword or name is at `k` (modifiers included).
fn decl_start(toks: &[Token], k: usize) -> usize {
    let mut s = k;
    while s > 0
        && matches!(&toks[s - 1].tok, Tok::Ident(w) if MODIFIERS.contains(&w.as_str()) || w == "const" || w == "let" || w == "var")
    {
        s -= 1;
    }
    s
}

fn set_pos(src: &str, tok: &Token, sym: Option<&mut Symbol>) {
    if let Some(s) = sym {
        (s.line, s.col) = line_col(src, tok.start);
    }
}

fn note_default(toks: &[Token], start: usize, name: &str, out: &mut FileSymbols) {
    let words: Vec<&str> = toks[start..]
        .iter()
        .take(3)
        .filter_map(|t| match &t.tok {
            Tok::Ident(w) => Some(w.as_str()),
            _ => None,
        })
        .collect();
    if words.starts_with(&["export", "default"]) {
        out.default_export = Some(name.to_string());
    }
}

/// The JSDoc comment right before token `start` (only whitespace in between).
fn doc_before(src: &str, toks: &[Token], start: usize) -> Doc {
    let from = if start == 0 { 0 } else { toks[start - 1].end };
    let gap = &src[from..toks[start].start];
    let Some(open) = gap.rfind("/**") else {
        return Doc::default();
    };
    let Some(close) = gap[open..].find("*/").map(|n| open + n + 2) else {
        return Doc::default();
    };
    if !gap[close..].trim().is_empty() {
        return Doc::default();
    }
    parse_jsdoc(&gap[open..close])
}

/// `/** Summary. @param {T} name - text @returns {T} text */`.
pub fn parse_jsdoc(comment: &str) -> Doc {
    let body = comment.trim_start_matches("/**").trim_end_matches("*/");
    let lines: Vec<&str> = body
        .lines()
        .map(|l| {
            let l = l.trim();
            let l = l.strip_prefix('*').unwrap_or(l);
            l.strip_prefix(' ').unwrap_or(l)
        })
        .collect();
    // Split into the description and `@tag` blocks (a tag runs until the next one).
    let mut summary: Vec<&str> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    for l in lines {
        if l.trim_start().starts_with('@') {
            tags.push(l.trim().to_string());
        } else if let Some(t) = tags.last_mut() {
            t.push(' ');
            t.push_str(l.trim());
        } else {
            summary.push(l);
        }
    }
    let mut doc = Doc {
        summary: inline_tags(&one_line(&summary.join("\n"))),
        ..Default::default()
    };
    for tag in tags {
        let (name, rest) = tag.split_once(char::is_whitespace).unwrap_or((&tag, ""));
        match name {
            "@param" | "@arg" | "@argument" => {
                let (ty, rest) = take_type(rest);
                let rest = rest.trim();
                let (pname, text) = if let Some(r) = rest.strip_prefix('[') {
                    let close = r.find(']').unwrap_or(r.len());
                    (
                        r[..close].split('=').next().unwrap_or(""),
                        r.get(close + 1..).unwrap_or(""),
                    )
                } else {
                    rest.split_once(char::is_whitespace).unwrap_or((rest, ""))
                };
                let text = text.trim().trim_start_matches('-').trim();
                doc.params.push(DocParam {
                    name: pname.trim().to_string(),
                    ty: one_line(&ty),
                    text: inline_tags(&one_line(text)),
                });
            }
            "@returns" | "@return" => {
                let (ty, rest) = take_type(rest);
                let text = rest.trim().trim_start_matches('-').trim();
                doc.returns = Some((one_line(&ty), inline_tags(&one_line(text))));
            }
            "@type" => doc.ty = one_line(&take_type(rest).0),
            _ => {}
        }
    }
    doc
}

/// A leading `{type}` (braces may nest) and the rest.
fn take_type(s: &str) -> (String, &str) {
    let s = s.trim_start();
    if !s.starts_with('{') {
        return (String::new(), s);
    }
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return (s[1..i].to_string(), &s[i + 1..]);
                }
            }
            _ => {}
        }
    }
    (s[1..].to_string(), "")
}

/// `{@link Foo}` / `{@link Foo|text}` → `Foo` / `text`.
fn inline_tags(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("{@") {
        out.push_str(&rest[..i]);
        let Some(close) = rest[i..].find('}') else {
            rest = &rest[i..];
            break;
        };
        let inner = &rest[i + 2..i + close];
        let inner = inner.split_once(char::is_whitespace).map_or("", |(_, r)| r);
        out.push_str(inner.rsplit('|').next().unwrap_or(inner).trim());
        rest = &rest[i + close + 1..];
    }
    out.push_str(rest);
    out
}

/// `import a, { b as c } from "./x"`: adds the imports and returns the token after them.
fn parse_import(path: &Path, toks: &[Token], k: usize, out: &mut FileSymbols) -> usize {
    let mut j = k + 1;
    let mut found: Vec<(String, Option<String>)> = Vec::new();
    let ident = |j: usize| match toks.get(j).map(|t| &t.tok) {
        Some(Tok::Ident(s)) => Some(s.clone()),
        _ => None,
    };
    let is =
        |j: usize, c: char| matches!(toks.get(j).map(|t| &t.tok), Some(Tok::Punct(p)) if *p == c);
    // TS `import type …`.
    if ident(j).as_deref() == Some("type") && (ident(j + 1).is_some() || is(j + 1, '{')) {
        j += 1;
    }
    if let Some(name) = ident(j)
        && name != "from"
    {
        found.push((name, None));
        j += 1;
        if is(j, ',') {
            j += 1;
        }
    }
    if is(j, '*') {
        // `* as ns`: nothing to resolve by name.
        j += 3;
    } else if is(j, '{') {
        j += 1;
        while j < toks.len() && !is(j, '}') {
            if let Some(imported) = ident(j) {
                if ident(j + 1).as_deref() == Some("as")
                    && let Some(local) = ident(j + 2)
                {
                    found.push((local, Some(imported)));
                    j += 3;
                    continue;
                }
                if imported != "type" {
                    found.push((imported.clone(), Some(imported)));
                }
            }
            j += 1;
        }
        j += 1;
    }
    if ident(j).as_deref() == Some("from")
        && let Some(Tok::Str(spec)) = toks.get(j + 1).map(|t| &t.tok)
    {
        if let Some(from) = resolve_import(path, spec) {
            out.imports
                .extend(found.into_iter().map(|(local, imported)| Import {
                    local,
                    imported,
                    from: from.clone(),
                }));
        } else if !spec.starts_with('.') && !spec.starts_with('/') {
            out.packages
                .extend(found.into_iter().map(|(local, _)| (local, spec.clone())));
        }
        return j + 2;
    }
    j.max(k + 1)
}

/// The file an import specifier names: relative paths, and `@/…` for the project's `src`.
pub fn resolve_import(file: &Path, spec: &str) -> Option<PathBuf> {
    let base = if spec.starts_with("./") || spec.starts_with("../") {
        file.parent()?.join(spec)
    } else {
        let rest = spec.strip_prefix("@/")?;
        let root = file
            .ancestors()
            .skip(1)
            .find(|d| d.join("package.json").is_file())?;
        root.join("src").join(rest)
    };
    const EXTS: &[&str] = &["js", "ts", "vue", "mjs", "jsx", "tsx", "cjs", "mts"];
    let mut candidates = vec![base.clone()];
    for e in EXTS {
        let mut s = base.clone().into_os_string();
        s.push(format!(".{e}"));
        candidates.push(PathBuf::from(s));
    }
    for e in EXTS {
        candidates.push(base.join(format!("index.{e}")));
    }
    let found = candidates.into_iter().find(|p| p.is_file())?;
    Some(std::fs::canonicalize(&found).unwrap_or(found))
}

/// A `.vue` file with everything outside its `<script>` blocks blanked (lines and columns
/// stay where they were).
fn script_only(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(open) = rest.find("<script") else {
            blank(rest, &mut out);
            break;
        };
        let Some(tag_end) = rest[open..].find('>').map(|n| open + n + 1) else {
            blank(rest, &mut out);
            break;
        };
        blank(&rest[..tag_end], &mut out);
        let close = rest[tag_end..]
            .find("</script>")
            .map_or(rest.len(), |n| tag_end + n);
        out.push_str(&rest[tag_end..close]);
        rest = &rest[close..];
    }
    out
}

fn blank(s: &str, out: &mut String) {
    out.extend(s.chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(f: &FileSymbols) -> Vec<(String, Kind, Option<String>)> {
        f.symbols
            .iter()
            .map(|s| (s.name.clone(), s.kind, s.container.clone()))
            .collect()
    }

    #[test]
    fn functions_with_jsdoc() {
        let src = r#"
/**
 * Format a number as currency.
 *
 * @param {number} amount - Amount in major units (e.g. euros).
 * @param {string} [currency] - ISO 4217 code.
 * @param {string} [locale='en-US'] BCP 47 locale tag.
 * @returns {string} Formatted amount, see {@link formatPercent}.
 */
export function formatCurrency(amount, currency = 'EUR', locale = 'en-US') {
  const inner = 1;
  function helper(x) {}
}

// Not a doc.
export const double = (n) => n * 2;
/** Halves. */
const half = async function (n) { return n / 2; };
const neg = n => -n;
/** @type {number} */
const LIMIT = 10;
"#;
        let f = scan(Path::new("/w/format.js"), src);
        let fc = &f.symbols[0];
        assert_eq!(fc.name, "formatCurrency");
        assert_eq!(fc.kind, Kind::Function);
        assert_eq!(
            fc.signature,
            "formatCurrency(amount, currency = 'EUR', locale = 'en-US')"
        );
        assert_eq!((fc.line, fc.col), (9, 16));
        assert_eq!(fc.doc.summary, "Format a number as currency.");
        assert_eq!(fc.doc.params.len(), 3);
        assert_eq!(fc.doc.params[0].ty, "number");
        assert_eq!(fc.doc.params[0].text, "Amount in major units (e.g. euros).");
        assert_eq!(fc.doc.params[1].name, "currency");
        assert_eq!(fc.doc.params[2].name, "locale");
        assert_eq!(fc.doc.params[2].text, "BCP 47 locale tag.");
        assert_eq!(
            fc.doc.returns,
            Some((
                "string".into(),
                "Formatted amount, see formatPercent.".into()
            ))
        );
        let n: Vec<_> = f.symbols.iter().map(|s| s.name.as_str()).collect();
        // Locals of a function body are not indexed; nested function declarations are.
        assert_eq!(
            n,
            ["formatCurrency", "helper", "double", "half", "neg", "LIMIT"]
        );
        assert!(f.symbols[2].doc.is_empty()); // a `//` comment is no doc
        assert_eq!(f.symbols[3].doc.summary, "Halves.");
        assert_eq!(f.symbols[4].params, ["n"]);
        assert_eq!(f.symbols[5].doc.ty, "number");
        assert_eq!(f.symbols[5].kind, Kind::Const);
    }

    #[test]
    fn classes_objects_and_vue_options() {
        let src = r#"
export default class Store extends Base {
  /** Make one. */
  constructor(opts) { super(); }
  static get count() { return 1; }
  async load(id) { if (x) { foo(1); } }
  onClick = (e) => {};
}
const api = {
  /** Fetch it. */
  get(url) { return fetch(url); },
  post: async (url, body) => {},
};
"#;
        let f = scan(Path::new("/w/store.js"), src);
        assert_eq!(
            names(&f),
            [
                ("Store".into(), Kind::Class, None),
                (
                    "constructor".into(),
                    Kind::Constructor,
                    Some("Store".into())
                ),
                ("count".into(), Kind::Method, Some("Store".into())),
                ("load".into(), Kind::Method, Some("Store".into())),
                ("onClick".into(), Kind::Method, Some("Store".into())),
                ("api".into(), Kind::Const, None),
                ("get".into(), Kind::Method, Some("api".into())),
                ("post".into(), Kind::Method, Some("api".into())),
            ]
        );
        assert_eq!(f.symbols[0].signature, "class Store extends Base");
        assert_eq!(f.symbols[1].doc.summary, "Make one.");
        assert_eq!(f.symbols[6].doc.summary, "Fetch it.");
        assert_eq!(f.default_export.as_deref(), Some("Store"));

        let vue = "<template>\n  <div @click=\"save(1)\">x</div>\n</template>\n<script>\nexport default {\n  methods: {\n    /** Save it. */\n    save(n) { this.n = n; },\n  },\n};\n</script>\n";
        let f = scan(Path::new("/w/Card.vue"), vue);
        assert_eq!(
            names(&f),
            [("save".into(), Kind::Method, Some("Card".into()))]
        );
        assert_eq!((f.symbols[0].line, f.symbols[0].col), (7, 4));
        assert_eq!(f.symbols[0].doc.summary, "Save it.");
    }

    #[test]
    fn imports_resolve_to_files() {
        let dir = std::env::temp_dir().join(format!("jmx-sym-imp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/utils")).unwrap();
        std::fs::create_dir_all(dir.join("src/components")).unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        std::fs::write(dir.join("src/utils/format.js"), "").unwrap();
        std::fs::write(dir.join("src/components/Card.vue"), "").unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let file = dir.join("src/App.vue");
        let src = "<script setup>\nimport Card from './components/Card.vue';\nimport { formatDate as fd, formatCurrency } from \"@/utils/format\";\nimport { ref } from 'vue';\nimport type { X } from './nope';\n</script>";
        let f = scan(&file, src);
        assert_eq!(f.packages, [("ref".to_string(), "vue".to_string())]);
        let fmt = dir.join("src/utils/format.js");
        assert_eq!(
            f.imports,
            [
                Import {
                    local: "Card".into(),
                    imported: None,
                    from: dir.join("src/components/Card.vue"),
                },
                Import {
                    local: "fd".into(),
                    imported: Some("formatDate".into()),
                    from: fmt.clone(),
                },
                Import {
                    local: "formatCurrency".into(),
                    imported: Some("formatCurrency".into()),
                    from: fmt,
                },
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
