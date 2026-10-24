//! Hover popups: the failure of a test (gutter mark), the problems at a spot (underline,
//! gutter mark) or the doc of the name there, shown when the mouse rests there or with
//! Ctrl+I at the cursor.

use super::Editor;
use crate::lint::Severity;
use crate::test_explorer::Status;

/// How a popup line is colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverKind {
    Error,
    Warning,
    Pass,
    Text,
    Dim,
    /// A declaration's signature.
    Code,
    /// The active parameter of parameter hints.
    Param,
}

/// Where a popup is anchored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HoverAt {
    /// Below this pixel position (the mouse).
    Point(f32, f32),
    /// Below this text position of a view (the cursor).
    Cell {
        pane: usize,
        line: usize,
        col: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hover {
    pub at: HoverAt,
    pub lines: Vec<(String, HoverKind)>,
    /// Shown above the anchor's line (parameter hints, so they don't cover the typing).
    pub above: bool,
}

/// Word-wrap popup text to `width` columns.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    super::page::wrap_text(text, width.max(10))
}

/// What the mouse rests on: a view's line, and the column (`None`: the gutter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoverTarget {
    pub pane: usize,
    pub line: usize,
    pub col: Option<usize>,
}

/// Live tab titles for the Problems and Tests pages: an icon (with its color) and a count,
/// e.g. `✖ Problems (4)`, `✔ Tests`. Other tabs keep their title.
pub struct TabTitle {
    pub text: String,
    /// Colored icons: byte ranges of `text`.
    pub spans: Vec<(std::ops::Range<usize>, HoverKind)>,
}

impl TabTitle {
    fn plain(text: String) -> Self {
        Self {
            text,
            spans: Vec::new(),
        }
    }

    fn icon(glyph: char, kind: HoverKind, rest: &str) -> Self {
        Self {
            text: format!("{glyph} {rest}"),
            spans: vec![(0..glyph.len_utf8(), kind)],
        }
    }
}

pub fn tab_title(ed: &Editor, doc: &super::document::Document) -> TabTitle {
    match doc.virtual_name.as_deref() {
        Some(super::PROBLEMS_TITLE) => {
            let (e, w, i) = ed.problems.counts3();
            let busy = ed.linting() || ed.tests_building();
            if e + w + i == 0 && !busy {
                return TabTitle::icon('✔', HoverKind::Pass, "Problems");
            }
            // `◐ Problems ✖ 1 ⚠ 66`: a spinner while checks run, then each count with its icon.
            let mut t = TabTitle::plain(String::new());
            let push_icon = |t: &mut TabTitle, glyph: char, kind: HoverKind| {
                let start = t.text.len();
                t.text.push(glyph);
                t.spans.push((start..t.text.len(), kind));
            };
            if busy {
                push_icon(&mut t, super::linting::spinner(), HoverKind::Text);
                t.text.push(' ');
            }
            t.text.push_str("Problems");
            for (n, glyph, kind) in [
                (e, '✖', HoverKind::Error),
                (w, '⚠', HoverKind::Warning),
                (i, 'ℹ', HoverKind::Text),
            ] {
                if n > 0 {
                    t.text.push(' ');
                    push_icon(&mut t, glyph, kind);
                    t.text.push_str(&format!(" {n}"));
                }
            }
            t
        }
        Some(super::REFERENCES_TITLE) => {
            if ed.refs.running() {
                TabTitle::icon(super::linting::spinner(), HoverKind::Text, "References")
            } else {
                TabTitle::plain(format!("References ({})", ed.refs.count()))
            }
        }
        Some(super::TESTS_TITLE) => {
            let (passed, failed, not_run, running) = ed.tests.tree.counts();
            if running > 0 {
                TabTitle::icon(
                    super::linting::spinner(),
                    HoverKind::Text,
                    &format!("Tests ({running} running)"),
                )
            } else if failed > 0 {
                TabTitle::icon('✖', HoverKind::Error, &format!("Tests ({failed})"))
            } else if passed > 0 && not_run == 0 {
                TabTitle::icon('✔', HoverKind::Pass, "Tests")
            } else {
                TabTitle::icon('○', HoverKind::Dim, "Tests")
            }
        }
        _ => TabTitle::plain(doc.title()),
    }
}

/// Popups show at most this many lines.
const MAX_LINES: usize = 20;

