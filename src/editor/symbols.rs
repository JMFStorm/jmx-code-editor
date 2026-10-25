//! Code navigation over the symbol index (`crate::symbols`): the doc popup for a name
//! (mouse hover, Ctrl+I), parameter hints while typing a call, and Go to Definition (F12,
//! Ctrl+click) with Go Back (Alt+Left). Names the workspace doesn't declare are looked up
//! in the .NET SDK's and packages' API docs (C#) or the MDN table (JS).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use ropey::Rope;

use super::hover::{Hover, HoverAt, HoverKind};
use super::linting::Target;
use super::{EDITOR, Editor};
use crate::symbols::dotnet_docs::{self, DotnetDocs, Receiver as Recv, Scope};
use crate::symbols::mdn::{self, WebDoc};
use crate::symbols::{self, Doc, Family, FileSymbols, Kind, Symbol, SymbolIndex};
use crate::ui::picker::FilePicker;

/// Open documents up to this size are rescanned right away when looked up; bigger ones in
/// the background after typing pauses.
const LIVE_SYNC_CHARS: usize = 256 * 1024;
/// Files whose docs are kept after being read for a popup.
const DOC_CACHE_FILES: usize = 16;
/// The pause before a big document is rescanned.
const LIVE_DELAY: Duration = Duration::from_millis(300);

/// Files bigger than this are not scanned (bundles, generated code).
const MAX_FILE: u64 = 1 << 20;
/// How far back parameter hints look for the call's `(`.
const CALL_LOOKBACK: usize = 4000;
/// Go Back remembers this many places.
const MAX_BACK: usize = 50;

#[derive(Default)]
pub struct SymbolState {
    pub index: RefCell<SymbolIndex>,
    /// Open documents, scanned from their text at this revision.
    live: RefCell<HashMap<PathBuf, u64>>,
    scans: Vec<Receiver<Vec<(PathBuf, FileSymbols)>>>,
    /// Where Go to Definition jumped from (Alt+Left goes back).
    pub back: Vec<Target>,
    /// Parameter hints for the call the cursor is in.
    pub signature: Option<Hover>,
    /// The .NET API docs of the workspace's C# projects (loaded in the background).
    pub dotnet: Option<Arc<DotnetDocs>>,
    dotnet_job: Option<Receiver<DotnetDocs>>,
    /// Background rescans of big open documents.
    live_jobs: Vec<Receiver<(PathBuf, u64, Option<FileSymbols>)>>,
    live_busy: std::collections::HashSet<PathBuf>,
    /// The revision of a big document's last edit and when it was first seen.
    live_edit: HashMap<PathBuf, (u64, Instant)>,
    /// Recently scanned files, for their docs: (path, text version, symbols).
    doc_cache: RefCell<Vec<(PathBuf, u64, Arc<FileSymbols>)>>,
    /// npm packages' parsed declaration files.
    pub dts: RefCell<crate::symbols::npm::DtsCache>,
    /// A C# project's implicit global usings.
    scopes: RefCell<HashMap<PathBuf, Scope>>,
    /// The Docs page: the API shown, the ones shown before it, other matches of the name
    /// looked up, and the API each page line opens.
    pub docs_current: Option<u32>,
    pub docs_back: Vec<u32>,
    pub docs_matches: Vec<u32>,
    pub docs_rows: Vec<Option<u32>>,
}

/// A name that is not declared in the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum External {
    /// A .NET API (index into `SymbolState::dotnet`) and its Microsoft Learn page.
    Dotnet { idx: u32, full: String, url: String },
    /// A JavaScript built-in's MDN page.
    Web(WebDoc),
    /// Declared in an npm package's type definitions (`path` is the `.d.ts` file); `see`:
    /// the docs page it links.
    Npm {
        package: String,
        see: Option<String>,
    },
    /// Imported from a package that has no type definitions for it.
    Package(String),
}

/// A symbol and the file it is declared in.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub path: PathBuf,
    pub sym: Symbol,
    /// Not a workspace declaration: where its docs come from.
    pub ext: Option<External>,
}

/// What stands before a name.
#[derive(Debug, Clone, PartialEq)]
enum Qual {
    Plain,
    /// `obj.name` (`this.name` included).
    Member(String),
    /// `new Name`
    New,
    /// `import('./models').Name` (a JSDoc type).
    ImportType(String),
}

/// Words that are never looked up.
const KEYWORDS: &[&str] = &[
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "case",
    "return",
    "function",
    "const",
    "let",
    "var",
    "new",
    "this",
    "class",
    "import",
    "export",
    "from",
    "default",
    "async",
    "await",
    "true",
    "false",
    "null",
    "undefined",
    "typeof",
    "instanceof",
    "in",
    "of",
    "try",
    "catch",
    "finally",
    "throw",
    "void",
    "static",
    "abstract",
];

/// C# keywords on top (JS uses some of these as names: `api.get`, `ref` from Vue).
const CS_KEYWORDS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "using",
    "namespace",
    "base",
    "readonly",
    "override",
    "virtual",
    "sealed",
    "get",
    "set",
    "init",
    "is",
    "as",
    "out",
    "ref",
    "var",
    "foreach",
    "struct",
    "record",
    "interface",
    "enum",
    "where",
    "partial",
    "required",
];

