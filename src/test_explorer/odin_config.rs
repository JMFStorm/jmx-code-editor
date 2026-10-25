//! A project's `jdc.json`: extra `odin test` flags, e.g. the folder of a library the tests
//! link (`-extra-linker-flags:-Llibs/linux`). Found by walking up from the test package;
//! tests then run in its folder (the project root, where tests' relative paths start).
//!
//! ```json
//! { "odin": { "test_args": ["-extra-linker-flags:-Llibs/linux", "-debug"] } }
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const FILE_NAME: &str = "jdc.json";

#[derive(Deserialize, Default)]
struct File {
    #[serde(default)]
    odin: Odin,
}

#[derive(Deserialize, Default)]
struct Odin {
    #[serde(default)]
    test_args: Vec<String>,
}

#[derive(Debug, Default, PartialEq)]
pub struct OdinTestConfig {
    /// Extra `odin test` arguments, with `-L` folders made absolute.
    pub args: Vec<String>,
    /// The `-L` folders: the test program loads its shared libraries from them.
    pub lib_dirs: Vec<PathBuf>,
    /// Where the tests run: the folder of `jdc.json` (`None`: the package folder).
    pub cwd: Option<PathBuf>,
}

/// The settings for the tests of package folder `pkg` (defaults without a `jdc.json`).
pub fn load(pkg: &Path) -> OdinTestConfig {
    let Some(path) = pkg
        .ancestors()
        .map(|d| d.join(FILE_NAME))
        .find(|p| p.is_file())
    else {
        return OdinTestConfig::default();
    };
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let base = path.parent().unwrap_or(Path::new("."));
    match serde_json::from_str::<File>(&text) {
        Ok(f) => OdinTestConfig {
            cwd: Some(base.to_path_buf()),
            ..resolve(&f.odin.test_args, base)
        },
        Err(e) => {
            log::warn!("{}: {e}", path.display());
            OdinTestConfig::default()
        }
    }
}

/// Make the `-L` folders of `-extra-linker-flags:` absolute against `base`.
fn resolve(args: &[String], base: &Path) -> OdinTestConfig {
    let mut lib_dirs = Vec::new();
    let args = args
        .iter()
        .map(|a| {
            let Some(flags) = a.strip_prefix("-extra-linker-flags:") else {
                return a.clone();
            };
            let flags: Vec<String> = flags
                .split_whitespace()
                .map(|f| match f.strip_prefix("-L") {
                    Some(dir) if !dir.is_empty() => {
                        let dir = base.join(dir);
                        let s = format!("-L{}", dir.display());
                        lib_dirs.push(dir);
                        s
                    }
                    _ => f.to_string(),
                })
                .collect();
            format!("-extra-linker-flags:{}", flags.join(" "))
        })
        .collect();
    OdinTestConfig {
        args,
        lib_dirs,
        cwd: None,
    }
}

/// The environment that lets the test program find libraries in `dirs`.
pub fn library_env(dirs: &[PathBuf]) -> Vec<(String, std::ffi::OsString)> {
    if dirs.is_empty() {
        return Vec::new();
    }
    let var = if cfg!(windows) {
        "PATH"
    } else if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let mut all: Vec<PathBuf> = dirs.to_vec();
    if let Some(old) = std::env::var_os(var) {
        all.extend(std::env::split_paths(&old));
    }
    match std::env::join_paths(all) {
        Ok(v) => vec![(var.to_string(), v)],
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_file_and_makes_library_folders_absolute() {
        let dir = std::env::temp_dir().join(format!("jdc-odin-config-{}", std::process::id()));
        let pkg = dir.join("src/pak");
        std::fs::create_dir_all(&pkg).unwrap();
        assert_eq!(load(&pkg), OdinTestConfig::default());
        std::fs::write(
            dir.join(FILE_NAME),
            r#"{ "odin": { "test_args": ["-extra-linker-flags:-Llibs/linux -lm", "-debug"] } }"#,
        )
        .unwrap();
        let c = load(&pkg);
        let libs = dir.join("libs/linux");
        assert_eq!(
            c.args,
            vec![
                format!("-extra-linker-flags:-L{} -lm", libs.display()),
                "-debug".to_string()
            ]
        );
        assert_eq!(c.lib_dirs, vec![libs.clone()]);
        assert_eq!(c.cwd.as_deref(), Some(dir.as_path()));
        // Absolute folders stay as they are.
        let abs = resolve(&["-extra-linker-flags:-L/opt/x".into()], &dir);
        assert_eq!(abs.lib_dirs, vec![PathBuf::from("/opt/x")]);
        // A broken file: defaults.
        std::fs::write(dir.join(FILE_NAME), "{ nope").unwrap();
        assert_eq!(load(&pkg), OdinTestConfig::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
