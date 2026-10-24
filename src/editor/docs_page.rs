//! The Docs page (right panel): a .NET API's documentation from the installed SDK and
//! packages, offline. Lists overloads, a type's members and other matches of the name; Enter
//! opens one, Backspace goes back, `w` offers the Microsoft Learn page.

use ropey::Rope;

use super::Editor;
use super::page::{PageText, same_spot};
use super::selection::Selection;
use crate::symbols::dotnet_docs::{self, DotnetDocs};
use crate::syntax::{Lang, Token};

pub const DOCS_TITLE: &str = "Docs";

/// Members listed for a type at most.
const MAX_MEMBERS: usize = 400;

impl Editor {
    pub(super) fn docs_page(&self) -> Option<(usize, usize)> {
        self.find_doc(|d| d.virtual_name.as_deref() == Some(DOCS_TITLE))
    }

    pub(super) fn on_docs_page(&self) -> bool {
        self.has_doc() && self.doc().virtual_name.as_deref() == Some(DOCS_TITLE)
    }

    /// Show API `idx` on the Docs page; `matches`: everything the name looked up matched.
    pub(super) fn show_api_docs(&mut self, idx: u32, matches: Vec<u32>) {
        if let Some(cur) = self.symbols.docs_current
            && cur != idx
        {
            self.symbols.docs_back.push(cur);
        }
        self.symbols.docs_current = Some(idx);
        if !matches.is_empty() {
            self.symbols.docs_matches = matches;
        }
        self.open_virtual(DOCS_TITLE, "");
        self.refresh_docs_page();
        if let Some(d) = &self.symbols.dotnet {
            let msg = format!("{} · w: open on Microsoft Learn", d.full_name(idx));
            self.info(msg);
        }
    }

    fn refresh_docs_page(&mut self) {
        self.lay_out_docs_page(false);
    }

    /// The Docs page laid out again at the panel's width, the cursor on the same text.
    pub(super) fn refresh_docs_page_kept(&mut self) {
        self.lay_out_docs_page(true);
    }

    fn lay_out_docs_page(&mut self, keep: bool) {
        let (Some(d), Some(idx)) = (self.symbols.dotnet.clone(), self.symbols.docs_current) else {
            return;
        };
        let Some((p, i)) = self.docs_page() else {
            return;
        };
        let page = docs_text(&d, idx, &self.symbols.docs_matches, self.panel_width());
        let new = Rope::from_str(&page.text());
        let doc = &mut self.panes[p].docs[i];
        let (head, top) = if keep {
            let top = doc.rope.line_to_char(
                doc.view
                    .top_line
                    .min(doc.rope.len_lines().saturating_sub(1)),
            );
            (
                same_spot(&doc.rope, doc.selection.head, &new),
                new.char_to_line(same_spot(&doc.rope, top, &new)),
            )
        } else {
            (0, 0)
        };
        doc.rope = new;
        doc.syntax.set_lang(Lang::Docs);
        doc.selection = Selection::point(head);
        doc.view.top_line = top;
        doc.view.left_col = 0;
        doc.boxes = page.boxes;
        self.symbols.docs_rows = page.rows;
    }

    /// Enter on the Docs page: open the API on the cursor's line.
    pub(super) fn docs_open_row(&mut self) {
        let line = self.doc().rope.char_to_line(self.doc().selection.head);
        if let Some(Some(idx)) = self.symbols.docs_rows.get(line).copied() {
            self.show_api_docs(idx, Vec::new());
        }
    }

    /// Backspace on the Docs page: the API shown before.
    pub(super) fn docs_go_back(&mut self) {
        match self.symbols.docs_back.pop() {
            Some(idx) => {
                self.symbols.docs_current = Some(idx);
                self.refresh_docs_page();
            }
            None => self.info("Nothing to go back to"),
        }
    }

    /// Typed keys on the Docs page: `w` offers the web page.
    pub(super) fn docs_page_key(&mut self, c: char) {
        if c.eq_ignore_ascii_case(&'w') {
            self.open_docs_in_browser();
        }
    }
}

