//! The Docs page (right panel): a .NET API's documentation from the installed SDK and
//! packages, offline. Lists overloads, a type's members and other matches of the name; Enter
//! opens one, Backspace goes back, `w` offers the Microsoft Learn page.

use ropey::Rope;

use super::selection::Selection;
use super::{Editor, PANEL};
use crate::symbols::dotnet_docs::{self, DotnetDocs};
use crate::syntax::Lang;

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
        let (Some(d), Some(idx)) = (self.symbols.dotnet.clone(), self.symbols.docs_current) else {
            return;
        };
        let Some((p, i)) = self.docs_page() else {
            return;
        };
        let width = self.panes[PANEL].cols.max(40);
        let (text, rows) = docs_text(&d, idx, &self.symbols.docs_matches, width);
        let doc = &mut self.panes[p].docs[i];
        doc.rope = Rope::from_str(&text);
        doc.syntax.set_lang(Lang::Docs);
        doc.selection = Selection::point(0);
        doc.view.top_line = 0;
        doc.view.left_col = 0;
        self.symbols.docs_rows = rows;
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

/// `text` cut to `width` columns with `…`.
fn cut(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The page text and the API each line opens.
fn docs_text(
    d: &DotnetDocs,
    idx: u32,
    matches: &[u32],
    width: usize,
) -> (String, Vec<Option<u32>>) {
    let mut lines: Vec<(String, Option<u32>)> = Vec::new();
    let doc = d.doc(idx);
    let (sig, _) = d.signature(idx, &doc);
    let wrap = |text: &str, indent: usize| -> Vec<String> {
        super::hover::wrap(text, width.saturating_sub(indent + 1))
            .into_iter()
            .map(|l| format!("{}{l}", " ".repeat(indent)))
            .collect()
    };
    lines.push((d.full_name(idx), None));
    lines.push((
        format!("· .NET API docs from {}.xml", d.assembly(idx)),
        None,
    ));
    lines.push((
        "· w: Microsoft Learn · Enter: open · Backspace: back".into(),
        None,
    ));
    lines.push((String::new(), None));
    for l in wrap(&sig, 2) {
        lines.push((l, None));
    }
    if !doc.summary.is_empty() {
        lines.push((String::new(), None));
        lines.extend(wrap(&doc.summary, 0).into_iter().map(|l| (l, None)));
    }
    let section = |lines: &mut Vec<(String, Option<u32>)>, title: &str| {
        lines.push((String::new(), None));
        lines.push((title.to_string(), None));
    };
    if !doc.typeparams.is_empty() {
        section(&mut lines, "Type parameters");
        lines.push((format!("  {}", doc.typeparams.join(", ")), None));
    }
    if !doc.params.is_empty() {
        section(&mut lines, "Parameters");
        for p in &doc.params {
            for l in wrap(&format!("{} — {}", p.name, p.text), 2) {
                lines.push((l, None));
            }
        }
    }
    if let Some((_, text)) = &doc.returns
        && !text.is_empty()
    {
        section(&mut lines, "Returns");
        lines.extend(wrap(text, 2).into_iter().map(|l| (l, None)));
    }
    if !doc.exceptions.is_empty() {
        section(&mut lines, "Exceptions");
        for (ty, text) in &doc.exceptions {
            for l in wrap(&format!("{ty} — {text}"), 2) {
                lines.push((l, None));
            }
        }
    }
    if !doc.remarks.is_empty() {
        section(&mut lines, "Remarks");
        lines.extend(wrap(&doc.remarks, 2).into_iter().map(|l| (l, None)));
    }
    // Overloads of a member; members of a type.
    let overloads = d.overloads(idx);
    if overloads.len() > 1 {
        section(&mut lines, &format!("Overloads ({})", overloads.len()));
        for o in overloads {
            let (s, _) = d.signature(o, &d.doc(o));
            let mark = if o == idx { "●" } else { "▸" };
            lines.push((format!("  {mark} {s}"), Some(o)));
        }
    }
    let (kind, _, _) = d.describe(idx);
    if kind == 'T' {
        let members = d.members(idx);
        if !members.is_empty() {
            section(&mut lines, &format!("Members ({})", members.len()));
            let mut seen = std::collections::HashSet::new();
            for m in members.into_iter().take(MAX_MEMBERS) {
                let (_, name, _) = d.describe(m);
                let key = format!("{}{name}", d.is_ctor(m));
                if !seen.insert(key) {
                    continue; // overloads: once
                }
                let summary = d.doc(m).summary;
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
                lines.push((cut(&row, width), Some(m)));
            }
        }
    }
    let others: Vec<u32> = matches.iter().copied().filter(|m| *m != idx).collect();
    if !others.is_empty() {
        section(&mut lines, "Other matches for this name");
        for m in others {
            lines.push((format!("  ▸ {}", d.full_name(m)), Some(m)));
        }
    }
    let url = dotnet_docs::learn_url(d.id(idx)).unwrap_or_default();
    lines.push((String::new(), None));
    lines.push((format!("· {url}"), None));
    let text = lines
        .iter()
        .map(|(l, _)| l.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (text, lines.into_iter().map(|(_, r)| r).collect())
}
