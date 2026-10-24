//! Listing workspace folders (with optional .gitignore support) and fuzzy file search.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

/// One entry of a folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// A file found by `index`, with the path shown in Quick Open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    pub abs: PathBuf,
    pub rel: String,
}

/// Walker over `dir` honoring (or not) .gitignore, .ignore and global git excludes.
/// Dotfiles are shown; the `.git` folder never is.
pub(crate) fn walker(dir: &Path, respect_gitignore: bool) -> WalkBuilder {
    let mut w = WalkBuilder::new(dir);
    w.hidden(false)
        .parents(respect_gitignore)
        .ignore(respect_gitignore)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|e| e.file_name() != ".git");
    w
}

/// Entries directly inside `dir`: folders first, then files, case-insensitively sorted.
pub fn list_dir(dir: &Path, respect_gitignore: bool) -> Vec<DirEntry> {
    let mut out: Vec<DirEntry> = walker(dir, respect_gitignore)
        .max_depth(Some(1))
        .build()
        .flatten()
        .filter(|e| e.depth() == 1)
        .map(|e| DirEntry {
            name: e.file_name().to_string_lossy().into_owned(),
            is_dir: e.file_type().is_some_and(|t| t.is_dir()),
            path: e.into_path(),
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// All files under the workspace folders, up to `limit`. Returns (files, hit_limit).
/// With several folders, paths are prefixed by the folder name.
pub fn index(
    folders: &[PathBuf],
    respect_gitignore: bool,
    limit: usize,
) -> (Vec<IndexedFile>, bool) {
    let mut out = Vec::new();
    let prefix = folders.len() > 1;
    for root in folders {
        for entry in walker(root, respect_gitignore).build().flatten() {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if out.len() >= limit {
                return (out, true);
            }
            let abs = entry.into_path();
            let rel = rel_path(root, &abs, prefix);
            out.push(IndexedFile { abs, rel });
        }
    }
    (out, false)
}

/// Path of `abs` shown to the user: relative to its workspace folder, with `/` separators and,
/// with several folders (`prefix`), led by the folder name.
pub fn rel_path(root: &Path, abs: &Path, prefix: bool) -> String {
    let rel = abs
        .strip_prefix(root)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/");
    if prefix {
        let root_name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        format!("{root_name}/{rel}")
    } else {
        rel
    }
}

/// Case-insensitive subsequence match. Higher is better; `None` if `query` does not match.
/// Rewards consecutive characters, matches at the start of the file name or after a
/// separator, and shorter candidates.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let cand: Vec<char> = candidate.chars().collect();
    let name_start = candidate
        .rfind('/')
        .map_or(0, |i| candidate[..=i].chars().count());
    // Greedy matching can lock onto an early letter in a folder name; also try matching
    // inside the file name only and keep the better score.
    let from_start = score_from(query, &cand, name_start, 0);
    let from_name = score_from(query, &cand, name_start, name_start);
    from_start.max(from_name)
}

fn score_from(query: &str, cand: &[char], name_start: usize, start: usize) -> Option<i32> {
    let lower: Vec<char> = cand.iter().map(|c| c.to_ascii_lowercase()).collect();
    let mut score = 0;
    let mut pos = start;
    let mut prev: Option<usize> = None;
    for q in query.chars().map(|c| c.to_ascii_lowercase()) {
        if q == ' ' {
            continue;
        }
        let found = (pos..lower.len()).find(|&i| lower[i] == q)?;
        score += 1;
        if prev == Some(found.wrapping_sub(1)) {
            score += 8;
        }
        if found == name_start {
            score += 12;
        } else if found == 0 || matches!(cand[found - 1], '/' | '_' | '-' | '.' | ' ') {
            score += 6;
        }
        if found >= name_start {
            score += 2;
        }
        prev = Some(found);
        pos = found + 1;
    }
    Some(score * 100 - cand.len() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jmx-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for d in ["src", "node_modules/pkg", ".git", "dist"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        for f in [
            "src/App.vue",
            "src/main.js",
            "node_modules/pkg/index.js",
            ".git/HEAD",
            "dist/out.js",
            "README.md",
            ".env",
        ] {
            fs::write(dir.join(f), "x").unwrap();
        }
        fs::write(dir.join(".gitignore"), "node_modules/\ndist/\n").unwrap();
        dir
    }

    fn names(entries: &[DirEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn listing_respects_gitignore_toggle() {
        let dir = fixture("list");
        assert_eq!(
            names(&list_dir(&dir, true)),
            ["src", ".env", ".gitignore", "README.md"]
        );
        assert_eq!(
            names(&list_dir(&dir, false)),
            [
                "dist",
                "node_modules",
                "src",
                ".env",
                ".gitignore",
                "README.md"
            ]
        );
        assert_eq!(
            names(&list_dir(&dir.join("src"), true)),
            ["App.vue", "main.js"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn index_and_limit() {
        let dir = fixture("index");
        let (files, full) = index(std::slice::from_ref(&dir), true, 100);
        let mut rels: Vec<_> = files.iter().map(|f| f.rel.as_str()).collect();
        rels.sort();
        assert_eq!(
            rels,
            [
                ".env",
                ".gitignore",
                "README.md",
                "src/App.vue",
                "src/main.js"
            ]
        );
        assert!(!full);
        let (all, _) = index(std::slice::from_ref(&dir), false, 100);
        assert!(all.iter().any(|f| f.rel == "node_modules/pkg/index.js"));
        assert!(!all.iter().any(|f| f.rel.starts_with(".git/")));
        let (few, full) = index(std::slice::from_ref(&dir), false, 2);
        assert_eq!(few.len(), 2);
        assert!(full);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fuzzy_ranking() {
        let cands = [
            "src/App.vue",
            "src/components/AppHeader.vue",
            "tests/app_view.spec.js",
            "README.md",
        ];
        let mut scored: Vec<_> = cands
            .iter()
            .filter_map(|c| fuzzy_score("appvue", c).map(|s| (s, *c)))
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        assert_eq!(scored[0].1, "src/App.vue");
        assert!(fuzzy_score("xyz", "src/App.vue").is_none());
        assert!(
            fuzzy_score("csv", "src/utils/csv.js").unwrap()
                > fuzzy_score("csv", "docs/c_s_v.md").unwrap()
        );
    }
}
