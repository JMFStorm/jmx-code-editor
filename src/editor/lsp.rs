//! Language servers in the editor: open documents are kept in sync with the server that
//! serves them, its diagnostics go to Problems, and hover, Go to Definition, Find All
//! References, parameter hints and formatting ask it instead of the built-in scanners.
//! Requests are asynchronous: each one remembers what asked for it until its reply arrives.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ropey::Rope;
use serde_json::{Value, json};

use super::hover::{Hover, HoverAt, HoverKind, HoverTarget};
use super::linting::Target;
use super::symbols::word_at;
use super::{EDITOR, Editor};
use crate::lint::{Diagnostic, Severity, Source};
use crate::lsp::{self, Incoming, Lsp};
use crate::symbols::refs::Reference;
use crate::ui::picker::FilePicker;

/// Edits are sent to the server after typing paused this long (or before a request).
const CHANGE_DELAY: Duration = Duration::from_millis(150);
/// How often servers are polled while a request is waiting, and otherwise.
const BUSY_POLL: Duration = Duration::from_millis(16);
const IDLE_POLL: Duration = Duration::from_millis(200);
/// A hover reply later than this is not waited for.
const HOVER_TIMEOUT: Duration = Duration::from_millis(1500);

enum Pending {
    Hover {
        at_cursor: bool,
    },
    Definition {
        name: String,
        web: bool,
    },
    References {
        name: String,
    },
    Signature {
        revision: u64,
        head: usize,
    },
    Format {
        path: PathBuf,
        revision: u64,
        then_save: bool,
    },
}

/// The doc lines of the word last hovered (`lines: None` while waiting).
struct HoverCache {
    path: PathBuf,
    revision: u64,
    word: (usize, usize),
    asked: Instant,
    lines: Option<Vec<(String, HoverKind)>>,
}

#[derive(Default)]
pub struct LspState {
    pub client: Lsp,
    /// Waiting requests by (server, id).
    pending: HashMap<(usize, u64), (Pending, Instant)>,
    hover: Option<HoverCache>,
    /// The revision of an open document's last edit and when it was first seen.
    edited: HashMap<PathBuf, (u64, Instant)>,
    /// Open documents' unsaved state at the last poll (a change to clean is a save).
    dirty: HashMap<PathBuf, bool>,
}

/// The Odin root (with `core/`, `base/`, `vendor/`): `$ODIN_ROOT`, else the folder of the
/// `odin` executable.
fn odin_root() -> Option<PathBuf> {
    let root = std::env::var_os("ODIN_ROOT")
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let odin = crate::process::find_tool("odin", "JDC_ODIN")?;
            let odin = std::fs::canonicalize(&odin).unwrap_or(odin);
            odin.parent().map(Path::to_path_buf)
        })?;
    root.join("core").is_dir().then_some(root)
}

/// The pkg.odin-lang.org page of `name` declared in `file`, if it is in Odin's own packages.
pub fn odin_docs_url(root: &Path, file: &Path, name: &str) -> Option<String> {
    let pkg = file.parent()?.strip_prefix(root).ok()?;
    let parts: Vec<String> = pkg
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if !matches!(
        parts.first().map(String::as_str),
        Some("core" | "base" | "vendor")
    ) {
        return None;
    }
    Some(format!(
        "https://pkg.odin-lang.org/{}/#{name}",
        parts.join("/")
    ))
}

/// A file's text: the open document's, else the file on disk.
fn file_rope(ed: &Editor, path: &Path) -> Option<Rope> {
    match ed.find_doc(|d| d.canonical.as_deref() == Some(path)) {
        Some((p, i)) => Some(ed.panes[p].docs[i].rope.clone()),
        None => std::fs::read_to_string(path)
            .ok()
            .map(|t| Rope::from_str(&t)),
    }
}

