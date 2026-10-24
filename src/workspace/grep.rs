//! Find in Files: a grep over the workspace folders on a background thread. Results stream back
//! over a channel, one message per file with matches.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use ignore::WalkState;
use ignore::overrides::{Override, OverrideBuilder};

use super::files::{rel_path, walker};
use crate::editor::search::Query;

/// Matches reported at most; the search stops once this many are found.
pub const MAX_MATCHES: usize = 20_000;
/// Files larger than this are skipped.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Preview text is cut to about this many chars around the first match.
const PREVIEW_CHARS: usize = 300;

/// One matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// 0-based line.
    pub line: usize,
    /// Char range of the first match in the line.
    pub start: usize,
    pub end: usize,
    /// The line, trimmed and with tabs shown as spaces.
    pub preview: String,
    /// Char ranges of all matches within `preview`.
    pub preview_ranges: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResult {
    pub abs: PathBuf,
    /// Path shown in the results.
    pub rel: String,
    pub hits: Vec<Hit>,
}

pub enum SearchMsg {
    File(FileResult),
    Done { capped: bool },
}

pub struct SearchRequest {
    pub roots: Vec<PathBuf>,
    pub respect_gitignore: bool,
    /// Comma-separated globs, e.g. `*.js, src/**, !*.spec.js`.
    pub include: String,
    pub query: Query,
    /// Text of documents with unsaved edits, searched instead of the file on disk.
    pub unsaved: HashMap<PathBuf, String>,
}

/// A running search. Dropping it cancels the search.
pub struct SearchJob {
    rx: Receiver<SearchMsg>,
    cancel: Arc<AtomicBool>,
    files_searched: Arc<AtomicUsize>,
    pub started: Instant,
    /// Set when the job finished.
    pub elapsed: Option<Duration>,
}

impl Drop for SearchJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl SearchJob {
    /// Start searching. Fails on a bad include glob.
    pub fn spawn(req: SearchRequest) -> Result<Self, String> {
        let overrides = req
            .roots
            .iter()
            .map(|r| include_overrides(r, &req.include))
            .collect::<Result<Vec<_>, _>>()?;
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let files_searched = Arc::new(AtomicUsize::new(0));
        let job = Self {
            rx,
            cancel: cancel.clone(),
            files_searched: files_searched.clone(),
            started: Instant::now(),
            elapsed: None,
        };
        std::thread::Builder::new()
            .name("jdc-grep".into())
            .spawn(move || run(req, overrides, tx, cancel, files_searched))
            .map_err(|e| format!("Cannot start search: {e}"))?;
        Ok(job)
    }

    pub fn files_searched(&self) -> usize {
        self.files_searched.load(Ordering::Relaxed)
    }

    pub fn running(&self) -> bool {
        self.elapsed.is_none()
    }

    /// Messages received so far (non-blocking).
    pub fn drain(&mut self) -> Vec<SearchMsg> {
        let msgs: Vec<SearchMsg> = self.rx.try_iter().collect();
        if msgs.iter().any(|m| matches!(m, SearchMsg::Done { .. })) {
            self.elapsed = Some(self.started.elapsed());
        }
        msgs
    }
}

/// Glob filter for one root. A bare extension (`.vue`) means `*.vue`, and a bare name without
/// a dot (`src`) means a folder of that name anywhere.
fn include_overrides(root: &Path, include: &str) -> Result<Override, String> {
    let mut b = OverrideBuilder::new(root);
    for raw in include.split(',').map(str::trim).filter(|g| !g.is_empty()) {
        let (neg, g) = match raw.strip_prefix('!') {
            Some(g) => ("!", g),
            None => ("", raw),
        };
        let plain = !g.contains(['*', '?', '[', '/', '{']);
        let glob = if plain && g.starts_with('.') {
            format!("*{g}")
        } else if plain && !g.contains('.') {
            format!("**/{g}/**")
        } else {
            g.to_string()
        };
        b.add(&format!("{neg}{glob}"))
            .map_err(|e| format!("Bad file filter \"{raw}\": {e}"))?;
    }
    b.build().map_err(|e| format!("Bad file filter: {e}"))
}

