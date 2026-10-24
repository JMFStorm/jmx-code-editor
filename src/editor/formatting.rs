//! Format the focused document: Prettier in the background for the file types it supports,
//! a basic whitespace cleanup otherwise. Ctrl+S, S formats and saves; Shift+Alt+F formats.

use std::path::Path;
use std::time::Duration;

use super::Editor;
use super::history::EditKind;
use super::selection::Selection;
use crate::format::{self, FormatJob, Outcome, Steps};
use crate::lint::eslint;
use crate::ui::minibuffer::AfterSave;

/// How often running formatter jobs are polled.
pub(super) const FORMAT_POLL: Duration = Duration::from_millis(16);

impl Editor {
    /// Format the focused document; with `then_save`, save it afterwards (also when the
    /// formatter fails: the user asked to save).
    pub(super) fn format_document(&mut self, then_save: bool) {
        if !self.has_doc() || self.doc().read_only {
            return;
        }
        let Some(path) = self.doc().path.clone() else {
            // Untitled: nothing to pick a formatter by; saving asks for a name.
            if then_save {
                self.save(AfterSave::Nothing);
            } else {
                self.cleanup_focused(None);
            }
            return;
        };
        let mut hint = None;
        let dir = path.parent().unwrap_or(Path::new("."));
        let prettier = if format::prettier_supports(&path) {
            let found = format::find_prettier(dir);
            if found.is_none() {
                hint = Some("Prettier not found (npm i -D prettier); did a basic cleanup");
            }
            found
        } else {
            None
        };
        // ESLint's automatic fixes run after Prettier in projects that use ESLint.
        let canonical = self.doc().canonical.clone().unwrap_or_else(|| path.clone());
        let eslint = eslint::lintable(&path)
            .then(|| eslint::config_root(&canonical))
            .flatten()
            .and_then(|root| eslint::find_eslint(&root).map(|bin| (bin, root)));
        if prettier.is_some() || eslint.is_some() {
            if self.format_jobs.iter().any(|j| j.path == path) {
                self.info("Already formatting");
                return;
            }
            log::info!(
                "formatting {} (prettier {:?}, eslint {:?})",
                path.display(),
                prettier,
                eslint.as_ref().map(|e| &e.0)
            );
            let doc = self.doc();
            let steps = Steps { prettier, eslint };
            let job = FormatJob::spawn(steps, path, doc.rope.to_string(), doc.revision, then_save);
            self.format_jobs.push(job);
            self.info("Formatting…");
            return;
        }
        self.cleanup_focused(hint);
        if then_save {
            self.save(AfterSave::Nothing);
            // Keep the "Prettier not found" hint visible over "Saved ...".
            let saved = self
                .message
                .as_ref()
                .is_some_and(|m| m.kind != super::MsgKind::Error);
            if let (Some(h), true) = (hint, saved) {
                self.info(format!("{h}; saved"));
            }
        }
    }

    /// Basic whitespace cleanup of the focused document.
    fn cleanup_focused(&mut self, hint: Option<&str>) {
        let (pane, index) = (self.focus, self.pane().active);
        let text = self.doc().rope.to_string();
        let changed = self.apply_formatted(pane, index, &format::basic_cleanup(&text));
        let msg = match (hint, changed) {
            (Some(h), _) => h.to_string(),
            (None, true) => "Cleaned up whitespace".to_string(),
            (None, false) => "Nothing to clean up".to_string(),
        };
        self.info(msg);
    }

    /// Collect finished formatter jobs. Returns true if anything changed.
    pub(super) fn poll_format(&mut self) -> bool {
        let mut changed = false;
        let mut i = 0;
        while i < self.format_jobs.len() {
            let Some(result) = self.format_jobs[i].try_result() else {
                i += 1;
                continue;
            };
            let job = self.format_jobs.remove(i);
            changed = true;
            self.finish_format(job, result);
        }
        changed
    }

    pub(super) fn formatting(&self) -> bool {
        !self.format_jobs.is_empty()
    }

    fn finish_format(&mut self, job: FormatJob, outcome: Outcome) {
        let Some((pane, index)) = self.find_doc(|d| d.path.as_deref() == Some(job.path.as_path()))
        else {
            return; // closed meanwhile
        };
        let title = self.panes[pane].docs[index].title();
        let mut have_diagnostics = false;
        if self.panes[pane].docs[index].revision != job.revision {
            self.warn(format!("{title} changed while formatting; not applied"));
        } else {
            let changed =
                !outcome.tools.is_empty() && self.apply_formatted(pane, index, &outcome.text);
            if let (Some(diags), Some(canonical)) = (
                outcome.diagnostics,
                self.panes[pane].docs[index].canonical.clone(),
            ) {
                self.set_doc_diagnostics(&canonical, diags);
                have_diagnostics = true;
            }
            match outcome.error {
                Some(e) => {
                    log::warn!("format {}: {e}", job.path.display());
                    self.error(e);
                }
                None if changed => {
                    self.info(format!("Formatted {title} ({})", outcome.tools.join(" + ")))
                }
                None => self.info(format!("{title} is already formatted")),
            }
        }
        if job.then_save {
            self.save_doc(pane, index);
            // The format run already linted the saved text when ESLint took part.
            if !have_diagnostics {
                self.lint_doc(pane, index);
            }
        }
    }