/// A word that is never looked up in this language.
fn is_keyword(word: &str, family: Family) -> bool {
    KEYWORDS.contains(&word) || family == Family::CSharp && CS_KEYWORDS.contains(&word)
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The identifier at `pos` (or ending right before it): its start, end and text.
pub fn word_at(rope: &Rope, pos: usize) -> Option<(usize, usize, String)> {
    let len = rope.len_chars();
    let pos = pos.min(len);
    let at = |p: usize| p < len && is_ident(rope.char(p));
    let p = if at(pos) {
        pos
    } else if pos > 0 && at(pos - 1) {
        pos - 1
    } else {
        return None;
    };
    let mut start = p;
    while start > 0 && at(start - 1) {
        start -= 1;
    }
    let mut end = p;
    while at(end) {
        end += 1;
    }
    let word: String = rope.slice(start..end).chars().collect();
    if word.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((start, end, word))
}

/// What comes before the name starting at `start`.
fn qualifier(rope: &Rope, start: usize) -> Qual {
    let mut p = start;
    while p > 0 && rope.char(p - 1).is_whitespace() {
        p -= 1;
    }
    if p > 0 && rope.char(p - 1) == '.' {
        let mut q = p - 1;
        // `a?.b`
        if q > 0 && rope.char(q - 1) == '?' {
            q -= 1;
        }
        // `import('./models').User` in a JSDoc type.
        if q > 0 && rope.char(q - 1) == ')' {
            let line = rope.char_to_line(q);
            let before: String = rope.slice(rope.line_to_char(line)..q).chars().collect();
            if let Some(k) = before.rfind("import(") {
                let inner = before[k + "import(".len()..].trim_end_matches(')').trim();
                let spec = inner.trim_matches(|c| c == '\'' || c == '"');
                if !spec.is_empty() && spec.len() + 2 == inner.len() {
                    return Qual::ImportType(spec.to_string());
                }
            }
        }
        // `foo().bar` / `a[0].b`: no name to go by.
        let end = q;
        let mut s = end;
        while s > 0 && is_ident(rope.char(s - 1)) {
            s -= 1;
        }
        return Qual::Member(rope.slice(s..end).chars().collect());
    }
    let mut s = p;
    while s > 0 && is_ident(rope.char(s - 1)) {
        s -= 1;
    }
    if rope.slice(s..p) == "new" {
        Qual::New
    } else {
        Qual::Plain
    }
}

/// Shared length of two paths' components (to prefer nearby files).
fn closeness(a: &Path, b: &Path) -> usize {
    a.components()
        .zip(b.components())
        .take_while(|(x, y)| x == y)
        .count()
}

impl Editor {
    // ------------------------------------------------------------ the index

    /// Scan the workspace in the background (called when a workspace opens).
    pub(super) fn start_symbols(&mut self) {
        self.symbols.index.borrow_mut().clear();
        self.symbols.live.borrow_mut().clear();
        self.symbols.dts.borrow_mut().clear();
        self.symbols.scans.clear();
        let folders: Vec<PathBuf> = self
            .workspace
            .as_ref()
            .map(|w| w.folders.clone())
            .unwrap_or_default();
        if folders.is_empty() {
            return;
        }
        self.load_dotnet_docs(&folders);
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("jdc-symbols".into())
            .spawn(move || {
                let started = Instant::now();
                // Every core: files are read and scanned in parallel.
                let out = std::sync::Mutex::new(Vec::new());
                for folder in &folders {
                    crate::workspace::files::source_walker(folder)
                        .build_parallel()
                        .run(|| {
                            let out = &out;
                            Box::new(move |entry| {
                                let Ok(entry) = entry else {
                                    return ignore::WalkState::Continue;
                                };
                                let path = entry.path();
                                if !entry.file_type().is_some_and(|t| t.is_file())
                                    || symbols::family(path).is_none()
                                    || entry_too_big(path)
                                    || path.components().any(|c| c.as_os_str() == "node_modules")
                                {
                                    return ignore::WalkState::Continue;
                                }
                                if let Ok(text) = std::fs::read_to_string(path) {
                                    let path =
                                        std::fs::canonicalize(path).unwrap_or(path.to_path_buf());
                                    if let Some(mut found) = symbols::scan_for_index(&path, &text) {
                                        // The index keeps no docs (see `doc_of`).
                                        for s in &mut found.symbols {
                                            s.doc = Doc::default();
                                        }
                                        out.lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .push((path, found));
                                    }
                                }
                                ignore::WalkState::Continue
                            })
                        });
                }
                let mut out = out.into_inner().unwrap_or_else(|e| e.into_inner());
                // Same order every time (the walk's order varies between threads).
                out.sort_by(|a, b| a.0.cmp(&b.0));
                log::info!(
                    "symbol scan: {} files in {:?}",
                    out.len(),
                    started.elapsed()
                );
                let _ = tx.send(out);
            })
            .expect("cannot start symbol scan");
        self.symbols.scans.push(rx);
    }

    /// Index the API docs of every C# project's framework and packages in the background.
    fn load_dotnet_docs(&mut self, folders: &[PathBuf]) {
        let projects: Vec<PathBuf> = folders
            .iter()
            .flat_map(|f| crate::lint::roslyn::projects_in(f))
            .collect();
        if projects.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("jdc-dotnet-docs".into())
            .spawn(move || {
                let started = Instant::now();
                let root = dotnet_docs::dotnet_root();
                let mut files: Vec<PathBuf> = projects
                    .iter()
                    .flat_map(|p| dotnet_docs::project_xml_files(p, root.as_deref()))
                    .collect();
                files.sort();
                files.dedup();
                let docs = DotnetDocs::load(files);
                log::info!(
                    ".NET API docs: {} APIs from {} files in {:?}",
                    docs.len(),
                    docs.file_count(),
                    started.elapsed()
                );
                let _ = tx.send(docs);
            })
            .expect("cannot start .NET docs loading");
        self.symbols.dotnet_job = Some(rx);
    }

    pub(super) fn stop_symbols(&mut self) {
        let back = std::mem::take(&mut self.symbols.back);
        self.symbols = SymbolState {
            back,
            ..Default::default()
        };
    }

    /// The workspace scan is still running.
    pub(super) fn symbols_scanning(&self) -> bool {
        !self.symbols.scans.is_empty() || self.symbols.dotnet_job.is_some()
    }

    /// Collect the background scan. Returns true when it arrived.
    pub(super) fn poll_symbols(&mut self) -> bool {
        let mut done = false;
        self.symbols.scans.retain(|rx| match rx.try_recv() {
            Ok(files) => {
                let mut ix = self.symbols.index.borrow_mut();
                let mut live = self.symbols.live.borrow_mut();
                for (path, found) in files {
                    // Open documents are scanned again from their (maybe edited) text.
                    live.remove(&path);
                    ix.set_file(path, found);
                }
                done = true;
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        if done {
            crate::perf::trim_memory();
        }
        if let Some(rx) = &self.symbols.dotnet_job {
            match rx.try_recv() {
                Ok(docs) => {
                    self.symbols.dotnet = Some(Arc::new(docs));
                    self.symbols.dotnet_job = None;
                    done = true;
                    crate::perf::trim_memory();
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.symbols.dotnet_job = None,
            }
        }
        done
    }

    /// A file changed on disk (watcher): scan it again unless it is open.
    pub(super) fn symbols_file_changed(&mut self, path: &Path, removed: bool) {
        if symbols::family(path).is_none() {
            return;
        }
        if removed {
            self.symbols.index.borrow_mut().remove_file(path);
            self.symbols.live.borrow_mut().remove(path);
            return;
        }
        if self
            .find_doc(|d| d.canonical.as_deref() == Some(path))
            .is_none()
        {
            self.load_symbols(path);
        }
    }

    /// Scan a file from disk into the index.
    fn load_symbols(&self, path: &Path) {
        if entry_too_big(path) {
            return;
        }
        if let Ok(text) = std::fs::read_to_string(path)
            && let Some(found) = symbols::scan_for_index(path, &text)
        {
            self.symbols
                .index
                .borrow_mut()
                .set_file(path.to_path_buf(), found);
        }
    }

    /// Scan open documents whose text changed since their last scan.
    /// Small documents are scanned right away (exact results while typing); big ones are
    /// left to `poll_live_symbols`, which scans them in the background after a pause.
    fn sync_live_symbols(&self) {
        let mut ix = self.symbols.index.borrow_mut();
        let mut live = self.symbols.live.borrow_mut();
        for doc in self.panes.iter().flat_map(|p| p.docs.iter()) {
            let Some(path) = &doc.canonical else {
                continue;
            };
            if symbols::family(path).is_none()
                || live.get(path) == Some(&doc.revision)
                || doc.rope.len_chars() > LIVE_SYNC_CHARS
            {
                continue;
            }
            if let Some(found) = symbols::scan_for_index(path, &doc.rope.to_string()) {
                ix.set_file(path.clone(), found);
            }
            live.insert(path.clone(), doc.revision);
        }
    }

    /// Big open documents edited since their last scan: scan them on a worker thread once
    /// typing has paused (`LIVE_DELAY`); collect finished scans. Returns true when the index
    /// changed.
    pub(super) fn poll_live_symbols(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        // Finished background scans (dropped if the text moved on meanwhile).
        let mut done = Vec::new();
        self.symbols.live_jobs.retain(|rx| match rx.try_recv() {
            Ok(r) => {
                done.push(r);
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        for (path, rev, found) in done {
            if let Some(found) = found {
                self.symbols
                    .index
                    .borrow_mut()
                    .set_file(path.clone(), found);
                changed = true;
            }
            self.symbols.live.borrow_mut().insert(path.clone(), rev);
            self.symbols.live_busy.remove(&path);
        }
        for doc in self.panes.iter().flat_map(|p| p.docs.iter()) {
            let Some(path) = &doc.canonical else {
                continue;
            };
            if doc.rope.len_chars() <= LIVE_SYNC_CHARS
                || symbols::family(path).is_none()
                || self.symbols.live.borrow().get(path) == Some(&doc.revision)
                || self.symbols.live_busy.contains(path)
            {
                continue;
            }
            let since = self
                .symbols
                .live_edit
                .get(path)
                .filter(|(r, _)| *r == doc.revision);
            match since {
                None => {
                    self.symbols
                        .live_edit
                        .insert(path.clone(), (doc.revision, now));
                }
                Some((_, at)) if now.duration_since(*at) >= LIVE_DELAY => {
                    let (tx, rx) = mpsc::channel();
                    let (rope, p, rev) = (doc.rope.clone(), path.clone(), doc.revision);
                    std::thread::Builder::new()
                        .name("jdc-live-scan".into())
                        .spawn(move || {
                            let found = symbols::scan_for_index(&p, &rope.to_string());
                            let _ = tx.send((p, rev, found));
                        })
                        .expect("cannot start live scan");
                    self.symbols.live_jobs.push(rx);
                    self.symbols.live_busy.insert(path.clone());
                }
                Some(_) => {}
            }
        }
        changed
    }

    /// When `poll_live_symbols` has something to do next.
    pub(super) fn live_symbols_deadline(&self) -> Option<Instant> {
        if !self.symbols.live_jobs.is_empty() {
            return Some(Instant::now() + Duration::from_millis(50));
        }
        self.symbols
            .live_edit
            .values()
            .map(|(_, at)| *at + LIVE_DELAY)
            .min()
            .filter(|_| self.has_stale_big_doc())
    }

    fn has_stale_big_doc(&self) -> bool {
        self.panes.iter().flat_map(|p| p.docs.iter()).any(|d| {
            d.rope.len_chars() > LIVE_SYNC_CHARS
                && d.canonical.as_ref().is_some_and(|p| {
                    symbols::family(p).is_some()
                        && self.symbols.live.borrow().get(p) != Some(&d.revision)
                })
        })
    }

    // ------------------------------------------------------------ resolving names

    /// The name at `pos` of view `pane`'s document and the declarations it may refer to
    /// (best first).
    pub fn resolve_at(&self, pane: usize, pos: usize) -> Option<(String, Vec<Found>)> {
        let doc = self.panes[pane].doc()?;
        let path = doc.canonical.clone()?;
        let family = symbols::family(&path)?;
        let (start, _, name) = word_at(&doc.rope, pos)?;
        if is_keyword(&name, family) {
            return None;
        }
        let qual = qualifier(&doc.rope, start);
        self.sync_live_symbols();
        let mut found = match &qual {
            Qual::ImportType(spec) => self.import_type(&path, spec, &name),
            _ => self.resolve(&path, family, &name, &qual),
        };
        found = self.follow_aliases(found);
        if found.is_empty() {
            let line = doc.rope.char_to_line(start);
            found = self.external(&path, family, &doc.rope, &name, &qual, line);
        }
        Some((name, found))
    }

    /// A name the workspace doesn't declare: a .NET API the C# file can see (its usings),
    /// a JS name imported from a package, or a JS built-in with an MDN page.
    fn external(
        &self,
        path: &Path,
        family: Family,
        text: &Rope,
        name: &str,
        qual: &Qual,
        line: usize,
    ) -> Vec<Found> {
        match family {
            Family::CSharp => {
                let Some(d) = &self.symbols.dotnet else {
                    return Vec::new();
                };
                // `using`s and the namespace sit at the top: the head of the file is enough
                // (no copy of a big file per lookup).
                let head = text.slice(..text.len_chars().min(64 * 1024)).to_string();
                let mut own = Scope::default();
                own.add_source(&head);
                let mut scope = self.cs_scope(path);
                scope.add_source(&head);
                // `string`, `int`, …: their framework types.
                let name = match dotnet_docs::keyword_type(name) {
                    Some(full) => {
                        scope.namespaces.push("System".into());
                        full.rsplit('.').next().unwrap_or(name)
                    }
                    _ => name,
                };
                let recv = match qual {
                    Qual::Plain => Recv::None,
                    Qual::New => Recv::New,
                    Qual::Member(o) if o == "this" || o == "base" => Recv::None,
                    Qual::Member(o) => Recv::Of(o),
                    Qual::ImportType(_) => Recv::None,
                };
                // Best first: members of the enclosing class's base types (a controller's
                // `Ok()`), then of namespaces this file imports itself.
                let bases = self.enclosing_bases(path, line);
                let mut ids = d.resolve(&scope, name, recv);
                ids.sort_by_cached_key(|i| {
                    let (_, _, ty) = d.describe(*i);
                    let ns = d.namespace_of_entry(*i);
                    (!bases.contains(&ty), !own.namespaces.contains(&ns))
                });
                // One per member, its simplest overload; the rest are on the Docs page.
                let mut best: Vec<(String, u32)> = Vec::new();
                for i in ids {
                    let full = d.full_name(i);
                    match best.iter_mut().find(|(f, _)| *f == full) {
                        Some((_, b)) if d.param_count(i) < d.param_count(*b) => *b = i,
                        Some(_) => {}
                        None => best.push((full, i)),
                    }
                }
                best.into_iter()
                    .take(30)
                    .map(|(_, i)| dotnet_found(d, i, recv))
                    .collect()
            }
            Family::Js => {
                if *qual == Qual::Plain
                    && let Some((_, imported, pkg)) = self
                        .symbols
                        .index
                        .borrow()
                        .file(path)
                        .and_then(|f| f.packages.iter().find(|(l, _, _)| l == name).cloned())
                {
                    // The package's own type definitions in node_modules.
                    let want = imported.unwrap_or_else(|| "default".into());
                    let found = crate::symbols::npm::package_types(path, &pkg)
                        .map(|entry| {
                            crate::symbols::npm::find_export(
                                &mut self.symbols.dts.borrow_mut(),
                                &entry,
                                &want,
                            )
                        })
                        .unwrap_or_default();
                    if !found.is_empty() {
                        return found
                            .into_iter()
                            .map(|(p, sym)| Found {
                                path: p,
                                ext: Some(External::Npm {
                                    package: pkg.clone(),
                                    see: sym
                                        .doc
                                        .see
                                        .iter()
                                        .find(|s| s.starts_with("https://"))
                                        .cloned(),
                                }),
                                sym,
                            })
                            .collect();
                    }
                    let mut sym = module_symbol(Path::new(name));
                    sym.signature = name.to_string();
                    return vec![Found {
                        path: PathBuf::new(),
                        sym,
                        ext: Some(External::Package(pkg)),
                    }];
                }
                let recv = match qual {
                    Qual::Member(o) => Some(o.as_str()),
                    _ => None,
                };
                mdn::lookup(name, recv)
                    .map(|w| Found {
                        path: PathBuf::new(),
                        sym: Symbol {
                            name: name.to_string(),
                            kind: Kind::Function,
                            container: None,
                            signature: w.title.clone(),
                            params: Vec::new(),
                            doc: Doc::default(),
                            line: 0,
                            col: 0,
                        },
                        ext: Some(External::Web(w)),
                    })
                    .into_iter()
                    .collect()
            }
        }
    }

    /// The base types named by the class around `line` of `path` (`: ControllerBase, IFoo`).
    fn enclosing_bases(&self, path: &Path, line: usize) -> Vec<String> {
        let ix = self.symbols.index.borrow();
        ix.file(path)
            .and_then(|f| {
                f.symbols
                    .iter()
                    .rfind(|s| s.kind == Kind::Class && s.line <= line)
            })
            .map(|class| base_names(&class.signature))
            .unwrap_or_default()
    }

    /// `import('<spec>').Name` in a JSDoc type: `Name` in that file.
    fn import_type(&self, path: &Path, spec: &str, name: &str) -> Vec<Found> {
        match symbols::js::resolve_import(path, spec) {
            Some(file) => self.import_type_at(&file, name),
            None => Vec::new(),
        }
    }

    /// Type aliases (`@typedef {import('./models').User} User`) replaced by what they name.
    fn follow_aliases(&self, found: Vec<Found>) -> Vec<Found> {
        let mut out = Vec::new();
        for f in found {
            let alias = (f.ext.is_none() && f.sym.kind == Kind::Type)
                .then(|| {
                    self.symbols.index.borrow().file(&f.path).and_then(|file| {
                        file.aliases
                            .iter()
                            .find(|(n, _, _)| *n == f.sym.name)
                            .map(|(_, target, x)| (target.clone(), x.clone()))
                    })
                })
                .flatten();
            if let Some((target, x)) = alias {
                let to = self.import_type_at(&target, &x);
                if !to.is_empty() {
                    for t in to {
                        if !out.contains(&t) {
                            out.push(t);
                        }
                    }
                    continue;
                }
            }
            if !out.contains(&f) {
                out.push(f);
            }
        }
        out
    }

    /// The top-level symbols called `name` in `file` (loaded on demand).
    fn import_type_at(&self, file: &Path, name: &str) -> Vec<Found> {
        if self.symbols.index.borrow().file(file).is_none() {
            self.load_symbols(file);
        }
        let ix = self.symbols.index.borrow();
        ix.file(file)
            .map(|f| {
                f.symbols
                    .iter()
                    .filter(|s| s.name == name && s.container.is_none())
                    .map(|s| Found {
                        path: file.to_path_buf(),
                        sym: s.clone(),
                        ext: None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The global usings of the C# project `path` belongs to (cached).
    fn cs_scope(&self, path: &Path) -> Scope {
        let Some(csproj) = crate::lint::roslyn::project_of(path) else {
            return Scope::default();
        };
        self.symbols
            .scopes
            .borrow_mut()
            .entry(csproj.clone())
            .or_insert_with(|| dotnet_docs::global_usings(&csproj))
            .clone()
    }

    fn resolve(&self, path: &Path, family: Family, name: &str, qual: &Qual) -> Vec<Found> {
        // Default-imported `.vue` components are files, not symbols: load import targets first.
        let imports = {
            let ix = self.symbols.index.borrow();
            ix.file(path).map(|f| f.imports.clone()).unwrap_or_default()
        };
        let import = imports.iter().find(|i| i.local == name);
        if let Some(i) = import
            && self.symbols.index.borrow().file(&i.from).is_none()
        {
            self.load_symbols(&i.from);
        }
        let ix = self.symbols.index.borrow();
        let all: Vec<(&Path, &Symbol)> = ix
            .named(name)
            // Package files (an open `.d.ts` in node_modules) are reached through their package.
            .filter(|(p, _)| symbols::family(p) == Some(family) && !in_node_modules(p))
            .collect();
        let found = |list: Vec<(&Path, &Symbol)>| -> Vec<Found> {
            let mut list = list;
            list.sort_by_key(|(p, _)| (*p != path, std::cmp::Reverse(closeness(p, path))));
            list.into_iter()
                .take(50)
                .map(|(p, s)| Found {
                    path: p.to_path_buf(),
                    sym: s.clone(),
                    ext: None,
                })
                .collect()
        };
        let pick = |pred: &dyn Fn(&Path, &Symbol) -> bool| -> Vec<(&Path, &Symbol)> {
            all.iter().copied().filter(|(p, s)| pred(p, s)).collect()
        };
        match qual {
            // Resolved by `import_type` before.
            Qual::ImportType(_) => Vec::new(),
            Qual::Member(obj) => {
                let members = pick(&|_, s| s.container.is_some());
                let of_type = pick(&|_, s| s.container.as_deref() == Some(obj.as_str()));
                if obj == "this" {
                    let here = pick(&|p, s| p == path && s.container.is_some());
                    if !here.is_empty() {
                        return found(here);
                    }
                } else if !of_type.is_empty() {
                    return found(of_type);
                }
                if members.is_empty() {
                    found(all)
                } else {
                    found(members)
                }
            }
            Qual::New => {
                let ctors = pick(&|_, s| s.kind == Kind::Constructor);
                if ctors.is_empty() {
                    found(pick(&|_, s| s.kind == Kind::Class))
                } else {
                    found(ctors)
                }
            }
            Qual::Plain => {
                // Imports name the file: follow them first.
                if let Some(i) = import {
                    let target = ix.file(&i.from);
                    let want = match &i.imported {
                        Some(n) => Some(n.clone()),
                        None => target.and_then(|f| f.default_export.clone()),
                    };
                    let sym = target.and_then(|f| {
                        f.symbols
                            .iter()
                            .find(|s| Some(&s.name) == want.as_ref() && s.container.is_none())
                    });
                    return vec![Found {
                        path: i.from.clone(),
                        sym: sym.cloned().unwrap_or_else(|| module_symbol(&i.from)),
                        ext: None,
                    }];
                }
                let local = pick(&|p, _| p == path);
                if !local.is_empty() {
                    return found(local);
                }
                let top = pick(&|_, s| s.container.is_none());
                if !top.is_empty() {
                    return found(top);
                }
                found(all)
            }
        }
    }

    /// A symbol's doc, or the doc of the member it implements (an interface or base type
    /// listed by its class) when it has none.
    fn effective_doc(&self, f: &Found) -> Doc {
        let own = self.doc_of(&f.path, &f.sym);
        if !own.is_empty() {
            return own;
        }
        let Some(container) = &f.sym.container else {
            return Doc::default();
        };
        let ix = self.symbols.index.borrow();
        // The bases: names after `:` / `extends` / `implements` in the class's declaration.
        let Some(class) = ix.file(&f.path).and_then(|file| {
            file.symbols
                .iter()
                .find(|s| &s.name == container && s.container.is_none() || &s.name == container)
        }) else {
            return Doc::default();
        };
        let bases = base_names(&class.signature);
        let family = symbols::family(&f.path);
        ix.named(&f.sym.name)
            .filter(|(p, s)| {
                symbols::family(p) == family
                    && s.container.as_ref().is_some_and(|c| bases.contains(c))
                    && s.params.len() == f.sym.params.len()
            })
            .map(|(p, s)| self.doc_of(p, s))
            .find(|d| !d.is_empty())
            .unwrap_or_default()
    }

    /// A symbol's docs. The index keeps none: they are read again from its file (the open
    /// buffer, if any) when shown, with the last few files' scans kept.
    pub(super) fn doc_of(&self, path: &Path, sym: &Symbol) -> Doc {
        if !sym.doc.is_empty() || path.as_os_str().is_empty() {
            return sym.doc.clone();
        }
        let open = self.find_doc(|d| d.canonical.as_deref() == Some(path));
        // Which text: the open buffer's revision, or the file's modification time.
        let key = match open {
            Some((p, i)) => self.panes[p].docs[i].revision | 1 << 63,
            None => std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos() as u64 & !(1 << 63)),
        };
        let mut cache = self.symbols.doc_cache.borrow_mut();
        let pos = cache.iter().position(|(p, k, _)| p == path && *k == key);
        let file = match pos {
            Some(i) => {
                let hit = cache.remove(i);
                let file = hit.2.clone();
                cache.push(hit);
                file
            }
            None => {
                let text = match open {
                    Some((p, i)) => self.panes[p].docs[i].rope.to_string(),
                    None => std::fs::read_to_string(path).unwrap_or_default(),
                };
                let Some(found) = symbols::scan(path, &text) else {
                    return Doc::default();
                };
                let file = Arc::new(found);
                cache.retain(|(p, _, _)| p != path);
                if cache.len() >= DOC_CACHE_FILES {
                    cache.remove(0);
                }
                cache.push((path.to_path_buf(), key, file.clone()));
                file
            }
        };
        file.symbols
            .iter()
            .find(|s| s.name == sym.name && s.line == sym.line && s.col == sym.col)
            .map(|s| s.doc.clone())
            .unwrap_or_default()
    }

    // ------------------------------------------------------------ the popup

    /// Popup lines for a symbol: signature, summary, parameters, return value and place.
    /// With `active`, the parameter hints for that parameter of a call.
    pub(super) fn symbol_lines(
        &self,
        f: &Found,
        more: usize,
        active: Option<usize>,
    ) -> Vec<(String, HoverKind)> {
        let s = &f.sym;
        let doc = self.effective_doc(f);
        let cs = symbols::family(&f.path) == Some(Family::CSharp);
        let mut lines = Vec::new();
        let signature = match s.kind {
            _ if cs || f.ext.is_some() => s.signature.clone(),
            Kind::Function => format!("function {}", s.signature),
            Kind::Method | Kind::Constructor => match &s.container {
                Some(c) => format!("{c}.{}", s.signature),
                None => s.signature.clone(),
            },
            Kind::Const if !doc.ty.is_empty() => format!("{}: {}", s.signature, doc.ty),
            Kind::Module => format!("module {}", s.signature),
            _ => s.signature.clone(),
        };
        lines.push((signature, HoverKind::Code));
        // Parameter hints: the active parameter first.
        if let Some(a) = active {
            if let Some(p) = s.params.get(a) {
                let name = Symbol::param_name(p);
                let mut line = format!("▸ {p}");
                if let Some(d) = doc.param(name) {
                    if !d.ty.is_empty() && !p.contains(':') {
                        line.push_str(&format!(": {}", d.ty));
                    }
                    if !d.text.is_empty() {
                        line.push_str(&format!(" — {}", d.text));
                    }
                }
                lines.push((line, HoverKind::Param));
            } else if !s.params.is_empty() {
                lines.push((
                    format!("▸ (only {} parameters)", s.params.len()),
                    HoverKind::Dim,
                ));
            }
            if !doc.summary.is_empty() {
                lines.push((doc.summary.clone(), HoverKind::Text));
            }
            return lines;
        }
        if !doc.summary.is_empty() {
            lines.push((doc.summary.clone(), HoverKind::Text));
        }
        let documented: Vec<_> = doc
            .params
            .iter()
            .filter(|p| !p.text.is_empty() || !p.ty.is_empty())
            .collect();
        if !documented.is_empty() {
            lines.push((String::new(), HoverKind::Text));
            for p in documented {
                let ty = if p.ty.is_empty() {
                    String::new()
                } else {
                    format!(": {}", p.ty)
                };
                let text = if p.text.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", p.text)
                };
                lines.push((format!("{}{ty}{text}", p.name), HoverKind::Text));
            }
        }
        if let Some((ty, text)) = &doc.returns
            && !(ty.is_empty() && text.is_empty())
        {
            let sep = if ty.is_empty() || text.is_empty() {
                ""
            } else {
                " — "
            };
            lines.push((format!("→ {ty}{sep}{text}"), HoverKind::Text));
        }
        // An interface's members (TypeScript declarations).
        if s.kind == Kind::Interface && !s.params.is_empty() {
            lines.push((String::new(), HoverKind::Text));
            for m in s.params.iter().take(12) {
                lines.push((format!("  {m}"), HoverKind::Code));
            }
            if s.params.len() > 12 {
                lines.push((format!("  … {} more", s.params.len() - 12), HoverKind::Dim));
            }
        }
        if !doc.example.is_empty() {
            lines.push((String::new(), HoverKind::Text));
            for l in doc.example.lines().take(8) {
                lines.push((format!("  {l}"), HoverKind::Code));
            }
        }
        let place = match &f.ext {
            None => format!(
                "{} · {}:{}",
                s.kind.name(),
                self.display_path(&f.path),
                s.line + 1
            ),
            Some(External::Dotnet { full, .. }) => {
                format!("{full} · .NET SDK docs · F12: details · Ctrl+F12: Microsoft Learn")
            }
            Some(External::Web(_)) => {
                "MDN · F12 opens the page in your browser (asks first)".into()
            }
            Some(External::Npm { package, see }) => {
                let file = f.path.to_string_lossy().replace('\\', "/");
                let file = file
                    .rsplit("node_modules/")
                    .next()
                    .unwrap_or(&file)
                    .to_string();
                let web = if see.is_some() {
                    " · Ctrl+F12: docs site"
                } else {
                    ""
                };
                format!("{package} · {file}:{} · F12: declaration{web}", s.line + 1)
            }
            Some(External::Package(pkg)) => {
                format!("from package `{pkg}` (no type definitions found for it)")
            }
        };
        lines.push((place, HoverKind::Dim));
        if more > 0 {
            lines.push((
                format!("+{more} more with this name (F12 lists them)"),
                HoverKind::Dim,
            ));
        }
        lines
    }

    /// The doc popup lines for the name at `pos` of view `pane`, if it names a declaration.
    pub(super) fn symbol_hover(&self, pane: usize, pos: usize) -> Option<Vec<(String, HoverKind)>> {
        if let Some(lines) = self.lsp_hover_lines(pane, pos) {
            return lines;
        }
        let (_, found) = self.resolve_at(pane, pos)?;
        let first = found.first()?;
        // npm overloads: F12 goes to the documented one, so they aren't counted.
        let more = match first.ext {
            Some(External::Npm { .. }) => 0,
            _ => found.len() - 1,
        };
        Some(self.symbol_lines(first, more, None))
    }

    // ------------------------------------------------------------ parameter hints

    /// Show, move or close the parameter hints for the call around the cursor. `open`: may
    /// show them when hidden (typing `(` or `,`, Ctrl+Shift+Space); otherwise they only
    /// follow the cursor while shown.
    pub(super) fn update_signature(&mut self, open: bool) {
        if self.lsp_signature(open) {
            return;
        }
        if !open && self.symbols.signature.is_none() {
            return;
        }
        let hint = self.signature_hint();
        self.symbols.signature = hint;
    }

    fn signature_hint(&self) -> Option<Hover> {
        if self.focus != EDITOR || self.picker.is_some() || self.multi_cursor() {
            return None;
        }
        let doc = self.panes[EDITOR].doc()?;
        let head = doc.selection.head;
        let (open, active) = enclosing_call(&doc.rope, head)?;
        // The callee ends right before the `(` (spaces allowed).
        let mut end = open;
        while end > 0 && doc.rope.char(end - 1) == ' ' {
            end -= 1;
        }
        let (start, _, name) = word_at(&doc.rope, end.checked_sub(1)?)?;
        if KEYWORDS.contains(&name.as_str()) && name != "new" {
            return None;
        }
        let (_, found) = self.resolve_at(EDITOR, end - 1)?;
        let f = found
            .iter()
            .find(|f| f.sym.kind.callable() || !f.sym.params.is_empty())?;
        let line = doc.rope.char_to_line(start);
        Some(Hover {
            at: HoverAt::Cell {
                pane: EDITOR,
                line,
                col: start - doc.rope.line_to_char(line),
            },
            lines: self.symbol_lines(f, 0, Some(active)),
            above: true,
        })
    }

    // ------------------------------------------------------------ Go to Definition

    /// F12 / Ctrl+click: jump to the declaration of the name at the cursor (several: pick one).
    pub(super) fn go_to_definition(&mut self) {
        if self.focus != EDITOR || !self.has_doc() {
            self.info("Go to Definition works in the editor view");
            return;
        }
        if self.lsp_definition(false) {
            return;
        }
        let head = self.doc().selection.head;
        let Some((name, found)) = self.resolve_at(EDITOR, head) else {
            self.info("No name at the cursor");
            return;
        };
        if found.is_empty() {
            let loading = if self.symbols.dotnet_job.is_some() {
                " (the .NET API docs are still loading)"
            } else {
                ""
            };
            self.info(format!("No definition found for `{name}`{loading}"));
            return;
        }
        let mut found = found;
        match &found[0].ext {
            Some(External::Package(pkg)) => {
                self.info(format!(
                    "`{name}` comes from package `{pkg}`, which has no type definitions for it"
                ));
                return;
            }
            // npm: the declaration in the package's types (the documented overload).
            Some(External::Npm { .. }) => found.truncate(1),
            Some(External::Web(w)) => {
                self.confirm_open_url(w.url.clone(), format!("the MDN page for {}", w.title));
                return;
            }
            Some(External::Dotnet { idx, .. }) => {
                let matches = found
                    .iter()
                    .filter_map(|f| match &f.ext {
                        Some(External::Dotnet { idx, .. }) => Some(*idx),
                        _ => None,
                    })
                    .collect();
                self.show_api_docs(*idx, matches);
                return;
            }
            None => {}
        }
        // On the declaration itself: go to what it implements (interface / base member), or
        // to its implementations.
        let mut found: Vec<(Found, &'static str)> = found.into_iter().map(|f| (f, "")).collect();
        if let Some(here) = self.declaration_at_cursor(found.iter().map(|(f, _)| f)) {
            found = self.related_members(&here);
            if found.is_empty() {
                self.info(format!(
                    "`{name}` is declared here; it implements nothing in the workspace \
                     (Shift+F12 lists its references)"
                ));
                return;
            }
        }
        self.push_back();
        if let [(f, role)] = found.as_slice() {
            let t = Target {
                path: f.path.clone(),
                line: f.sym.line,
                col: f.sym.col,
            };
            self.show_location(&t, true);
            let what = match &f.sym.container {
                Some(c) => format!("{c}.{}", f.sym.name),
                None => f.sym.name.clone(),
            };
            let role = if role.is_empty() {
                f.sym.kind.name()
            } else {
                role
            };
            self.info(format!("{role} {what}"));
            return;
        }
        let items = found
            .iter()
            .map(|(f, role)| {
                let what = match &f.sym.container {
                    Some(c) => format!("{c}.{}", f.sym.name),
                    None => f.sym.name.clone(),
                };
                let role = if role.is_empty() {
                    f.sym.kind.name()
                } else {
                    role
                };
                (
                    format!(
                        "{what}  {}:{}  ({role})",
                        self.display_path(&f.path),
                        f.sym.line + 1,
                    ),
                    f.path.clone(),
                    f.sym.line + 1,
                    f.sym.col + 1,
                )
            })
            .collect();
        self.picker = Some(FilePicker::definitions(&name, items));
    }

    /// The candidate that is the declaration under the editor cursor (its name starts there).
    fn declaration_at_cursor<'a>(
        &self,
        mut found: impl Iterator<Item = &'a Found>,
    ) -> Option<Found> {
        let doc = self.panes[EDITOR].doc()?;
        let path = doc.canonical.as_ref()?;
        let (start, _, _) = word_at(&doc.rope, doc.selection.head)?;
        let line = doc.rope.char_to_line(start);
        let col = start - doc.rope.line_to_char(line);
        found
            .find(|f| f.ext.is_none() && &f.path == path && f.sym.line == line && f.sym.col == col)
            .cloned()
    }

    /// What a declaration implements (members of its class's declared bases, or the bases
    /// themselves for a type) and what implements it (the same members of classes that list
    /// its type as a base, or those classes), with their role.
    fn related_members(&self, decl: &Found) -> Vec<(Found, &'static str)> {
        let ix = self.symbols.index.borrow();
        let family = symbols::family(&decl.path);
        let is_type = matches!(decl.sym.kind, Kind::Class | Kind::Interface);
        let owner = if is_type {
            decl.sym.name.clone()
        } else {
            match &decl.sym.container {
                Some(c) => c.clone(),
                None => return Vec::new(),
            }
        };
        let types = |p: &Path| -> Vec<Symbol> {
            ix.file(p)
                .map(|f| {
                    f.symbols
                        .iter()
                        .filter(|s| matches!(s.kind, Kind::Class | Kind::Interface))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        };
        let bases = types(&decl.path)
            .iter()
            .find(|s| s.name == owner)
            .map(|s| base_names(&s.signature))
            .unwrap_or_default();
        // Types (anywhere in the workspace) that list `owner` as a base.
        let implementers: Vec<String> = ix
            .paths()
            .filter(|p| symbols::family(p) == family)
            .flat_map(|p| types(p))
            .filter(|s| base_names(&s.signature).contains(&owner))
            .map(|s| s.name)
            .collect();
        let same_member =
            |s: &Symbol| s.kind == decl.sym.kind && s.params.len() == decl.sym.params.len();
        let mut out: Vec<(Found, &'static str)> = Vec::new();
        let mut add = |p: &Path, s: &Symbol, role: &'static str| {
            let f = Found {
                path: p.to_path_buf(),
                sym: s.clone(),
                ext: None,
            };
            if f != *decl && !out.iter().any(|(o, _)| *o == f) {
                out.push((f, role));
            }
        };
        if is_type {
            for b in &bases {
                for (p, s) in ix.named(b) {
                    if symbols::family(p) == family
                        && matches!(s.kind, Kind::Class | Kind::Interface)
                    {
                        add(p, s, s.kind.name());
                    }
                }
            }
            for i in &implementers {
                for (p, s) in ix.named(i) {
                    if symbols::family(p) == family
                        && matches!(s.kind, Kind::Class | Kind::Interface)
                    {
                        add(p, s, "implementation");
                    }
                }
            }
        } else {
            for (p, s) in ix.named(&decl.sym.name) {
                if symbols::family(p) != family || !same_member(s) {
                    continue;
                }
                match &s.container {
                    Some(c) if bases.contains(c) => add(p, s, s.kind.name()),
                    Some(c) if implementers.contains(c) => add(p, s, "implementation"),
                    _ => {}
                }
            }
        }
        out
    }

    /// Ctrl+F12: the web page of the name at the cursor (Microsoft Learn / MDN), or of the
    /// API on the Docs page, after asking.
    pub(super) fn open_docs_in_browser(&mut self) {
        if self.on_docs_page() {
            if let (Some(d), Some(idx)) = (&self.symbols.dotnet, self.symbols.docs_current) {
                let url = dotnet_docs::learn_url(d.id(idx)).unwrap_or_default();
                let title = format!("the Microsoft Learn page for {}", d.full_name(idx));
                self.confirm_open_url(url, title);
            }
            return;
        }
        if self.focus != EDITOR || !self.has_doc() {
            self.info("Put the cursor on a name in the editor");
            return;
        }
        if self.lsp_definition(true) {
            return;
        }
        let head = self.doc().selection.head;
        let Some((name, found)) = self.resolve_at(EDITOR, head) else {
            self.info("No name at the cursor");
            return;
        };
        match found.first().and_then(|f| f.ext.clone()) {
            Some(External::Dotnet { full, url, .. }) => {
                self.confirm_open_url(url, format!("the Microsoft Learn page for {full}"))
            }
            Some(External::Web(w)) => {
                self.confirm_open_url(w.url, format!("the MDN page for {}", w.title))
            }
            Some(External::Npm {
                see: Some(url),
                package,
            }) => self.confirm_open_url(url, format!("the {package} docs for `{name}`")),
            Some(External::Npm { package, .. }) => self.info(format!(
                "No web page known for `{name}` (its {package} declaration links none)"
            )),
            Some(External::Package(pkg)) => self.info(format!(
                "`{name}` comes from package `{pkg}` (no web page known)"
            )),
            None if found.is_empty() => self.info(format!("No docs found for `{name}`")),
            None => self.info(format!(
                "`{name}` is declared in the workspace: F12 goes there"
            )),
        }
    }

    /// Ask before opening `url` in the browser.
    pub(super) fn confirm_open_url(&mut self, url: String, title: String) {
        self.minibuffer = Some(crate::ui::minibuffer::Minibuffer::new(
            crate::ui::minibuffer::PromptKind::ConfirmOpenUrl { url, title },
            String::new(),
        ));
    }

    /// Open a web page (the user said yes). Headless runs and tests only record it.
    pub(super) fn open_url(&mut self, url: &str) {
        self.opened_url = Some(url.to_string());
        if !self.launch_urls {
            self.info(format!("Would open {url}"));
            return;
        }
        match crate::process::open_url(url) {
            Ok(()) => self.info(format!("Opened {url}")),
            Err(e) => self.warn(e),
        }
    }

    /// Remember the cursor for Alt+Left (before Go to Definition jumps).
    pub(super) fn push_back(&mut self) {
        if let Some(here) = self.cursor_target() {
            self.symbols.back.push(here);
            if self.symbols.back.len() > MAX_BACK {
                self.symbols.back.remove(0);
            }
        }
    }

    /// Alt+Left: back to where the last Go to Definition jumped from.
    pub(super) fn go_back(&mut self) {
        let here = self.cursor_target();
        while let Some(t) = self.symbols.back.pop() {
            if Some(&t) == here.as_ref() || !t.path.exists() {
                continue;
            }
            self.show_location(&t, true);
            return;
        }
        self.info("Nothing to go back to");
    }

    /// The editor view's file and cursor.
    fn cursor_target(&self) -> Option<Target> {
        let doc = self.panes[EDITOR].doc()?;
        let path = doc.canonical.clone()?;
        let line = doc.rope.char_to_line(doc.selection.head);
        Some(Target {
            path,
            line,
            col: doc.selection.head - doc.rope.line_to_char(line),
        })
    }
}

/// The base types a class declaration names: `class A : B, IC` / `class A extends B`.
fn base_names(sig: &str) -> Vec<String> {
    let tail = sig
        .find(" : ")
        .map(|k| &sig[k + 3..])
        .or_else(|| sig.find(" extends ").map(|k| &sig[k + 9..]))
        .unwrap_or("");
    tail.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty() && *w != "implements")
        .map(String::from)
        .collect()
}

/// A .NET API as a `Found`: its signature (C#), parameters (without the receiver of an
/// extension method called on a value) and docs.
fn dotnet_found(d: &DotnetDocs, idx: u32, recv: Recv) -> Found {
    let doc = d.doc(idx);
    let (signature, mut params) = d.signature(idx, &doc);
    if d.is_extension_call(idx, recv) && !params.is_empty() {
        params.remove(0);
    }
    let (kind, name, ty) = d.describe(idx);
    let kind = match kind {
        'T' => Kind::Class,
        'M' if d.is_ctor(idx) => Kind::Constructor,
        'M' => Kind::Method,
        _ => Kind::Property,
    };
    Found {
        path: PathBuf::new(),
        sym: Symbol {
            name,
            kind,
            container: (kind != Kind::Class).then_some(ty),
            signature,
            params,
            doc,
            line: 0,
            col: 0,
        },
        ext: Some(External::Dotnet {
            idx,
            full: d.full_name(idx),
            url: dotnet_docs::learn_url(d.id(idx)).unwrap_or_default(),
        }),
    }
}

/// A file inside a `node_modules` folder.
pub(super) fn in_node_modules(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "node_modules")
}

fn entry_too_big(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_FILE)
}

/// A whole file as a symbol (a default-imported component).
fn module_symbol(path: &Path) -> Symbol {
    Symbol {
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        kind: Kind::Module,
        container: None,
        signature: path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        params: vec![],
        doc: Doc::default(),
        line: 0,
        col: 0,
    }
}

/// The `(` of the innermost call around `pos` and the index of the argument `pos` is in.
/// Strings and `//` comments are skipped; brackets must balance. Inside a function body
/// passed to a call (`f(() => { … })`) there is no call.
pub fn enclosing_call(rope: &Rope, pos: usize) -> Option<(usize, usize)> {
    let pos = pos.min(rope.len_chars());
    let from = pos.saturating_sub(CALL_LOOKBACK);
    // Open brackets: (char, position, commas seen directly inside); a body `{` is `B`.
    let mut stack: Vec<(char, usize, usize)> = Vec::new();
    let mut last_solid = '\0';
    let mut quote: Option<char> = None;
    let mut comment = false;
    let mut prev = '\0';
    let mut escaped = false;
    for (k, c) in rope.slice(from..pos).chars().enumerate() {
        let at = from + k;
        if comment {
            if c == '\n' {
                comment = false;
            }
        } else if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q || c == '\n' && q != '`' {
                quote = None;
            }
        } else {
            match c {
                '"' | '\'' | '`' => quote = Some(c),
                '/' if prev == '/' => comment = true,
                '{' if matches!(last_solid, '>' | ')') => stack.push(('B', at, 0)),
                '(' | '[' | '{' => stack.push((c, at, 0)),
                ')' | ']' | '}' => {
                    stack.pop();
                }
                ',' => {
                    if let Some(top) = stack.last_mut() {
                        top.2 += 1;
                    }
                }
                _ => {}
            }
        }
        if !comment && quote.is_none() && !c.is_whitespace() {
            last_solid = c;
        }
        prev = c;
    }
    // Commas inside `{…}` / `[…]` within the call were counted for those, not the call.
    let (_, open, commas) = stack
        .iter()
        .rev()
        .take_while(|(c, _, _)| *c != 'B')
        .find(|(c, _, _)| *c == '(')?;
    Some((*open, *commas))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, Motion};
    use crate::config::Config;
    use crate::editor::selection::Selection;

    #[test]
    fn calls_and_arguments() {
        let r = |s: &str| {
            let pos = s.find('|').unwrap();
            let text = s.replace('|', "");
            let rope = Rope::from_str(&text);
            enclosing_call(&rope, text[..pos].chars().count())
        };
        assert_eq!(r("f(a, b|"), Some((1, 1)));
        assert_eq!(r("f(a, g(x, y), |"), Some((1, 2)));
        assert_eq!(r("f(a, g(x, |"), Some((6, 1)));
        assert_eq!(r("f({ a: 1, b: 2 }, |"), Some((1, 1)));
        assert_eq!(r("f(\"a, (b\", |"), Some((1, 1)));
        assert_eq!(r("f(a) |"), None);
        assert_eq!(r("// f(\n|"), None);
        assert_eq!(r("f(() => {\n  g;|"), None);
        assert_eq!(r("f(function (a, b) {\n  g(1, |"), Some((23, 1)));
    }

    #[test]
    fn names_and_qualifiers() {
        let rope = Rope::from_str("x = this.save(a); new Foo(); api?.get(1)");
        assert_eq!(word_at(&rope, 10).unwrap().2, "save");
        assert_eq!(word_at(&rope, 13).unwrap().2, "save"); // just after the name
        assert_eq!(qualifier(&rope, 9), Qual::Member("this".into()));
        assert_eq!(qualifier(&rope, 22), Qual::New);
        assert_eq!(qualifier(&rope, 34), Qual::Member("api".into()));
        assert_eq!(qualifier(&rope, 0), Qual::Plain);
    }

    /// A small project: `format.js` documents `formatCurrency`, `app.js` imports and calls
    /// it; `Store` has a method `save`; C# `Svc` implements a documented interface.
    fn project(name: &str) -> (Editor, PathBuf) {
        let dir = std::env::temp_dir().join(format!("jdc-sym-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        std::fs::write(
            dir.join("src/format.js"),
            "/**\n * Format money.\n * @param {number} amount - How much.\n * @param {string} [currency] - ISO code.\n * @returns {string} The text.\n */\nexport function formatCurrency(amount, currency = 'EUR') {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/store.js"),
            "export class Store {\n  /** Save it. */\n  save(item) {}\n}\nexport class Cache {\n  save(key, value) {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/app.js"),
            "import { formatCurrency as money } from './format';\nconst s = money(1, 'USD');\nstore.save(x);\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("Svc.cs"),
            "public interface ISvc {\n    /// <summary>Find one.</summary>\n    Item Find(int id);\n}\npublic class Svc : ISvc {\n    public Item Find(int id) => null;\n    void Use() { Find(1); }\n}\n",
        )
        .unwrap();
        let mut ed = Editor::new(Config::default(), &[dir.join("src/app.js")]);
        let mut ws = crate::workspace::Workspace::new("t");
        ws.folders = vec![dir.clone()];
        ed.workspace = Some(ws);
        ed.start_symbols();
        let until = Instant::now() + std::time::Duration::from_secs(10);
        while !ed.poll_symbols() && Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        (ed, dir)
    }

    fn put_cursor(ed: &mut Editor, needle: &str, offset: usize) {
        let text = ed.doc().rope.to_string();
        let byte = text.find(needle).unwrap() + offset;
        let pos = ed.doc().rope.byte_to_char(byte);
        ed.doc_mut().selection = super::super::selection::Selection::point(pos);
    }

    #[test]
    fn hover_follows_imports_and_go_to_definition_jumps() {
        let (mut ed, dir) = project("goto");
        assert!(ed.symbols.index.borrow().len() >= 4);
        put_cursor(&mut ed, "money(1", 2);
        let head = ed.doc().selection.head;
        let lines = ed.symbol_hover(EDITOR, head).unwrap();
        assert_eq!(
            lines[0],
            (
                "function formatCurrency(amount, currency = 'EUR')".into(),
                HoverKind::Code
            )
        );
        assert_eq!(lines[1].0, "Format money.");
        assert!(lines.iter().any(|l| l.0 == "amount: number — How much."));
        assert!(lines.iter().any(|l| l.0 == "→ string — The text."));
        // F12: format.js at the declaration; Alt+Left: back to the call.
        ed.execute(Command::GoToDefinition);
        assert_eq!(ed.doc().canonical, Some(dir.join("src/format.js")));
        let line = ed.doc().rope.char_to_line(ed.doc().selection.head);
        assert_eq!(line, 6);
        ed.execute(Command::GoBack);
        assert_eq!(ed.doc().canonical, Some(dir.join("src/app.js")));
        assert_eq!(ed.doc().selection.head, head);
        // `store.save`: two classes have `save`, so a picker lists both.
        put_cursor(&mut ed, "save(x", 1);
        ed.execute(Command::GoToDefinition);
        let p = ed.picker.as_ref().expect("picker");
        assert_eq!(p.len(), 2);
        ed.execute(Command::Newline);
        assert!(ed.picker.is_none());
        assert_eq!(ed.doc().canonical, Some(dir.join("src/store.js")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parameter_hints_follow_the_arguments() {
        let (mut ed, dir) = project("hints");
        put_cursor(&mut ed, "const s", 0);
        ed.execute(Command::Move(Motion::End, false));
        ed.execute(Command::Newline);
        ed.insert_text("money(");
        let sig = ed.symbols.signature.as_ref().expect("hints after `(`");
        assert!(sig.above);
        assert_eq!(sig.lines[1].0, "▸ amount: number — How much.");
        ed.insert_text("1, ");
        let sig = ed.symbols.signature.as_ref().unwrap();
        assert_eq!(sig.lines[1].0, "▸ currency = 'EUR': string — ISO code.");
        assert_eq!(sig.lines[1].1, HoverKind::Param);
        // An edit in the open file shows up without saving.
        ed.insert_text("'x')");
        assert!(ed.symbols.signature.is_none(), "closed with `)`");
        ed.execute(Command::Move(Motion::Left, false));
        ed.execute(Command::SignatureHelp);
        assert!(ed.symbols.signature.is_some());
        ed.execute(Command::Cancel);
        assert!(ed.symbols.signature.is_none(), "Esc closes the hints");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn csharp_members_borrow_interface_docs_and_live_edits_count() {
        let (mut ed, dir) = project("cs");
        ed.open_path(&dir.join("Svc.cs"));
        put_cursor(&mut ed, "Find(1)", 0);
        let head = ed.doc().selection.head;
        let lines = ed.symbol_hover(EDITOR, head).unwrap();
        assert_eq!(lines[0].0, "Item Find(int id)", "{lines:?}"); // same file first: ISvc's
        assert_eq!(lines[1].0, "Find one.");
        // The implementation has no doc of its own: it borrows the interface's.
        let ix = ed.symbols.index.borrow();
        let imp = ix
            .named("Find")
            .find(|(_, s)| s.container.as_deref() == Some("Svc"))
            .map(|(p, s)| Found {
                path: p.to_path_buf(),
                sym: s.clone(),
                ext: None,
            })
            .unwrap();
        drop(ix);
        assert_eq!(ed.effective_doc(&imp).summary, "Find one.");
        // A new method typed into the open file resolves right away.
        put_cursor(&mut ed, "void Use", 0);
        ed.insert_text("/// <summary>Brand new.</summary>");
        ed.execute(Command::Newline);
        ed.insert_text("void Fresh() {}");
        ed.execute(Command::Newline);
        put_cursor(&mut ed, "Find(1)", 0);
        ed.insert_text("Fresh(); ");
        put_cursor(&mut ed, "Fresh();", 1);
        let head = ed.doc().selection.head;
        let lines = ed.symbol_hover(EDITOR, head).unwrap();
        assert_eq!(lines[1].0, "Brand new.");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn f12_on_a_declaration_goes_between_interface_and_implementations() {
        let (mut ed, dir) = project("impl");
        std::fs::write(
            dir.join("Svc2.cs"),
            "public class Svc2 : ISvc {\n    public Item Find(int id) => null;\n}\n",
        )
        .unwrap();
        ed.symbols_file_changed(&dir.join("Svc2.cs"), false);
        ed.open_path(&dir.join("Svc.cs"));
        let line_of = |ed: &Editor| ed.doc().rope.char_to_line(ed.doc().selection.head);
        // On `Svc.Find`'s declaration: up to `ISvc.Find`.
        put_cursor(&mut ed, "Find(int id) =>", 0);
        ed.execute(Command::GoToDefinition);
        assert!(ed.picker.is_none());
        assert_eq!(line_of(&ed), 2, "ISvc.Find");
        assert!(ed.message.as_ref().unwrap().text.contains("ISvc.Find"));
        // On `ISvc.Find`: both implementations, in the picker.
        ed.execute(Command::GoToDefinition);
        let p = ed.picker.as_ref().expect("picker");
        assert_eq!(p.len(), 2);
        assert!(p.entries().all(|e| e.name.contains("(implementation)")));
        ed.execute(Command::Cancel);
        // A member that implements nothing.
        put_cursor(&mut ed, "Use()", 0);
        ed.execute(Command::GoToDefinition);
        assert!(
            ed.message
                .as_ref()
                .unwrap()
                .text
                .contains("implements nothing")
        );
        // The class: up to the interface.
        put_cursor(&mut ed, "Svc : ISvc", 0);
        ed.execute(Command::GoToDefinition);
        assert_eq!(line_of(&ed), 0);
        assert!(ed.doc().rope.line(0).to_string().contains("interface ISvc"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn npm_package_docs_and_jsdoc_types() {
        let dir = std::env::temp_dir().join(format!("jdc-sym-npm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let w = |rel: &str, text: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        w("package.json", "{}");
        w(
            "node_modules/vue/package.json",
            r#"{"types": "dist/vue.d.ts"}"#,
        );
        w(
            "node_modules/vue/dist/vue.d.ts",
            "/**\n * Takes a getter function and returns a readonly reactive ref object.\n *\n * @param getter - Function that produces the next value.\n * @example\n * ```js\n * const plusOne = computed(() => count.value + 1)\n * ```\n * @see {@link https://vuejs.org/api/reactivity-core.html#computed}\n */\nexport declare function computed<T>(getter: () => T): ComputedRef<T>;\n",
        );
        w(
            "types/models.d.ts",
            "/** A person using the app. */\nexport interface User {\n  id: number;\n  name: string;\n  /** ISO date */\n  joinedAt: string;\n}\nexport type Role = 'admin' | 'viewer';\n",
        );
        w(
            "src/App.vue",
            "<script setup>\nimport { computed } from 'vue';\n/**\n * @typedef {import('../types/models').User} User\n */\n/** @type {ReturnType<typeof load<User[]>>} */\nconst users = load();\nconst n = computed(() => 1);\n/** @type {import('../types/models').Role} */\nconst role = 'admin';\n</script>\n",
        );
        let dir = std::fs::canonicalize(&dir).unwrap();
        let app = dir.join("src/App.vue");
        let mut ed = Editor::new(Config::default(), std::slice::from_ref(&app));
        // Hover on `computed(`: the package's own docs, with the example.
        put_cursor(&mut ed, "computed(() => 1", 2);
        let head = ed.doc().selection.head;
        let lines = ed.symbol_hover(EDITOR, head).unwrap();
        assert!(
            lines[0]
                .0
                .starts_with("computed<T>(getter: () => T): ComputedRef<T>"),
            "{lines:?}"
        );
        assert!(lines[1].0.starts_with("Takes a getter function"));
        assert!(
            lines
                .iter()
                .any(|l| l.0 == "  const plusOne = computed(() => count.value + 1)")
        );
        let place = &lines.last().unwrap().0;
        assert!(
            place.starts_with("vue · vue/dist/vue.d.ts:11 · F12: declaration · Ctrl+F12"),
            "{place}"
        );
        // Parameter hints come from the declaration too (cursor inside the call).
        put_cursor(&mut ed, "computed(() => 1", 9);
        ed.execute(Command::SignatureHelp);
        assert_eq!(
            ed.symbols.signature.as_ref().unwrap().lines[1].0,
            "▸ getter: () => T — Function that produces the next value."
        );
        ed.execute(Command::Cancel);
        // Ctrl+F12: the @see page, after asking.
        put_cursor(&mut ed, "computed(() => 1", 2);
        ed.execute(Command::OpenDocsInBrowser);
        ed.insert_text("y");
        assert_eq!(
            ed.opened_url.as_deref(),
            Some("https://vuejs.org/api/reactivity-core.html#computed")
        );
        // F12: the declaration in node_modules.
        ed.execute(Command::GoToDefinition);
        assert_eq!(
            ed.doc().canonical,
            Some(dir.join("node_modules/vue/dist/vue.d.ts"))
        );
        assert_eq!(ed.doc().rope.char_to_line(ed.doc().selection.head), 10);
        ed.execute(Command::GoBack);
        // `User` inside a JSDoc type: through the typedef, to the interface.
        put_cursor(&mut ed, "User[]", 0);
        let head = ed.doc().selection.head;
        let lines = ed.symbol_hover(EDITOR, head).unwrap();
        assert_eq!(lines[0].0, "interface User");
        assert_eq!(lines[1].0, "A person using the app.");
        assert!(
            lines.iter().any(|l| l.0 == "  joinedAt: string"),
            "{lines:?}"
        );
        ed.execute(Command::GoToDefinition);
        assert_eq!(ed.doc().canonical, Some(dir.join("types/models.d.ts")));
        assert_eq!(ed.doc().rope.char_to_line(ed.doc().selection.head), 1);
        ed.execute(Command::GoBack);
        // `import('…').Role` in a comment.
        put_cursor(&mut ed, "Role}", 0);
        ed.execute(Command::GoToDefinition);
        assert_eq!(ed.doc().canonical, Some(dir.join("types/models.d.ts")));
        assert_eq!(ed.doc().rope.char_to_line(ed.doc().selection.head), 7);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    const FAKE_XML: &str = r#"<?xml version="1.0"?>
<doc><members>
    <member name="T:System.ComponentModel.DataAnnotations.RequiredAttribute">
      <summary>Specifies that a data field value is required.</summary>
    </member>
    <member name="T:System.String">
      <summary>Represents text.</summary>
    </member>
    <member name="M:System.String.Join(System.String,System.String[])">
      <summary>Joins strings.</summary>
      <param name="separator">The separator.</param>
      <param name="value">The parts.</param>
    </member>
    <member name="M:System.String.Join(System.Char,System.Object[])">
      <summary>Joins objects.</summary>
      <param name="separator">The char.</param>
      <param name="values">The objects.</param>
    </member>
    <member name="M:System.Linq.Enumerable.Where``1(System.Collections.Generic.IEnumerable{``0},System.Func{``0,System.Boolean})">
      <summary>Filters a sequence.</summary>
      <param name="source">The input.</param>
      <param name="predicate">The test.</param>
      <typeparam name="TSource">The element type.</typeparam>
    </member>
</members></doc>
"#;

    #[test]
    fn dotnet_names_show_sdk_docs_and_offer_learn() {
        let dir = std::env::temp_dir().join(format!("jdc-sym-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let xml = dir.join("Fake.xml");
        std::fs::write(&xml, FAKE_XML).unwrap();
        let file = dir.join("M.cs");
        std::fs::write(
            &file,
            "using System.ComponentModel.DataAnnotations;\nusing System.Linq;\nclass M\n{\n    [Required] public string Name { get; set; }\n    void F()\n    {\n        var x = items.Where(i => true);\n        \n    }\n}\n",
        )
        .unwrap();
        let mut ed = Editor::new(Config::default(), std::slice::from_ref(&file));
        ed.symbols.dotnet = Some(Arc::new(DotnetDocs::load(vec![xml])));
        // Hover on `[Required]`: the SDK summary, offline.
        put_cursor(&mut ed, "Required]", 2);
        let head = ed.doc().selection.head;
        let lines = ed.symbol_hover(EDITOR, head).unwrap();
        assert_eq!(
            lines[0].0,
            "System.ComponentModel.DataAnnotations.RequiredAttribute"
        );
        assert_eq!(lines[1].0, "Specifies that a data field value is required.");
        assert!(
            lines
                .last()
                .unwrap()
                .0
                .contains("Ctrl+F12: Microsoft Learn")
        );
        // F12: the Docs page; `w` asks before opening Microsoft Learn; `y` opens it.
        ed.execute(Command::GoToDefinition);
        assert!(ed.on_docs_page());
        let page = ed.doc().rope.to_string();
        assert!(
            page.starts_with("System.ComponentModel.DataAnnotations.RequiredAttribute\n"),
            "{page}"
        );
        ed.insert_text("w");
        assert!(matches!(
            ed.minibuffer.as_ref().map(|m| &m.kind),
            Some(crate::ui::minibuffer::PromptKind::ConfirmOpenUrl { .. })
        ));
        ed.insert_text("y");
        assert_eq!(
            ed.opened_url.as_deref(),
            Some(
                "https://learn.microsoft.com/dotnet/api/system.componentmodel.dataannotations.requiredattribute"
            )
        );
        // Parameter hints for an extension method skip the receiver.
        ed.execute(Command::FocusLeft);
        put_cursor(&mut ed, "items.Where(", 12);
        ed.execute(Command::SignatureHelp);
        let sig = ed.symbols.signature.as_ref().expect("hints");
        assert_eq!(
            sig.lines[1].0,
            "▸ Func<TSource, bool> predicate — The test."
        );
        ed.execute(Command::Cancel);
        // `string.Join(`: both overloads on the Docs page; Enter opens the other, Backspace
        // comes back.
        put_cursor(&mut ed, "        \n    }", 8);
        ed.insert_text("string.Join(");
        let sig = ed.symbols.signature.as_ref().expect("hints");
        assert_eq!(sig.lines[1].0, "▸ string separator — The separator.");
        ed.execute(Command::Move(Motion::Left, false));
        ed.execute(Command::GoToDefinition);
        let page = ed.doc().rope.to_string();
        assert!(page.contains("Overloads (2)"), "{page}");
        let row = page
            .lines()
            .position(|l| l.starts_with("  ▸ String.Join(char"))
            .unwrap();
        let doc = ed.doc_mut();
        doc.selection = Selection::point(doc.rope.line_to_char(row));
        ed.execute(Command::Newline);
        assert!(ed.doc().rope.to_string().contains("Joins objects."));
        ed.execute(Command::Backspace);
        assert!(ed.doc().rope.to_string().contains("Joins strings."));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn js_built_ins_link_to_mdn_and_packages_are_named() {
        let dir = std::env::temp_dir().join(format!("jdc-sym-mdn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let file = dir.join("a.js");
        std::fs::write(
            &file,
            "import { ref } from 'vue';\nconst data = JSON.parse(text);\nconst r = ref(rows.map(f));\n",
        )
        .unwrap();
        let mut ed = Editor::new(Config::default(), std::slice::from_ref(&file));
        put_cursor(&mut ed, "map(f)", 1);
        let head = ed.doc().selection.head;
        let lines = ed.symbol_hover(EDITOR, head).unwrap();
        assert_eq!(lines[0].0, "Array.prototype.map");
        // F12 on `JSON.parse` asks, then opens MDN.
        put_cursor(&mut ed, "parse(", 1);
        ed.execute(Command::GoToDefinition);
        assert!(ed.minibuffer.is_some());
        ed.insert_text("n");
        assert!(ed.opened_url.is_none());
        ed.execute(Command::GoToDefinition);
        ed.insert_text("y");
        assert_eq!(
            ed.opened_url.as_deref(),
            Some(
                "https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/JSON/parse"
            )
        );
        // `ref` comes from a package.
        put_cursor(&mut ed, "ref(rows", 1);
        ed.execute(Command::GoToDefinition);
        let msg = ed.message.as_ref().unwrap().text.clone();
        assert!(msg.contains("package `vue`"), "{msg}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
