//! Code formatting: Prettier (the user's own installation, run as a child process) for the
//! file types it supports, and a basic whitespace cleanup for everything else.
//!
//! Prettier finds its configuration itself (`.prettierrc*`, `prettier.config.*`, the
//! `"prettier"` key of `package.json`, `.editorconfig`) by walking up from the file, and honors
//! `.prettierignore`, so a config file applies to its folder and everything below it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use crate::lint::Diagnostic;
use crate::process;

/// File types Prettier formats.
pub fn prettier_supports(path: &Path) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "js" | "mjs"
            | "cjs"
            | "jsx"
            | "ts"
            | "mts"
            | "cts"
            | "tsx"
            | "json"
            | "jsonc"
            | "json5"
            | "css"
            | "scss"
            | "less"
            | "html"
            | "htm"
            | "vue"
            | "md"
            | "markdown"
            | "yaml"
            | "yml"
            | "graphql"
            | "gql"
            | "handlebars"
            | "hbs"
    )
}

/// The Prettier executable to use for a file in `dir`: `$JDC_PRETTIER`, else the nearest
/// project-local `node_modules/.bin/prettier`, else one on `PATH`.
pub fn find_prettier(dir: &Path) -> Option<PathBuf> {
    process::find_node_tool("prettier", "JDC_PRETTIER", dir)
}

/// Trim trailing blanks on every line and end the text with exactly one newline (an empty
/// text stays empty).
pub fn basic_cleanup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split('\n') {
        out.push_str(line.trim_end_matches([' ', '\t']));
        out.push('\n');
    }
    let trimmed = out.trim_end_matches('\n');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

/// The tools a format run uses, in order.
#[derive(Debug, Clone, Default)]
pub struct Steps {
    pub prettier: Option<PathBuf>,
    /// ESLint executable and the config root to run it in.
    pub eslint: Option<(PathBuf, PathBuf)>,
}

/// What a format run produced.
#[derive(Debug)]
pub struct Outcome {
    /// The formatted text (unchanged input if every step failed).
    pub text: String,
    /// Diagnostics left after ESLint's fixes, if ESLint ran.
    pub diagnostics: Option<Vec<Diagnostic>>,
    /// Names of the tools that ran successfully.
    pub tools: Vec<&'static str>,
    /// The first error, if a tool failed.
    pub error: Option<String>,
}

/// A formatter running in the background.
pub struct FormatJob {
    rx: Receiver<Outcome>,
    /// The document being formatted.
    pub path: PathBuf,
    /// Its text revision when the job started; a different one means it was edited since.
    pub revision: u64,
    /// Save once the result is applied.
    pub then_save: bool,
}

impl FormatJob {
    /// Run Prettier (`--stdin-filepath`), then ESLint's fixes, on `text`.
    pub fn spawn(
        steps: Steps,
        path: PathBuf,
        text: String,
        revision: u64,
        then_save: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let file = path.clone();
        std::thread::Builder::new()
            .name("jdc-format".into())
            .spawn(move || {
                let started = Instant::now();
                let outcome = run_steps(&steps, &file, text);
                log::debug!(
                    "format {} in {:?}: {:?}, error {:?}",
                    file.display(),
                    started.elapsed(),
                    outcome.tools,
                    outcome.error
                );
                let _ = tx.send(outcome);
            })
            .expect("cannot start formatter thread");
        Self {
            rx,
            path,
            revision,
            then_save,
        }
    }

    /// The outcome, once the formatter has finished.
    pub fn try_result(&self) -> Option<Outcome> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Outcome {
                text: String::new(),
                diagnostics: None,
                tools: Vec::new(),
                error: Some("formatter crashed".into()),
            }),
        }
    }
}

fn run_steps(steps: &Steps, file: &Path, input: String) -> Outcome {
    let mut out = Outcome {
        text: input,
        diagnostics: None,
        tools: Vec::new(),
        error: None,
    };
    if let Some(prettier) = &steps.prettier {
        match run_prettier(prettier, file, &out.text) {
            Ok(text) => {
                out.text = text;
                out.tools.push("Prettier");
            }
            // Broken syntax: ESLint would only report the same parse error.
            Err(e) => {
                out.error = Some(e);
                return out;
            }
        }
    }
    if let Some((eslint, root)) = &steps.eslint {
        match crate::lint::eslint::fix_text(eslint, root, file, &out.text) {
            Ok((text, diags)) => {
                out.text = text;
                out.diagnostics = Some(diags);
                out.tools.push("ESLint");
            }
            Err(e) => out.error = Some(e),
        }
    }
    out
}

