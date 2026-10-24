//! .NET framework and package API docs, offline: the XML documentation files the installed
//! SDK (`packs/Microsoft.NETCore.App.Ref/…/ref/<tfm>/*.xml`, ASP.NET Core likewise) and the
//! project's NuGet packages ship. Every type and member is listed there by its full ID
//! (`M:System.String.IsNullOrEmpty(System.String)`), so a name plus the file's `using`s gives
//! the exact API, its docs and its Microsoft Learn page.
//!
//! Only IDs and file offsets are kept in memory; a member's doc is read when it is shown.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::Doc;

/// One documented API: its ID and where its `<member>` element starts.
#[derive(Debug, Clone)]
pub struct DocEntry {
    pub id: String,
    file: u32,
    offset: u64,
}

/// The parts of an ID: `M:System.Linq.Enumerable.Where``1(System.Func{``0,System.Boolean})`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiId<'a> {
    /// `T` type, `M` method, `P` property, `F` field, `E` event.
    pub kind: char,
    /// The type with namespace and arity: `System.Collections.Generic.List`1`.
    pub ty: &'a str,
    /// The member (`Where``1`, `#ctor`, `Count`); `None` for a type.
    pub member: Option<&'a str>,
    /// Parameter types as written in the ID.
    pub params: Option<&'a str>,
}

pub fn parse_id(id: &str) -> Option<ApiId<'_>> {
    let (kind, rest) = id.split_once(':')?;
    let kind = kind.chars().next()?;
    let (path, params) = match rest.find('(') {
        Some(k) => (
            &rest[..k],
            Some(
                rest[k + 1..]
                    .trim_end_matches(|c| c != ')')
                    .trim_end_matches(')'),
            ),
        ),
        None => (rest.split('~').next().unwrap_or(rest), None),
    };
    if kind == 'T' {
        return Some(ApiId {
            kind,
            ty: path,
            member: None,
            params: None,
        });
    }
    let dot = path.rfind('.')?;
    Some(ApiId {
        kind,
        ty: &path[..dot],
        member: Some(&path[dot + 1..]),
        params,
    })
}

/// `List`1` → `List`, `Where``1` → `Where`.
fn plain(name: &str) -> &str {
    name.split('`').next().unwrap_or(name)
}

fn simple_type(ty: &str) -> &str {
    plain(ty.rsplit('.').next().unwrap_or(ty))
}

/// C# keyword names of framework types.
const KEYWORD_TYPES: &[(&str, &str)] = &[
    ("System.Boolean", "bool"),
    ("System.Byte", "byte"),
    ("System.SByte", "sbyte"),
    ("System.Char", "char"),
    ("System.Decimal", "decimal"),
    ("System.Double", "double"),
    ("System.Single", "float"),
    ("System.Int16", "short"),
    ("System.UInt16", "ushort"),
    ("System.Int32", "int"),
    ("System.UInt32", "uint"),
    ("System.Int64", "long"),
    ("System.UInt64", "ulong"),
    ("System.Object", "object"),
    ("System.String", "string"),
    ("System.Void", "void"),
    ("System.IntPtr", "nint"),
    ("System.UIntPtr", "nuint"),
];

/// `string` → `System.String`.
pub fn keyword_type(word: &str) -> Option<&'static str> {
    KEYWORD_TYPES
        .iter()
        .find(|(_, k)| *k == word)
        .map(|(full, _)| *full)
}

/// A type as written in an ID, shown as C#: `System.Collections.Generic.IEnumerable{``0}` →
/// `IEnumerable<TSource>` (with the method's / type's type parameter names).
fn cs_type(s: &str, type_tps: &[String], method_tps: &[String]) -> String {
    let mut out = String::new();
    let mut by_ref = false;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            let method = chars.get(i + 1) == Some(&'`');
            let start = if method { i + 2 } else { i + 1 };
            let mut j = start;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let n: usize = chars[start..j]
                .iter()
                .collect::<String>()
                .parse()
                .unwrap_or(0);
            let tps = if method { method_tps } else { type_tps };
            out.push_str(&tps.get(n).cloned().unwrap_or_else(|| {
                if method {
                    format!("TM{n}")
                } else {
                    format!("T{n}")
                }
            }));
            i = j;
        } else if c.is_alphanumeric() || c == '_' || c == '.' {
            let mut j = i;
            while j < chars.len()
                && (chars[j].is_alphanumeric() || chars[j] == '_' || chars[j] == '.')
            {
                j += 1;
            }
            let name: String = chars[i..j].iter().collect();
            // `System.Nullable{X}` → `X?`
            if name == "System.Nullable" && chars.get(j) == Some(&'{') {
                let close = matching_brace(&chars, j);
                let inner: String = chars[j + 1..close].iter().collect();
                out.push_str(&cs_type(&inner, type_tps, method_tps));
                out.push('?');
                i = close + 1;
                continue;
            }
            match KEYWORD_TYPES.iter().find(|(full, _)| *full == name) {
                Some((_, k)) => out.push_str(k),
                None => out.push_str(name.rsplit('.').next().unwrap_or(&name)),
            }
            i = j;
        } else {
            match c {
                '{' => out.push('<'),
                '}' => out.push('>'),
                ',' => out.push_str(", "),
                '@' => by_ref = true,
                '[' => {
                    // `[0:,0:]` → `[,]`
                    let close = (i..chars.len())
                        .find(|&k| chars[k] == ']')
                        .unwrap_or(chars.len() - 1);
                    let commas = chars[i..close].iter().filter(|&&c| c == ',').count();
                    out.push('[');
                    out.push_str(&",".repeat(commas));
                    out.push(']');
                    i = close + 1;
                    continue;
                }
                _ => out.push(c),
            }
            i += 1;
        }
    }
    if by_ref { format!("ref {out}") } else { out }
}

