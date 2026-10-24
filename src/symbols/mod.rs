//! The symbol index behind the doc popups and Go to Definition: declarations (functions,
//! classes, methods …) with their doc comments, found by scanning JS/TS/Vue and C# source.
//! Static and name based: no type inference, so `x.name` matches every member called `name`.

pub mod cs;
pub mod dotnet_docs;
pub mod dts;
pub mod js;
pub mod mdn;
pub mod npm;
pub mod refs;

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
    /// A type alias (`type X = …`, JSDoc `@typedef`).
    Type,
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
            Kind::Type => "type",
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

/// A declaration's docs. Most symbols have none, and the workspace index keeps none (they are
/// read again from the file when shown), so an empty `Doc` is one pointer and no allocation;
/// the fields are reached through `Deref`, and writing to them allocates the data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Doc(Option<Box<DocData>>);

static EMPTY_DOC: std::sync::LazyLock<DocData> = std::sync::LazyLock::new(DocData::default);

impl std::ops::Deref for Doc {
    type Target = DocData;
    fn deref(&self) -> &DocData {
        self.0.as_deref().unwrap_or(&EMPTY_DOC)
    }
}

impl std::ops::DerefMut for Doc {
    fn deref_mut(&mut self) -> &mut DocData {
        self.0.get_or_insert_with(Default::default)
    }
}