    /// Save a document that may not be the focused one, keeping the current status message
    /// unless saving fails.
    fn save_doc(&mut self, pane: usize, index: usize) {
        let doc = &mut self.panes[pane].docs[index];
        match doc.save() {
            Ok(()) => {
                log::info!("saved {} after formatting", doc.title());
                if let Some(path) = doc.path.clone() {
                    self.after_save_checks(&path);
                }
            }
            Err(e) => {
                log::error!("save failed: {e:#}");
                self.error(format!("{e:#}"));
            }
        }
    }

    /// Replace a document's text with `new` as one undo step, changing only the part that
    /// differs so the cursor and scroll stay put. Returns false if nothing changed.
    pub(super) fn apply_formatted(&mut self, pane: usize, index: usize, new: &str) -> bool {
        let doc = &mut self.panes[pane].docs[index];
        let old: Vec<char> = doc.rope.chars().collect();
        let new: Vec<char> = new.chars().collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let max_suffix = old.len().min(new.len()) - prefix;
        let suffix = old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .take(max_suffix)
            .take_while(|(a, b)| a == b)
            .count();
        let (old_end, new_end) = (old.len() - suffix, new.len() - suffix);
        if prefix == old_end && prefix == new_end {
            return false;
        }
        // Where the cursor goes: unchanged around the edit, same line and column inside it.
        let head = doc.selection.head;
        let line_col = (head > prefix && head < old_end).then(|| {
            let line = doc.rope.char_to_line(head);
            (line, head - doc.rope.line_to_char(line))
        });
        let shift = new_end as isize - old_end as isize;
        let after = if head <= prefix {
            head
        } else if head >= old_end {
            (head as isize + shift) as usize
        } else {
            prefix
        };
        let replacement: String = new[prefix..new_end].iter().collect();
        doc.collapse_extra();
        doc.transact(
            EditKind::Other,
            vec![(prefix, old_end, replacement)],
            Selection::point(after),
        );
        doc.history.break_coalescing();
        if let Some((line, col)) = line_col {
            let line = line.min(doc.rope.len_lines().saturating_sub(1));
            let start = doc.rope.line_to_char(line);
            let len = super::movement::line_content_end(&doc.rope, line) - start;
            doc.selection = Selection::point(start + col.min(len));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Command;
    use crate::config::Config;
    use ropey::Rope;

    fn editor(text: &str) -> Editor {
        let mut ed = Editor::new(Config::default(), &[]);
        ed.doc_mut().rope = Rope::from_str(text);
        ed
    }

    #[test]
    fn apply_changes_only_the_differing_part() {
        let mut ed = editor("keep\nmid  dle\nend\n");
        ed.doc_mut().selection = Selection::point(15); // in "end"
        assert!(ed.apply_formatted(0, 0, "keep\nmiddle\nend\n"));
        assert_eq!(ed.doc().rope.to_string(), "keep\nmiddle\nend\n");
        assert_eq!(ed.doc().selection.head, 13); // moved with the suffix
        assert!(!ed.apply_formatted(0, 0, "keep\nmiddle\nend\n"));
        ed.execute(Command::Undo);
        assert_eq!(ed.doc().rope.to_string(), "keep\nmid  dle\nend\n");
    }

    #[test]
    fn cleanup_of_untitled_and_unsupported_files() {
        let mut ed = editor("a  \nb\t\n\n\n");
        ed.execute(Command::FormatDocument);
        assert_eq!(ed.doc().rope.to_string(), "a\nb\n");
        assert!(ed.message.as_ref().unwrap().text.contains("Cleaned up"));

        let dir = std::env::temp_dir().join(format!("jmx-fmtcs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("P.cs");
        std::fs::write(&file, "class P { }   \n\n\n").unwrap();
        let mut ed = Editor::new(Config::default(), std::slice::from_ref(&file));
        ed.execute(Command::FormatAndSave);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "class P { }\n");
        assert!(!ed.doc().is_dirty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn prettier_results_apply_unless_the_file_changed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("jmx-fmtjs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules/.bin")).unwrap();
        // A project-local "prettier" that uppercases its input after a short delay.
        let fake = dir.join("node_modules/.bin/prettier");
        std::fs::write(&fake, "#!/bin/sh\nsleep 0.2\ntr a-z A-Z\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = dir.join("a.js");
        std::fs::write(&file, "let x\n").unwrap();
        if std::env::var_os("JMX_PRETTIER").is_some() {
            return;
        }

        let mut ed = Editor::new(Config::default(), std::slice::from_ref(&file));
        ed.execute(Command::FormatAndSave);
        assert!(ed.formatting());
        ed.wait_jobs(Duration::from_secs(10));
        assert_eq!(ed.doc().rope.to_string(), "LET X\n");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "LET X\n");

        // Typing while Prettier runs: the stale result is dropped.
        ed.execute(Command::FormatDocument);
        ed.insert_text("!");
        ed.wait_jobs(Duration::from_secs(10));
        assert!(ed.message.as_ref().unwrap().text.contains("not applied"));
        assert!(ed.doc().rope.to_string().contains('!'));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