fn matching_brace(chars: &[char], open: usize) -> usize {
    let mut depth = 0;
    for (k, c) in chars.iter().enumerate().skip(open) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return k;
                }
            }
            _ => {}
        }
    }
    chars.len() - 1
}

/// Split ID parameter types on top-level commas.
fn split_id_params(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if !s.is_empty() {
        out.push(&s[start..]);
    }
    out
}

/// The Microsoft Learn page of an API.
pub fn learn_url(id: &str) -> Option<String> {
    let api = parse_id(id)?;
    let fix = |s: &str| {
        // `List`1` → `list-1`; `Where``1` → `where`; `#ctor` → `-ctor`.
        let s = match s.find("``") {
            Some(k) => &s[..k],
            None => s,
        };
        s.replace(['`', '#'], "-").to_lowercase()
    };
    let mut url = format!("https://learn.microsoft.com/dotnet/api/{}", fix(api.ty));
    if let Some(m) = api.member {
        url.push('.');
        url.push_str(&fix(m));
    }
    Some(url)
}

/// What stands before a name, for looking it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receiver<'a> {
    /// `Name`, `[Name]`
    None,
    /// `new Name(`
    New,
    /// `x.Name`
    Of(&'a str),
}

/// The namespaces a C# file sees: its `using`s, the project's global usings, its own
/// namespace and that namespace's parents.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scope {
    pub namespaces: Vec<String>,
    /// `using static X.Y.Type;`
    pub static_types: Vec<String>,
    /// `using A = X.Y;`
    pub aliases: Vec<(String, String)>,
}

static USING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*(?:global\s+)?using\s+(static\s+)?(?:([A-Za-z_]\w*)\s*=\s*)?([A-Za-z_][\w.]*)\s*;").unwrap()
});
static NAMESPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*namespace\s+([A-Za-z_][\w.]*)").unwrap());

impl Scope {
    /// The usings of C# source text (and `global using`s, for the generated file).
    pub fn add_source(&mut self, text: &str) {
        for c in USING.captures_iter(text) {
            let target = c[3].to_string();
            if c.get(1).is_some() {
                self.static_types.push(target);
            } else if let Some(alias) = c.get(2) {
                self.aliases.push((alias.as_str().to_string(), target));
            } else if !self.namespaces.contains(&target) {
                self.namespaces.push(target);
            }
        }
        for c in NAMESPACE.captures_iter(text) {
            let mut ns = c[1].to_string();
            loop {
                if !self.namespaces.contains(&ns) {
                    self.namespaces.push(ns.clone());
                }
                match ns.rfind('.') {
                    Some(k) => ns.truncate(k),
                    None => break,
                }
            }
        }
    }

    fn sees(&self, ns: &str) -> bool {
        self.namespaces.iter().any(|n| n == ns)
    }
}

/// The implicit usings of a project: its generated `*.GlobalUsings.g.cs` (after a build),
/// else the SDK's defaults when `<ImplicitUsings>` is on.
pub fn global_usings(csproj: &Path) -> Scope {
    let mut scope = Scope::default();
    let dir = csproj.parent().unwrap_or(Path::new("."));
    let generated = walk_files(&dir.join("obj"), 4)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".GlobalUsings.g.cs"))
        })
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    if let Some(text) = generated.and_then(|p| std::fs::read_to_string(p).ok()) {
        scope.add_source(&text);
        return scope;
    }
    let proj = std::fs::read_to_string(csproj).unwrap_or_default();
    if proj.contains("<ImplicitUsings>enable</ImplicitUsings>") {
        let mut ns = vec![
            "System",
            "System.Collections.Generic",
            "System.IO",
            "System.Linq",
            "System.Net.Http",
            "System.Threading",
            "System.Threading.Tasks",
        ];
        if proj.contains("Microsoft.NET.Sdk.Web") {
            ns.extend([
                "System.Net.Http.Json",
                "Microsoft.AspNetCore.Builder",
                "Microsoft.AspNetCore.Hosting",
                "Microsoft.AspNetCore.Http",
                "Microsoft.AspNetCore.Routing",
                "Microsoft.Extensions.Configuration",
                "Microsoft.Extensions.DependencyInjection",
                "Microsoft.Extensions.Hosting",
                "Microsoft.Extensions.Logging",
            ]);
        }
        scope.namespaces = ns.into_iter().map(String::from).collect();
    }
    scope
}

