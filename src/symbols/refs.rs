//! Find All References: the places a name is used in a file. Name based, like the rest of
//! the code navigation, but on tokens: comments and string literals don't count. A member
//! (a method or property) only counts where it is written `x.Name`, unqualified in its own
//! file, or at its declaration, so a same-named local elsewhere is not listed.

use std::path::Path;

use super::{Family, line_col};

/// What to look for.
#[derive(Debug, Clone)]
pub struct RefQuery {
    pub family: Family,
    pub name: String,
    /// The name is a member of a type: unqualified uses only count in `def_file`.
    pub member: bool,
    pub def_file: Option<std::path::PathBuf>,
    /// The declaration's 0-based line (marked in the list).
    pub def_line: Option<usize>,
}

/// One use of the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub line: usize,
    pub col: usize,
    /// The whole source line, trimmed.
    pub text: String,
    /// This is the declaration.
    pub definition: bool,
}

/// The uses of the query's name in one file; `aliases` are other local names for it in
/// this file (`import { name as alias }`).
pub fn find_in(path: &Path, text: &str, q: &RefQuery, aliases: &[String]) -> Vec<Reference> {
    super::with_line_index(|| find_uses(path, text, q, aliases))
}

fn find_uses(path: &Path, text: &str, q: &RefQuery, aliases: &[String]) -> Vec<Reference> {
    let in_def_file = q.def_file.as_deref() == Some(path);
    let wanted = |w: &str| w == q.name || aliases.iter().any(|a| a == w);
    // (byte offset, preceded by `.`)
    let mut hits: Vec<(usize, bool)> = Vec::new();
    match q.family {
        Family::CSharp => {
            use crate::test_explorer::discover_cs::{Tok, tokenize};
            let toks = tokenize(text);
            for (k, t) in toks.iter().enumerate() {
                if let Tok::Ident(w) = &t.tok
                    && wanted(w)
                {
                    let dot = k > 0 && toks[k - 1].tok == Tok::Punct('.');
                    hits.push((t.start, dot));
                }
            }
        }
        Family::Js => {
            use crate::test_explorer::discover_js::{Tok, tokenize};
            let vue = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("vue"));
            let script = if vue {
                super::js::script_only(text)
            } else {
                text.to_string()
            };
            let toks = tokenize(&script);
            for (k, t) in toks.iter().enumerate() {
                if let Tok::Ident(w) = &t.tok
                    && wanted(w)
                {
                    let dot = k > 0 && toks[k - 1].tok == Tok::Punct('.');
                    hits.push((t.start, dot));
                }
            }
            // The template: expressions live in attributes and `{{ }}`, so whole words.
            if vue {
                for (start, end) in template_ranges(text) {
                    for (at, dot) in whole_words(&text[start..end], &q.name).into_iter().chain(
                        aliases
                            .iter()
                            .flat_map(|a| whole_words(&text[start..end], a)),
                    ) {
                        hits.push((start + at, dot));
                    }
                }
            }
        }
    }
    hits.sort();
    hits.dedup();
    let lines: Vec<&str> = text.lines().collect();
    hits.into_iter()
        .filter_map(|(at, dot)| {
            let (line, col) = line_col(text, at);
            let definition = in_def_file && q.def_line == Some(line);
            // A member: `x.Name`, its declaration, or unqualified in its own file.
            if q.member && !dot && !definition && !in_def_file {
                return None;
            }
            Some(Reference {
                line,
                col,
                text: lines.get(line).map_or("", |l| l.trim()).to_string(),
                definition,
            })
        })
        .collect()
}

/// Byte ranges of a `.vue` file outside its `<script>` blocks.
fn template_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(open) = text[at..].find("<script").map(|n| at + n) {
        out.push((at, open));
        at = text[open..]
            .find("</script>")
            .map_or(text.len(), |n| open + n + "</script>".len());
    }
    out.push((at, text.len()));
    out
}

/// Whole-word occurrences of `name` in `s`: (byte offset, preceded by `.`).
fn whole_words(s: &str, name: &str) -> Vec<(usize, bool)> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(k) = s[from..].find(name).map(|n| from + n) {
        let before = s[..k].chars().next_back();
        let after = s[k + name.len()..].chars().next();
        if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
            out.push((k, before == Some('.')));
        }
        from = k + name.len().max(1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn q(family: Family, name: &str, member: bool, def: Option<(&str, usize)>) -> RefQuery {
        RefQuery {
            family,
            name: name.into(),
            member,
            def_file: def.map(|(f, _)| PathBuf::from(f)),
            def_line: def.map(|(_, l)| l),
        }
    }

    fn lines(r: &[Reference]) -> Vec<(usize, usize, bool)> {
        r.iter().map(|r| (r.line, r.col, r.definition)).collect()
    }

    #[test]
    fn js_uses_skip_comments_and_strings_and_follow_aliases() {
        let def = "/** Format. */\nexport function formatCurrency(n) {}\n// formatCurrency in a comment\nconst s = 'formatCurrency';\n";
        let query = q(
            Family::Js,
            "formatCurrency",
            false,
            Some(("/w/format.js", 1)),
        );
        let r = find_in(Path::new("/w/format.js"), def, &query, &[]);
        assert_eq!(lines(&r), [(1, 16, true)]);
        assert_eq!(r[0].text, "export function formatCurrency(n) {}");
        let user = "import { formatCurrency as money } from './format';\nconst a = money(1) + formatCurrencyX;\n";
        let r = find_in(Path::new("/w/app.js"), user, &query, &["money".into()]);
        assert_eq!(lines(&r), [(0, 9, false), (0, 27, false), (1, 10, false)]);
        // A .vue template.
        let vue = "<template>\n  <td>{{ formatCurrency(x) }}</td>\n</template>\n<script setup>\nimport { formatCurrency } from './format';\n</script>\n";
        let r = find_in(Path::new("/w/Card.vue"), vue, &query, &[]);
        assert_eq!(lines(&r), [(1, 9, false), (4, 9, false)]);
    }

    #[test]
    fn members_count_when_qualified_or_in_their_file() {
        let svc =
            "class Svc {\n    public Item Find(int id) => null;\n    void Use() { Find(1); }\n}\n";
        let query = q(Family::CSharp, "Find", true, Some(("/w/Svc.cs", 1)));
        let r = find_in(Path::new("/w/Svc.cs"), svc, &query, &[]);
        assert_eq!(lines(&r), [(1, 16, true), (2, 17, false)]);
        let other = "class Other {\n    void Go(Svc s) { s.Find(2); var Find = 3; }\n}\n";
        let r = find_in(Path::new("/w/Other.cs"), other, &query, &[]);
        assert_eq!(
            lines(&r),
            [(1, 23, false)],
            "the local `Find` is not a reference"
        );
        // Not a member: every use counts.
        let free = q(Family::CSharp, "Find", false, None);
        assert_eq!(
            find_in(Path::new("/w/Other.cs"), other, &free, &[]).len(),
            2
        );
    }
}
