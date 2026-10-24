//! Find All References (Shift+F12): every use of the name at the cursor across the
//! workspace, searched in the background and listed on the References page in the panel.
//! Up/Down preview a reference in the editor, Enter opens it, F4 steps through them.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use ropey::Rope;

use super::linting::Target;
use super::page::PageText;
use super::selection::Selection;
use super::{EDITOR, Editor};
use crate::symbols::refs::{RefQuery, Reference, find_in};
use crate::symbols::{self, Kind};
use crate::syntax::Lang;

pub const REFERENCES_TITLE: &str = "References";

/// References listed at most.
const MAX_REFS: usize = 5000;

type RefResults = Vec<(PathBuf, Vec<Reference>)>;

#[derive(Default)]
pub struct RefState {
    pub query: Option<RefQuery>,
    job: Option<Receiver<RefResults>>,
    pub results: RefResults,
    pub capped: bool,
    /// The place each page line opens.
    pub rows: Vec<Option<Target>>,
    /// F4 steps through references (until the Problems list is shown again).
    pub f4: bool,
}

impl RefState {
    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    pub fn count(&self) -> usize {
        self.results.iter().map(|(_, r)| r.len()).sum()
    }

    /// Every reference in list order.
    fn targets(&self) -> Vec<Target> {
        self.results
            .iter()
            .flat_map(|(p, refs)| {
                refs.iter().map(move |r| Target {
                    path: p.clone(),
                    line: r.line,
                    col: r.col,
                })
            })
            .collect()
    }
}

impl Editor {
    pub(super) fn references_page(&self) -> Option<(usize, usize)> {
        self.find_doc(|d| d.virtual_name.as_deref() == Some(REFERENCES_TITLE))
    }

    pub(super) fn on_references_page(&self) -> bool {
        self.has_doc() && self.doc().virtual_name.as_deref() == Some(REFERENCES_TITLE)
    }

