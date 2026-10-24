//! Diagnostics (errors and warnings) from linters, kept per file for the Problems page and the
//! editor markers. ESLint is the first source; compilers (tsc, Roslyn) can be added later as
//! further `Source`s without changing the rest.

pub mod eslint;
pub mod roslyn;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    /// Hints and notes (compilers report these; ESLint does not).
    #[allow(dead_code)]
    Info,
}

/// Which tool reported a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Eslint,
    /// The C# compiler, via `dotnet build`.
    Roslyn,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Eslint => "ESLint",
            Source::Roslyn => "Roslyn",
        }
    }
}

/// One problem. Positions are 0-based lines and chars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub source: Source,
    pub severity: Severity,
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub message: String,
    /// Rule or error code, e.g. `no-unused-vars`.
    pub code: Option<String>,
}

/// All known diagnostics, by canonical file path.
#[derive(Debug, Default)]
pub struct Problems {
    files: BTreeMap<PathBuf, Vec<Diagnostic>>,
}

impl Problems {
    /// Replace `source`'s diagnostics for one file.
    pub fn set_file(&mut self, source: Source, path: &Path, diags: Vec<Diagnostic>) {
        let list = self.files.entry(path.to_path_buf()).or_default();
        list.retain(|d| d.source != source);
        list.extend(diags);
        list.sort_by_key(|d| (d.line, d.col, d.severity));
        if list.is_empty() {
            self.files.remove(path);
        }
    }

    /// Replace all of `source`'s diagnostics under `root` (a workspace lint) with `files`.
    pub fn set_under(
        &mut self,
        source: Source,
        root: &Path,
        files: BTreeMap<PathBuf, Vec<Diagnostic>>,
    ) {
        let stale: Vec<PathBuf> = self
            .files
            .keys()
            .filter(|p| p.starts_with(root))
            .cloned()
            .collect();
        for p in stale {
            self.set_file(source, &p, Vec::new());
        }
        for (p, diags) in files {
            self.set_file(source, &p, diags);
        }
    }

    pub fn for_file(&self, path: &Path) -> &[Diagnostic] {
        self.files.get(path).map_or(&[], Vec::as_slice)
    }

    /// (errors, warnings) over all files.
    pub fn counts(&self) -> (usize, usize) {
        let all = self.files.values().flatten();
        all.fold((0, 0), |(e, w), d| match d.severity {
            Severity::Error => (e + 1, w),
            Severity::Warning => (e, w + 1),
            Severity::Info => (e, w),
        })
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Files with their diagnostics, sorted by path (and each list by position).
    pub fn files(&self) -> impl Iterator<Item = (&PathBuf, &Vec<Diagnostic>)> {
        self.files.iter()
    }

    /// Every diagnostic in display order, with its file.
    pub fn all(&self) -> impl Iterator<Item = (&PathBuf, &Diagnostic)> {
        self.files
            .iter()
            .flat_map(|(p, list)| list.iter().map(move |d| (p, d)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(line: usize, severity: Severity) -> Diagnostic {
        Diagnostic {
            source: Source::Eslint,
            severity,
            line,
            col: 0,
            end_line: line,
            end_col: 1,
            message: "m".into(),
            code: None,
        }
    }

    #[test]
    fn per_file_and_per_root_replacement() {
        let mut p = Problems::default();
        let a = Path::new("/w/src/a.js");
        let b = Path::new("/w/src/b.js");
        let other = Path::new("/x/c.js");
        p.set_file(
            Source::Eslint,
            b,
            vec![diag(5, Severity::Warning), diag(1, Severity::Error)],
        );
        p.set_file(Source::Eslint, a, vec![diag(2, Severity::Warning)]);
        p.set_file(Source::Eslint, other, vec![diag(0, Severity::Error)]);
        assert_eq!(p.counts(), (2, 2));
        assert_eq!(p.for_file(b)[0].line, 1); // sorted by position
        let order: Vec<_> = p.all().map(|(f, d)| (f.as_path(), d.line)).collect();
        assert_eq!(order, [(a, 2), (b, 1), (b, 5), (other, 0)]);

        // A workspace lint of /w replaces everything under it, and nothing else.
        let mut fresh = BTreeMap::new();
        fresh.insert(a.to_path_buf(), vec![diag(9, Severity::Error)]);
        p.set_under(Source::Eslint, Path::new("/w"), fresh);
        assert_eq!(p.for_file(b), &[]);
        assert_eq!(p.for_file(a)[0].line, 9);
        assert_eq!(p.file_count(), 2);
        p.set_file(Source::Eslint, a, Vec::new());
        assert_eq!(p.file_count(), 1);
    }
}
