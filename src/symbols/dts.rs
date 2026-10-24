//! TypeScript declarations (`.d.ts` files, and the types in `.ts` files): exported
//! functions (overloads included), constants, interfaces with their members, type aliases,
//! classes and enums, with their JSDoc; and the re-exports that npm packages use to expose
//! them (`export * from`, `export { a as b } from`, `const x: typeof y`).

use super::js::{decl_start, doc_before};
use super::{Kind, Symbol, line_col, one_line, split_params};
use crate::test_explorer::discover_js::{Tok, Token, matching, tokenize};

/// Signatures longer than this are cut.
const MAX_SIGNATURE: usize = 200;
/// Interface members kept for the popup.
const MAX_MEMBERS: usize = 60;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DtsFile {
    pub symbols: Vec<Symbol>,
    /// `export * from 'spec'`
    pub star: Vec<String>,
    /// `export { original as exported } [from 'spec']`: (exported, original, spec).
    pub reexports: Vec<(String, String, Option<String>)>,
    /// `import { imported as local } from 'spec'`: (local, imported, spec).
    pub imports: Vec<(String, String, String)>,
    /// `declare const name: typeof local`: (name, local).
    pub typeof_alias: Vec<(String, String)>,
}

pub fn scan(src: &str) -> DtsFile {
    let toks = tokenize(src);
    let mut out = DtsFile::default();
    // Open braces: true when transparent (`declare module '…' {`, `namespace X {`).
    let mut stack: Vec<bool> = Vec::new();
    let ident = |k: usize| match toks.get(k).map(|t| &t.tok) {
        Some(Tok::Ident(s)) => Some(s.as_str()),
        _ => None,
    };
    let string = |k: usize| match toks.get(k).map(|t| &t.tok) {
        Some(Tok::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let punct =
        |k: usize, c: char| matches!(toks.get(k).map(|t| &t.tok), Some(Tok::Punct(p)) if *p == c);
    let mut k = 0;
    while k < toks.len() {
        let top = stack.iter().all(|t| *t);
        match &toks[k].tok {
            Tok::Punct('{') => {
                // `declare module 'x' {`, `namespace A.B {`, `declare global {`: their
                // declarations count as top level.
                let mut j = k;
                while j > 0 && (ident(j - 1).is_some() || punct(j - 1, '.')) {
                    j -= 1;
                }
                let transparent = (k >= 2
                    && string(k - 1).is_some()
                    && ident(k - 2) == Some("module"))
                    || (j < k
                        && (matches!(ident(j), Some("namespace" | "module" | "global"))
                            || (j > 0 && matches!(ident(j - 1), Some("namespace" | "module")))));
                stack.push(transparent);
            }
            Tok::Punct('}') => {
                stack.pop();
            }
            Tok::Ident(w) if top => match w.as_str() {
                "import" => {
                    k = parse_import(&toks, k, &mut out);
                    continue;
                }
                "export" if punct(k + 1, '*') => {
                    // `export * from 's'` (not `export * as ns from`).
                    if ident(k + 2) == Some("from")
                        && let Some(s) = string(k + 3)
                    {
                        out.star.push(s);
                    }
                }
                "export" if punct(k + 1, '{') => {
                    let close = matching(&toks, k + 1);
                    let spec = (ident(close + 1) == Some("from"))
                        .then(|| string(close + 2))
                        .flatten();
                    let mut j = k + 2;
                    while j < close {
                        if let Some(orig) = ident(j) {
                            if orig == "type"
                                && ident(j + 1).is_some()
                                && ident(j + 1) != Some("as")
                            {
                                j += 1;
                                continue;
                            }
                            let (exported, step) = if ident(j + 1) == Some("as") {
                                (ident(j + 2).unwrap_or(orig).to_string(), 3)
                            } else {
                                (orig.to_string(), 1)
                            };
                            out.reexports
                                .push((exported, orig.to_string(), spec.clone()));
                            j += step;
                        } else {
                            j += 1;
                        }
                    }
                    k = close + 1;
                    continue;
                }
                "export" if ident(k + 1) == Some("default") => {
                    if let Some(name) = ident(k + 2)
                        && !matches!(name, "function" | "class" | "interface" | "abstract")
                    {
                        out.reexports
                            .push(("default".into(), name.to_string(), None));
                    }
                }
                "function" => {
                    if let Some(name) = ident(k + 1) {
                        let mut j = k + 2;
                        if punct(j, '<') {
                            j = skip_angles(&toks, j);
                        }
                        if punct(j, '(') {
                            let close = matching(&toks, j);
                            let end = statement_end(&toks, close + 1);
                            let params = split_params(&src[toks[j].end..toks[close].start]);
                            let start = decl_start(&toks, k);
                            push(
                                &mut out,
                                src,
                                &toks,
                                name,
                                Kind::Function,
                                k + 1,
                                end,
                                start,
                                params,
                            );
                            if is_default(&toks, start) {
                                out.reexports
                                    .push(("default".into(), name.to_string(), None));
                            }
                        }
                    }
                }
                "const" | "let" | "var" => {
                    if let Some(name) = ident(k + 1)
                        && punct(k + 2, ':')
                    {
                        let end = statement_end(&toks, k + 3);
                        // `declare const computed: typeof computed$1;`
                        if ident(k + 3) == Some("typeof")
                            && let Some(target) = ident(k + 4)
                            && end == k + 5
                        {
                            out.typeof_alias
                                .push((name.to_string(), target.to_string()));
                        }
                        let start = decl_start(&toks, k);
                        push(
                            &mut out,
                            src,
                            &toks,
                            name,
                            Kind::Const,
                            k,
                            end,
                            start,
                            Vec::new(),
                        );
                    }
                }
                "interface" | "class" | "enum" => {
                    if let Some(name) = ident(k + 1) {
                        let open = (k + 2..toks.len())
                            .find(|&j| punct(j, '{'))
                            .unwrap_or(toks.len() - 1);
                        let kind = match w.as_str() {
                            "interface" => Kind::Interface,
                            "enum" => Kind::Enum,
                            _ => Kind::Class,
                        };
                        let start = decl_start(&toks, k);
                        let members = if kind == Kind::Interface {
                            let close = matching(&toks, open);
                            members(&src[toks[open].end..toks[close].start])
                        } else {
                            Vec::new()
                        };
                        push(&mut out, src, &toks, name, kind, k, open, start, members);
                        if is_default(&toks, start) {
                            out.reexports
                                .push(("default".into(), name.to_string(), None));
                        }
                    }
                }
                "type" => {
                    // `type Name<…> = …;` (not a property called `type`).
                    if let Some(name) = ident(k + 1)
                        && (punct(k + 2, '=') || punct(k + 2, '<'))
                    {
                        let end = statement_end(&toks, k + 2);
                        let start = decl_start(&toks, k);
                        push(
                            &mut out,
                            src,
                            &toks,
                            name,
                            Kind::Type,
                            k,
                            end,
                            start,
                            Vec::new(),
                        );
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

/// Add a declaration: its signature runs from token `from` up to (not including) `end`.
#[allow(clippy::too_many_arguments)]
fn push(
    out: &mut DtsFile,
    src: &str,
    toks: &[Token],
    name: &str,
    kind: Kind,
    from: usize,
    end: usize,
    start: usize,
    params: Vec<String>,
) {
    let name_tok = (from..toks.len().min(from + 3))
        .find(|&j| matches!(&toks[j].tok, Tok::Ident(w) if w == name))
        .unwrap_or(from);
    let stop = toks.get(end).map_or(src.len(), |t| t.start);
    let mut signature = one_line(&src[toks[from].start..stop.max(toks[from].end)]);
    if signature.chars().count() > MAX_SIGNATURE {
        signature = format!(
            "{}…",
            signature.chars().take(MAX_SIGNATURE).collect::<String>()
        );
    }
    let (line, col) = line_col(src, toks[name_tok].start);
    out.symbols.push(Symbol {
        name: name.to_string(),
        kind,
        container: None,
        signature,
        params,
        doc: doc_before(src, toks, start),
        line,
        col,
    });
}

fn is_default(toks: &[Token], start: usize) -> bool {
    toks[start..]
        .iter()
        .take(3)
        .any(|t| matches!(&t.tok, Tok::Ident(w) if w == "default"))
}

/// The token after `<…>` starting at `open`.
fn skip_angles(toks: &[Token], open: usize) -> usize {
    let mut depth = 0i32;
    for (j, t) in toks.iter().enumerate().skip(open) {
        match t.tok {
            Tok::Punct('<') => depth += 1,
            // `=>` inside a generic default is not a closing angle.
            Tok::Punct('>') if j == 0 || toks[j - 1].tok != Tok::Punct('=') => {
                depth -= 1;
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
    }
    toks.len()
}

/// The `;` (or `{` of a body, or a new declaration) ending a statement that starts at `from`:
/// brackets of type expressions are skipped.
fn statement_end(toks: &[Token], from: usize) -> usize {
    let mut depth = 0i32;
    for (j, t) in toks.iter().enumerate().skip(from) {
        match &t.tok {
            Tok::Punct('(' | '[' | '{' | '<') => depth += 1,
            Tok::Punct(')' | ']' | '}') => depth -= 1,
            Tok::Punct('>') if j > 0 && toks[j - 1].tok != Tok::Punct('=') => depth -= 1,
            Tok::Punct(';') if depth <= 0 => return j,
            Tok::Ident(w)
                if depth <= 0
                    && matches!(w.as_str(), "export" | "declare" | "interface")
                    && j > from =>
            {
                return j;
            }
            _ => {}
        }
        if depth < 0 {
            return j;
        }
    }
    toks.len()
}

/// The members of an interface body, one line each (`id: number`, `get(key: K): V`).
fn members(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        // Comments.
        if c == '/' && chars.peek() == Some(&'*') {
            let mut prev = ' ';
            for d in chars.by_ref() {
                if prev == '*' && d == '/' {
                    break;
                }
                prev = d;
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            for d in chars.by_ref() {
                if d == '\n' {
                    break;
                }
            }
            continue;
        }
        match c {
            '{' | '(' | '[' | '<' => depth += 1,
            '}' | ')' | ']' | '>' => depth -= 1,
            _ => {}
        }
        if depth <= 0 && (c == ';' || c == '\n' || c == ',') {
            let m = one_line(&cur);
            if !m.is_empty() && out.len() < MAX_MEMBERS {
                out.push(m);
            }
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    let m = one_line(&cur);
    if !m.is_empty() && out.len() < MAX_MEMBERS {
        out.push(m);
    }
    out
}

/// `import { a as b, c } from 's'`, `import X from 's'`: returns the token after it.
fn parse_import(toks: &[Token], k: usize, out: &mut DtsFile) -> usize {
    let ident = |j: usize| match toks.get(j).map(|t| &t.tok) {
        Some(Tok::Ident(s)) => Some(s.clone()),
        _ => None,
    };
    let is =
        |j: usize, c: char| matches!(toks.get(j).map(|t| &t.tok), Some(Tok::Punct(p)) if *p == c);
    let mut j = k + 1;
    if ident(j).as_deref() == Some("type") {
        j += 1;
    }
    let mut names: Vec<(String, String)> = Vec::new();
    if let Some(name) = ident(j)
        && name != "from"
    {
        names.push((name, "default".into()));
        j += 1;
        if is(j, ',') {
            j += 1;
        }
    }
    if is(j, '*') {
        j += 3;
    } else if is(j, '{') {
        let close = matching(toks, j);
        let mut i = j + 1;
        while i < close {
            if let Some(imported) = ident(i) {
                if imported == "type"
                    && ident(i + 1).is_some()
                    && ident(i + 1).as_deref() != Some("as")
                {
                    i += 1;
                    continue;
                }
                if ident(i + 1).as_deref() == Some("as")
                    && let Some(local) = ident(i + 2)
                {
                    names.push((local, imported));
                    i += 3;
                    continue;
                }
                names.push((imported.clone(), imported));
            }
            i += 1;
        }
        j = close + 1;
    }
    if ident(j).as_deref() == Some("from")
        && let Some(Tok::Str(spec)) = toks.get(j + 1).map(|t| &t.tok)
    {
        out.imports
            .extend(names.into_iter().map(|(l, i)| (l, i, spec.clone())));
        return j + 2;
    }
    j.max(k + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REACTIVITY: &str = r#"import { IfAny } from '@vue/shared';
/**
 * Takes an inner value and returns a reactive and mutable ref object, which
 * has a single property `.value` that points to the inner value.
 *
 * @param value - The object to wrap in the ref.
 * @see {@link https://vuejs.org/api/reactivity-core.html#ref}
 */
export declare function ref<T>(value: T): [T] extends [Ref] ? IfAny<T, Ref<T>, T> : Ref<UnwrapRef<T>, UnwrapRef<T> | T>;
export declare function ref<T = any>(): Ref<T | undefined>;
/**
 * Takes a getter function and returns a readonly reactive ref object.
 *
 * @example
 * ```js
 * const count = ref(1)
 * const plusOne = computed(() => count.value + 1)
 * ```
 */
export declare function computed<T>(getter: ComputedGetter<T>, debugOptions?: DebuggerOptions): ComputedRef<T>;
export interface Ref<T = any, S = T> {
    /** The value. */
    get value(): T;
    set value(_: S);
    [RefSymbol]: true;
}
export type MaybeRef<T = any> = T | Ref<T>;
declare enum TrackOpTypes { GET = "get" }
export { TrackOpTypes };
"#;

    #[test]
    fn declarations_with_docs() {
        let f = scan(REACTIVITY);
        let names: Vec<(&str, Kind)> = f
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind))
            .collect();
        assert_eq!(
            names,
            [
                ("ref", Kind::Function),
                ("ref", Kind::Function),
                ("computed", Kind::Function),
                ("Ref", Kind::Interface),
                ("MaybeRef", Kind::Type),
                ("TrackOpTypes", Kind::Enum),
            ]
        );
        let r = &f.symbols[0];
        assert!(r.doc.summary.starts_with("Takes an inner value"));
        assert_eq!(r.doc.params[0].name, "value");
        assert_eq!(
            r.doc.see,
            ["https://vuejs.org/api/reactivity-core.html#ref"]
        );
        assert!(
            r.signature
                .starts_with("ref<T>(value: T): [T] extends [Ref]"),
            "{}",
            r.signature
        );
        assert_eq!(r.params, ["value: T"]);
        assert_eq!((r.line, r.col), (8, 24));
        assert!(f.symbols[1].doc.is_empty());
        assert_eq!(
            f.symbols[2].doc.example,
            "const count = ref(1)\nconst plusOne = computed(() => count.value + 1)"
        );
        assert_eq!(
            f.symbols[3].params,
            ["get value(): T", "set value(_: S)", "[RefSymbol]: true"]
        );
        assert_eq!(
            f.symbols[4].signature,
            "type MaybeRef<T = any> = T | Ref<T>"
        );
        assert_eq!(
            f.imports,
            [("IfAny".into(), "IfAny".into(), "@vue/shared".into())]
        );
        assert_eq!(
            f.reexports,
            [("TrackOpTypes".into(), "TrackOpTypes".into(), None)]
        );
    }

    #[test]
    fn re_exports_and_aliases() {
        let src = "import { computed as computed$1, Ref } from '@vue/reactivity';\nexport * from '@vue/runtime-core';\nexport * as ns from './ns';\nexport { ref, type Ref as R, isRef as isARef } from '@vue/reactivity';\nexport declare const computed: typeof computed$1;\ndeclare module '@vue/runtime-core' {\n  export interface Extra { a: string }\n}\nexport default App;\n";
        let f = scan(src);
        assert_eq!(f.star, ["@vue/runtime-core"]);
        assert_eq!(
            f.reexports,
            [
                ("ref".into(), "ref".into(), Some("@vue/reactivity".into())),
                ("R".into(), "Ref".into(), Some("@vue/reactivity".into())),
                (
                    "isARef".into(),
                    "isRef".into(),
                    Some("@vue/reactivity".into())
                ),
                ("default".into(), "App".into(), None),
            ]
        );
        assert_eq!(f.typeof_alias, [("computed".into(), "computed$1".into())]);
        assert_eq!(
            f.imports[0],
            (
                "computed$1".into(),
                "computed".into(),
                "@vue/reactivity".into()
            )
        );
        // Declarations inside `declare module` count.
        assert!(
            f.symbols
                .iter()
                .any(|s| s.name == "Extra" && s.kind == Kind::Interface)
        );
    }
}