fn walk_files(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth > 0 {
                out.extend(walk_files(&p, depth - 1));
            }
        } else {
            out.push(p);
        }
    }
    out
}

// ---------------------------------------------------------------- finding the XML files

/// The folder the `dotnet` command runs from (where `packs/` is).
pub fn dotnet_root() -> Option<PathBuf> {
    if let Some(r) = std::env::var_os("DOTNET_ROOT").map(PathBuf::from)
        && r.join("packs").is_dir()
    {
        return Some(r);
    }
    let exe = crate::test_explorer::run::find_dotnet()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    exe.parent().map(Path::to_path_buf)
}

/// Version folder names, newest first: `10.0.13` > `9.0.4` > `10.0.0-preview`.
fn by_version(dirs: &mut [PathBuf]) {
    let key = |p: &PathBuf| -> (Vec<u64>, bool) {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let (nums, pre) = name
            .split_once('-')
            .map_or((name, false), |(a, _)| (a, true));
        (
            nums.split('.').map(|n| n.parse().unwrap_or(0)).collect(),
            !pre,
        )
    };
    dirs.sort_by_key(|p| std::cmp::Reverse(key(p)));
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

fn xml_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "xml"))
                .collect()
        })
        .unwrap_or_default()
}

/// The XML docs of a reference pack (`Microsoft.NETCore.App.Ref`) for `tfm`: the newest
/// version that has it (any target when `tfm` is unknown).
fn pack_xml(root: &Path, pack: &str, tfm: Option<&str>) -> Vec<PathBuf> {
    let mut versions = subdirs(&root.join("packs").join(pack));
    by_version(&mut versions);
    for v in versions {
        let refs = v.join("ref");
        let dir = match tfm {
            Some(t) => refs.join(t),
            None => {
                let mut tfms = subdirs(&refs);
                tfms.sort();
                match tfms.pop() {
                    Some(d) => d,
                    None => continue,
                }
            }
        };
        if dir.is_dir() {
            return xml_in(&dir);
        }
    }
    Vec::new()
}

static TFM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<TargetFrameworks?>\s*([^;<\s]+)").unwrap());

