//! The symbol index behind the doc popups and Go to Definition: declarations (functions,
//! classes, methods …) with their doc comments, found by scanning JS/TS/Vue and C# source.
//! Static and name based: no type inference, so `x.name` matches every member called `name`.

pub mod cs;
pub mod dotnet_docs;
pub mod js;
pub mod mdn;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Function,
    Method,
    Constructor,
    Class,
    Interface,
    Enum,
    Property,
    Const,
    /// A whole file (a default-imported `.vue` component).
    Module,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Function => "function",
            Kind::Method => "method",
            Kind::Constructor => "constructor",
            Kind::Class => "class",
            Kind::Interface => "interface",
            Kind::Enum => "enum",
            Kind::Property => "property",
            Kind::Const => "const",
            Kind::Module => "module",
        }
    }

    pub fn callable(self) -> bool {
        matches!(self, Kind::Function | Kind::Method | Kind::Constructor)
    }
}

/// A documented parameter: `@param {type} name text` / `<param name="x">text</param>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocParam {
    pub name: String,
    pub ty: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Doc {
    pub summary: String,
    pub params: Vec<DocParam>,
    /// Type (JSDoc only) and text.
    pub returns: Option<(String, String)>,
    /// `@type {T}` of a JSDoc'd constant.
    pub ty: String,
    /// C# `<remarks>`.
    pub remarks: String,
    /// C# `<exception cref="T">text</exception>`: the exception type and when.
    pub exceptions: Vec<(String, String)>,
    /// C# `<typeparam name="T">` names, in order.
    pub typeparams: Vec<String>,
}

impl Doc {
    pub fn is_empty(&self) -> bool {
        self.summary.is_empty()
            && self.params.is_empty()
            && self.returns.is_none()
            && self.ty.is_empty()
    }

    pub fn param(&self, name: &str) -> Option<&DocParam> {
        self.params.iter().find(|p| p.name == name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub kind: Kind,
    /// The class / object / type it belongs to.
    pub container: Option<String>,
    /// The declaration on one line, e.g. `formatCurrency(amount, currency = 'EUR')`.
    pub signature: String,
    /// Parameters as written, for parameter hints.
    pub params: Vec<String>,
    pub doc: Doc,
    /// 0-based line and column (chars).
    pub line: usize,
    pub col: usize,
}

impl Symbol {
    /// The name a parameter as written declares: `currency = 'EUR'` → `currency`,
    /// `int id` → `id`, `...rest` → `rest`.
    pub fn param_name(param: &str) -> &str {
        let p = param.trim();
        let p = p.split_once('=').map_or(p, |(a, _)| a).trim();
        if p.starts_with('{') || p.starts_with('[') {
            return p;
        }
        // TS `name?: type`, C# `Type name`.
        let p = p.split_once(':').map_or(p, |(a, _)| a).trim();
        let p = p.rsplit(char::is_whitespace).next().unwrap_or(p);
        p.trim_start_matches("...").trim_end_matches('?')
    }
}

/// A JS import: `import { imported as local } from "from"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub local: String,
    /// `None`: the default export.
    pub imported: Option<String>,
    pub from: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileSymbols {
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
    /// The name exported as default (`export default function f`).
    pub default_export: Option<String>,
    /// Names imported from packages (`import { ref } from 'vue'`): local name and package.
    pub packages: Vec<(String, String)>,
}

/// Language family: symbols only match within one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Js,
    CSharp,
}

pub fn family(path: &Path) -> Option<Family> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" | "vue" => Some(Family::Js),
        "cs" => Some(Family::CSharp),
        _ => None,
    }
}

/// Scan a file's text (`path` decides the language and resolves imports).
pub fn scan(path: &Path, text: &str) -> Option<FileSymbols> {
    match family(path)? {
        Family::Js => Some(js::scan(path, text)),
        Family::CSharp => Some(cs::scan(text)),
    }
}