/// The page and the API each line opens.
fn docs_text(d: &DotnetDocs, idx: u32, matches: &[u32], width: usize) -> PageText<u32> {
    let mut page: PageText<u32> = PageText::new(width);
    let doc = d.doc(idx);
    let (sig, _) = d.signature(idx, &doc);
    page.wrapped(&d.full_name(idx), 0, None);
    page.hint(&format!(".NET API docs from {}.xml", d.assembly(idx)));
    page.hint("w: Microsoft Learn · Enter: open · Backspace: back");
    page.line("", None);
    page.wrapped(&format!("  {sig}"), 2, None);
    let section = |page: &mut PageText<u32>, title: &str| {
        page.line("", None);
        page.line(title, None);
    };
    // Long texts go in boxes.
    if !doc.summary.is_empty() {
        page.line("", None);
        page.boxed(&doc.summary, 0, None, Some(Token::Function));
    }
    if !doc.typeparams.is_empty() {
        section(&mut page, "Type parameters");
        page.wrapped(&format!("  {}", doc.typeparams.join(", ")), 2, None);
    }
    // `name — text`, continuation lines under the text.
    let described = |page: &mut PageText<u32>, name: &str, text: &str| {
        let hang = (2 + name.chars().count() + 3).min(width / 2);
        page.wrapped(&format!("  {name} — {text}"), hang, None);
    };
    if !doc.params.is_empty() {
        section(&mut page, "Parameters");
        for p in &doc.params {
            described(&mut page, &p.name, &p.text);
        }
    }
    if let Some((_, text)) = &doc.returns
        && !text.is_empty()
    {
        section(&mut page, "Returns");
        page.wrapped(&format!("  {text}"), 2, None);
    }
    if !doc.exceptions.is_empty() {
        section(&mut page, "Exceptions");
        for (ty, text) in &doc.exceptions {
            described(&mut page, ty, text);
        }
    }
    if !doc.remarks.is_empty() {
        section(&mut page, "Remarks");
        page.boxed(&doc.remarks, 2, None, Some(Token::Function));
    }
    // Overloads of a member; members of a type.
    let overloads = d.overloads(idx);
    if overloads.len() > 1 {
        section(&mut page, &format!("Overloads ({})", overloads.len()));
        for o in overloads {
            let (s, _) = d.signature(o, &d.doc(o));
            let mark = if o == idx { "●" } else { "▸" };
            page.wrapped(&format!("  {mark} {s}"), 4, Some(o));
        }
    }
    let (kind, _, _) = d.describe(idx);
    if kind == 'T' {
        let members = d.members(idx);
        if !members.is_empty() {
            section(&mut page, &format!("Members ({})", members.len()));
            let mut seen = std::collections::HashSet::new();
            for m in members.into_iter().take(MAX_MEMBERS) {
                let (_, name, _) = d.describe(m);
                let key = format!("{}{name}", d.is_ctor(m));
                if !seen.insert(key) {
                    continue; // overloads: once
                }
                let summary = d.doc(m).summary.clone();
                let first = summary
                    .split(". ")
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('.');
                let label = if d.is_ctor(m) {
                    format!("new {name}")
                } else {
                    name
                };
                let row = if first.is_empty() {
                    format!("  ▸ {label}")
                } else {
                    format!("  ▸ {label} — {first}")
                };
                page.wrapped(&row, 4, Some(m));
            }
        }
    }
    let others: Vec<u32> = matches.iter().copied().filter(|m| *m != idx).collect();
    if !others.is_empty() {
        section(&mut page, "Other matches for this name");
        for m in others {
            page.wrapped(&format!("  ▸ {}", d.full_name(m)), 4, Some(m));
        }
    }
    page.line("", None);
    page.hint(&dotnet_docs::learn_url(d.id(idx)).unwrap_or_default());
    page
}
