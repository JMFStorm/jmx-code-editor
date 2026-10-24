//! Declarations in C#: types (class, record, struct, interface, enum), methods,
//! constructors and properties, with their `///` XML doc comments.

use std::sync::LazyLock;

use regex::Regex;

use super::{Doc, DocParam, FileSymbols, Kind, Symbol, line_col, one_line, split_params};
use crate::test_explorer::discover_cs::{Tok, Token, matching, tokenize};

/// Words before a member name that are not its return type's end.
const NOT_TYPE_END: &[&str] = &[
    "new", "return", "await", "throw", "in", "is", "as", "case", "else", "yield", "using",
    "nameof", "typeof", "sizeof", "default", "when", "out", "ref",
];

const STATEMENTS: &[&str] = &[
    "if",
    "for",
    "foreach",
    "while",
    "switch",
    "catch",
    "using",
    "lock",
    "return",
    "nameof",
    "typeof",
    "sizeof",
    "base",
    "this",
    "fixed",
    "when",
    "default",
    "operator",
    "checked",
    "unchecked",
];

#[derive(Debug, Clone, PartialEq)]
enum Ctx {
    Namespace,
    Type(String),
    Block,
}

pub fn scan(src: &str) -> FileSymbols {
    let toks = tokenize(src);
    let mut out = FileSymbols::default();
    let mut stack: Vec<Ctx> = Vec::new();
    let mut pending_type: Option<String> = None;
    let mut pending_ns = false;
    let ident = |k: usize| match toks.get(k).map(|t| &t.tok) {
        Some(Tok::Ident(s)) => Some(s.as_str()),
        _ => None,
    };
    let punct =
        |k: usize, c: char| matches!(toks.get(k).map(|t| &t.tok), Some(Tok::Punct(p)) if *p == c);
    for k in 0..toks.len() {
        let in_type = match stack.last() {
            Some(Ctx::Type(n)) => Some(n.clone()),
            _ => None,
        };
        let container = stack.iter().rev().find_map(|c| match c {
            Ctx::Type(n) => Some(n.clone()),
            _ => None,
        });
        match &toks[k].tok {
            Tok::Punct('{') => {
                let ctx = if let Some(n) = pending_type.take() {
                    Ctx::Type(n)
                } else if std::mem::take(&mut pending_ns) {
                    Ctx::Namespace
                } else {
                    Ctx::Block
                };
                stack.push(ctx);
            }
            Tok::Punct('}') => {
                stack.pop();
            }
            Tok::Punct(';') => {
                pending_type = None;
                pending_ns = false;
            }
            Tok::Ident(w) if w == "namespace" => pending_ns = true,
            Tok::Ident(w)
                if matches!(
                    w.as_str(),
                    "class" | "struct" | "interface" | "enum" | "record"
                ) && !stack.contains(&Ctx::Block) =>
            {
                // `record class X` / `record struct X`: the name is one further.
                let mut n = k + 1;
                if w == "record" && matches!(ident(n), Some("class" | "struct")) {
                    n += 1;
                }
                if w != "record" && ident(k.wrapping_sub(1)) == Some("record") {
                    continue;
                }
                let Some(name) = ident(n) else {
                    continue;
                };
                let kind = match w.as_str() {
                    "interface" => Kind::Interface,
                    "enum" => Kind::Enum,
                    _ => Kind::Class,
                };
                let start = member_start(&toks, k);
                // Up to the body (a record's primary constructor included).
                let mut end = n + 1;
                let mut params = Vec::new();
                while end < toks.len()
                    && !punct(end, '{')
                    && !punct(end, ';')
                    && ident(end) != Some("where")
                {
                    if punct(end, '(') {
                        let close = matching(&toks, end);
                        params = split_params(&src[toks[end].end..toks[close].start]);
                        end = close;
                    }
                    end += 1;
                }
                let end_byte = toks.get(end).map_or(src.len(), |t| t.start);
                let (line, col) = line_col(src, toks[n].start);
                out.symbols.push(Symbol {
                    name: name.to_string(),
                    kind,
                    container: container.clone(),
                    signature: one_line(&src[toks[start].start..end_byte]),
                    params,
                    doc: doc_before(src, &toks, start),
                    line,
                    col,
                });
                pending_type = Some(name.to_string());
            }
            Tok::Ident(name) if in_type.is_some() && !STATEMENTS.contains(&name.as_str()) => {
                let tname = in_type.unwrap_or_default();
                let prev_ok = k > 0
                    && match &toks[k - 1].tok {
                        Tok::Ident(p) => !NOT_TYPE_END.contains(&p.as_str()),
                        // `List<T> Name(`, but not `=> Name(`.
                        Tok::Punct('>') => !punct(k - 2, '='),
                        Tok::Punct(']') => true,
                        // Nullable `Product? Name(` touches its type; a ternary `?` doesn't.
                        Tok::Punct('?') => k >= 2 && toks[k - 2].end == toks[k - 1].start,
                        _ => false,
                    };
                let is_ctor = name == &tname;
                // Generic method: `Name<T>(`.
                let mut open = k + 1;
                if punct(open, '<') {
                    let mut depth = 0;
                    while open < toks.len().min(k + 40) {
                        if punct(open, '<') {
                            depth += 1;
                        } else if punct(open, '>') {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        open += 1;
                    }
                    open += 1;
                }
                let start = member_start(&toks, k);
                if punct(open, '(') && (prev_ok || is_ctor) {
                    let close = matching(&toks, open);
                    // A declaration has a body, `=>`, `;`, `: base(…)` or `where` next.
                    let next_ok = punct(close + 1, '{')
                        || punct(close + 1, ';')
                        || punct(close + 1, ':')
                        || punct(close + 1, '=') && punct(close + 2, '>')
                        || ident(close + 1) == Some("where");
                    if !next_ok {
                        continue;
                    }
                    let (line, col) = line_col(src, toks[k].start);
                    let params = split_params(&src[toks[open].end..toks[close].start]);
                    out.symbols.push(Symbol {
                        name: name.clone(),
                        kind: if is_ctor {
                            Kind::Constructor
                        } else {
                            Kind::Method
                        },
                        container: Some(tname),
                        signature: one_line(&src[toks[start].start..toks[close].end]),
                        params,
                        doc: doc_before(src, &toks, start),
                        line,
                        col,
                    });
                } else if prev_ok {
                    // Properties `{ get; set; }` / `=> …`, and documented fields.
                    let accessor = punct(k + 1, '{')
                        && (matches!(
                            ident(k + 2),
                            Some(
                                "get"
                                    | "set"
                                    | "init"
                                    | "private"
                                    | "protected"
                                    | "internal"
                                    | "public"
                            )
                        ) || punct(k + 2, '['));
                    let expr = punct(k + 1, '=') && punct(k + 2, '>');
                    let doc = doc_before(src, &toks, start);
                    let field = (punct(k + 1, ';') || punct(k + 1, '=')) && !doc.is_empty();
                    if accessor || expr || field {
                        let (line, col) = line_col(src, toks[k].start);
                        out.symbols.push(Symbol {
                            name: name.clone(),
                            kind: Kind::Property,
                            container: Some(tname),
                            signature: one_line(&src[toks[start].start..toks[k].end]),
                            params: vec![],
                            doc,
                            line,
                            col,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The first token of the member whose name (or keyword) is at `k`: back over modifiers and
/// the type (`static Task<List<int>>? Name`), not over attributes.
fn member_start(toks: &[Token], k: usize) -> usize {
    let mut s = k;
    while s > 0 {
        let ok = match &toks[s - 1].tok {
            Tok::Ident(_) => true,
            Tok::Punct('<' | '>' | '?' | '.' | ',') => true,
            // `int[]`
            Tok::Punct(']') => s >= 2 && toks[s - 2].tok == Tok::Punct('['),
            Tok::Punct('[') => toks[s].tok == Tok::Punct(']'),
            _ => false,
        };
        if !ok {
            break;
        }
        s -= 1;
    }
    // A `,` or `>` can't start a member: step forward to the first identifier.
    while s < k && !matches!(toks[s].tok, Tok::Ident(_)) {
        s += 1;
    }
    s
}

/// The `///` comment lines right before the member starting at token `start` (attributes in
/// between are skipped).
fn doc_before(src: &str, toks: &[Token], start: usize) -> Doc {
    let mut s = start;
    // Back over `[Attribute(…)]` groups.
    while s > 0 && toks[s - 1].tok == Tok::Punct(']') {
        let mut depth = 0;
        let mut j = s - 1;
        loop {
            match toks[j].tok {
                Tok::Punct(']') => depth += 1,
                Tok::Punct('[') => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            if j == 0 {
                return Doc::default();
            }
            j -= 1;
        }
        s = j;
    }
    let from = if s == 0 { 0 } else { toks[s - 1].end };
    let gap = &src[from..toks[s].start];
    // The last run of `///` lines, with nothing but whitespace after it.
    let mut run: Vec<&str> = Vec::new();
    for l in gap.lines().map(str::trim).rev() {
        if l.is_empty() && run.is_empty() {
            continue;
        }
        match l.strip_prefix("///") {
            Some(text) => run.push(text),
            None => break,
        }
    }
    if run.is_empty() {
        return Doc::default();
    }
    run.reverse();
    parse_xml_doc(&run.join("\n"))
}

static SUMMARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<summary>(.*?)</summary>").unwrap());
static PARAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<param\s+name\s*=\s*"([^"]*)"\s*>(.*?)</param>"#).unwrap());
static RETURNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<returns>(.*?)</returns>").unwrap());
static REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<(?:see|seealso)\s+(?:cref|langword|href)\s*=\s*"([^"]*)"\s*/>|<(?:paramref|typeparamref)\s+name\s*=\s*"([^"]*)"\s*/>"#).unwrap()
});
static REMARKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<remarks>(.*?)</remarks>").unwrap());
static EXCEPTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<exception\s+cref\s*=\s*"([^"]*)"\s*>(.*?)</exception>"#).unwrap()
});
static TYPEPARAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<typeparam\s+name\s*=\s*"([^"]*)""#).unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());