impl Editor {
    /// The popup content for a spot, if there is anything to say.
    pub fn hover_lines(&self, target: HoverTarget) -> Option<Vec<(String, HoverKind)>> {
        let doc = self.panes[target.pane].doc()?;
        let path = doc.canonical.as_ref()?;
        let mut lines: Vec<(String, HoverKind)> = Vec::new();
        // Problems under the mouse (or anywhere on the line, for the gutter).
        let line_len = super::movement::line_content_end(
            &doc.rope,
            target.line.min(doc.rope.len_lines().saturating_sub(1)),
        ) - doc
            .rope
            .line_to_char(target.line.min(doc.rope.len_lines().saturating_sub(1)));
        for d in self.problems.for_file(path) {
            let on_line = d.line <= target.line && target.line <= d.end_line;
            let hit = match target.col {
                None => d.line == target.line,
                Some(col) => {
                    let start = if d.line == target.line { d.col } else { 0 };
                    let mut end = if d.end_line == target.line {
                        d.end_col
                    } else {
                        line_len
                    };
                    if end <= start {
                        end = start + 1;
                    }
                    on_line && col >= start && col < end
                }
            };
            if !hit {
                continue;
            }
            let (glyph, kind) = match d.severity {
                Severity::Error => ('✖', HoverKind::Error),
                _ => ('⚠', HoverKind::Warning),
            };
            let code = d.code.as_deref().unwrap_or("");
            if !lines.is_empty() {
                lines.push((String::new(), HoverKind::Text));
            }
            lines.push((format!("{glyph} {code} ({})", d.source.name()), kind));
            lines.extend(d.message.lines().map(|l| (l.to_string(), HoverKind::Text)));
        }
        // The doc of the name under the mouse (when no problem is there).
        if lines.is_empty()
            && let Some(col) = target.col
            && col < line_len
        {
            let l = target.line.min(doc.rope.len_lines().saturating_sub(1));
            let pos = doc.rope.line_to_char(l) + col;
            let c = doc.rope.char(pos);
            if (c.is_alphanumeric() || c == '_' || c == '$')
                && let Some(doc_lines) = self.symbol_hover(target.pane, pos)
            {
                lines = doc_lines;
            }
        }
        // The test whose mark is on this line (gutter), or which the spot is inside.
        let test = self.tests.tree.tests.iter().find(|t| {
            &t.file == path
                && match target.col {
                    None => t.found.line == target.line,
                    Some(_) => t.found.line <= target.line && target.line <= t.found.end_line,
                }
        });
        if let Some(t) = test
            && (target.col.is_none() || lines.is_empty())
        {
            if !lines.is_empty() {
                lines.push((String::new(), HoverKind::Text));
            }
            let r = t.current_result();
            match (t.status(), r) {
                (Status::Failed, Some(r)) => {
                    lines.push((format!("✖ Test failed: {}", t.found.name), HoverKind::Error));
                    // Paths relative to the project; dependency frames dropped.
                    let root = self.tests.tree.project_of(&t.file).map(|p| p.root.clone());
                    let text = r
                        .detail
                        .as_ref()
                        .or(r.message.as_ref())
                        .map(|s| super::testing::tidy_failure(s, root.as_deref()))
                        .unwrap_or_default();
                    for l in text.lines() {
                        let kind = if l.trim_start().starts_with("at ") {
                            HoverKind::Dim
                        } else {
                            HoverKind::Text
                        };
                        lines.push((l.to_string(), kind));
                    }
                }
                (Status::Passed, Some(r)) => {
                    let ms = r
                        .duration
                        .map(|d| format!(" in {} ms", d.as_millis()))
                        .unwrap_or_default();
                    lines.push((format!("✔ {} passed{ms}", t.found.name), HoverKind::Pass));
                }
                (Status::Running, _) => {
                    lines.push((format!("● {} is running…", t.found.name), HoverKind::Text))
                }
                (Status::NotRunnable, _) => lines.push((
                    format!(
                        "◇ {}: not runnable (stories need Storybook's Vitest addon)",
                        t.found.name
                    ),
                    HoverKind::Dim,
                )),
                _ if target.col.is_none() => lines.push((
                    format!("○ {}: not run (F5 runs it)", t.found.name),
                    HoverKind::Dim,
                )),
                _ => {}
            }
        }
        if lines.is_empty() {
            return None;
        }
        if lines.len() > MAX_LINES {
            lines.truncate(MAX_LINES);
            lines.push(("…".into(), HoverKind::Dim));
        }
        Some(lines)
    }