impl From<DocData> for Doc {
    fn from(d: DocData) -> Self {
        Doc(Some(Box::new(d)))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocData {
    pub summary: String,
    pub params: Vec<DocParam>,
    /// Type (JSDoc only) and text.
    pub returns: Option<(String, String)>,
    /// `@type {T}` of a JSDoc'd constant.
    pub ty: String,
    /// JSDoc `@see` links / references.
    pub see: Vec<String>,
    /// JSDoc `@example`, as written.
    pub example: String,
    /// C# `<remarks>`.
    pub remarks: String,
    /// C# `<exception cref="T">text</exception>`: the exception type and when.
    pub exceptions: Vec<(String, String)>,
    /// C# `<typeparam name="T">` names, in order.
    pub typeparams: Vec<String>,
}

impl DocData {
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
    /// Names imported from packages (`import { ref as r } from 'vue'`): local name, the
    /// imported name (`None`: the default export) and the package.
    pub packages: Vec<(String, Option<String>, String)>,
    /// Type aliases to another file's type: `@typedef {import('./models').User} User` →
    /// (`User`, the models file, `User`).
    pub aliases: Vec<(String, PathBuf, String)>,
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
    with_line_index(|| scan_file(path, text))
}

fn scan_file(path: &Path, text: &str) -> Option<FileSymbols> {
    crate::perf::bump(&crate::perf::SYMBOL_SCANS);
    match family(path)? {
        Family::Js => {
            let mut f = js::scan(path, text);
            // TypeScript: interfaces, type aliases, enums (and all of a `.d.ts`).
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let dts_file = [".d.ts", ".d.mts", ".d.cts"]
                .iter()
                .any(|e| name.ends_with(e));
            let ts = [".ts", ".tsx", ".mts", ".cts"]
                .iter()
                .any(|e| name.ends_with(e));
            if dts_file || ts {
                let d = dts::scan(text);
                for s in d.symbols {
                    let wanted =
                        dts_file || matches!(s.kind, Kind::Interface | Kind::Type | Kind::Enum);
                    if wanted
                        && !f
                            .symbols
                            .iter()
                            .any(|o| o.name == s.name && o.line == s.line)
                    {
                        f.symbols.push(s);
                    }
                }
            }
            Some(f)
        }
        Family::CSharp => Some(cs::scan(text)),
    }
}

/// Every scanned file, with a by-name lookup. Names map to small file ids (not a copy of
/// the path per name), and each file is listed once per name.
#[derive(Debug, Default)]
pub struct SymbolIndex {
    files: HashMap<PathBuf, FileSymbols>,
    /// File ids, never reused (an id of a removed file is just not in `files` any more).
    ids: HashMap<PathBuf, u32>,
    paths: Vec<PathBuf>,
    by_name: HashMap<Box<str>, Vec<u32>>,
}

impl SymbolIndex {
    /// Every indexed file.
    pub fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.files.keys()
    }

    pub fn file(&self, path: &Path) -> Option<&FileSymbols> {
        self.files.get(path)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    fn id_of(&mut self, path: &Path) -> u32 {
        if let Some(id) = self.ids.get(path) {
            return *id;
        }
        let id = self.paths.len() as u32;
        self.paths.push(path.to_path_buf());
        self.ids.insert(path.to_path_buf(), id);
        id
    }

    pub fn set_file(&mut self, path: PathBuf, mut syms: FileSymbols) {
        // Docs are read again from the file when shown: most are never looked at, and they
        // are most of a symbol's size.
        for s in &mut syms.symbols {
            s.doc = Doc::default();
            s.params.shrink_to_fit();
            s.signature.shrink_to_fit();
        }
        syms.symbols.shrink_to_fit();
        syms.imports.shrink_to_fit();
        self.remove_file(&path);
        let id = self.id_of(&path);
        let names: std::collections::HashSet<&str> =
            syms.symbols.iter().map(|s| s.name.as_str()).collect();
        for name in names {
            match self.by_name.get_mut(name) {
                Some(list) => list.push(id),
                None => {
                    self.by_name.insert(name.into(), vec![id]);
                }
            }
        }
        self.files.insert(path, syms);
    }

    pub fn remove_file(&mut self, path: &Path) {
        let Some(old) = self.files.remove(path) else {
            return;
        };
        let Some(&id) = self.ids.get(path) else {
            return;
        };
        let names: std::collections::HashSet<&str> =
            old.symbols.iter().map(|s| s.name.as_str()).collect();
        for name in names {
            if let Some(list) = self.by_name.get_mut(name) {
                list.retain(|i| *i != id);
                if list.is_empty() {
                    self.by_name.remove(name);
                }
            }
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Every symbol called `name`, with its file.
    pub fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = (&'a Path, &'a Symbol)> {
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(move |id| {
                let p = self.paths.get(*id as usize)?;
                Some((p.as_path(), self.files.get(p)?))
            })
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

/// The line starts of a text, keyed by its address and length.
type LineTable = (usize, usize, Vec<usize>);

thread_local! {
    /// Within `with_line_index`: how deep the scopes are nested, and the line table of the
    /// text being scanned.
    static LINES: std::cell::RefCell<(usize, Option<LineTable>)> =
        const { std::cell::RefCell::new((0, None)) };
}

thread_local! {
    /// Within `scan_for_index`: doc comments are noticed but not parsed.
    static SKIP_DOCS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Scan for the workspace index, which keeps no docs (they are read again from the file when
/// a popup shows them): doc comments aren't parsed, saving time and memory.
pub fn scan_for_index(path: &Path, text: &str) -> Option<FileSymbols> {
    let was = SKIP_DOCS.with(|s| s.replace(true));
    let r = scan(path, text);
    SKIP_DOCS.with(|s| s.set(was));
    r
}

pub(crate) fn skipping_docs() -> bool {
    SKIP_DOCS.with(|s| s.get())
}

/// "Has a doc comment" without its content (some declarations are only indexed when
/// documented).
pub(crate) fn doc_marker() -> Doc {
    let mut d = Doc::default();
    d.summary.push('…');
    d
}

/// Run a scan with fast `line_col`: the text's line starts are found once and looked up by
/// binary search (a scan asks for the position of every symbol; counting newlines from the
/// start each time is quadratic in big files). The table is dropped when the scope ends.
pub fn with_line_index<R>(f: impl FnOnce() -> R) -> R {
    LINES.with(|l| l.borrow_mut().0 += 1);
    let r = f();
    LINES.with(|l| {
        let mut l = l.borrow_mut();
        l.0 -= 1;
        if l.0 == 0 {
            l.1 = None;
        }
    });
    r
}

/// 0-based line and char column of a byte offset.
pub fn line_col(src: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(src.len());
    let fast = LINES.with(|l| {
        let mut l = l.borrow_mut();
        if l.0 == 0 {
            return None;
        }
        let key = (src.as_ptr() as usize, src.len());
        if !l.1.as_ref().is_some_and(|(p, n, _)| (*p, *n) == key) {
            let starts = std::iter::once(0)
                .chain(
                    src.bytes()
                        .enumerate()
                        .filter(|(_, b)| *b == b'\n')
                        .map(|(i, _)| i + 1),
                )
                .collect();
            l.1 = Some((key.0, key.1, starts));
        }
        let starts = &l.1.as_ref()?.2;
        let line = starts.partition_point(|&s| s <= byte) - 1;
        Some((line, src[starts[line]..byte].chars().count()))
    });
    fast.unwrap_or_else(|| {
        let before = &src[..byte];
        let line = before.bytes().filter(|&b| b == b'\n').count();
        let col = before.rsplit('\n').next().unwrap_or("").chars().count();
        (line, col)
    })
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
