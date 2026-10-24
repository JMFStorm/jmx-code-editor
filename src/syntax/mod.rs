//! Syntax highlighting with small hand-written lexers. Each line is lexed from the state left
//! by the line before it, so a document only keeps one `State` per line start and re-lexes
//! from the first edited line.

mod csharp;
mod css;
mod docs_page;
mod js;
mod json;
mod markup;
mod problems;
mod references;
mod tests_page;

use std::path::Path;

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

use ropey::Rope;

/// Lines longer than this (minified files) are not highlighted.
const MAX_LINE_CHARS: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    Plain,
    Json,
    Xml,
    Html,
    Vue,
    JavaScript,
    Css,
    CSharp,
    /// The Problems page (not a file type).
    Problems,
    /// The Tests page.
    Tests,
    /// The Docs page (.NET API docs).
    Docs,
    /// The References page.
    References,
}

impl Lang {
    pub fn from_path(path: &Path) -> Lang {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
        match ext {
            "json" | "jsonc" | "jdcws" => Lang::Json,
            "xml" | "svg" | "xsd" | "xsl" | "xslt" | "plist" | "csproj" | "props" => Lang::Xml,
            "html" | "htm" | "xhtml" => Lang::Html,
            "vue" => Lang::Vue,
            "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => Lang::JavaScript,
            "css" => Lang::Css,
            "cs" | "csx" => Lang::CSharp,
            _ => Lang::Plain,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Plain => "Plain Text",
            Lang::Json => "JSON",
            Lang::Xml => "XML",
            Lang::Html => "HTML",
            Lang::Vue => "Vue",
            Lang::JavaScript => "JavaScript",
            Lang::Css => "CSS",
            Lang::CSharp => "C#",
            Lang::Problems => "Problems",
            Lang::Docs => "Docs",
            Lang::References => "References",
            Lang::Tests => "Tests",
        }
    }
}

/// What a piece of text is, for coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Comment,
    /// `@param` and friends inside a doc comment.
    DocTag,
    String,
    Number,
    Keyword,
    /// true, false, null, undefined, this.
    Literal,
    Function,
    Property,
    Type,
    Tag,
    Attribute,
    Punct,
    Regex,
    Entity,
    Escape,
    /// Problem severities (Problems page).
    Error,
    Warning,
    /// Test statuses (Tests page).
    Pass,
    Running,
}

/// A colored range of a line, in chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub token: Token,
}

/// Lexer state at a line start. One struct for every language keeps embedding simple
/// (markup carries the JavaScript and CSS states of `<script>` and `<style>`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    pub json: json::State,
    pub js: js::State,
    pub css: css::State,
    pub csharp: csharp::State,
    pub markup: markup::State,
}

/// Highlight one line (without its newline) starting in `state`.
pub fn highlight_line(lang: Lang, state: &State, line: &str) -> (Vec<Span>, State) {
    crate::perf::bump(&crate::perf::LINES_LEXED);
    let mut st = state.clone();
    let chars: Vec<char> = line.chars().collect();
    if chars.len() > MAX_LINE_CHARS {
        return (Vec::new(), st);
    }
    let mut lx = Lexer::new(&chars);
    match lang {
        Lang::Plain => {}
        Lang::Json => json::lex(&mut lx, &mut st.json),
        Lang::JavaScript => js::lex(&mut lx, &mut st.js, chars.len()),
        Lang::Css => css::lex(&mut lx, &mut st.css, chars.len()),
        Lang::CSharp => csharp::lex(&mut lx, &mut st.csharp, chars.len()),
        Lang::Problems => problems::lex(&mut lx),
        Lang::Tests => tests_page::lex(&mut lx),
        Lang::Docs => docs_page::lex(&mut lx),
        Lang::References => references::lex(&mut lx),
        Lang::Xml | Lang::Html | Lang::Vue => markup::lex(&mut lx, &mut st, lang),
    }
    (lx.spans, st)
}

/// Cursor over one line's chars that collects spans.
pub struct Lexer<'a> {
    pub chars: &'a [char],
    pub i: usize,
    pub spans: Vec<Span>,
}

impl<'a> Lexer<'a> {
    fn new(chars: &'a [char]) -> Self {
        Self {
            chars,
            i: 0,
            spans: Vec::new(),
        }
    }