/// Every XML doc file a C# project's code can see: the framework (and ASP.NET Core for web
/// projects) from the SDK, and its NuGet packages from `obj/project.assets.json`.
pub fn project_xml_files(csproj: &Path, root: Option<&Path>) -> Vec<PathBuf> {
    let dir = csproj.parent().unwrap_or(Path::new("."));
    let proj = std::fs::read_to_string(csproj).unwrap_or_default();
    let assets: Option<serde_json::Value> =
        std::fs::read_to_string(dir.join("obj/project.assets.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
    let tfm = assets
        .as_ref()
        .and_then(|a| a.get("targets")?.as_object()?.keys().next().cloned())
        .map(|t| t.split('/').next().unwrap_or(&t).to_string())
        .or_else(|| TFM.captures(&proj).map(|c| c[1].to_string()));
    let mut files = Vec::new();
    if let Some(root) = root {
        files.extend(pack_xml(root, "Microsoft.NETCore.App.Ref", tfm.as_deref()));
        if proj.contains("Microsoft.NET.Sdk.Web") || proj.contains("Microsoft.AspNetCore.App") {
            files.extend(pack_xml(
                root,
                "Microsoft.AspNetCore.App.Ref",
                tfm.as_deref(),
            ));
        }
    }
    if let Some(a) = &assets {
        let folders: Vec<PathBuf> = a
            .get("packageFolders")
            .and_then(|f| f.as_object())
            .map(|f| f.keys().map(PathBuf::from).collect())
            .unwrap_or_default();
        let libraries = a.get("libraries").and_then(|l| l.as_object());
        let target = a
            .get("targets")
            .and_then(|t| t.as_object())
            .and_then(|t| t.values().next())
            .and_then(|t| t.as_object());
        for (name, lib) in target.into_iter().flatten() {
            if lib.get("type").and_then(|t| t.as_str()) != Some("package") {
                continue;
            }
            let Some(path) = libraries
                .and_then(|l| l.get(name))
                .and_then(|l| l.get("path"))
                .and_then(|p| p.as_str())
            else {
                continue;
            };
            for dll in lib
                .get("compile")
                .and_then(|c| c.as_object())
                .into_iter()
                .flat_map(|c| c.keys())
                .filter(|k| k.ends_with(".dll"))
            {
                let rel = Path::new(path).join(dll).with_extension("xml");
                if let Some(xml) = folders.iter().map(|f| f.join(&rel)).find(|p| p.is_file()) {
                    files.push(xml);
                }
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

// ---------------------------------------------------------------- the index

#[derive(Debug, Default)]
pub struct DotnetDocs {
    files: Vec<PathBuf>,
    pub entries: Vec<DocEntry>,
    /// Simple name (no arity; attributes also without `Attribute`) → entries.
    by_name: HashMap<String, Vec<u32>>,
    /// Type ID name → its member entries.
    by_type: HashMap<String, Vec<u32>>,
    /// Type ID name → its `T:` entry.
    types: HashMap<String, u32>,
}

const MEMBER: &str = "<member name=\"";

impl DotnetDocs {
    /// Index the `<member name="…">` elements of these XML doc files.
    pub fn load(files: Vec<PathBuf>) -> Self {
        let mut d = DotnetDocs::default();
        // Facade assemblies repeat members documented elsewhere: keep the first.
        let mut seen: HashSet<String> = HashSet::new();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let file = d.files.len() as u32;
            d.files.push(path);
            let mut at = 0;
            while let Some(k) = text[at..].find(MEMBER) {
                let start = at + k;
                let name_start = start + MEMBER.len();
                let Some(len) = text[name_start..].find('"') else {
                    break;
                };
                let id = text[name_start..name_start + len]
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&amp;", "&");
                at = name_start + len;
                if !seen.insert(id.clone()) {
                    continue;
                }
                d.add(DocEntry {
                    id,
                    file,
                    offset: start as u64,
                });
            }
        }
        d
    }

    fn add(&mut self, e: DocEntry) {
        let Some(api) = parse_id(&e.id) else {
            return;
        };
        let idx = self.entries.len() as u32;
        let mut keys: Vec<String> = Vec::new();
        match api.member {
            None => {
                let name = simple_type(api.ty).to_string();
                if let Some(short) = name.strip_suffix("Attribute")
                    && !short.is_empty()
                {
                    keys.push(short.to_string());
                }
                keys.push(name);
                self.types.insert(api.ty.to_string(), idx);
            }
            Some(m) => {
                if m != "#ctor" && m != "#cctor" {
                    keys.push(plain(m).to_string());
                }
                self.by_type
                    .entry(api.ty.to_string())
                    .or_default()
                    .push(idx);
            }
        }
        for k in keys {
            self.by_name.entry(k).or_default().push(idx);
        }
        self.entries.push(e);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn id(&self, idx: u32) -> &str {
        &self.entries[idx as usize].id
    }

    /// The namespace of a type ID name: its longest prefix that is not itself a type
    /// (nested types sit under their outer type).
    pub fn namespace_of<'a>(&self, ty: &'a str) -> &'a str {
        let mut end = ty.len();
        while let Some(k) = ty[..end].rfind('.') {
            if !self.types.contains_key(&ty[..k]) {
                return &ty[..k];
            }
            end = k;
        }
        ""
    }

    /// The docs of an entry (read from its file).
    pub fn doc(&self, idx: u32) -> Doc {
        let e = &self.entries[idx as usize];
        let Ok(mut f) = std::fs::File::open(&self.files[e.file as usize]) else {
            return Doc::default();
        };
        if f.seek(SeekFrom::Start(e.offset)).is_err() {
            return Doc::default();
        }
        let mut buf = Vec::with_capacity(4096);
        let mut chunk = [0u8; 8192];
        // Members are small; stop at the closing tag (or 256 KiB).
        while buf.len() < 256 * 1024 {
            match f.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
            if let Some(k) = find(&buf, b"</member>") {
                buf.truncate(k);
                break;
            }
            // `<member name="…" />` without a body.
            if let Some(k) = find(&buf, b"/>")
                && !buf[..k].contains(&b'\n')
                && find(&buf[..k], b"<summary").is_none()
            {
                buf.truncate(k);
                break;
            }
        }
        super::cs::parse_xml_doc(&String::from_utf8_lossy(&buf))
    }

    /// Entries for `name` as the file in `scope` sees it, best first (overloads together).
    pub fn resolve(&self, scope: &Scope, name: &str, recv: Receiver) -> Vec<u32> {
        let named = self.by_name.get(name).map_or(&[][..], |v| v.as_slice());
        let api = |i: &u32| parse_id(self.id(*i));
        let visible_type = |ty: &str| scope.sees(self.namespace_of(ty));
        // Types named `name` that the file can use (aliases included).
        let types: Vec<u32> = match scope.aliases.iter().find(|(a, _)| a == name) {
            Some((_, full)) => self
                .types
                .iter()
                .filter(|(t, _)| plain(t) == full)
                .map(|(_, i)| *i)
                .collect(),
            None => named
                .iter()
                .copied()
                .filter(|i| api(i).is_some_and(|a| a.member.is_none() && visible_type(a.ty)))
                .collect(),
        };
        let members_of = |ty: &str| -> Vec<u32> {
            self.by_type
                .get(ty)
                .into_iter()
                .flatten()
                .copied()
                .filter(|i| {
                    api(i)
                        .and_then(|a| a.member)
                        .is_some_and(|m| plain(m) == name)
                })
                .collect()
        };
        match recv {
            Receiver::New => {
                let ctors: Vec<u32> = types
                    .iter()
                    .flat_map(|t| {
                        let ty = parse_id(self.id(*t))
                            .map(|a| a.ty.to_string())
                            .unwrap_or_default();
                        self.by_type
                            .get(&ty)
                            .into_iter()
                            .flatten()
                            .copied()
                            .filter(|i| api(i).and_then(|a| a.member) == Some("#ctor"))
                            .collect::<Vec<_>>()
                    })
                    .collect();
                if ctors.is_empty() { types } else { ctors }
            }
            Receiver::None => {
                if !types.is_empty() {
                    return types;
                }
                // `using static` types, then members of visible types (inherited ones, like
                // a controller's `Ok()`, included).
                let statics: Vec<u32> = scope
                    .static_types
                    .iter()
                    .flat_map(|t| members_of(t))
                    .collect();
                if !statics.is_empty() {
                    return statics;
                }
                self.visible_members(named, &visible_type)
            }
            Receiver::Of(obj) => {
                // `string.Join`, `Console.WriteLine`, `Task.FromResult`: members of that type.
                let owner: Vec<String> = match keyword_type(obj) {
                    Some(full) => vec![full.to_string()],
                    None => self
                        .by_name
                        .get(obj)
                        .into_iter()
                        .flatten()
                        .filter_map(api)
                        .filter(|a| a.member.is_none() && visible_type(a.ty))
                        .map(|a| a.ty.to_string())
                        .collect(),
                };
                let of_owner: Vec<u32> = owner.iter().flat_map(|t| members_of(t)).collect();
                if !of_owner.is_empty() {
                    return of_owner;
                }
                // `x.Where(…)`, `_logger.LogInformation(…)`: members (extension methods
                // included) of any visible type.
                self.visible_members(named, &visible_type)
            }
        }
    }

    fn visible_members(&self, named: &[u32], visible_type: &dyn Fn(&str) -> bool) -> Vec<u32> {
        let mut out: Vec<u32> = named
            .iter()
            .copied()
            .filter(|i| {
                parse_id(self.id(*i)).is_some_and(|a| a.member.is_some() && visible_type(a.ty))
            })
            .collect();
        // Overloads of one member together, in file order.
        out.sort_by(|a, b| {
            let (ta, tb) = (parse_id(self.id(*a)), parse_id(self.id(*b)));
            ta.map(|x| x.ty).cmp(&tb.map(|x| x.ty)).then(a.cmp(b))
        });
        out
    }

    /// The other overloads of an entry's member (itself included).
    pub fn overloads(&self, idx: u32) -> Vec<u32> {
        let Some(a) = parse_id(self.id(idx)) else {
            return vec![idx];
        };
        let Some(m) = a.member else {
            return vec![idx];
        };
        self.by_type
            .get(a.ty)
            .into_iter()
            .flatten()
            .copied()
            .filter(|i| {
                parse_id(self.id(*i))
                    .and_then(|b| b.member)
                    .is_some_and(|n| plain(n) == plain(m))
            })
            .collect()
    }

    /// The members of a type entry, in file order.
    pub fn members(&self, type_idx: u32) -> Vec<u32> {
        parse_id(self.id(type_idx))
            .and_then(|a| self.by_type.get(a.ty))
            .cloned()
            .unwrap_or_default()
    }

    /// The type parameter names of a type ID name (from its docs).
    fn type_params(&self, ty: &str) -> Vec<String> {
        let arity: usize = ty
            .rsplit('`')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        if arity == 0 {
            return Vec::new();
        }
        let names = self
            .types
            .get(ty)
            .map(|i| self.doc(*i).typeparams)
            .unwrap_or_default();
        (0..arity)
            .map(|k| names.get(k).cloned().unwrap_or_else(|| format!("T{k}")))
            .collect()
    }

    /// How an entry reads in C#, and its parameters (`Type name`), using `doc` for names:
    /// `Enumerable.Where<TSource>(IEnumerable<TSource> source, Func<TSource, bool> predicate)`.
    pub fn signature(&self, idx: u32, doc: &Doc) -> (String, Vec<String>) {
        let Some(a) = parse_id(self.id(idx)) else {
            return (self.id(idx).to_string(), Vec::new());
        };
        let type_tps = self.type_params(a.ty);
        let generic = |name: &str, tps: &[String]| {
            if tps.is_empty() {
                name.to_string()
            } else {
                format!("{name}<{}>", tps.join(", "))
            }
        };
        let ty_name = generic(simple_type(a.ty), &type_tps);
        let Some(member) = a.member else {
            // `System.Collections.Generic.List<T>` (outer types of nested ones included).
            let full = match a.ty.rfind('.') {
                Some(k) => format!("{}.{ty_name}", &a.ty[..k]),
                None => ty_name,
            };
            return (full, Vec::new());
        };
        let method_arity: usize = member
            .split("``")
            .nth(1)
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let method_tps: Vec<String> = (0..method_arity)
            .map(|k| {
                doc.typeparams
                    .get(k)
                    .cloned()
                    .unwrap_or_else(|| format!("T{k}"))
            })
            .collect();
        let params: Vec<String> = a
            .params
            .map(|p| {
                split_id_params(p)
                    .into_iter()
                    .enumerate()
                    .map(|(k, t)| {
                        let ty = cs_type(t, &type_tps, &method_tps);
                        match doc.params.get(k) {
                            Some(dp) => format!("{ty} {}", dp.name),
                            None => ty,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let head = match (a.kind, member) {
            ('M', "#ctor") => format!("new {ty_name}"),
            ('M', m) => format!("{}.{}", ty_name, generic(plain(m), &method_tps)),
            ('E', m) => format!("event {ty_name}.{m}"),
            (_, m) => format!("{ty_name}.{m}"),
        };
        let sig = if a.kind == 'M' {
            format!("{head}({})", params.join(", "))
        } else {
            head
        };
        (sig, params)
    }

    /// An entry's kind (`T`, `M`, `P`, `F`, `E`), its name and its type's simple name; a
    /// constructor is named after its type.
    pub fn describe(&self, idx: u32) -> (char, String, String) {
        match parse_id(self.id(idx)) {
            Some(a) => {
                let ty = simple_type(a.ty).to_string();
                let name = match a.member {
                    None | Some("#ctor") => ty.clone(),
                    Some(m) => plain(m).to_string(),
                };
                (a.kind, name, ty)
            }
            None => ('?', self.id(idx).to_string(), String::new()),
        }
    }

    /// The namespace of an entry's type.
    pub fn namespace_of_entry(&self, idx: u32) -> String {
        parse_id(self.id(idx))
            .map(|a| self.namespace_of(a.ty).to_string())
            .unwrap_or_default()
    }

    /// How many parameters a method takes (0 for other entries).
    pub fn param_count(&self, idx: u32) -> usize {
        parse_id(self.id(idx))
            .and_then(|a| a.params)
            .map_or(0, |p| split_id_params(p).len())
    }

    /// Whether the entry is a constructor.
    pub fn is_ctor(&self, idx: u32) -> bool {
        parse_id(self.id(idx)).is_some_and(|a| a.member == Some("#ctor"))
    }

    /// The XML file an entry is documented in (its assembly's name).
    pub fn assembly(&self, idx: u32) -> String {
        let e = &self.entries[idx as usize];
        self.files[e.file as usize]
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The full name of an entry's type or member, for titles: `System.String.IsNullOrEmpty`.
    pub fn full_name(&self, idx: u32) -> String {
        match parse_id(self.id(idx)) {
            Some(a) => {
                let ty = a.ty.split('`').next().unwrap_or(a.ty);
                match a.member {
                    Some("#ctor") => format!("{ty} constructor"),
                    Some(m) => format!("{ty}.{}", plain(m)),
                    None => ty.to_string(),
                }
            }
            None => self.id(idx).to_string(),
        }
    }

    /// Whether the entry is an extension-style static method called on a receiver (its
    /// first parameter is the receiver): LINQ, logging and the like.
    pub fn is_extension_call(&self, idx: u32, recv: Receiver) -> bool {
        let Receiver::Of(obj) = recv else {
            return false;
        };
        parse_id(self.id(idx)).is_some_and(|a| {
            let ty = simple_type(a.ty);
            a.kind == 'M'
                && ty != obj
                && keyword_type(obj).is_none_or(|k| simple_type(k) != ty)
                && (ty.ends_with("Extensions")
                    || matches!(ty, "Enumerable" | "Queryable" | "MemoryExtensions"))
        })
    }

    /// Distinct types of the files (for the log).
    pub fn file_count(&self) -> usize {
        self.files.iter().collect::<HashSet<_>>().len()
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<doc>
  <assembly><name>Fake</name></assembly>
  <members>
    <member name="T:System.String">
      <summary>Represents text.</summary>
    </member>
    <member name="M:System.String.IsNullOrEmpty(System.String)">
      <summary>Indicates whether the string is <see langword="null" /> or empty.</summary>
      <param name="value">The string to test.</param>
      <returns><see langword="true" /> if empty.</returns>
    </member>
    <member name="M:System.String.Join(System.String,System.String[])">
      <summary>Joins strings.</summary>
      <param name="separator">The separator.</param>
      <param name="value">The parts.</param>
    </member>
    <member name="M:System.String.Join(System.Char,System.Object[])">
      <summary>Joins objects.</summary>
      <param name="separator">The separator.</param>
      <param name="values">The parts.</param>
    </member>
    <member name="T:System.Collections.Generic.List`1">
      <summary>A list.</summary>
      <typeparam name="T">The element type.</typeparam>
    </member>
    <member name="M:System.Collections.Generic.List`1.#ctor(System.Int32)">
      <summary>Makes a list.</summary>
      <param name="capacity">Room.</param>
    </member>
    <member name="M:System.Collections.Generic.List`1.Add(`0)">
      <summary>Adds.</summary>
      <param name="item">The item.</param>
    </member>
    <member name="M:System.Linq.Enumerable.Where``1(System.Collections.Generic.IEnumerable{``0},System.Func{``0,System.Boolean})">
      <summary>Filters a sequence.</summary>
      <param name="source">The input.</param>
      <param name="predicate">The test.</param>
      <typeparam name="TSource">The element type.</typeparam>
      <exception cref="T:System.ArgumentNullException"><paramref name="source" /> is null.</exception>
    </member>
    <member name="T:System.ComponentModel.DataAnnotations.RequiredAttribute">
      <summary>Specifies that a data field value is required.</summary>
    </member>
    <member name="T:System.Environment.SpecialFolder">
      <summary>A folder.</summary>
    </member>
    <member name="T:System.Environment">
      <summary>The environment.</summary>
    </member>
    <member name="M:System.Int32.TryParse(System.String,System.Int32@)">
      <summary>Parses.</summary>
      <param name="s">Text.</param>
      <param name="result">The number.</param>
    </member>
  </members>
</doc>
"#;

    fn docs(name: &str) -> (DotnetDocs, PathBuf) {
        let dir = std::env::temp_dir().join(format!("jdc-dnd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("Fake.xml");
        std::fs::write(&f, XML).unwrap();
        (DotnetDocs::load(vec![f]), dir)
    }

    fn scope(src: &str) -> Scope {
        let mut s = Scope::default();
        s.add_source(src);
        s
    }

    #[test]
    fn ids_urls_and_signatures() {
        let (d, dir) = docs("ids");
        assert_eq!(d.len(), 12);
        assert_eq!(
            learn_url("T:System.ComponentModel.DataAnnotations.RequiredAttribute").unwrap(),
            "https://learn.microsoft.com/dotnet/api/system.componentmodel.dataannotations.requiredattribute"
        );
        assert_eq!(
            learn_url("T:System.Collections.Generic.List`1").unwrap(),
            "https://learn.microsoft.com/dotnet/api/system.collections.generic.list-1"
        );
        assert_eq!(
            learn_url("M:System.Linq.Enumerable.Where``1(System.Func{``0,System.Boolean})")
                .unwrap(),
            "https://learn.microsoft.com/dotnet/api/system.linq.enumerable.where"
        );
        assert_eq!(
            learn_url("M:System.Collections.Generic.List`1.#ctor(System.Int32)").unwrap(),
            "https://learn.microsoft.com/dotnet/api/system.collections.generic.list-1.-ctor"
        );
        let find = |id: &str| d.entries.iter().position(|e| e.id == id).unwrap() as u32;
        let sig = |id: &str| {
            let i = find(id);
            d.signature(i, &d.doc(i))
        };
        let where_id = "M:System.Linq.Enumerable.Where``1(System.Collections.Generic.IEnumerable{``0},System.Func{``0,System.Boolean})";
        assert_eq!(
            sig(where_id),
            (
                "Enumerable.Where<TSource>(IEnumerable<TSource> source, Func<TSource, bool> predicate)".into(),
                vec!["IEnumerable<TSource> source".into(), "Func<TSource, bool> predicate".into()]
            )
        );
        assert_eq!(
            sig("M:System.Collections.Generic.List`1.Add(`0)").0,
            "List<T>.Add(T item)"
        );
        assert_eq!(
            sig("M:System.Collections.Generic.List`1.#ctor(System.Int32)").0,
            "new List<T>(int capacity)"
        );
        assert_eq!(
            sig("M:System.Int32.TryParse(System.String,System.Int32@)").0,
            "Int32.TryParse(string s, ref int result)"
        );
        assert_eq!(
            sig("T:System.Collections.Generic.List`1").0,
            "System.Collections.Generic.List<T>"
        );
        assert_eq!(
            sig("T:System.Environment.SpecialFolder").0,
            "System.Environment.SpecialFolder"
        );
        assert_eq!(d.namespace_of("System.Environment.SpecialFolder"), "System");
        let w = d.doc(find(where_id));
        assert_eq!(w.summary, "Filters a sequence.");
        assert_eq!(
            w.exceptions,
            [("ArgumentNullException".into(), "source is null.".into())]
        );
        let e = d.doc(find("M:System.String.IsNullOrEmpty(System.String)"));
        assert_eq!(e.summary, "Indicates whether the string is null or empty.");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_resolve_through_usings() {
        let (d, dir) = docs("scope");
        let ids = |v: Vec<u32>| {
            v.into_iter()
                .map(|i| d.id(i).to_string())
                .collect::<Vec<_>>()
        };
        let model = scope(
            "using System;\nusing System.ComponentModel.DataAnnotations;\nnamespace App.Models;\n",
        );
        assert_eq!(
            ids(d.resolve(&model, "Required", Receiver::None)),
            ["T:System.ComponentModel.DataAnnotations.RequiredAttribute"]
        );
        // Without the using, `Required` is unknown.
        assert!(
            d.resolve(&scope("using System;"), "Required", Receiver::None)
                .is_empty()
        );
        // `string.Join`: both overloads; `string.IsNullOrEmpty`.
        assert_eq!(d.resolve(&model, "Join", Receiver::Of("string")).len(), 2);
        assert_eq!(
            ids(d.resolve(&model, "IsNullOrEmpty", Receiver::Of("String"))),
            ["M:System.String.IsNullOrEmpty(System.String)"]
        );
        // `x.Where(…)`: an extension method of a visible namespace only.
        assert!(d.resolve(&model, "Where", Receiver::Of("items")).is_empty());
        let linq = scope("using System.Linq;");
        let w = d.resolve(&linq, "Where", Receiver::Of("items"));
        assert_eq!(w.len(), 1);
        assert!(d.is_extension_call(w[0], Receiver::Of("items")));
        // `new List<int>(`: the constructor.
        let generic = scope("using System.Collections.Generic;");
        assert_eq!(
            ids(d.resolve(&generic, "List", Receiver::New)),
            ["M:System.Collections.Generic.List`1.#ctor(System.Int32)"]
        );
        assert_eq!(
            d.overloads(d.resolve(&model, "Join", Receiver::Of("string"))[0])
                .len(),
            2
        );
        // `using static` and aliases.
        let st = scope("using static System.String;");
        assert_eq!(d.resolve(&st, "IsNullOrEmpty", Receiver::None).len(), 1);
        let al = scope("using Req = System.ComponentModel.DataAnnotations.RequiredAttribute;");
        assert_eq!(d.resolve(&al, "Req", Receiver::None).len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scope_reads_usings_and_namespaces() {
        let s = scope(
            "global using System;\nusing static System.Math;\nusing J = System.Text.Json;\nusing System.Linq;\nnamespace Inventory.Api.Services;\n",
        );
        assert_eq!(
            s.namespaces,
            [
                "System",
                "System.Linq",
                "Inventory.Api.Services",
                "Inventory.Api",
                "Inventory"
            ]
        );
        assert_eq!(s.static_types, ["System.Math"]);
        assert_eq!(
            s.aliases,
            [("J".to_string(), "System.Text.Json".to_string())]
        );
    }

    /// The real SDK and the sandbox project, when installed.
    #[test]
    fn installed_sdk_and_packages() {
        let csproj = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("sandbox/dotnet-inventory/src/Inventory.Api/Inventory.Api.csproj");
        let Some(root) = dotnet_root() else {
            return;
        };
        let files = project_xml_files(&csproj, Some(&root));
        if files.is_empty() {
            return;
        }
        let started = std::time::Instant::now();
        let d = DotnetDocs::load(files);
        eprintln!(
            "{} entries from {} files in {:?}",
            d.len(),
            d.file_count(),
            started.elapsed()
        );
        let mut s = global_usings(&csproj);
        s.add_source(
            "using System.ComponentModel.DataAnnotations;\nusing Microsoft.EntityFrameworkCore;\n",
        );
        let req = d.resolve(&s, "Required", Receiver::None);
        assert_eq!(
            req.iter().map(|i| d.id(*i)).collect::<Vec<_>>(),
            ["T:System.ComponentModel.DataAnnotations.RequiredAttribute"]
        );
        assert_eq!(
            d.doc(req[0]).summary,
            "Specifies that a data field value is required."
        );
        assert!(
            !d.resolve(&s, "LogInformation", Receiver::Of("_logger"))
                .is_empty()
        );
        assert!(
            !d.resolve(&s, "ToListAsync", Receiver::Of("query"))
                .is_empty(),
            "EF Core package docs"
        );
        assert!(
            !d.resolve(&s, "WriteLine", Receiver::Of("Console"))
                .is_empty()
        );
    }
}