fn run(
    req: SearchRequest,
    overrides: Vec<Override>,
    tx: Sender<SearchMsg>,
    cancel: Arc<AtomicBool>,
    files_searched: Arc<AtomicUsize>,
) {
    let started = Instant::now();
    let matches = Arc::new(AtomicUsize::new(0));
    let capped = Arc::new(AtomicBool::new(false));
    let prefix = req.roots.len() > 1;
    let req = Arc::new(req);
    for (root, ov) in req.roots.iter().zip(overrides) {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        walker(root, req.respect_gitignore)
            .overrides(ov)
            .build_parallel()
            .run(|| {
                let tx = tx.clone();
                let (req, cancel, files_searched, matches, capped) = (
                    req.clone(),
                    cancel.clone(),
                    files_searched.clone(),
                    matches.clone(),
                    capped.clone(),
                );
                Box::new(move |entry| {
                    if cancel.load(Ordering::Relaxed) || capped.load(Ordering::Relaxed) {
                        return WalkState::Quit;
                    }
                    let Ok(entry) = entry else {
                        return WalkState::Continue;
                    };
                    if !entry.file_type().is_some_and(|t| t.is_file()) {
                        return WalkState::Continue;
                    }
                    let path = entry.path();
                    let Some(text) = file_text(path, &req.unsaved) else {
                        return WalkState::Continue;
                    };
                    files_searched.fetch_add(1, Ordering::Relaxed);
                    let hits = search_text(&req.query, &text);
                    if hits.is_empty() {
                        return WalkState::Continue;
                    }
                    let total = matches.fetch_add(hits.len(), Ordering::Relaxed) + hits.len();
                    if total >= MAX_MATCHES {
                        capped.store(true, Ordering::Relaxed);
                    }
                    let result = FileResult {
                        rel: rel_path(root, path, prefix),
                        abs: path.to_path_buf(),
                        hits,
                    };
                    if tx.send(SearchMsg::File(result)).is_err() {
                        return WalkState::Quit;
                    }
                    WalkState::Continue
                })
            });
    }
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let capped = capped.load(Ordering::Relaxed);
    log::debug!(
        "find in files: {} files, {} matches{} in {:?}",
        files_searched.load(Ordering::Relaxed),
        matches.load(Ordering::Relaxed),
        if capped { " (capped)" } else { "" },
        started.elapsed()
    );
    let _ = tx.send(SearchMsg::Done { capped });
}