fn run_prettier(prettier: &Path, file: &Path, text: &str) -> Result<String, String> {
    let cwd = file.parent().unwrap_or(Path::new("."));
    let out = process::run(
        prettier,
        &["--stdin-filepath".as_ref(), file.as_os_str()],
        cwd,
        text,
    )?;
    if out.code == Some(0) {
        Ok(out.stdout.replace("\r\n", "\n"))
    } else {
        Err(process::first_error_line("Prettier", &out.stderr)
            .unwrap_or_else(|| format!("Prettier failed (exit {:?})", out.code)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn cleanup_trims_and_ends_with_one_newline() {
        assert_eq!(basic_cleanup("a  \n\tb\t\n\n\n"), "a\n\tb\n");
        assert_eq!(basic_cleanup("x"), "x\n");
        assert_eq!(basic_cleanup(""), "");
        assert_eq!(basic_cleanup("\n \n"), "");
        assert_eq!(basic_cleanup("a\n\n  b \n"), "a\n\n  b\n");
    }

    #[test]
    fn supported_types() {
        assert!(prettier_supports(Path::new("src/App.vue")));
        assert!(prettier_supports(Path::new("a.JSON")));
        assert!(!prettier_supports(Path::new("Program.cs")));
        assert!(!prettier_supports(Path::new("Makefile")));
    }

    #[cfg(unix)]
    fn fake(dir: &Path, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    fn wait(job: FormatJob) -> Outcome {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(r) = job.try_result() {
                return r;
            }
            assert!(std::time::Instant::now() < end, "formatter did not finish");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    #[test]
    fn prettier_then_eslint_fixes() {
        let dir = std::env::temp_dir().join(format!("jdc-fmt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.js");
        // Prettier stand-in: uppercases stdin and appends the --stdin-filepath argument.
        let prettier = fake(&dir, "prettier", r#"tr a-z A-Z; echo "$2""#);
        let steps = Steps {
            prettier: Some(prettier.clone()),
            eslint: None,
        };
        let o = wait(FormatJob::spawn(
            steps,
            file.clone(),
            "let x\n".into(),
            1,
            false,
        ));
        assert_eq!(o.text, format!("LET X\n{}\n", file.display()));
        assert_eq!(o.tools, ["Prettier"]);

        // ESLint stand-in for --fix-dry-run: reports fixed output and one remaining warning.
        let report = format!(
            r#"[{{"filePath":"{}","messages":[{{"ruleId":"no-console","severity":1,"message":"Unexpected console","line":1,"column":1}}],"output":"FIXED\n"}}]"#,
            file.display()
        );
        let eslint = fake(
            &dir,
            "eslint",
            &format!("cat > /dev/null; printf '%s\\n' '{report}'; exit 1"),
        );
        let steps = Steps {
            prettier: Some(prettier),
            eslint: Some((eslint, dir.clone())),
        };
        let o = wait(FormatJob::spawn(
            steps,
            file.clone(),
            "let x\n".into(),
            1,
            false,
        ));
        assert_eq!(o.text, "FIXED\n");
        assert_eq!(o.tools, ["Prettier", "ESLint"]);
        assert_eq!(
            o.diagnostics.unwrap()[0].code.as_deref(),
            Some("no-console")
        );

        // A Prettier error stops the chain and keeps the text.
        let bad = fake(
            &dir,
            "bad",
            "echo '[error] a.js: SyntaxError: Unexpected token (1:5)' >&2; exit 2",
        );
        let steps = Steps {
            prettier: Some(bad),
            eslint: None,
        };
        let o = wait(FormatJob::spawn(steps, file, "let\n".into(), 1, false));
        assert_eq!(o.text, "let\n");
        assert_eq!(
            o.error.unwrap(),
            "Prettier: a.js: SyntaxError: Unexpected token (1:5)"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