/// `Location`, `Location[]` or `LocationLink[]` as (path, LSP start position).
fn locations(v: &Value) -> Vec<(PathBuf, Value)> {
    let list = match v {
        Value::Array(a) => a.clone(),
        Value::Null => Vec::new(),
        one => vec![one.clone()],
    };
    list.iter()
        .filter_map(|l| {
            let uri = l["uri"].as_str().or_else(|| l["targetUri"].as_str())?;
            let range = if l["range"].is_object() {
                &l["range"]
            } else {
                &l["targetSelectionRange"]
            };
            let path = lsp::uri_to_path(uri)?;
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            Some((path, range["start"].clone()))
        })
        .collect()
}

impl Editor {
    fn lsp_folders(&self) -> Vec<PathBuf> {
        self.workspace
            .as_ref()
            .map(|w| w.folders.clone())
            .unwrap_or_default()
    }

    /// The server for view `pane`'s document, its path, with the server up to date with its
    /// text (sent now if edits were waiting). `None`: no server serves it.
    fn lsp_doc(&mut self, pane: usize) -> Option<(usize, PathBuf)> {
        let doc = self.panes[pane].doc()?;
        let path = doc.canonical.clone()?;
        lsp::config::for_path(&path)?;
        let (rev, text) = (doc.revision, doc.rope.to_string());
        let folders = self.lsp_folders();
        let client = &mut self.lsp.client;
        let i = match client.server_with(&path) {
            Some(i) => i,
            None => {
                let i = client.server_for(&path, &folders)?;
                client.servers[i].did_open(&path, text.clone(), rev);
                i
            }
        };
        let s = &mut client.servers[i];
        if s.open.get(&path) != Some(&rev) {
            s.did_change(&path, text, rev);
        }
        self.lsp.edited.remove(&path);
        Some((i, path))
    }

    fn lsp_request(&mut self, server: usize, method: &str, params: Value, p: Pending) {
        let id = self.lsp.client.servers[server].request(method, params);
        self.lsp.pending.insert((server, id), (p, Instant::now()));
    }

    /// `{ textDocument, position }` for char `pos` of view `pane`'s document.
    fn lsp_position(&self, pane: usize, server: usize, path: &Path, pos: usize) -> Value {
        let utf16 = self.lsp.client.servers[server].utf16;
        let rope = &self.panes[pane].doc().expect("document").rope;
        json!({
            "textDocument": { "uri": lsp::path_to_uri(path) },
            "position": lsp::position(rope, pos, utf16),
        })
    }

    // ------------------------------------------------------------ syncing and polling

    /// Sync open documents, collect server messages. Returns true if anything visible
    /// changed.
    pub(super) fn poll_lsp(&mut self) -> bool {
        self.sync_lsp_docs();
        let mut changed = false;
        for (server, msg) in self.lsp.client.poll() {
            changed |= self.lsp_message(server, msg);
        }
        if let Some(e) = self.lsp.client.start_error.take() {
            self.warn(e);
            changed = true;
        }
        // Requests whose server died are dropped.
        let servers = &self.lsp.client.servers;
        self.lsp
            .pending
            .retain(|(s, _), _| servers.get(*s).is_some_and(|s| !s.dead));
        changed
    }

    fn sync_lsp_docs(&mut self) {
        let folders = self.lsp_folders();
        let now = Instant::now();
        let mut seen = HashSet::new();
        let state = &mut self.lsp;
        for doc in self.panes.iter().flat_map(|p| p.docs.iter()) {
            let Some(path) = &doc.canonical else {
                continue;
            };
            if lsp::config::for_path(path).is_none() || !seen.insert(path.clone()) {
                continue;
            }
            let dirty = doc.is_dirty();
            let was_dirty = state.dirty.insert(path.clone(), dirty);
            let Some(i) = state.client.server_with(path) else {
                if let Some(i) = state.client.server_for(path, &folders) {
                    state.client.servers[i].did_open(path, doc.rope.to_string(), doc.revision);
                }
                continue;
            };
            let s = &mut state.client.servers[i];
            if s.open.get(path) != Some(&doc.revision) {
                let since = match state.edited.get(path) {
                    Some((r, t)) if *r == doc.revision => *t,
                    _ => {
                        state.edited.insert(path.clone(), (doc.revision, now));
                        now
                    }
                };
                // A save sends the text right away (the server checks saved files).
                if now - since < CHANGE_DELAY && !(was_dirty == Some(true) && !dirty) {
                    continue;
                }
                s.did_change(path, doc.rope.to_string(), doc.revision);
                state.edited.remove(path);
            }
            if was_dirty == Some(true) && !dirty {
                s.did_save(path, doc.rope.to_string());
            }
        }
        for s in &mut state.client.servers {
            let closed: Vec<PathBuf> = s
                .open
                .keys()
                .filter(|p| !seen.contains(*p))
                .cloned()
                .collect();
            for p in closed {
                s.did_close(&p);
            }
        }
        state.dirty.retain(|p, _| seen.contains(p));
        state.edited.retain(|p, _| seen.contains(p));
    }

