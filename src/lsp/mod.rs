//! Language Server Protocol client: JSON-RPC over a server's stdin/stdout. A reader thread
//! decodes the server's messages onto a channel that the editor polls like its other
//! background jobs. Documents are synced as full text.

pub mod config;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use ropey::Rope;
use serde_json::{Value, json};

pub use config::ServerConfig;

/// A server that died is started again after this long.
const RESTART_DELAY: Duration = Duration::from_secs(5);
/// A server that died this many times is not started again.
const MAX_STARTS: u32 = 3;

/// A message from a server.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The reply to our request `id`: its result, or the error message.
    Response {
        id: u64,
        result: Result<Value, String>,
    },
    Notification {
        method: String,
        params: Value,
    },
    /// A request from the server (it waits for a reply).
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

impl Incoming {
    fn from_json(mut v: Value) -> Option<Incoming> {
        let method = v.get("method").and_then(Value::as_str).map(str::to_string);
        let id = v.get_mut("id").map(Value::take);
        match (method, id) {
            (Some(method), Some(id)) if !id.is_null() => Some(Incoming::Request {
                id,
                method,
                params: v["params"].take(),
            }),
            (Some(method), _) => Some(Incoming::Notification {
                method,
                params: v["params"].take(),
            }),
            (None, Some(id)) => {
                let id = id.as_u64()?;
                let result = match v.get("error") {
                    Some(e) if !e.is_null() => {
                        Err(e["message"].as_str().unwrap_or("unknown error").to_string())
                    }
                    _ => Ok(v["result"].take()),
                };
                Some(Incoming::Response { id, result })
            }
            (None, None) => None,
        }
    }
}