/// Text to search for `path`: unsaved edits if any, else the file unless it is too big or binary.
fn file_text(path: &Path, unsaved: &HashMap<PathBuf, String>) -> Option<String> {
    if let Some(text) = unsaved.get(path) {
        return Some(text.clone());
    }
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    Some(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

/// Every line of `text` with a match.
/// Hits kept from one file (a log where every line matches would otherwise flood the list
/// and memory before the global cap is checked).
pub const MAX_HITS_PER_FILE: usize = 1000;

pub fn search_text(query: &Query, text: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if hits.len() >= MAX_HITS_PER_FILE {
            break;
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        let ranges = query.matches_in(line);
        if let Some(&(start, end)) = ranges.first() {
            let (preview, preview_ranges) = preview(line, &ranges);
            hits.push(Hit {
                line: i,
                start,
                end,
                preview,
                preview_ranges,
            });
        }
    }
    hits
}

/// The line without leading blanks, tabs shown as one space, cut around the first match when
/// long. Ranges are moved to preview coordinates (one char per char, so they stay valid).
fn preview(line: &str, ranges: &[(usize, usize)]) -> (String, Vec<(usize, usize)>) {
    let chars: Vec<char> = line.chars().collect();
    let indent = chars.iter().take_while(|c| c.is_whitespace()).count();
    let first = ranges[0].0;
    let mut start = indent.min(first);
    let mut lead = "";
    if chars.len() - start > PREVIEW_CHARS && first > start + 40 {
        start = first - 40;
        lead = "…";
    }
    let end = chars.len().min(start + PREVIEW_CHARS);
    let mut text: String = lead.to_string();
    text.extend(
        chars[start..end]
            .iter()
            .map(|&c| if c == '\t' { ' ' } else { c }),
    );
    if end < chars.len() {
        text.push('…');
    }
    let shift = lead.chars().count();
    let ranges = ranges
        .iter()
        .filter(|&&(s, _)| s >= start && s < end)
        .map(|&(s, e)| (s - start + shift, e.min(end) - start + shift))
        .collect();
    (text, ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::search::SearchOptions;
    use std::fs;

    fn query(text: &str, opts: SearchOptions) -> Query {
        Query::new(text, opts).unwrap().unwrap()
    }

    fn wait(mut job: SearchJob) -> (Vec<FileResult>, bool) {
        let mut files = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            for m in job.drain() {
                match m {
                    SearchMsg::File(f) => files.push(f),
                    SearchMsg::Done { capped } => {
                        files.sort_by(|a, b| a.rel.cmp(&b.rel));
                        return (files, capped);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("search did not finish");
    }

    #[test]
    fn preview_trims_and_maps_ranges() {
        let q = query("foo", SearchOptions::default());
        let hits = search_text(&q, "a\n\t\tlet foo = foo;\r\n");
        assert_eq!(hits.len(), 1);
        let h = &hits[0];
        assert_eq!((h.line, h.start, h.end), (1, 6, 9));
        assert_eq!(h.preview, "let foo = foo;");
        assert_eq!(h.preview_ranges, [(4, 7), (10, 13)]);

        let long = format!("{}foo{}", "x".repeat(500), "y".repeat(500));
        let h = &search_text(&q, &long)[0];
        assert!(h.preview.starts_with('…') && h.preview.ends_with('…'));
        let (s, e) = h.preview_ranges[0];
        assert_eq!(
            h.preview.chars().skip(s).take(e - s).collect::<String>(),
            "foo"
        );
    }

    #[test]
    fn searches_tree_with_filters_and_unsaved_text() {
        let dir = std::env::temp_dir().join(format!("jdc-grep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("node_modules/x")).unwrap();
        fs::write(dir.join(".gitignore"), "node_modules/\n").unwrap();
        fs::write(dir.join("src/a.js"), "const Needle = 1;\nneedle()\n").unwrap();
        fs::write(dir.join("src/a.spec.js"), "needle\n").unwrap();
        fs::write(dir.join("src/b.vue"), "<p>needle</p>\n").unwrap();
        fs::write(dir.join("node_modules/x/i.js"), "needle\n").unwrap();
        fs::write(dir.join("bin.dat"), b"needle\0\0").unwrap();
        let dir = fs::canonicalize(dir).unwrap();

        let req =
            |include: &str, opts: SearchOptions, unsaved: HashMap<PathBuf, String>| SearchRequest {
                roots: vec![dir.clone()],
                respect_gitignore: true,
                include: include.into(),
                query: query("needle", opts),
                unsaved,
            };
        let rels = |files: &[FileResult]| files.iter().map(|f| f.rel.clone()).collect::<Vec<_>>();

        let (files, capped) =
            wait(SearchJob::spawn(req("", Default::default(), HashMap::new())).unwrap());
        assert!(!capped);
        assert_eq!(rels(&files), ["src/a.js", "src/a.spec.js", "src/b.vue"]);
        assert_eq!(files[0].hits.len(), 2);

        let cs = SearchOptions {
            case_sensitive: true,
            ..Default::default()
        };
        let (files, _) =
            wait(SearchJob::spawn(req("*.js, !*.spec.js", cs, HashMap::new())).unwrap());
        assert_eq!(rels(&files), ["src/a.js"]);
        assert_eq!(files[0].hits[0].line, 1);

        let (files, _) =
            wait(SearchJob::spawn(req(".vue", Default::default(), HashMap::new())).unwrap());
        assert_eq!(rels(&files), ["src/b.vue"]);

        // Unsaved edits replace the disk text.
        let unsaved = HashMap::from([(dir.join("src/b.vue"), "nothing here\n".to_string())]);
        let (files, _) = wait(SearchJob::spawn(req("src", Default::default(), unsaved)).unwrap());
        assert_eq!(rels(&files), ["src/a.js", "src/a.spec.js"]);

        assert!(SearchJob::spawn(req("[", Default::default(), HashMap::new())).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