/// Every scanned file, with a by-name lookup.
#[derive(Debug, Default)]
pub struct SymbolIndex {
    files: HashMap<PathBuf, FileSymbols>,
    by_name: HashMap<String, Vec<PathBuf>>,
}

impl SymbolIndex {
    pub fn file(&self, path: &Path) -> Option<&FileSymbols> {
        self.files.get(path)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn set_file(&mut self, path: PathBuf, syms: FileSymbols) {
        self.remove_file(&path);
        for s in &syms.symbols {
            let list = self.by_name.entry(s.name.clone()).or_default();
            if !list.contains(&path) {
                list.push(path.clone());
            }
        }
        self.files.insert(path, syms);
    }

    pub fn remove_file(&mut self, path: &Path) {
        if let Some(old) = self.files.remove(path) {
            for s in &old.symbols {
                if let Some(list) = self.by_name.get_mut(&s.name) {
                    list.retain(|p| p != path);
                    if list.is_empty() {
                        self.by_name.remove(&s.name);
                    }
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.files.clear();
        self.by_name.clear();
    }

    /// Every symbol called `name`, with its file.
    pub fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = (&'a Path, &'a Symbol)> {
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(move |p| Some((p.as_path(), self.files.get(p)?)))
            .flat_map(move |(p, f)| {
                f.symbols
                    .iter()
                    .filter(move |s| s.name == name)
                    .map(move |s| (p, s))
            })
    }
}

/// Split a parameter list on its top-level commas: `a, b = f(1, 2), c: Map<K, V>`.
pub fn split_params(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in text.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth <= 0 => {
                out.push(std::mem::take(&mut cur).trim().to_string());
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    let last = cur.trim();
    if !last.is_empty() || !out.is_empty() {
        out.push(last.to_string());
    }
    out
}

/// Whitespace runs (newlines included) become one space.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 0-based line and char column of a byte offset.
pub fn line_col(src: &str, byte: usize) -> (usize, usize) {
    let before = &src[..byte.min(src.len())];
    let line = before.bytes().filter(|&b| b == b'\n').count();
    let col = before.rsplit('\n').next().unwrap_or("").chars().count();
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_split_and_name() {
        assert_eq!(
            split_params("a, b = f(1, 2), c: Map<K, V>, s = 'x,y'"),
            ["a", "b = f(1, 2)", "c: Map<K, V>", "s = 'x,y'"]
        );
        assert!(split_params("  ").is_empty());
        assert_eq!(Symbol::param_name("currency = 'EUR'"), "currency");
        assert_eq!(Symbol::param_name("int id"), "id");
        assert_eq!(Symbol::param_name("this IQueryable<T> source"), "source");
        assert_eq!(Symbol::param_name("...rest"), "rest");
        assert_eq!(Symbol::param_name("name?: string"), "name");
        assert_eq!(Symbol::param_name("{ a, b }"), "{ a, b }");
    }

    #[test]
    fn index_tracks_names_per_file() {
        let sym = |name: &str| Symbol {
            name: name.into(),
            kind: Kind::Function,
            container: None,
            signature: format!("{name}()"),
            params: vec![],
            doc: Doc::default(),
            line: 0,
            col: 0,
        };
        let mut ix = SymbolIndex::default();
        let file = |names: &[&str]| FileSymbols {
            symbols: names.iter().map(|n| sym(n)).collect(),
            ..Default::default()
        };
        ix.set_file("/a.js".into(), file(&["f", "g"]));
        ix.set_file("/b.js".into(), file(&["f"]));
        assert_eq!(ix.named("f").count(), 2);
        ix.set_file("/a.js".into(), file(&["g"]));
        assert_eq!(ix.named("f").count(), 1);
        ix.remove_file(Path::new("/b.js"));
        assert_eq!(ix.named("f").count(), 0);
        assert_eq!(ix.named("g").count(), 1);
    }
}
