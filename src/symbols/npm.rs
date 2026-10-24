//! npm packages' declarations, offline: a package's own type definitions in `node_modules`
//! (its `exports` / `types` entry, or `@types/<name>`), and the exported name followed
//! through `export *`, renamed re-exports and `typeof` aliases to where it is declared.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Symbol;
use super::dts::{self, DtsFile};

/// How deep re-export chains are followed.
const MAX_DEPTH: usize = 12;

/// `@vue/reactivity/sub` → (`@vue/reactivity`, `Some("sub")`).
fn split_spec(spec: &str) -> Option<(&str, Option<&str>)> {
    let mut parts = spec.splitn(if spec.starts_with('@') { 3 } else { 2 }, '/');
    let first = parts.next()?;
    let name_len = if spec.starts_with('@') {
        first.len() + 1 + parts.next()?.len()
    } else {
        first.len()
    };
    let sub = spec.get(name_len + 1..).filter(|s| !s.is_empty());
    Some((&spec[..name_len], sub))
}

/// A `types` path in an `exports` condition object (or string).
fn types_in(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => {
            (s.ends_with(".d.ts") || s.ends_with(".d.mts") || s.ends_with(".d.cts"))
                .then(|| s.clone())
        }
        serde_json::Value::Object(m) => ["types", "import", "require", "default", "node"]
            .iter()
            .filter_map(|k| m.get(*k))
            .find_map(types_in),
        _ => None,
    }
}

/// The declaration file of a package folder (`sub`: a subpath export).
fn package_entry(dir: &Path, sub: Option<&str>) -> Option<PathBuf> {
    let pkg: serde_json::Value = std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let key = sub.map_or(".".to_string(), |s| format!("./{s}"));
    let from_exports = pkg.get("exports").and_then(|e| match e.get(&key) {
        Some(v) => types_in(v),
        // `"exports": { "types": …, "import": … }` is the `.` entry.
        None if sub.is_none() && e.get(".").is_none() => types_in(e),
        None => None,
    });
    let declared = if sub.is_none() {
        ["types", "typings"]
            .iter()
            .find_map(|k| pkg.get(*k).and_then(|v| v.as_str()).map(String::from))
    } else {
        None
    };
    let mut candidates: Vec<PathBuf> = from_exports
        .into_iter()
        .chain(declared)
        .map(|rel| dir.join(rel.trim_start_matches("./")))
        .collect();
    match sub {
        None => candidates.push(dir.join("index.d.ts")),
        Some(s) => {
            candidates.push(dir.join(format!("{s}.d.ts")));
            candidates.push(dir.join(s).join("index.d.ts"));
        }
    }
    let found = candidates.into_iter().find(|p| p.is_file())?;
    Some(std::fs::canonicalize(&found).unwrap_or(found))
}

/// The declaration file for `import … from '<spec>'` in `from`: the package in the nearest
/// `node_modules`, else its `@types` package.
pub fn package_types(from: &Path, spec: &str) -> Option<PathBuf> {
    let (name, sub) = split_spec(spec)?;
    let types_name = format!("@types/{}", name.trim_start_matches('@').replace('/', "__"));
    for dir in from.ancestors().skip(1) {
        let modules = dir.join("node_modules");
        if !modules.is_dir() {
            continue;
        }
        if let Some(p) = package_entry(&modules.join(name), sub) {
            return Some(p);
        }
        if let Some(p) = package_entry(&modules.join(&types_name), sub) {
            return Some(p);
        }
    }
    None
}

/// A module specifier inside a declaration file: relative ones next to it, bare ones as
/// packages.
fn resolve(from: &Path, spec: &str) -> Option<PathBuf> {
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return package_types(from, spec);
    }
    let base = from.parent()?.join(spec);
    let s = base.to_string_lossy().to_string();
    // `./x.js` in types means `./x.d.ts`.
    let stem = s
        .strip_suffix(".js")
        .or_else(|| s.strip_suffix(".mjs"))
        .unwrap_or(&s)
        .to_string();
    let candidates = [
        PathBuf::from(format!("{stem}.d.ts")),
        PathBuf::from(format!("{stem}.d.mts")),
        PathBuf::from(format!("{stem}.d.cts")),
        base.join("index.d.ts"),
        base.clone(),
    ];
    let found = candidates.into_iter().find(|p| p.is_file())?;
    Some(std::fs::canonicalize(&found).unwrap_or(found))
}