    /// Ctrl+I: the popup for the cursor position, until the next key.
    pub(super) fn show_hover_at_cursor(&mut self) {
        if !self.has_doc() {
            return;
        }
        let doc = self.doc();
        let line = doc.rope.char_to_line(doc.selection.head);
        let col = doc.selection.head - doc.rope.line_to_char(line);
        let at = |col: usize| HoverTarget {
            pane: self.focus,
            line,
            col: Some(col),
        };
        // The cursor right after a name counts as on it (like F12).
        let lines = self
            .hover_lines(at(col))
            .or_else(|| col.checked_sub(1).and_then(|c| self.hover_lines(at(c))));
        match lines {
            Some(lines) => {
                self.hover = Some(Hover {
                    at: HoverAt::Cell {
                        pane: self.focus,
                        line,
                        col,
                    },
                    lines,
                    above: false,
                })
            }
            None => {
                self.info("Nothing to show here (no problem, test or known name at the cursor)")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::lint::{Diagnostic, Source};
    use crate::test_explorer::{FileTests, Found, Framework, RunResult};

    #[test]
    fn finds_test_failures_and_problems() {
        let dir = std::env::temp_dir().join(format!("jdc-hover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        let file = dir.join("a.spec.js");
        std::fs::write(&file, "it('x', () => {\n  let unused = 1;\n});\n").unwrap();
        let mut ed = Editor::new(Config::default(), std::slice::from_ref(&file));
        ed.tests.tree.replace_file(
            &file,
            FileTests {
                tests: vec![Found {
                    name: "x".into(),
                    suite: vec![],
                    line: 0,
                    end_line: 2,
                    hash: 7,
                    full_name: None,
                    framework: Framework::Vitest,
                }],
                suites: vec![],
            },
        );
        ed.tests.tree.tests[0].result = Some(RunResult {
            status: Status::Failed,
            duration: None,
            message: Some("expected 1".into()),
            detail: Some("expected 1 to be 2\n  at tests/a.spec.js:2:3".into()),
            hash: 7,
        });
        ed.problems.set_file(
            Source::Eslint,
            &file,
            vec![Diagnostic {
                source: Source::Eslint,
                severity: Severity::Warning,
                line: 1,
                col: 6,
                end_line: 1,
                end_col: 12,
                message: "'unused' is assigned a value but never used.".into(),
                code: Some("no-unused-vars".into()),
            }],
        );
        let at = |line, col| HoverTarget { pane: 0, line, col };
        // Gutter mark of the failed test.
        let g = ed.hover_lines(at(0, None)).unwrap();
        assert_eq!(g[0], ("✖ Test failed: x".to_string(), HoverKind::Error));
        assert_eq!(g[2].1, HoverKind::Dim); // stack line
        // On the underline: the problem (not the test).
        let p = ed.hover_lines(at(1, Some(8))).unwrap();
        assert_eq!(p[0].0, "⚠ no-unused-vars (ESLint)");
        assert_eq!(p.len(), 2);
        // Elsewhere in the test body: the failure.
        let b = ed.hover_lines(at(1, Some(0))).unwrap();
        assert_eq!(b[0].0, "✖ Test failed: x");
        assert!(ed.hover_lines(at(5, Some(0))).is_none());
        // The status bar names the failure from anywhere inside the test.
        ed.doc_mut().selection = crate::editor::selection::Selection::point(20);
        assert!(ed.cursor_test_failure().unwrap().contains("expected 1"));
        // Panel tabs: icon and count.
        ed.execute(crate::commands::Command::ShowProblems);
        let doc = ed.panes[1].doc().unwrap();
        let t = tab_title(&ed, doc);
        assert_eq!(t.text, "Problems ⚠ 1");
        assert_eq!(&t.text[t.spans[0].0.clone()], "⚠");
        assert_eq!(t.spans[0].1, HoverKind::Warning);
        ed.problems.set_file(Source::Eslint, &file, vec![]);
        let doc = ed.panes[1].doc().unwrap();
        assert_eq!(tab_title(&ed, doc).text, "✔ Problems");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
