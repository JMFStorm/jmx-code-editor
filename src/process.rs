//! Running external tools (Prettier, ESLint, later compilers): finding them and running them
//! with piped stdin/stdout, a timeout and no console window on Windows.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A tool running longer than this is killed.
const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Output {
    pub stdout: String,
    pub stderr: String,
    /// Exit code; `None` if the process was killed by a signal.
    pub code: Option<i32>,
}

/// A Node tool for a file in `dir`: `$env_var` if set, else the nearest project-local
/// `node_modules/.bin/<name>` (walking up), else one on `PATH`.
pub fn find_node_tool(name: &str, env_var: &str, dir: &Path) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env_var).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let names: Vec<String> = if cfg!(windows) {
        vec![
            format!("{name}.cmd"),
            format!("{name}.exe"),
            name.to_string(),
        ]
    } else {
        vec![name.to_string()]
    };
    let local = dir.ancestors().find_map(|d| {
        names
            .iter()
            .map(|n| d.join("node_modules").join(".bin").join(n))
            .find(|p| p.is_file())
    });
    local.or_else(|| {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .find_map(|d| names.iter().map(|n| d.join(n)).find(|p| p.is_file()))
    })
}

/// Run `program args` in `cwd`, writing `stdin` to it, and collect its output.
pub fn run(
    program: &Path,
    args: &[&std::ffi::OsStr],
    cwd: &Path,
    stdin: &str,
) -> Result<Output, String> {
    run_with_timeout(program, args, cwd, stdin, TIMEOUT)
}

/// Like `run`, killing the tool after `timeout` (test runs and builds take longer).
pub fn run_with_timeout(
    program: &Path,
    args: &[&std::ffi::OsStr],
    cwd: &Path,
    stdin: &str,
    timeout: Duration,
) -> Result<Output, String> {
    run_cancellable(program, args, cwd, stdin, timeout, &AtomicBool::new(false))
}

/// Like `run_with_timeout`; setting `cancel` kills the tool.
pub fn run_cancellable(
    program: &Path,
    args: &[&std::ffi::OsStr],
    cwd: &Path,
    stdin: &str,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<Output, String> {
    let name = program.file_stem().map_or_else(
        || program.display().to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if cwd.is_dir() {
        cmd.current_dir(cwd);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    // Feed stdin and read the pipes on their own threads so large outputs cannot deadlock.
    let mut pipe_in = child.stdin.take().expect("piped stdin");
    let input = stdin.to_string();
    let writer = std::thread::spawn(move || {
        let _ = pipe_in.write_all(input.as_bytes());
    });
    let mut pipe_out = child.stdout.take().expect("piped stdout");
    let mut pipe_err = child.stderr.take().expect("piped stderr");
    let out_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = pipe_out.read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = pipe_err.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if cancel.load(Ordering::Relaxed) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{name} cancelled"));
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{name} took longer than {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("{name} failed: {e}")),
        }
    };
    let _ = writer.join();
    Ok(Output {
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
        code: status.code(),
    })
}

/// The useful first line of a tool's error output, prefixed with the tool name, e.g.
/// `Prettier: SyntaxError: Unexpected token (3:5)`.
pub fn first_error_line(tool: &str, stderr: &str) -> Option<String> {
    stderr
        .lines()
        .map(|l| l.trim().trim_start_matches("[error]").trim())
        // Skip ESLint's crash banner and version line.
        .find(|l| {
            !l.is_empty()
                && !l.starts_with("Oops!")
                && !l
                    .strip_prefix("ESLint: ")
                    .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
        })
        .map(|l| format!("{tool}: {l}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_project_local_tools_walking_up() {
        let dir = std::env::temp_dir().join(format!("jmx-findtool-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let bin = dir.join("node_modules/.bin");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(dir.join("src/deep")).unwrap();
        let name = if cfg!(windows) {
            "jmxtool.cmd"
        } else {
            "jmxtool"
        };
        fs::write(bin.join(name), "").unwrap();
        let found = find_node_tool("jmxtool", "JMX_TEST_NO_SUCH_VAR", &dir.join("src/deep"));
        assert_eq!(found, Some(bin.join(name)));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn error_lines() {
        let err = "\n[error] a.js: SyntaxError: Unexpected token (1:5)\n[error] > 1 | x\n";
        assert_eq!(
            first_error_line("Prettier", err).unwrap(),
            "Prettier: a.js: SyntaxError: Unexpected token (1:5)"
        );
        let eslint =
            "\nOops! Something went wrong! :(\n\nESLint: 9.17.0\n\nCould not find config\n";
        assert_eq!(
            first_error_line("ESLint", eslint).unwrap(),
            "ESLint: Could not find config"
        );
    }

    #[cfg(unix)]
    #[test]
    fn runs_with_stdin_and_exit_codes() {
        let out = run(
            Path::new("/bin/sh"),
            &["-c".as_ref(), "tr a-z A-Z; exit 3".as_ref()],
            Path::new("/"),
            "hi",
        )
        .unwrap();
        assert_eq!(out.stdout, "HI");
        assert_eq!(out.code, Some(3));
        assert!(run(Path::new("/no/such/tool"), &[], Path::new("/"), "").is_err());
    }
}