/// `<summary>`, `<param name="x">`, `<returns>`, `<remarks>`, `<exception>` and
/// `<typeparam>` of an XML doc comment, as plain text.
pub fn parse_xml_doc(xml: &str) -> Doc {
    let text = |s: &str| -> String {
        let s = REF.replace_all(s, |c: &regex::Captures| {
            let r = c.get(1).or(c.get(2)).map_or("", |m| m.as_str());
            // `T:Ns.Type` / `M:Ns.Type.Method(int)` → `Type` / `Method`.
            let r = r.split_once(':').map_or(r, |(_, b)| b);
            let r = r.split('(').next().unwrap_or(r);
            r.rsplit('.').next().unwrap_or(r).to_string()
        });
        let s = TAG.replace_all(&s, "");
        one_line(
            &s.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&amp;", "&"),
        )
    };
    let summary = match SUMMARY.captures(xml) {
        Some(c) => text(&c[1]),
        // Plain `/// text` without tags.
        None if !xml.contains('<') => text(xml),
        None => String::new(),
    };
    Doc {
        summary,
        params: PARAM
            .captures_iter(xml)
            .map(|c| DocParam {
                name: c[1].to_string(),
                ty: String::new(),
                text: text(&c[2]),
            })
            .collect(),
        returns: RETURNS.captures(xml).map(|c| (String::new(), text(&c[1]))),
        remarks: REMARKS
            .captures(xml)
            .map(|c| text(&c[1]))
            .unwrap_or_default(),
        exceptions: EXCEPTION
            .captures_iter(xml)
            .map(|c| {
                let ty = c[1].split_once(':').map_or(&c[1], |(_, b)| b);
                let ty = ty.rsplit('.').next().unwrap_or(ty).to_string();
                (ty, text(&c[2]))
            })
            .collect(),
        typeparams: TYPEPARAM
            .captures_iter(xml)
            .map(|c| c[1].to_string())
            .collect(),
        ty: String::new(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_members_and_xml_docs() {
        let src = r#"
namespace Inventory.Api.Services;

/// <summary>
/// Stock rules for <see cref="T:Inventory.Api.Models.Product"/> items.
/// </summary>
public sealed class ProductService : IProductService
{
    private readonly AppDb _db;
    private Foo _f = new Foo(1);

    /// <summary>Make one.</summary>
    public ProductService(AppDb db) : base() { _db = db; }

    /// <summary>Finds a product.</summary>
    /// <param name="id">The <paramref name="id"/> to look for.</param>
    /// <returns>The product, or <see langword="null"/>.</returns>
    [HttpGet("{id}")]
    public async Task<Product?> FindAsync(int id, CancellationToken ct = default)
    {
        if (id < 0) { return null; }
        var x = Compute(id);
        return await _db.Products.FindAsync(id);
    }

    public IReadOnlyList<int> Ids { get; private set; } = [];
    public int Count => Ids.Count;
    public IActionResult Delete(int id) =>
        Ok(id) ? NoContent() : NotFound();
    /// <summary>Upper bound.</summary>
    public const int Max = 5;
    private static T Pick<T>(T a) where T : class => a;
    int Compute(int v) => v * 2;
}

public record Money(decimal Amount, string Currency);
public interface IProductService { Task<Product?> FindAsync(int id, CancellationToken ct); }
public enum Level { Low, High }
"#;
        let f = scan(src);
        let got: Vec<(&str, Kind, Option<&str>)> = f
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.container.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("ProductService", Kind::Class, None),
                ("ProductService", Kind::Constructor, Some("ProductService")),
                ("FindAsync", Kind::Method, Some("ProductService")),
                ("Ids", Kind::Property, Some("ProductService")),
                ("Count", Kind::Property, Some("ProductService")),
                ("Delete", Kind::Method, Some("ProductService")),
                ("Max", Kind::Property, Some("ProductService")),
                ("Pick", Kind::Method, Some("ProductService")),
                ("Compute", Kind::Method, Some("ProductService")),
                ("Money", Kind::Class, None),
                ("IProductService", Kind::Interface, None),
                ("FindAsync", Kind::Method, Some("IProductService")),
                ("Level", Kind::Enum, None),
            ]
        );
        let class = &f.symbols[0];
        assert_eq!(
            class.signature,
            "public sealed class ProductService : IProductService"
        );
        assert_eq!(class.doc.summary, "Stock rules for Product items.");
        assert_eq!(f.symbols[1].doc.summary, "Make one.");
        let find = &f.symbols[2];
        assert_eq!(
            find.signature,
            "public async Task<Product?> FindAsync(int id, CancellationToken ct = default)"
        );
        assert_eq!(find.params, ["int id", "CancellationToken ct = default"]);
        assert_eq!(find.doc.summary, "Finds a product.");
        assert_eq!(find.doc.params[0].name, "id");
        assert_eq!(find.doc.params[0].text, "The id to look for.");
        assert_eq!(
            find.doc.returns.as_ref().unwrap().1,
            "The product, or null."
        );
        assert_eq!((find.line, find.col), (18, 32));
        assert_eq!(f.symbols[6].doc.summary, "Upper bound.");
        assert_eq!(f.symbols[9].params, ["decimal Amount", "string Currency"]);
        assert_eq!(
            f.symbols[9].signature,
            "public record Money(decimal Amount, string Currency)"
        );
    }

    #[test]
    fn plain_doc_text() {
        assert_eq!(parse_xml_doc(" Just text.").summary, "Just text.");
    }
}