    pub fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    pub fn peek_at(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).copied()
    }

    /// The text at the cursor starts with `s`.
    pub fn at(&self, s: &str) -> bool {
        (self.i..)
            .zip(s.chars())
            .all(|(k, c)| self.chars.get(k) == Some(&c))
    }

    /// First position at or after the cursor (and before `end`) where `s` starts.
    pub fn find(&self, s: &str, end: usize) -> Option<usize> {
        let pat: Vec<char> = s.chars().collect();
        (self.i..end.min(self.chars.len()))
            .find(|&k| self.chars[k..].starts_with(&pat) && k + pat.len() <= end)
    }

    /// Like `find`, ignoring ASCII case.
    pub fn find_ci(&self, s: &str, end: usize) -> Option<usize> {
        let pat: Vec<char> = s.chars().collect();
        (self.i..end.min(self.chars.len())).find(|&k| {
            k + pat.len() <= end
                && self.chars[k..k + pat.len()]
                    .iter()
                    .zip(&pat)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
        })
    }

    pub fn push(&mut self, start: usize, end: usize, token: Token) {
        if end > start {
            self.spans.push(Span { start, end, token });
        }
    }

    /// Advance while `f` holds (but not past `end`).
    pub fn eat_while(&mut self, end: usize, f: impl Fn(char) -> bool) {
        while self.i < end && f(self.chars[self.i]) {
            self.i += 1;
        }
    }

    /// Next non-blank char after position `k`, on this line.
    pub fn next_non_blank(&self, k: usize) -> Option<char> {
        self.chars[k.min(self.chars.len())..]
            .iter()
            .copied()
            .find(|c| !c.is_whitespace())
    }

    /// Last non-blank char before position `k`.
    pub fn prev_non_blank(&self, k: usize) -> Option<char> {
        self.chars[..k.min(self.chars.len())]
            .iter()
            .rev()
            .copied()
            .find(|c| !c.is_whitespace())
    }
}

/// Per-document highlighting: the language and the lexer state at each line start (as far as
/// it has been computed), plus the spans of recently drawn lines.
///
/// After an edit only the edited lines are re-lexed, and lexing stops as soon as a line starts
/// in the same state as before (the rest of the file is unchanged). Lexing far ahead (Ctrl+End
/// in a huge file) is spread over frames by a time budget.
#[derive(Debug, Clone, Default)]
pub struct Syntax {
    pub lang: Lang,
    /// `states[i]` is the state at the start of line `i`. Entries after `valid` are from
    /// before the last edit (shifted to their new lines) and may be out of date.
    states: Vec<State>,
    /// `states[..=valid]` are correct.
    valid: usize,
    /// The pending edits end at this line: past it, a state equal to the old one means the
    /// rest is unchanged. `None`: no such shortcut (several edits at once; re-lex everything).
    dirty_end: Option<usize>,
    /// Line count when the states were last brought up to date (to learn an edit's shift).
    lines: usize,
    /// Spans of recently drawn lines.
    spans: RefCell<HashMap<usize, Vec<Span>>>,
}

/// Spans kept at most (a few screens).
const SPAN_CACHE: usize = 2000;

impl Syntax {
    pub fn new(lang: Lang) -> Self {
        Self {
            lang,
            states: vec![State::default()],
            ..Default::default()
        }
    }

    pub fn set_lang(&mut self, lang: Lang) {
        *self = Self::new(lang);
    }

    /// Text changed from `line` on: states after it must be recomputed.
    pub fn invalidate_from(&mut self, line: usize) {
        self.valid = self
            .valid
            .min(line)
            .min(self.states.len().saturating_sub(1));
        self.dirty_end = None;
        self.spans.borrow_mut().retain(|&l, _| l < line);
    }

    /// One edit replaced lines `first..` of the old text with lines `first..=last` of the new
    /// text, the document now having `lines` lines: keep the states after it (shifted), so
    /// re-lexing can stop where the old and new states agree again.
    pub fn edited(&mut self, first: usize, last: usize, lines: usize) {
        if self.lines == 0 || self.states.len() <= first + 1 {
            self.invalidate_from(first);
            self.lines = lines;
            return;
        }
        let delta = lines as isize - self.lines as isize;
        let old_last = (last as isize - delta).max(first as isize) as usize;
        let from = first + 1;
        let to = (old_last + 1).min(self.states.len()).max(from);
        let fresh = last - first;
        // Another edit still pending: its region moves with this one, and both are dirty.
        let pending = self.valid + 1 < self.states.len();
        self.states
            .splice(from..to, std::iter::repeat_n(State::default(), fresh));
        self.dirty_end = if pending {
            self.dirty_end.map(|e| {
                let e = if e > first {
                    (e as isize + delta).max(first as isize) as usize
                } else {
                    e
                };
                e.max(last)
            })
        } else {
            Some(last)
        };
        self.valid = self.valid.min(first);
        self.lines = lines;
        self.spans.borrow_mut().retain(|&l, _| l < first);
    }