/// Read one `Content-Length` framed message. `Ok(None)`: the stream ended.
pub fn read_message(r: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut len = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let l = line.trim_end();
        if l.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = l.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; len.unwrap_or(0)];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub fn frame(v: &Value) -> Vec<u8> {
    let body = v.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

// ------------------------------------------------------------ positions and URIs

/// An LSP position for char index `pos` of `rope`.
pub fn position(rope: &Rope, pos: usize, utf16: bool) -> Value {
    let pos = pos.min(rope.len_chars());
    let line = rope.char_to_line(pos);
    let start = rope.line_to_char(line);
    let character = if utf16 {
        rope.char_to_utf16_cu(pos) - rope.char_to_utf16_cu(start)
    } else {
        pos - start
    };
    json!({ "line": line, "character": character })
}

/// The char index of an LSP position in `rope` (clamped to the line).
pub fn char_index(rope: &Rope, p: &Value, utf16: bool) -> usize {
    let line = (p["line"].as_u64().unwrap_or(0) as usize).min(rope.len_lines().saturating_sub(1));
    let character = p["character"].as_u64().unwrap_or(0) as usize;
    let start = rope.line_to_char(line);
    let line_len = rope.line(line).len_chars();
    let col = if utf16 {
        let base = rope.char_to_utf16_cu(start);
        let max = rope.char_to_utf16_cu(start + line_len);
        rope.utf16_cu_to_char((base + character).min(max)) - start
    } else {
        character
    };
    start + col.min(line_len)
}

/// The char column of UTF-16 offset `character` in `line`.
pub fn char_col(line: &str, character: usize, utf16: bool) -> usize {
    if !utf16 {
        return character;
    }
    let mut units = 0;
    for (i, c) in line.chars().enumerate() {
        if units >= character {
            return i;
        }
        units += c.len_utf16();
    }
    line.chars().count()
}

pub fn path_to_uri(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from(if s.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = rest
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let s = String::from_utf8_lossy(&out).into_owned();
    // `file:///C:/x` on Windows.
    let s = match s.strip_prefix('/') {
        Some(win) if cfg!(windows) && win.get(1..2) == Some(":") => win.to_string(),
        _ => s,
    };
    Some(PathBuf::from(s))
}

/// Hover / docs markdown as plain popup text: code fences become signature lines, `---`
/// rules blank lines; returns (is code, text) per line.
pub fn markdown_lines(md: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut code = false;
    for l in md.lines() {
        let t = l.trim_end();
        if t.trim_start().starts_with("```") {
            code = !code;
            continue;
        }
        if !code && (t == "---" || t == "***") {
            if out
                .last()
                .is_some_and(|(_, s): &(bool, String)| !s.is_empty())
            {
                out.push((false, String::new()));
            }
            continue;
        }
        out.push((code, t.to_string()));
    }
    while out.last().is_some_and(|(_, s)| s.is_empty()) {
        out.pop();
    }
    out
}

// ------------------------------------------------------------ one server

pub struct Server {
    pub config: &'static ServerConfig,
    pub root: PathBuf,
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: u64,
    init_id: u64,
    /// The `initialize` reply arrived; until then other messages wait in `queued`.
    pub ready: bool,
    queued: Vec<Value>,
    /// Positions count UTF-16 code units (the LSP default).
    pub utf16: bool,
    /// The server's capabilities.
    pub caps: Value,
    /// Open documents and the revision last sent.
    pub open: HashMap<PathBuf, u64>,
    pub dead: bool,
}

impl Server {
    fn start(config: &'static ServerConfig, root: &Path) -> Result<Server, String> {
        let program =
            crate::process::find_tool(config.program, config.env_var).ok_or_else(|| {
                format!(
                    "{} not found (put it on PATH or set {})",
                    config.program, config.env_var
                )
            })?;
        let mut cmd = Command::new(&program);
        cmd.current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        crate::perf::bump(&crate::perf::PROCESSES);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = mpsc::channel();
        let name = config.name;
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                match read_message(&mut r) {
                    Ok(Some(v)) => {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        log::warn!("{name}: bad message: {e}");
                        break;
                    }
                }
            }
        });
        log::info!(
            "started {} ({}) for {}",
            config.name,
            program.display(),
            root.display()
        );
        let mut s = Server {
            config,
            root: root.to_path_buf(),
            child,
            stdin,
            rx,
            next_id: 1,
            init_id: 0,
            ready: false,
            queued: Vec::new(),
            utf16: true,
            caps: Value::Null,
            open: HashMap::new(),
            dead: false,
        };
        let root_uri = path_to_uri(root);
        let params = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "rootPath": root.to_string_lossy(),
            "workspaceFolders": [{ "uri": root_uri, "name": root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default() }],
            "initializationOptions": (config.init_options)(root),
            "capabilities": {
                "general": { "positionEncodings": ["utf-32", "utf-16"] },
                "textDocument": {
                    "synchronization": { "didSave": true },
                    "hover": { "contentFormat": ["markdown", "plaintext"] },
                    "definition": { "linkSupport": false },
                    "signatureHelp": { "signatureInformation": {
                        "documentationFormat": ["markdown", "plaintext"],
                        "parameterInformation": { "labelOffsetSupport": true }
                    } },
                    "publishDiagnostics": { "relatedInformation": false },
                },
                "workspace": { "configuration": false, "workspaceFolders": true },
            },
        });
        s.init_id = s.next_id;
        s.next_id += 1;
        s.write(
            &json!({ "jsonrpc": "2.0", "id": s.init_id, "method": "initialize", "params": params }),
        );
        Ok(s)
    }

    fn write(&mut self, v: &Value) {
        if self.dead {
            return;
        }
        if let Err(e) = self
            .stdin
            .write_all(&frame(v))
            .and_then(|_| self.stdin.flush())
        {
            log::warn!("{}: write failed: {e}", self.config.name);
            self.dead = true;
        }
    }

    fn send(&mut self, v: Value) {
        if self.ready {
            self.write(&v);
        } else {
            self.queued.push(v);
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Send a request; returns its id (the reply comes back as `Incoming::Response`).
    pub fn request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    /// Messages that arrived. Handles the `initialize` reply and server requests itself.
    fn drain(&mut self) -> Vec<Incoming> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(v) => match Incoming::from_json(v) {
                    Some(Incoming::Response { id, result }) if id == self.init_id => {
                        self.initialized(result)
                    }
                    Some(Incoming::Request { id, method, params }) => {
                        // Answer what servers commonly ask; nothing else is supported.
                        let result = match method.as_str() {
                            "workspace/configuration" => {
                                let n = params["items"].as_array().map_or(0, Vec::len);
                                Value::Array(vec![Value::Null; n])
                            }
                            _ => Value::Null,
                        };
                        self.write(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
                    }
                    Some(m) => out.push(m),
                    None => {}
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.dead {
                        log::warn!("{} exited", self.config.name);
                    }
                    self.dead = true;
                    break;
                }
            }
        }
        out
    }

    fn initialized(&mut self, result: Result<Value, String>) {
        match result {
            Ok(mut r) => {
                self.caps = r["capabilities"].take();
                self.utf16 = !matches!(self.caps["positionEncoding"].as_str(), Some("utf-32"));
                self.ready = true;
                self.write(&json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
                for v in std::mem::take(&mut self.queued) {
                    self.write(&v);
                }
            }
            Err(e) => {
                log::error!("{} initialize failed: {e}", self.config.name);
                self.dead = true;
            }
        }
    }

    pub fn did_open(&mut self, path: &Path, text: String, revision: u64) {
        let uri = path_to_uri(path);
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": uri, "languageId": self.config.language_id,
                "version": revision as i64, "text": text,
            } }),
        );
        self.open.insert(path.to_path_buf(), revision);
    }

    pub fn did_change(&mut self, path: &Path, text: String, revision: u64) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": path_to_uri(path), "version": revision as i64 },
                "contentChanges": [{ "text": text }],
            }),
        );
        self.open.insert(path.to_path_buf(), revision);
    }

    pub fn did_save(&mut self, path: &Path, text: String) {
        self.notify(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": path_to_uri(path) }, "text": text }),
        );
    }

    pub fn did_close(&mut self, path: &Path) {
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": path_to_uri(path) } }),
        );
        self.open.remove(path);
    }

    fn shutdown(&mut self) {
        if !self.dead && self.ready {
            self.request("shutdown", Value::Null);
            self.notify("exit", Value::Null);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ------------------------------------------------------------ all servers

#[derive(Default)]
pub struct Lsp {
    pub servers: Vec<Server>,
    /// Starts per (server, root), and when it last failed.
    starts: HashMap<(&'static str, PathBuf), (u32, Option<Instant>)>,
    /// Why the last start failed (shown once).
    pub start_error: Option<String>,
}

impl Lsp {
    /// The running server for `file`, started if needed. `None`: no server serves the file,
    /// or it cannot be started (now).
    pub fn server_for(&mut self, file: &Path, folders: &[PathBuf]) -> Option<usize> {
        let cfg = config::for_path(file)?;
        let root = config::root_for(cfg, file, folders);
        if let Some(i) = self
            .servers
            .iter()
            .position(|s| std::ptr::eq(s.config, cfg) && s.root == root && !s.dead)
        {
            return Some(i);
        }
        let key = (cfg.name, root.clone());
        let (n, failed) = self.starts.get(&key).copied().unwrap_or((0, None));
        if n >= MAX_STARTS || failed.is_some_and(|t| t.elapsed() < RESTART_DELAY) {
            return None;
        }
        match Server::start(cfg, &root) {
            Ok(s) => {
                self.starts.insert(key, (n + 1, None));
                self.servers.push(s);
                Some(self.servers.len() - 1)
            }
            Err(e) => {
                log::warn!("{e}");
                self.starts.insert(key, (MAX_STARTS, Some(Instant::now())));
                self.start_error = Some(e);
                None
            }
        }
    }

    /// The running server that has `file` open.
    pub fn server_with(&self, file: &Path) -> Option<usize> {
        self.servers
            .iter()
            .position(|s| !s.dead && s.open.contains_key(file))
    }

    /// Messages from all servers, by server index. Dead servers are marked for a restart.
    pub fn poll(&mut self) -> Vec<(usize, Incoming)> {
        let mut out = Vec::new();
        for (i, s) in self.servers.iter_mut().enumerate() {
            let was_dead = s.dead;
            out.extend(s.drain().into_iter().map(|m| (i, m)));
            if s.dead && !was_dead {
                let key = (s.config.name, s.root.clone());
                if let Some(e) = self.starts.get_mut(&key) {
                    e.1 = Some(Instant::now());
                }
            }
        }
        out
    }

    pub fn any_running(&self) -> bool {
        self.servers.iter().any(|s| !s.dead)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let a = json!({ "jsonrpc": "2.0", "id": 1, "result": { "x": "é" } });
        let b = json!({ "jsonrpc": "2.0", "method": "m", "params": [] });
        let mut bytes = frame(&a);
        bytes.extend(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n".iter());
        bytes.extend(frame(&b));
        let mut r = std::io::Cursor::new(bytes);
        assert_eq!(read_message(&mut r).unwrap(), Some(a.clone()));
        assert_eq!(read_message(&mut r).unwrap(), Some(b));
        assert_eq!(read_message(&mut r).unwrap(), None);
        assert_eq!(
            Incoming::from_json(a),
            Some(Incoming::Response {
                id: 1,
                result: Ok(json!({ "x": "é" }))
            })
        );
        let err = json!({ "id": 2, "error": { "code": -1, "message": "no" } });
        assert_eq!(
            Incoming::from_json(err),
            Some(Incoming::Response {
                id: 2,
                result: Err("no".into())
            })
        );
        let req = json!({ "id": "a", "method": "window/workDoneProgress/create", "params": {} });
        assert!(matches!(
            Incoming::from_json(req),
            Some(Incoming::Request { .. })
        ));
    }

    #[test]
    fn utf16_positions() {
        let rope = Rope::from_str("a😀b\nxé😀y\n");
        // `b`: char 2, UTF-16 unit 3.
        assert_eq!(
            position(&rope, 2, true),
            json!({ "line": 0, "character": 3 })
        );
        assert_eq!(
            position(&rope, 2, false),
            json!({ "line": 0, "character": 2 })
        );
        let y = rope.line_to_char(1) + 3;
        assert_eq!(
            position(&rope, y, true),
            json!({ "line": 1, "character": 4 })
        );
        assert_eq!(
            char_index(&rope, &json!({ "line": 1, "character": 4 }), true),
            y
        );
        assert_eq!(
            char_index(&rope, &json!({ "line": 0, "character": 3 }), true),
            2
        );
        // Past the end of a line: clamped to it.
        assert!(char_index(&rope, &json!({ "line": 0, "character": 99 }), true) <= 4);
        assert_eq!(char_col("xé😀y", 4, true), 3);
        assert_eq!(char_col("xé😀y", 4, false), 4);
    }

    #[test]
    fn uris_and_markdown() {
        let p = Path::new("/home/a b/x#1.odin");
        let uri = path_to_uri(p);
        assert_eq!(uri, "file:///home/a%20b/x%231.odin");
        assert_eq!(uri_to_path(&uri).unwrap(), p);
        let md = "```odin\nfmt.println :: proc(args: ..any)\n```\n---\nFormats using the default.";
        assert_eq!(
            markdown_lines(md),
            vec![
                (true, "fmt.println :: proc(args: ..any)".into()),
                (false, String::new()),
                (false, "Formats using the default.".into()),
            ]
        );
    }
}