    /// When servers should be polled next.
    pub(super) fn lsp_deadline(&self) -> Option<Instant> {
        if !self.lsp.client.any_running() {
            return None;
        }
        let busy = !self.lsp.pending.is_empty()
            || self.lsp.client.servers.iter().any(|s| !s.dead && !s.ready);
        if busy {
            return Some(Instant::now() + BUSY_POLL);
        }
        let edit = self
            .lsp
            .edited
            .values()
            .map(|(_, t)| *t + CHANGE_DELAY)
            .min();
        let idle = Instant::now() + IDLE_POLL;
        Some(edit.map_or(idle, |e| e.min(idle)))
    }

    /// Requests are waiting, edits are unsent, or a server is starting (headless `wait`).
    pub(super) fn lsp_busy(&self) -> bool {
        !self.lsp.pending.is_empty()
            || !self.lsp.edited.is_empty()
            || self.lsp.client.servers.iter().any(|s| !s.dead && !s.ready)
    }

    fn lsp_message(&mut self, server: usize, msg: Incoming) -> bool {
        match msg {
            Incoming::Notification { method, params } => match method.as_str() {
                "textDocument/publishDiagnostics" => self.lsp_diagnostics(server, &params),
                "window/showMessage" => {
                    let text = params["message"].as_str().unwrap_or_default().to_string();
                    let name = self.lsp.client.servers[server].config.name;
                    match params["type"].as_u64() {
                        Some(1) => self.error(format!("{name}: {text}")),
                        Some(2) => self.warn(format!("{name}: {text}")),
                        _ => self.info(format!("{name}: {text}")),
                    }
                    true
                }
                "window/logMessage" => {
                    let name = self.lsp.client.servers[server].config.name;
                    log::debug!("{name}: {}", params["message"].as_str().unwrap_or_default());
                    false
                }
                _ => false,
            },
            Incoming::Response { id, result } => {
                let Some((pending, _)) = self.lsp.pending.remove(&(server, id)) else {
                    return false;
                };
                let result = match result {
                    Ok(r) => r,
                    Err(e) => {
                        let name = self.lsp.client.servers[server].config.name;
                        log::warn!("{name}: request failed: {e}");
                        Value::Null
                    }
                };
                self.lsp_reply(server, pending, result);
                true
            }
            Incoming::Request { .. } => false,
        }
    }

    fn lsp_diagnostics(&mut self, server: usize, params: &Value) -> bool {
        let Some(path) = params["uri"].as_str().and_then(lsp::uri_to_path) else {
            return false;
        };
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let s = &self.lsp.client.servers[server];
        let (utf16, source) = (s.utf16, Source::Lsp(s.config.name));
        let list = params["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let rope = if list.is_empty() {
            None
        } else {
            file_rope(self, &path)
        };
        let diags = list
            .iter()
            .filter_map(|d| {
                let rope = rope.as_ref()?;
                let pos = |p: &Value| {
                    let c = lsp::char_index(rope, p, utf16);
                    let line = rope.char_to_line(c);
                    (line, c - rope.line_to_char(line))
                };
                let (line, col) = pos(&d["range"]["start"]);
                let (end_line, end_col) = pos(&d["range"]["end"]);
                Some(Diagnostic {
                    source,
                    severity: match d["severity"].as_u64() {
                        Some(1) | None => Severity::Error,
                        Some(2) => Severity::Warning,
                        _ => Severity::Info,
                    },
                    line,
                    col,
                    end_line,
                    end_col,
                    message: d["message"].as_str().unwrap_or_default().to_string(),
                    code: match &d["code"] {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    },
                })
            })
            .collect();
        self.problems.set_file(source, &path, diags);
        self.refresh_problems_page();
        true
    }