    /// Shift+F12: search the workspace for uses of the name at the cursor.
    pub(super) fn find_all_references(&mut self) {
        if self.focus != EDITOR || !self.has_doc() {
            self.info("Find All References works in the editor view");
            return;
        }
        let head = self.doc().selection.head;
        let Some(path) = self.doc().canonical.clone() else {
            self.info("Save the file first");
            return;
        };
        let Some(family) = symbols::family(&path) else {
            self.info("Find All References works in JavaScript, TypeScript, Vue and C# files");
            return;
        };
        let Some((name, found)) = self.resolve_at(EDITOR, head) else {
            self.info("No name at the cursor");
            return;
        };
        // The declaration, when the workspace has one (on an import alias: the real name).
        let def = found
            .first()
            .filter(|f| f.ext.is_none() && f.sym.kind != Kind::Module);
        let name = def.map_or(name, |f| f.sym.name.clone());
        let query = RefQuery {
            family,
            name: name.clone(),
            member: def.is_some_and(|f| {
                f.sym.container.is_some()
                    && matches!(
                        f.sym.kind,
                        Kind::Method | Kind::Property | Kind::Constructor
                    )
            }),
            def_file: def.map(|f| f.path.clone()),
            def_line: def.map(|f| f.sym.line),
        };
        // Files of the language: the index's, and open documents with their unsaved text.
        let mut files: Vec<(PathBuf, Option<String>, Vec<String>)> = Vec::new();
        {
            let ix = self.symbols.index.borrow();
            for p in ix.paths() {
                if symbols::family(p) != Some(family) || super::symbols::in_node_modules(p) {
                    continue;
                }
                // Other local names for it: `import { name as alias } from '<its file>'`.
                let aliases = ix
                    .file(p)
                    .map(|f| {
                        f.imports
                            .iter()
                            .filter(|i| {
                                i.imported.as_deref() == Some(name.as_str())
                                    && i.local != name
                                    && query.def_file.as_ref().is_none_or(|d| *d == i.from)
                            })
                            .map(|i| i.local.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                files.push((p.clone(), None, aliases));
            }
        }
        for doc in self.panes.iter().flat_map(|p| p.docs.iter()) {
            let Some(p) = &doc.canonical else {
                continue;
            };
            if symbols::family(p) != Some(family) || super::symbols::in_node_modules(p) {
                continue;
            }
            match files.iter_mut().find(|(f, _, _)| f == p) {
                Some(entry) => entry.1 = Some(doc.rope.to_string()),
                None => files.push((p.clone(), Some(doc.rope.to_string()), Vec::new())),
            }
        }
        let (tx, rx) = mpsc::channel();
        let q = query.clone();
        std::thread::Builder::new()
            .name("jdc-references".into())
            .spawn(move || {
                let started = Instant::now();
                let mut out: RefResults = Vec::new();
                let mut total = 0;
                for (path, text, aliases) in files {
                    let Some(text) = text.or_else(|| std::fs::read_to_string(&path).ok()) else {
                        continue;
                    };
                    let refs = find_in(&path, &text, &q, &aliases);
                    if !refs.is_empty() {
                        total += refs.len();
                        out.push((path, refs));
                    }
                    if total >= MAX_REFS {
                        break;
                    }
                }
                // The declaration's file first, then by path.
                out.sort_by(|a, b| {
                    (Some(&a.0) != q.def_file.as_ref(), &a.0)
                        .cmp(&(Some(&b.0) != q.def_file.as_ref(), &b.0))
                });
                log::info!(
                    "references of {}: {total} in {:?}",
                    q.name,
                    started.elapsed()
                );
                let _ = tx.send(out);
            })
            .expect("cannot start the reference search");
        self.refs = RefState {
            query: Some(query),
            job: Some(rx),
            f4: true,
            ..Default::default()
        };
        self.open_virtual(REFERENCES_TITLE, "");
        self.refresh_references_page();
        // Back to the editor: the list fills in while you keep working.
        self.focus_view(EDITOR);
    }

    /// Collect the search result. Returns true when it arrived.
    pub(super) fn poll_references(&mut self) -> bool {
        let Some(rx) = &self.refs.job else {
            return false;
        };
        match rx.try_recv() {
            Ok(results) => {
                self.refs.job = None;
                self.refs.capped = results.iter().map(|(_, r)| r.len()).sum::<usize>() >= MAX_REFS;
                self.refs.results = results;
                self.refresh_references_page();
                let n = self.refs.count();
                let name = self
                    .refs
                    .query
                    .as_ref()
                    .map_or("", |q| q.name.as_str())
                    .to_string();
                self.info(format!(
                    "{n} reference{} to `{name}` · F4 next",
                    if n == 1 { "" } else { "s" }
                ));
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.refs.job = None;
                true
            }
        }
    }

    pub(super) fn refresh_references_page(&mut self) {
        let Some((p, i)) = self.references_page() else {
            return;
        };
        let doc = &self.panes[p].docs[i];
        let line = doc.rope.char_to_line(doc.selection.head);
        let keep = self.refs.rows.get(line).cloned().flatten();
        let page = self.references_text();
        let new_line = keep
            .and_then(|t| page.rows.iter().position(|r| r.as_ref() == Some(&t)))
            .unwrap_or(line);
        let text = page.text();
        let doc = &mut self.panes[p].docs[i];
        if doc.rope != text.as_str() {
            doc.rope = Rope::from_str(&text);
        }
        doc.syntax.set_lang(Lang::References);
        doc.boxes = page.boxes;
        doc.view.left_col = 0;
        let line = new_line.min(doc.rope.len_lines().saturating_sub(1));
        doc.selection = Selection::point(doc.rope.line_to_char(line));
        self.refs.rows = page.rows;
    }

    fn references_text(&self) -> PageText<Target> {
        let mut page: PageText<Target> = PageText::new(self.panel_width());
        let Some(q) = &self.refs.query else {
            page.line("Shift+F12 on a name lists its references.", None);
            return page;
        };
        let n = self.refs.count();
        let head = if self.refs.running() {
            format!("'{}' — searching {} …", q.name, super::linting::spinner())
        } else {
            format!(
                "'{}' — {n} reference{} in {} file{}{}",
                q.name,
                if n == 1 { "" } else { "s" },
                self.refs.results.len(),
                if self.refs.results.len() == 1 {
                    ""
                } else {
                    "s"
                },
                if self.refs.capped {
                    " (first ones shown)"
                } else {
                    ""
                }
            )
        };
        page.wrapped(&head, 2, None);
        page.hint("Enter open · F4 next · ◆ declaration");
        page.line("", None);
        if !self.refs.running() && n == 0 {
            page.line("No references found.", None);
        }
        for (path, refs) in &self.refs.results {
            page.wrapped(
                &format!("{}  ({})", self.display_path(path), refs.len()),
                0,
                None,
            );
            for r in refs {
                let mark = if r.definition { '◆' } else { '•' };
                let pos = format!("{}:{}", r.line + 1, r.col + 1);
                let target = Target {
                    path: path.clone(),
                    line: r.line,
                    col: r.col,
                };
                page.wrapped(
                    &format!("  {mark} {pos}  {}", r.text),
                    6 + pos.chars().count(),
                    Some(target),
                );
            }
        }
        page
    }

    fn references_cursor_target(&self) -> Option<Target> {
        let doc = self.doc();
        let line = doc.rope.char_to_line(doc.selection.head);
        self.refs.rows.get(line).cloned().flatten()
    }

    /// Up / Down on the References page: the previous / next reference, shown in the editor.
    pub(super) fn references_step(&mut self, delta: isize) {
        let line = self.doc().rope.char_to_line(self.doc().selection.head) as isize;
        let rows = &self.refs.rows;
        let here = rows.get(line as usize).cloned().flatten();
        let mut k = line;
        loop {
            k += delta;
            if k < 0 || k as usize >= rows.len() {
                return;
            }
            if rows[k as usize].is_some() && rows[k as usize] != here {
                break;
            }
        }
        let mut k = k as usize;
        while k > 0 && rows[k - 1] == rows[k] {
            k -= 1;
        }
        let doc = self.doc_mut();
        doc.selection = Selection::point(doc.rope.line_to_char(k));
        self.preview_reference();
    }

    pub(super) fn preview_reference(&mut self) {
        if let Some(t) = self.references_cursor_target() {
            self.show_location(&t, false);
        }
    }

    /// Enter / double-click on the References page.
    pub(super) fn open_reference(&mut self) {
        if let Some(t) = self.references_cursor_target() {
            self.preview_doc = None;
            self.show_location(&t, true);
        }
    }

    /// F4 / Shift+F4 while references are the latest list: the next / previous one.
    pub(super) fn step_reference(&mut self, forward: bool) {
        let all = self.refs.targets();
        if all.is_empty() {
            return;
        }
        let here = self.panes[EDITOR].doc().and_then(|d| {
            let path = d.canonical.clone()?;
            let line = d.rope.char_to_line(d.selection.head);
            Some(Target {
                path,
                line,
                col: d.selection.head - d.rope.line_to_char(line),
            })
        });
        let index = match here.and_then(|h| all.iter().position(|t| *t == h)) {
            Some(i) if forward => (i + 1) % all.len(),
            Some(i) => (i + all.len() - 1) % all.len(),
            None if forward => 0,
            None => all.len() - 1,
        };
        let t = all[index].clone();
        // Keep the page's cursor in step.
        if let Some((p, i)) = self.references_page()
            && let Some(row) = self.refs.rows.iter().position(|r| r.as_ref() == Some(&t))
        {
            let doc = &mut self.panes[p].docs[i];
            doc.selection = Selection::point(doc.rope.line_to_char(row));
        }
        let in_panel = self.on_references_page();
        self.show_location(&t, !in_panel);
        self.info(format!("Reference {}/{}", index + 1, all.len()));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, Motion};
    use crate::config::Config;
    use crate::editor::PANEL;

    fn wait(ed: &mut Editor) {
        let until = Instant::now() + std::time::Duration::from_secs(10);
        while ed.jobs_pending() && Instant::now() < until {
            ed.poll_jobs();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        ed.poll_jobs();
    }

    #[test]
    fn lists_uses_across_files_and_steps_through_them() {
        let dir = std::env::temp_dir().join(format!("jdc-refs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        std::fs::write(
            dir.join("src/format.js"),
            "export function formatCurrency(n) {}\n// formatCurrency is documented elsewhere\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/app.js"),
            "import { formatCurrency as money } from './format';\nconst a = money(1);\nconst s = 'formatCurrency';\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/Card.vue"),
            "<template><b>{{ formatCurrency(x) }}</b></template>\n<script setup>\nimport { formatCurrency } from './format';\n</script>\n",
        )
        .unwrap();
        let app = dir.join("src/app.js");
        let mut ed = Editor::new(Config::default(), std::slice::from_ref(&app));
        let mut ws = crate::workspace::Workspace::new("t");
        ws.folders = vec![dir.clone()];
        ed.workspace = Some(ws);
        ed.start_symbols();
        wait(&mut ed);
        // On the alias `money`: the references of `formatCurrency`.
        let at = ed.doc().rope.to_string().find("money(1)").unwrap();
        ed.doc_mut().selection = Selection::point(at + 1);
        ed.execute(Command::FindAllReferences);
        wait(&mut ed);
        assert!(!ed.refs.running());
        let q = ed.refs.query.as_ref().unwrap();
        assert_eq!(q.name, "formatCurrency");
        // format.js (the declaration, first), app.js (import + alias use), Card.vue (template
        // + import); the comment and the string don't count.
        let files: Vec<String> = ed
            .refs
            .results
            .iter()
            .map(|(p, r)| format!("{}:{}", p.file_name().unwrap().to_string_lossy(), r.len()))
            .collect();
        assert_eq!(files, ["format.js:1", "Card.vue:2", "app.js:3"]);
        assert!(ed.refs.results[0].1[0].definition);
        let page = ed.panes[PANEL].doc().unwrap().rope.to_string();
        assert!(
            page.starts_with("'formatCurrency' — 6 references in 3 files"),
            "{page}"
        );
        assert!(
            page.contains("  ◆ 1:17  export function formatCurrency(n) {}"),
            "{page}"
        );
        // The editor keeps the cursor; F4 walks the references.
        assert_eq!(ed.focus, EDITOR);
        ed.execute(Command::NextProblem);
        assert_eq!(ed.doc().canonical, Some(dir.join("src/format.js")));
        ed.execute(Command::NextProblem);
        assert_eq!(ed.doc().canonical, Some(dir.join("src/Card.vue")));
        // On the page: Down previews, Enter opens.
        ed.execute(Command::FocusRight);
        assert!(ed.on_references_page());
        ed.execute(Command::Move(Motion::DocStart, false));
        ed.execute(Command::Move(Motion::Down, false));
        assert_eq!(
            ed.panes[EDITOR].doc().unwrap().canonical,
            Some(dir.join("src/format.js"))
        );
        // The editor marks the reference: its line, and the name's columns.
        let shown = ed.panes[EDITOR].doc().unwrap();
        let loc = ed.located_in(shown).expect("highlighted");
        assert_eq!((loc.line, loc.start_col, loc.end_col), (0, 16, 30));
        ed.execute(Command::Move(Motion::Down, false));
        ed.execute(Command::Newline);
        assert_eq!(ed.focus, EDITOR);
        assert_eq!(ed.doc().canonical, Some(dir.join("src/Card.vue")));
        // Still marked in the editor; typing there (or leaving the line, or Esc) ends it.
        assert!(ed.located_in(ed.doc()).is_some());
        ed.insert_text("x");
        assert!(ed.located_in(ed.doc()).is_none());
        ed.execute(Command::FocusRight);
        ed.execute(Command::Move(Motion::Up, false));
        assert!(ed.located_in(ed.panes[EDITOR].doc().unwrap()).is_some());
        ed.execute(Command::FocusLeft);
        ed.execute(Command::Move(Motion::Down, false));
        assert!(
            ed.located_in(ed.doc()).is_none(),
            "the cursor left its line"
        );
        ed.execute(Command::Move(Motion::Up, false));
        ed.execute(Command::Cancel);
        assert!(ed.located.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
