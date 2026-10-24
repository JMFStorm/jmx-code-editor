//! Syntax highlighting with small hand-written lexers. Each line is lexed from the state left
//! by the line before it, so a document only keeps one `State` per line start and re-lexes
//! from the first edited line.

mod csharp;
mod css;
mod js;
mod json;
mod markup;
mod problems;
mod tests_page;

use std::path::Path;

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
}

impl Lang {
    pub fn from_path(path: &Path) -> Lang {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
        match ext {
            "json" | "jsonc" | "jmxws" => Lang::Json,
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
/// it has been computed).
#[derive(Debug, Clone, Default)]
pub struct Syntax {
    pub lang: Lang,
    /// `states[i]` is the state at the start of line `i`; always holds at least line 0.
    states: Vec<State>,
}

impl Syntax {
    pub fn new(lang: Lang) -> Self {
        Self {
            lang,
            states: vec![State::default()],
        }
    }

    pub fn set_lang(&mut self, lang: Lang) {
        *self = Self::new(lang);
    }

    /// Text changed on `line`: states after it must be recomputed.
    pub fn invalidate_from(&mut self, line: usize) {
        self.states.truncate(line + 1);
        if self.states.is_empty() {
            self.states.push(State::default());
        }
    }

    /// Compute line start states up to `line` (inclusive).
    pub fn ensure(&mut self, rope: &Rope, line: usize) {
        if self.lang == Lang::Plain {
            return;
        }
        let last = line.min(rope.len_lines().saturating_sub(1));
        while self.states.len() <= last {
            let l = self.states.len() - 1;
            let text = line_text(rope, l);
            let (_, next) = highlight_line(self.lang, &self.states[l], &text);
            self.states.push(next);
        }
    }

    /// Spans of `line`, if its start state is known (see `ensure`).
    pub fn line_spans(&self, rope: &Rope, line: usize) -> Option<Vec<Span>> {
        if self.lang == Lang::Plain {
            return None;
        }
        let state = self.states.get(line)?;
        Some(highlight_line(self.lang, state, &line_text(rope, line)).0)
    }
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
        syn.ensure(&rope, 10);
        assert_eq!(syn.states.len(), 4);
        let spans = syn.line_spans(&rope, 1).unwrap();
        assert_eq!(spans[0].token, Token::Comment); // still inside the comment
        // Remove the comment start: line 1 is code again after invalidation.
        rope.remove(0..2);
        syn.invalidate_from(0);
        assert_eq!(syn.states.len(), 1);
        syn.ensure(&rope, 2);
        let spans = syn.line_spans(&rope, 2).unwrap();
        assert_eq!(spans[0].token, Token::Keyword);
    }
}