    fn lsp_reply(&mut self, server: usize, pending: Pending, result: Value) {
        match pending {
            Pending::Hover { at_cursor } => {
                let lines = hover_lines(&result);
                if let Some(h) = &mut self.lsp.hover {
                    h.lines = Some(lines);
                }
                if at_cursor {
                    self.show_hover_at_cursor();
                }
            }
            Pending::Definition { name, web } => {
                let utf16 = self.lsp.client.servers[server].utf16;
                self.lsp_definition_reply(&name, web, &result, utf16)
            }
            Pending::References { name } => {
                let mut by_file: Vec<(PathBuf, Vec<Reference>)> = Vec::new();
                let utf16 = self.lsp.client.servers[server].utf16;
                for (path, start) in locations(&result) {
                    let Some(rope) = file_rope(self, &path) else {
                        continue;
                    };
                    let c = lsp::char_index(&rope, &start, utf16);
                    let line = rope.char_to_line(c);
                    let r = Reference {
                        line,
                        col: c - rope.line_to_char(line),
                        text: rope.line(line).to_string().trim().to_string(),
                        definition: false,
                    };
                    match by_file.iter_mut().find(|(p, _)| *p == path) {
                        Some((_, refs)) => refs.push(r),
                        None => by_file.push((path, vec![r])),
                    }
                }
                for (_, refs) in &mut by_file {
                    refs.sort_by_key(|r| (r.line, r.col));
                    refs.dedup_by_key(|r| (r.line, r.col));
                }
                by_file.sort_by(|a, b| a.0.cmp(&b.0));
                self.show_lsp_references(name, by_file);
            }
            Pending::Signature { revision, head } => {
                let doc = self.panes[EDITOR].doc();
                if doc.is_none_or(|d| d.revision != revision || d.selection.head != head) {
                    return;
                }
                self.symbols.signature = self.signature_from(&result, head);
            }
            Pending::Format {
                path,
                revision,
                then_save,
            } => {
                let Some((p, i)) = self.find_doc(|d| d.canonical.as_ref() == Some(&path)) else {
                    return;
                };
                if self.panes[p].docs[i].revision != revision {
                    self.info("The file changed while formatting; not applied");
                    return;
                }
                let utf16 = self.lsp.client.servers[server].utf16;
                match apply_text_edits(&self.panes[p].docs[i].rope, &result, utf16) {
                    Some(new) => {
                        let changed = self.apply_formatted(p, i, &new);
                        self.info(if changed {
                            "Formatted"
                        } else {
                            "Already formatted"
                        });
                    }
                    None => self.info("The formatter made no changes"),
                }
                if then_save {
                    self.save_doc(p, i);
                }
            }
        }
    }

    // ------------------------------------------------------------ hover