    /// Compute line start states up to `line` (inclusive), stopping at `deadline`. Returns
    /// false when it ran out of time (draw what is known; call again next frame).
    pub fn ensure(&mut self, rope: &Rope, line: usize, deadline: Option<Instant>) -> bool {
        if self.lang == Lang::Plain {
            return true;
        }
        let total = rope.len_lines();
        self.lines = total;
        let last = line.min(total.saturating_sub(1));
        let mut n = 0u32;
        while self.valid < last {
            let l = self.valid;
            let (spans, next) = lex_line(self.lang, &self.states[l], rope, l);
            // Lines just before the viewport are about to be drawn: keep their spans.
            if l + SPAN_CACHE_LINES_AHEAD >= last {
                self.spans.borrow_mut().insert(l, spans);
            }
            if l + 1 < self.states.len() {
                // Past the edit and back in step: the rest is as before.
                if self.dirty_end.is_some_and(|e| l + 1 > e) && self.states[l + 1] == next {
                    self.states.truncate(total.max(1));
                    self.valid = self.states.len() - 1;
                    continue;
                }
                if self.states[l + 1] != next {
                    self.spans.borrow_mut().remove(&(l + 1));
                }
                self.states[l + 1] = next;
            } else {
                self.states.push(next);
            }
            self.valid = l + 1;
            n += 1;
            if n.is_multiple_of(256) && deadline.is_some_and(|d| Instant::now() >= d) {
                return false;
            }
        }
        self.states.truncate(total.max(1));
        true
    }

    /// Spans of `line`, if its start state is known (see `ensure`).
    pub fn line_spans(&self, rope: &Rope, line: usize) -> Option<Vec<Span>> {
        if self.lang == Lang::Plain || line > self.valid {
            return None;
        }
        if let Some(s) = self.spans.borrow().get(&line) {
            return Some(s.clone());
        }
        let state = self.states.get(line)?;
        let spans = lex_line(self.lang, state, rope, line).0;
        let mut cache = self.spans.borrow_mut();
        if cache.len() >= SPAN_CACHE {
            cache.clear();
        }
        cache.insert(line, spans.clone());
        Some(spans)
    }
}

/// Lines before the viewport whose spans are kept while lexing up to it.
const SPAN_CACHE_LINES_AHEAD: usize = 200;

/// Lex line `l` from `state`; lines longer than `MAX_LINE_CHARS` are left plain without
/// copying them out of the rope.
fn lex_line(lang: Lang, state: &State, rope: &Rope, l: usize) -> (Vec<Span>, State) {
    if rope.line(l).len_chars() > MAX_LINE_CHARS + 2 {
        return (Vec::new(), state.clone());
    }
    highlight_line(lang, state, &line_text(rope, l))
}