/// Parsed declaration files, by path (shared: lookups walk several of them).
pub type DtsCache = HashMap<PathBuf, Arc<DtsFile>>;

fn load(cache: &mut DtsCache, path: &Path) -> Option<Arc<DtsFile>> {
    if !cache.contains_key(path) {
        let text = std::fs::read_to_string(path).ok()?;
        cache.insert(path.to_path_buf(), Arc::new(dts::scan(&text)));
    }
    cache.get(path).cloned()
}

/// Where `name`, exported by the declaration file `entry`, is declared: every declaration of
/// it (overloads), documented ones first.
pub fn find_export(cache: &mut DtsCache, entry: &Path, name: &str) -> Vec<(PathBuf, Symbol)> {
    let mut seen = HashSet::new();
    let mut out = find(cache, entry, name, 0, &mut seen);
    out.sort_by_key(|(_, s)| s.doc.is_empty());
    out
}

fn find(
    cache: &mut DtsCache,
    file: &Path,
    name: &str,
    depth: usize,
    seen: &mut HashSet<(PathBuf, String)>,
) -> Vec<(PathBuf, Symbol)> {
    if depth > MAX_DEPTH || !seen.insert((file.to_path_buf(), name.to_string())) {
        return Vec::new();
    }
    let Some(f) = load(cache, file) else {
        return Vec::new();
    };
    // `declare const computed: typeof computed$1` with `import { computed as computed$1 }`.
    if let Some((_, local)) = f.typeof_alias.iter().find(|(n, _)| n == name)
        && let Some((_, imported, spec)) = f.imports.iter().find(|(l, _, _)| l == local)
        && let Some(target) = resolve(file, spec)
    {
        let found = find(cache, &target, imported, depth + 1, seen);
        if !found.is_empty() {
            return found;
        }
    }
    let direct: Vec<(PathBuf, Symbol)> = f
        .symbols
        .iter()
        .filter(|s| s.name == name)
        .map(|s| (file.to_path_buf(), s.clone()))
        .collect();
    if !direct.is_empty() {
        return direct;
    }
    for (exported, original, spec) in &f.reexports {
        if exported != name {
            continue;
        }
        let found = match spec {
            Some(spec) => match resolve(file, spec) {
                Some(target) => find(cache, &target, original, depth + 1, seen),
                None => Vec::new(),
            },
            // `export { a as b }` of an import or a local.
            None => match f.imports.iter().find(|(l, _, _)| l == original) {
                Some((_, imported, spec)) => match resolve(file, spec) {
                    Some(target) => find(cache, &target, imported, depth + 1, seen),
                    None => Vec::new(),
                },
                None => find(cache, file, original, depth + 1, seen),
            },
        };
        if !found.is_empty() {
            return found;
        }
    }
    for spec in &f.star {
        if let Some(target) = resolve(file, spec) {
            let found = find(cache, &target, name, depth + 1, seen);
            if !found.is_empty() {
                return found;
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn specs_split() {
        assert_eq!(split_spec("vue"), Some(("vue", None)));
        assert_eq!(
            split_spec("@vue/reactivity"),
            Some(("@vue/reactivity", None))
        );
        assert_eq!(split_spec("@a/b/c/d"), Some(("@a/b", Some("c/d"))));
        assert_eq!(split_spec("lodash/fp"), Some(("lodash", Some("fp"))));
    }

    #[test]
    fn follows_packages_and_re_exports() {
        let dir = std::env::temp_dir().join(format!("jdc-npm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let nm = "app/node_modules";
        write(&dir, "app/src/App.vue", "");
        write(
            &dir,
            &format!("{nm}/vue/package.json"),
            r#"{"exports": {".": {"import": {"types": "./dist/vue.d.mts"}, "require": {"types": "./dist/vue.d.ts"}}}}"#,
        );
        write(
            &dir,
            &format!("{nm}/vue/dist/vue.d.mts"),
            "export * from '@vue/runtime-dom';\n",
        );
        write(
            &dir,
            &format!("{nm}/@vue/runtime-dom/package.json"),
            r#"{"types": "dist/runtime-dom.d.ts"}"#,
        );
        write(
            &dir,
            &format!("{nm}/@vue/runtime-dom/dist/runtime-dom.d.ts"),
            "export * from '@vue/runtime-core';\n",
        );
        write(
            &dir,
            &format!("{nm}/@vue/runtime-core/package.json"),
            r#"{"types": "dist/runtime-core.d.ts"}"#,
        );
        write(
            &dir,
            &format!("{nm}/@vue/runtime-core/dist/runtime-core.d.ts"),
            "import { computed as computed$1 } from '@vue/reactivity';\nexport { ref } from '@vue/reactivity';\nexport declare const computed: typeof computed$1;\nexport * from '@vue/runtime-dom';\n",
        );
        write(
            &dir,
            &format!("{nm}/@vue/reactivity/package.json"),
            r#"{"types": "./dist/reactivity.d.ts"}"#,
        );
        write(
            &dir,
            &format!("{nm}/@vue/reactivity/dist/reactivity.d.ts"),
            "/** Makes a ref. */\nexport declare function ref<T>(value: T): Ref<T>;\nexport declare function ref<T = any>(): Ref<T | undefined>;\n/** A computed ref. */\nexport declare function computed<T>(getter: () => T): ComputedRef<T>;\n",
        );
        // A package without types of its own, typed by @types.
        write(
            &dir,
            &format!("{nm}/lodash/package.json"),
            r#"{"main": "lodash.js"}"#,
        );
        write(
            &dir,
            &format!("{nm}/@types/lodash/index.d.ts"),
            "/** Chunks. */\nexport declare function chunk(a: any[]): any[][];\n",
        );
        let dir = std::fs::canonicalize(&dir).unwrap();
        let app = dir.join("app/src/App.vue");
        let entry = package_types(&app, "vue").unwrap();
        assert!(entry.ends_with("vue/dist/vue.d.mts"));
        let mut cache = DtsCache::new();
        // `ref`: a named re-export two `export *` hops away; documented overload first.
        let r = find_export(&mut cache, &entry, "ref");
        assert_eq!(r.len(), 2);
        assert!(r[0].0.ends_with("reactivity.d.ts"));
        assert_eq!(r[0].1.doc.summary, "Makes a ref.");
        // `computed`: through the `typeof` alias.
        let c = find_export(&mut cache, &entry, "computed");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].1.doc.summary, "A computed ref.");
        // The cycle (runtime-core ⇄ runtime-dom) stops.
        assert!(find_export(&mut cache, &entry, "nope").is_empty());
        let l = package_types(&app, "lodash").unwrap();
        assert!(l.ends_with("@types/lodash/index.d.ts"));
        assert_eq!(
            find_export(&mut cache, &l, "chunk")[0].1.doc.summary,
            "Chunks."
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The real Vue packages of the sandbox, when installed.
    #[test]
    fn sandbox_vue() {
        let app = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("sandbox/vue-dashboard/src/components/StatCard.vue");
        let Some(entry) = package_types(&app, "vue") else {
            return;
        };
        let mut cache = DtsCache::new();
        for (name, summary) in [
            ("ref", "Takes an inner value"),
            ("computed", "Takes a getter function"),
        ] {
            let found = find_export(&mut cache, &entry, name);
            assert!(!found.is_empty(), "{name}");
            assert!(
                found[0].1.doc.summary.starts_with(summary),
                "{name}: {:?}",
                found[0].1.doc
            );
        }
        assert!(!find_export(&mut cache, &entry, "defineProps").is_empty());
    }
}
