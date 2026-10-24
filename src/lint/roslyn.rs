//! Roslyn (C# compiler) diagnostics: `dotnet build` in the background, its MSBuild output
//! parsed into errors and warnings per file.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use super::{Diagnostic, Severity, Source};
use crate::process;

/// Builds can take a while on a cold machine.
const BUILD_TIMEOUT: Duration = Duration::from_secs(600);

/// Parse MSBuild output lines such as
/// `/src/A.cs(12,5): warning CS0168: The variable 'x' is declared but never used [/src/A.csproj]`.
/// Paths are taken relative to `root` when not absolute. Repeated lines are reported once.
pub fn parse_build_output(output: &str, root: &Path) -> BTreeMap<PathBuf, Vec<Diagnostic>> {
    let mut files: BTreeMap<PathBuf, Vec<Diagnostic>> = BTreeMap::new();
    for line in output.lines() {
        let Some(d) = parse_line(line.trim(), root) else {
            continue;
        };
        let list = files.entry(d.0).or_default();
        if !list.contains(&d.1) {
            list.push(d.1);
        }
    }
    files
}

fn parse_line(line: &str, root: &Path) -> Option<(PathBuf, Diagnostic)> {
    // Split "location: severity CODE: message [project]".
    let (sev_at, severity) = [
        (": error ", Severity::Error),
        (": warning ", Severity::Warning),
    ]
    .into_iter()
    .filter_map(|(pat, sev)| line.find(pat).map(|i| (i, pat.len(), sev)))
    .min_by_key(|(i, _, _)| *i)
    .map(|(i, len, sev)| ((i, len), sev))?;
    let (loc, rest) = (&line[..sev_at.0], &line[sev_at.0 + sev_at.1..]);
    let (code, message) = rest.split_once(": ")?;
    if code.contains(' ') {
        return None;
    }
    let message = match message.rfind(" [") {
        Some(k) if message.ends_with(']') => &message[..k],
        _ => message,
    };
    // `path(line,col)` or `path(line,col,eline,ecol)`; project-level lines have no position.
    let (path, pos) = match loc.rfind('(') {
        Some(k) if loc.ends_with(')') => (&loc[..k], Some(&loc[k + 1..loc.len() - 1])),
        _ => (loc, None),
    };
    let nums: Vec<usize> = pos
        .map(|p| p.split(',').filter_map(|n| n.trim().parse().ok()).collect())
        .unwrap_or_default();
    let path = path.trim();
    if path.is_empty() || path.eq_ignore_ascii_case("CSC") || path.starts_with("MSBUILD") {
        return None;
    }
    let path = PathBuf::from(path.replace('\\', std::path::MAIN_SEPARATOR_STR));
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let line_no = nums.first().map_or(0, |n| n.saturating_sub(1));
    let col = nums.get(1).map_or(0, |n| n.saturating_sub(1));
    let end_line = nums.get(2).map_or(line_no, |n| n.saturating_sub(1));
    let end_col = nums.get(3).map_or(col, |n| n.saturating_sub(1));
    Some((
        path,
        Diagnostic {
            source: Source::Roslyn,
            severity,
            line: line_no,
            col,
            end_line,
            end_col,
            message: message.trim().to_string(),
            code: Some(code.to_string()),
        },
    ))
}

/// The C# project file for `path`: the nearest `.csproj` in its folder or above.
pub fn project_of(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    start.ancestors().find_map(|d| {
        std::fs::read_dir(d)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("csproj"))
            })
    })
}

/// Every `.csproj` under `folder` (bin/obj and ignored folders skipped).
pub fn projects_in(folder: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = crate::workspace::files::walker(folder, true)
        .max_depth(Some(6))
        .filter_entry(|e| {
            !matches!(
                e.file_name().to_str(),
                Some("bin" | "obj" | "node_modules" | ".git")
            )
        })
        .build()
        .flatten()
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("csproj"))
        })
        .collect();
    out.sort();
    out
}

/// A `dotnet build` in the background.
pub struct BuildJob {
    rx: Receiver<Result<BTreeMap<PathBuf, Vec<Diagnostic>>, String>>,
    pub csproj: PathBuf,
}

impl BuildJob {
    pub fn spawn(dotnet: PathBuf, csproj: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let project = csproj.clone();
        std::thread::Builder::new()
            .name("jmx-roslyn".into())
            .spawn(move || {
                let dir = project.parent().unwrap_or(Path::new(".")).to_path_buf();
                let args: [&OsStr; 6] = [
                    "build".as_ref(),
                    project.as_os_str(),
                    "-nologo".as_ref(),
                    "-v".as_ref(),
                    "q".as_ref(),
                    "-clp:NoSummary".as_ref(),
                ];
                let started = Instant::now();
                let result = process::run_with_timeout(&dotnet, &args, &dir, "", BUILD_TIMEOUT)
                    .map(|out| {
                        parse_build_output(&format!("{}\n{}", out.stdout, out.stderr), &dir)
                    });
                log::info!(
                    "dotnet build {} in {:?}: {}",
                    project.display(),
                    started.elapsed(),
                    result.as_ref().map_or_else(
                        |e| e.clone(),
                        |f| format!("{} files with problems", f.len())
                    )
                );
                let _ = tx.send(result);
            })
            .expect("cannot start build thread");
        Self { rx, csproj }
    }

    pub fn try_result(&self) -> Option<Result<BTreeMap<PathBuf, Vec<Diagnostic>>, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("build crashed".into())),
        }
    }

    /// The folder whose files this build reports on.
    pub fn root(&self) -> PathBuf {
        self.csproj.parent().unwrap_or(Path::new(".")).to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_msbuild_lines() {
        let out = "\
  Determining projects to restore...
/w/src/Api/Services/ProductService.cs(41,17): warning CS0168: The variable 'ex' is declared but never used [/w/src/Api/Api.csproj]
/w/src/Api/Services/ProductService.cs(41,17): warning CS0168: The variable 'ex' is declared but never used [/w/src/Api/Api.csproj]
Models/Product.cs(9,30,9,34): error CS1002: ; expected [/w/src/Api/Api.csproj]
/w/src/Api/Api.csproj : error NU1101: Unable to find package Foo. No packages exist with this id in source(s): nuget.org
CSC : error CS5001: Program does not contain a static 'Main' method suitable for an entry point
Build FAILED.
";
        let files = parse_build_output(out, Path::new("/w/src/Api"));
        let svc = &files[Path::new("/w/src/Api/Services/ProductService.cs")];
        assert_eq!(svc.len(), 1); // duplicate line reported once
        assert_eq!(
            (svc[0].line, svc[0].col, svc[0].severity),
            (40, 16, Severity::Warning)
        );
        assert_eq!(svc[0].code.as_deref(), Some("CS0168"));
        assert_eq!(
            svc[0].message,
            "The variable 'ex' is declared but never used"
        );
        let model = &files[Path::new("/w/src/Api/Models/Product.cs")];
        assert_eq!((model[0].line, model[0].col, model[0].end_col), (8, 29, 33));
        assert_eq!(model[0].severity, Severity::Error);
        let proj = &files[Path::new("/w/src/Api/Api.csproj")];
        assert_eq!(proj[0].code.as_deref(), Some("NU1101"));
        assert_eq!(files.len(), 3); // the CSC line has no file
    }
}