fn line_text(rope: &Rope, line: usize) -> String {
    let mut s: String = rope.line(line).into();
    while s.ends_with(['\n', '\r']) {
        s.pop();
    }
    s
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;

    /// `(text, token)` pairs of the colored pieces of each line.
    pub fn tokens(lang: Lang, text: &str) -> Vec<Vec<(String, Token)>> {
        let mut st = State::default();
        text.lines()
            .map(|line| {
                let (spans, next) = highlight_line(lang, &st, line);
                st = next;
                let chars: Vec<char> = line.chars().collect();
                spans
                    .iter()
                    .map(|s| (chars[s.start..s.end].iter().collect(), s.token))
                    .collect()
            })
            .collect()
    }

    /// The token of the first piece equal to `piece` on any line.
    pub fn token_of(lang: Lang, text: &str, piece: &str) -> Option<Token> {
        tokens(lang, text)
            .into_iter()
            .flatten()
            .find(|(t, _)| t == piece)
            .map(|(_, k)| k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Incremental highlighting after edits (states kept and shifted, re-lexing stopped where
    /// old and new states agree, budgeted) must equal a full re-lex, for random edits that
    /// add and remove lines and open and close comments and strings.
    #[test]
    fn incremental_states_match_a_full_relex() {
        let base = "function a(x) {\n  /* c */ const s = `t ${x}`;\n  return 'q';\n}\n// end\n";
        let mut rope = Rope::from_str(&base.repeat(40));
        let mut syn = Syntax::new(Lang::JavaScript);
        let lines = rope.len_lines();
        assert!(syn.ensure(&rope, lines, None));
        let pieces = ["/*", "*/", "\n", "`", "'", "x", "\n\n", "// c\n", "${", "}"];
        let mut seed = 7u64;
        let mut rnd = |n: usize| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize % n
        };
        for round in 0..300 {
            let len = rope.len_chars();
            let pos = rnd(len.max(1));
            let (first, last) = if round % 3 == 0 && len > 10 {
                // A deletion (possibly over line breaks).
                let end = (pos + rnd(12)).min(len);
                rope.remove(pos..end);
                let l = rope.char_to_line(pos.min(rope.len_chars()));
                (l, l)
            } else {
                let text = pieces[rnd(pieces.len())];
                rope.insert(pos, text);
                let f = rope.char_to_line(pos);
                (f, rope.char_to_line(pos + text.chars().count()).max(f))
            };
            syn.edited(first, last, rope.len_lines());
            // Sometimes several edits before the next update; sometimes a budgeted update.
            if round % 4 != 1 {
                while !syn.ensure(&rope, rope.len_lines(), Some(Instant::now())) {}
            }
            if round % 4 == 3 {
                let mut full = Syntax::new(Lang::JavaScript);
                full.ensure(&rope, rope.len_lines(), None);
                let n = rope.len_lines();
                assert_eq!(syn.states[..n], full.states[..n], "round {round}");
                for l in 0..n {
                    assert_eq!(
                        syn.line_spans(&rope, l),
                        full.line_spans(&rope, l),
                        "round {round} line {l}"
                    );
                }
            }
        }
    }

    #[test]
    fn long_lines_are_not_materialized() {
        let rope = Rope::from_str(&format!("{}\nlet a = 1;\n", "x".repeat(MAX_LINE_CHARS * 3)));
        let mut syn = Syntax::new(Lang::JavaScript);
        assert!(syn.ensure(&rope, 2, None));
        assert_eq!(syn.line_spans(&rope, 0), Some(Vec::new()));
        assert!(!syn.line_spans(&rope, 1).unwrap().is_empty());
    }

    #[test]
    fn languages_from_paths() {
        assert_eq!(Lang::from_path(Path::new("a/App.vue")), Lang::Vue);
        assert_eq!(Lang::from_path(Path::new("x.d.ts")), Lang::JavaScript);
        assert_eq!(Lang::from_path(Path::new("p.JSON")), Lang::Json);
        assert_eq!(Lang::from_path(Path::new("logo.svg")), Lang::Xml);
        assert_eq!(Lang::from_path(Path::new("Makefile")), Lang::Plain);
    }

    /// Every prefix of tricky input, in every language: lexers must always move forward and
    /// never index out of range (a hang here would freeze the editor).
    #[test]
    fn lexers_never_hang_or_panic() {
        let sample = "<a b='c\"d' :e=\"f(`g${h}`)\" @i>{{ j / k /l/g }}</a>&amp; <!-- x --> \
                      <![CDATA[ y ]]> <?p?> <!D> /* c */ // d\n\"s\\\": -1.5e+3, .5, 0x1F, @x \
                      a { b: #fff !important; c:hover { } } @media (x: 1) { } `t ${ { } } u` \
                      /[/]\\//gi; 'q\\'; </script> </style> é日本 \t \\ \
                      @\"v\"\"w\" $\"i {j} {{k}}\" $@\"x{y}\" \"\"\"r\"\"\" $\"\"\"{z}\"\"\" /// <s> #region";
        let langs = [
            Lang::Json,
            Lang::Xml,
            Lang::Html,
            Lang::Vue,
            Lang::JavaScript,
            Lang::Css,
            Lang::CSharp,
            Lang::Problems,
            Lang::Tests,
            Lang::Docs,
            Lang::References,
        ];
        let chars: Vec<char> = sample.chars().collect();
        for lang in langs {
            let mut st = State::default();
            for n in 0..=chars.len() {
                let line: String = chars[..n].iter().collect();
                let (spans, next) = highlight_line(lang, &st, &line);
                for s in &spans {
                    assert!(s.start < s.end && s.end <= n, "{lang:?} {s:?} in {line:?}");
                }
                // Also carry the state on, as consecutive lines would.
                if n % 7 == 0 {
                    st = next;
                }
            }
        }
    }

    #[test]
    fn states_are_cached_and_invalidated() {
        let mut rope = Rope::from_str("/* a\nb */ x\nlet y = 1;\n");
        let mut syn = Syntax::new(Lang::JavaScript);
        syn.ensure(&rope, 10, None);
        assert_eq!(syn.states.len(), 4);
        let spans = syn.line_spans(&rope, 1).unwrap();
        assert_eq!(spans[0].token, Token::Comment); // still inside the comment
        // Remove the comment start: line 1 is code again after invalidation.
        rope.remove(0..2);
        syn.invalidate_from(0);
        assert_eq!(syn.valid, 0);
        assert_eq!(
            syn.line_spans(&rope, 1),
            None,
            "not known until lexed again"
        );
        syn.ensure(&rope, 2, None);
        let line1 = syn.line_spans(&rope, 1).unwrap();
        assert!(
            !line1
                .iter()
                .any(|s| s.start == 0 && s.token == Token::Comment)
        );
        let spans = syn.line_spans(&rope, 2).unwrap();
        assert_eq!(spans[0].token, Token::Keyword);
    }
}
