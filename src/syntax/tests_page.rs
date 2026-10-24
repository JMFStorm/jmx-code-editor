//! Coloring of the Tests page: the counts line, project and file headers, and one row per
//! test with its status glyph.

use super::{Lexer, Token};

pub fn lex(lx: &mut Lexer) {
    let chars = lx.chars;
    let end = chars.len();
    if end == 0 {
        return;
    }
    let first = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(end);
    let starts = |s: &str| {
        chars[first..]
            .iter()
            .copied()
            .take(s.chars().count())
            .eq(s.chars())
    };
    match chars.get(first) {
        Some(&g @ ('✔' | '✖' | '○' | '●' | '⊘' | '◇')) => {
            let token = match g {
                '✔' => Token::Pass,
                '✖' => Token::Error,
                '●' => Token::Running,
                '⊘' => Token::Warning,
                _ => Token::Comment,
            };
            lx.push(first, first + 1, token);
            // Details after the name: `   — message`, `   3 ms`, `   running…`.
            if let Some(k) =
                (first..end.saturating_sub(2)).find(|&k| chars[k..k + 3] == [' ', ' ', ' '])
            {
                let detail = if chars.get(k + 3) == Some(&'—') {
                    Token::Error
                } else {
                    Token::Comment
                };
                lx.push(k, end, detail);
            }
            if g == '○' || g == '◇' {
                lx.push(first + 1, end, Token::Comment);
            }
        }
        Some('▾' | '▸') => {
            // Projects, files and suites by indentation; then the group's summary glyph.
            let token = match first {
                0 => Token::Function,
                2 => Token::Type,
                _ => Token::Property,
            };
            lx.push(first, end, token);
            lx.push(first, first + 1, Token::Comment);
            let g = first + 2;
            let glyph = match chars.get(g) {
                Some('✔') => Some(Token::Pass),
                Some('✖') => Some(Token::Error),
                Some('●') => Some(Token::Running),
                Some('○') => Some(Token::Comment),
                _ => None,
            };
            if let Some(t) = glyph {
                lx.push(g, g + 1, t);
            }
            // A run problem after the counts: `  — dotnet not found`.
            if let Some(k) =
                (first..end.saturating_sub(3)).find(|&k| chars[k..k + 3] == [' ', ' ', '—'])
            {
                lx.push(k, end, Token::Error);
            }
        }
        _ if starts("Tests:") => lx.push(0, end, Token::Keyword),
        _ => lx.push(0, end, Token::Comment),
    }
    lx.i = end;
}

#[cfg(test)]
mod tests {
    use super::super::test_util::tokens;
    use super::super::{Lang, Token};

    #[test]
    fn rows_and_headers() {
        let t = tokens(
            Lang::Tests,
            "Tests: 1 passed\nF5 run\n\n▾ ✖ web  (Vitest)  1/2 passed\n  ▾ ✖ a.spec.js  (2)\n      ✔ s › ok   3 ms\n      ✖ s › bad   — expected 1\n    ▸ ✔ suite  (2)",
        );
        assert_eq!(t[0][0].1, Token::Keyword);
        assert_eq!(t[1][0].1, Token::Comment);
        assert_eq!(t[3][0].1, Token::Function);
        assert_eq!(t[4][0].1, Token::Type);
        assert_eq!(t[3][2], ("✖".into(), Token::Error));
        assert_eq!(t[7][0].1, Token::Property);
        assert_eq!(t[7][2], ("✔".into(), Token::Pass));
        assert_eq!(t[5][0], ("✔".into(), Token::Pass));
        assert_eq!(t[5][1], ("   3 ms".into(), Token::Comment));
        assert_eq!(t[6][0], ("✖".into(), Token::Error));
        assert_eq!(t[6][1], ("   — expected 1".into(), Token::Error));
    }
}
