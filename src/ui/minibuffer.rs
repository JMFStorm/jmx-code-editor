//! Single-line prompt at the bottom of the window (open, save as, find, go to line, confirms).

use std::fs;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};

/// What happens once a Save As prompt finishes successfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterSave {
    Nothing,
    CloseDoc,
    ContinueQuit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    Open,
    SaveAs(AfterSave),
    Find {
        /// Cursor position when the search started; incremental search runs from here.
        origin: usize,
    },
    GotoLine,
    /// Unsaved changes in tab `index` of view `pane`: (y)es save / (n)o discard / (c)ancel.
    ConfirmClose {
        pane: usize,
        index: usize,
    },
    /// Unsaved changes in some documents: (s)ave all / (d)iscard / (c)ancel.
    ConfirmQuit,
}

#[derive(Debug)]
pub struct Minibuffer {
    pub kind: PromptKind,
    pub input: String,
    /// Cursor as a char index into `input`.
    pub cursor: usize,
}

impl Minibuffer {
    pub fn new(kind: PromptKind, initial: String) -> Self {
        let cursor = initial.chars().count();
        Self {
            kind,
            input: initial,
            cursor,
        }
    }

    pub fn is_confirm(&self) -> bool {
        matches!(
            self.kind,
            PromptKind::ConfirmClose { .. } | PromptKind::ConfirmQuit
        )
    }

    pub fn label(&self, doc_title: &str) -> String {
        match &self.kind {
            PromptKind::Open => "Open: ".into(),
            PromptKind::SaveAs(_) => "Save as: ".into(),
            PromptKind::Find { .. } => "Find: ".into(),
            PromptKind::GotoLine => "Go to line[:col]: ".into(),
            PromptKind::ConfirmClose { .. } => {
                format!("Save changes to {doc_title}? (y)es / (n)o / (c)ancel ")
            }
            PromptKind::ConfirmQuit => {
                "Unsaved changes. (s)ave all / (d)iscard and quit / (c)ancel ".into()
            }
        }
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_idx)
            .map_or(self.input.len(), |(b, _)| b)
    }

    pub fn insert(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        let b = self.byte_at(self.cursor);
        self.input.insert_str(b, &text);
        self.cursor += text.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let b = self.byte_at(self.cursor - 1);
            self.input.remove(b);
            self.cursor -= 1;
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.input.chars().count() {
            let b = self.byte_at(self.cursor);
            self.input.remove(b);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.input.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.input.chars().count();
    }

    /// Delete the previous path component / word.
    pub fn delete_word(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut p = self.cursor;
        while p > 0 && is_sep(chars[p - 1]) {
            p -= 1;
        }
        while p > 0 && !is_sep(chars[p - 1]) && chars[p - 1] != ' ' {
            p -= 1;
        }
        let (a, b) = (self.byte_at(p), self.byte_at(self.cursor));
        self.input.replace_range(a..b, "");
        self.cursor = p;
    }

    /// Tab completion for path prompts. Returns candidate names when ambiguous.
    pub fn complete_path(&mut self) -> Vec<String> {
        let (dir_part, prefix) = match self.input.rfind(is_sep) {
            Some(i) => (&self.input[..=i], &self.input[i + 1..]),
            None => ("", self.input.as_str()),
        };
        let dir = if dir_part.is_empty() {
            PathBuf::from(".")
        } else {
            expand_tilde(dir_part)
        };
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut matches: Vec<(String, bool)> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let is_dir =
                    e.file_type().map(|t| t.is_dir()).unwrap_or(false) || e.path().is_dir();
                name.starts_with(prefix).then_some((name, is_dir))
            })
            .collect();
        if !prefix.starts_with('.') {
            matches.retain(|(n, _)| !n.starts_with('.'));
        }
        matches.sort();

        let completed = match matches.as_slice() {
            [] => return Vec::new(),
            [(name, is_dir)] => {
                let mut s = format!("{dir_part}{name}");
                if *is_dir {
                    s.push(MAIN_SEPARATOR);
                }
                s
            }
            many => format!(
                "{dir_part}{}",
                common_prefix(many.iter().map(|(n, _)| n.as_str()))
            ),
        };
        self.input = completed;
        self.end();
        if matches.len() > 1 {
            matches
                .into_iter()
                .map(|(n, d)| if d { format!("{n}{MAIN_SEPARATOR}") } else { n })
                .collect()
        } else {
            Vec::new()
        }
    }
}

fn is_sep(c: char) -> bool {
    c == '/' || c == MAIN_SEPARATOR
}

fn common_prefix<'a>(mut names: impl Iterator<Item = &'a str>) -> String {
    let Some(first) = names.next() else {
        return String::new();
    };
    let mut len = first.len();
    for n in names {
        len = first
            .char_indices()
            .zip(n.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((i, a), _)| i + a.len_utf8())
            .min(len);
    }
    first[..len].to_string()
}

/// `~/foo` → `$HOME/foo`.
pub fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix('~')
        && (rest.is_empty() || rest.starts_with(is_sep))
    {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        if let Some(home) = home {
            return Path::new(&home).join(rest.trim_start_matches(is_sep));
        }
    }
    PathBuf::from(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix() {
        assert_eq!(
            common_prefix(["main.rs", "mainloop.rs"].into_iter()),
            "main"
        );
        assert_eq!(common_prefix(["äb", "äc"].into_iter()), "ä");
        assert_eq!(common_prefix(["x", "y"].into_iter()), "");
    }

    #[test]
    fn editing() {
        let mut m = Minibuffer::new(PromptKind::Open, "src/ma".into());
        m.insert("in");
        assert_eq!(m.input, "src/main");
        m.delete_word();
        assert_eq!(m.input, "src/");
        m.backspace();
        m.home();
        m.delete();
        assert_eq!(m.input, "rc");
    }

    #[test]
    fn completion() {
        let dir = std::env::temp_dir().join(format!("jmx-mb-{}", std::process::id()));
        fs::create_dir_all(dir.join("subdir")).unwrap();
        fs::write(dir.join("alpha.txt"), "").unwrap();
        fs::write(dir.join("alpine.txt"), "").unwrap();
        let base = format!("{}{MAIN_SEPARATOR}", dir.display());
        let mut m = Minibuffer::new(PromptKind::Open, format!("{base}al"));
        let cands = m.complete_path();
        assert_eq!(m.input, format!("{base}alp"));
        assert_eq!(cands.len(), 2);
        let mut m = Minibuffer::new(PromptKind::Open, format!("{base}su"));
        m.complete_path();
        assert_eq!(m.input, format!("{base}subdir{MAIN_SEPARATOR}"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