    /// Before a hover popup for `target` opens: ask the server for the docs of the word
    /// there. True while the reply is awaited (the popup waits too).
    pub fn lsp_hover_wait(&mut self, target: HoverTarget, at_cursor: bool) -> bool {
        let Some(col) = target.col else {
            return false;
        };
        let Some(doc) = self.panes[target.pane].doc() else {
            return false;
        };
        let Some(path) = doc.canonical.clone() else {
            return false;
        };
        if lsp::config::for_path(&path).is_none() {
            return false;
        }
        let rope = &doc.rope;
        let line = target.line.min(rope.len_lines().saturating_sub(1));
        let pos = (rope.line_to_char(line) + col).min(rope.len_chars());
        // Only names have docs (as in `hover_lines`).
        if !rope
            .get_char(pos)
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            return false;
        }
        let Some((start, end, _)) = word_at(rope, pos) else {
            return false;
        };
        let revision = doc.revision;
        if let Some(h) = &self.lsp.hover
            && h.path == path
            && h.revision == revision
            && h.word == (start, end)
        {
            return h.lines.is_none() && h.asked.elapsed() < HOVER_TIMEOUT;
        }
        let Some((server, path)) = self.lsp_doc(target.pane) else {
            return false;
        };
        let params = self.lsp_position(target.pane, server, &path, pos);
        log::debug!("hover request at {}:{}", target.line + 1, col + 1);
        self.lsp_request(
            server,
            "textDocument/hover",
            params,
            Pending::Hover { at_cursor },
        );
        self.lsp.hover = Some(HoverCache {
            path,
            revision,
            word: (start, end),
            asked: Instant::now(),
            lines: None,
        });
        true
    }

    /// The server's docs for the word at `pos` of view `pane`, once they arrived.
    pub(super) fn lsp_hover_lines(
        &self,
        pane: usize,
        pos: usize,
    ) -> Option<Option<Vec<(String, HoverKind)>>> {
        let doc = self.panes[pane].doc()?;
        let path = doc.canonical.as_ref()?;
        lsp::config::for_path(path)?;
        let h = self.lsp.hover.as_ref();
        let (start, end, _) = word_at(&doc.rope, pos)?;
        Some(
            h.filter(|h| &h.path == path && h.revision == doc.revision && h.word == (start, end))
                .and_then(|h| h.lines.clone())
                .filter(|l| !l.is_empty()),
        )
    }

    // ------------------------------------------------------------ definition and docs

    /// F12 (or Ctrl+F12 with `web`) in a document a server serves: ask it. False: no server
    /// serves the document (the built-in lookup runs).
    pub(super) fn lsp_definition(&mut self, web: bool) -> bool {
        let Some((server, path)) = self.lsp_doc(EDITOR) else {
            return false;
        };
        let doc = self.doc();
        let head = doc.selection.head;
        let Some((_, _, name)) = word_at(&doc.rope, head) else {
            self.info("No name at the cursor");
            return true;
        };
        let params = self.lsp_position(EDITOR, server, &path, head);
        self.lsp_request(
            server,
            "textDocument/definition",
            params,
            Pending::Definition { name, web },
        );
        true
    }

    fn lsp_definition_reply(&mut self, name: &str, web: bool, result: &Value, utf16: bool) {
        let found: Vec<Target> = locations(result)
            .into_iter()
            .map(|(path, start)| {
                let line = start["line"].as_u64().unwrap_or(0) as usize;
                let character = start["character"].as_u64().unwrap_or(0) as usize;
                let col = file_rope(self, &path)
                    .filter(|r| line < r.len_lines())
                    .map_or(character, |r| {
                        lsp::char_col(&r.line(line).to_string(), character, utf16)
                    });
                Target { path, line, col }
            })
            .collect();
        if found.is_empty() {
            self.info(format!("No definition found for `{name}`"));
            return;
        }
        if web {
            let url = odin_root().and_then(|root| odin_docs_url(&root, &found[0].path, name));
            match url {
                Some(url) => {
                    self.confirm_open_url(url, format!("the Odin package docs for `{name}`"))
                }
                None => self.info(format!(
                    "`{name}` is not in Odin's own packages: F12 goes to its declaration"
                )),
            }
            return;
        }
        self.push_back();
        if let [t] = found.as_slice() {
            self.show_location(t, true);
            self.info(format!(
                "{name} — {}:{}",
                self.display_path(&t.path),
                t.line + 1
            ));
            return;
        }
        let items = found
            .iter()
            .map(|t| {
                (
                    format!("{name}  {}:{}", self.display_path(&t.path), t.line + 1),
                    t.path.clone(),
                    t.line + 1,
                    t.col + 1,
                )
            })
            .collect();
        self.picker = Some(FilePicker::definitions(name, items));
    }

    // ------------------------------------------------------------ references

    /// Shift+F12 in a document a server serves. False: none serves it.
    pub(super) fn lsp_references(&mut self) -> bool {
        let Some((server, path)) = self.lsp_doc(EDITOR) else {
            return false;
        };
        let head = self.doc().selection.head;
        let Some((_, _, name)) = word_at(&self.doc().rope, head) else {
            self.info("No name at the cursor");
            return true;
        };
        let mut params = self.lsp_position(EDITOR, server, &path, head);
        params["context"] = json!({ "includeDeclaration": true });
        self.lsp_request(
            server,
            "textDocument/references",
            params,
            Pending::References { name: name.clone() },
        );
        self.info(format!("Finding references to `{name}`…"));
        true
    }

    // ------------------------------------------------------------ parameter hints

    /// Parameter hints in a document a server serves. False: none serves it.
    pub(super) fn lsp_signature(&mut self, open: bool) -> bool {
        if self.focus != EDITOR || self.multi_cursor() {
            return false;
        }
        let Some(path) = self.panes[EDITOR].doc().and_then(|d| d.canonical.clone()) else {
            return false;
        };
        if lsp::config::for_path(&path).is_none() {
            return false;
        }
        if !open && self.symbols.signature.is_none() {
            return true;
        }
        let Some((server, path)) = self.lsp_doc(EDITOR) else {
            return true;
        };
        let doc = self.doc();
        let (head, revision) = (doc.selection.head, doc.revision);
        let params = self.lsp_position(EDITOR, server, &path, head);
        self.lsp_request(
            server,
            "textDocument/signatureHelp",
            params,
            Pending::Signature { revision, head },
        );
        true
    }

    fn signature_from(&self, result: &Value, head: usize) -> Option<Hover> {
        let sigs = result["signatures"].as_array()?;
        let k = result["activeSignature"].as_u64().unwrap_or(0) as usize;
        let sig = sigs.get(k).or(sigs.first())?;
        let label = sig["label"].as_str()?.to_string();
        let active = sig["activeParameter"]
            .as_u64()
            .or(result["activeParameter"].as_u64())
            .unwrap_or(0) as usize;
        let mut lines = vec![(label.clone(), HoverKind::Code)];
        if let Some(p) = sig["parameters"].as_array().and_then(|p| p.get(active)) {
            let text = match &p["label"] {
                Value::String(s) => s.clone(),
                Value::Array(r) => {
                    let (a, b) = (r[0].as_u64().unwrap_or(0), r[1].as_u64().unwrap_or(0));
                    // Offsets count UTF-16 units of the label.
                    let units: Vec<u16> = label.encode_utf16().collect();
                    String::from_utf16_lossy(units.get(a as usize..b as usize).unwrap_or(&[]))
                }
                _ => String::new(),
            };
            if !text.is_empty() {
                let mut line = format!("▸ {text}");
                if let Some(d) = doc_text(&p["documentation"]).filter(|d| !d.is_empty()) {
                    line.push_str(&format!(" — {d}"));
                }
                lines.push((line, HoverKind::Param));
            }
        }
        if let Some(d) = doc_text(&sig["documentation"]) {
            lines.extend(
                lsp::markdown_lines(&d)
                    .into_iter()
                    .filter(|(_, l)| !l.is_empty())
                    .take(3)
                    .map(|(code, l)| {
                        (
                            l,
                            if code {
                                HoverKind::Code
                            } else {
                                HoverKind::Text
                            },
                        )
                    }),
            );
        }
        let rope = &self.panes[EDITOR].doc()?.rope;
        let line = rope.char_to_line(head);
        Some(Hover {
            at: HoverAt::Cell {
                pane: EDITOR,
                line,
                col: head - rope.line_to_char(line),
            },
            lines,
            above: true,
        })
    }

    // ------------------------------------------------------------ formatting

    /// Format the focused document with its server. False: no server serves it.
    pub(super) fn lsp_format(&mut self, then_save: bool) -> bool {
        let Some((server, path)) = self.lsp_doc(self.focus) else {
            return false;
        };
        if self.lsp.client.servers[server].caps["documentFormattingProvider"].is_null() {
            return false;
        }
        let doc = self.doc();
        let (tab, spaces) = match doc.indent {
            super::document::Indent::Spaces(n) => (n, true),
            super::document::Indent::Tabs => (self.config.tab_width, false),
        };
        let params = json!({
            "textDocument": { "uri": lsp::path_to_uri(&path) },
            "options": { "tabSize": tab, "insertSpaces": spaces },
        });
        let revision = doc.revision;
        self.lsp_request(
            server,
            "textDocument/formatting",
            params,
            Pending::Format {
                path,
                revision,
                then_save,
            },
        );
        self.info("Formatting…");
        true
    }
}

/// `MarkupContent`, `MarkedString`(s) or a plain string as text.
fn doc_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => match (o.get("language"), o.get("value")) {
            (Some(lang), Some(val)) => Some(format!(
                "```{}\n{}\n```",
                lang.as_str().unwrap_or_default(),
                val.as_str().unwrap_or_default()
            )),
            (_, Some(val)) => val.as_str().map(str::to_string),
            _ => None,
        },
        Value::Array(a) => Some(
            a.iter()
                .filter_map(doc_text)
                .collect::<Vec<_>>()
                .join("\n---\n"),
        ),
        _ => None,
    }
}

/// A hover reply as popup lines.
fn hover_lines(result: &Value) -> Vec<(String, HoverKind)> {
    let Some(text) = doc_text(&result["contents"]) else {
        return Vec::new();
    };
    lsp::markdown_lines(&text)
        .into_iter()
        .map(|(code, l)| {
            (
                l,
                if code {
                    HoverKind::Code
                } else {
                    HoverKind::Text
                },
            )
        })
        .collect()
}

/// `TextEdit[]` applied to `rope`; `None` if there are none.
fn apply_text_edits(rope: &Rope, edits: &Value, utf16: bool) -> Option<String> {
    let edits = edits.as_array().filter(|e| !e.is_empty())?;
    let mut ranges: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            let a = lsp::char_index(rope, &e["range"]["start"], utf16);
            let b = lsp::char_index(rope, &e["range"]["end"], utf16);
            (a, b.max(a), e["newText"].as_str().unwrap_or_default())
        })
        .collect();
    // Back to front so earlier positions stay valid.
    ranges.sort_by_key(|r| std::cmp::Reverse((r.0, r.1)));
    let mut out = rope.clone();
    for (a, b, text) in ranges {
        let b = b.min(out.len_chars());
        let a = a.min(b);
        out.remove(a..b);
        out.insert(a, text);
    }
    Some(out.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_text_edits_and_doc_urls() {
        let r = json!({ "contents": { "kind": "markdown",
            "value": "```odin\nadd :: proc(a, b: int) -> int\n```\n---\nAdds two numbers." } });
        assert_eq!(
            hover_lines(&r),
            vec![
                ("add :: proc(a, b: int) -> int".to_string(), HoverKind::Code),
                (String::new(), HoverKind::Text),
                ("Adds two numbers.".to_string(), HoverKind::Text),
            ]
        );
        let rope = Rope::from_str("a :=  1\nb:=2\n");
        let edits = json!([
            { "range": { "start": { "line": 0, "character": 4 }, "end": { "line": 0, "character": 6 } }, "newText": " " },
            { "range": { "start": { "line": 1, "character": 1 }, "end": { "line": 1, "character": 1 } }, "newText": " " },
        ]);
        assert_eq!(
            apply_text_edits(&rope, &edits, true).as_deref(),
            Some("a := 1\nb :=2\n")
        );
        assert_eq!(apply_text_edits(&rope, &json!([]), true), None);
        let root = Path::new("/opt/odin");
        assert_eq!(
            odin_docs_url(
                root,
                Path::new("/opt/odin/core/encoding/json/marshal.odin"),
                "marshal"
            )
            .as_deref(),
            Some("https://pkg.odin-lang.org/core/encoding/json/#marshal")
        );
        assert_eq!(
            odin_docs_url(root, Path::new("/opt/odin/tests/x.odin"), "x"),
            None
        );
        assert_eq!(odin_docs_url(root, Path::new("/home/me/x.odin"), "x"), None);
    }
}
