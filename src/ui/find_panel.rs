//! Find in Files panel state: the query and file filter fields, the streamed results grouped by
//! file, and the selection. The editor drives the search (see `editor::find_files`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::workspace::grep::{FileResult, Hit, SearchJob};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Field {
    #[default]
    Query,
    Include,
}

/// One line of the results list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// File header: index into `results`.
    File(usize),
    /// A matching line: (file, hit).
    Hit(usize, usize),
}

#[derive(Default)]
pub struct FindPanel {
    pub open: bool,
    pub query: String,
    /// Files to include, e.g. `*.js, src/**, !*.spec.js`.
    pub include: String,
    pub field: Field,
    pub results: Vec<FileResult>,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub scroll: usize,
    /// Number of list rows on screen, updated by the layout.
    pub visible_rows: usize,
    pub job: Option<SearchJob>,
    /// Re-run the search at this time (debounced typing).
    pub debounce_at: Option<Instant>,
    /// Invalid regex or file filter.
    pub error: Option<String>,
    pub capped: bool,
    /// Duration of the last finished search.
    pub elapsed: Option<Duration>,
    /// Where the search runs, shown in the title.
    pub scope: String,
    pub match_count: usize,
    /// A result was opened since the search ran; F8 then moves on instead of opening the
    /// selected (first) match.
    pub visited: bool,
}

impl FindPanel {
    pub fn field_mut(&mut self) -> &mut String {
        match self.field {
            Field::Query => &mut self.query,
            Field::Include => &mut self.include,
        }
    }

    pub fn running(&self) -> bool {
        self.job.as_ref().is_some_and(SearchJob::running)
    }

    /// Forget the results before a new search.
    pub fn clear_results(&mut self) {
        self.job = None;
        self.results.clear();
        self.rows.clear();
        self.selected = 0;
        self.scroll = 0;
        self.error = None;
        self.capped = false;
        self.elapsed = None;
        self.match_count = 0;
        self.visited = false;
    }

    /// Add one file's matches, keeping files sorted by path and the selected line selected.
    pub fn add_results(&mut self, files: Vec<FileResult>) {
        if files.is_empty() {
            return;
        }
        let keep = self.selected_key();
        for f in files {
            self.match_count += f.hits.len();
            let at = self.results.partition_point(|r| r.rel < f.rel);
            self.results.insert(at, f);
        }
        self.rebuild_rows();
        // Before the user moves, stay on the first match; afterwards follow the selected one.
        self.selected = keep
            .and_then(|k| self.rows.iter().position(|&r| self.key(r) == k))
            .unwrap_or(if self.rows.len() > 1 { 1 } else { 0 });
        self.ensure_visible();
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (i, f) in self.results.iter().enumerate() {
            self.rows.push(Row::File(i));
            self.rows.extend((0..f.hits.len()).map(|j| Row::Hit(i, j)));
        }
    }

    /// Identity of a row that survives inserting files: (path, hit index or None).
    fn key(&self, row: Row) -> (PathBuf, Option<usize>) {
        match row {
            Row::File(i) => (self.results[i].abs.clone(), None),
            Row::Hit(i, j) => (self.results[i].abs.clone(), Some(j)),
        }
    }

    fn selected_key(&self) -> Option<(PathBuf, Option<usize>)> {
        // Only once the user picked something other than the default first match.
        if self.selected <= 1 {
            return None;
        }
        self.rows.get(self.selected).map(|&r| self.key(r))
    }

    /// The selected match; a file header stands for its first match.
    pub fn selected_hit(&self) -> Option<(&FileResult, &Hit)> {
        let (i, j) = match self.rows.get(self.selected)? {
            Row::File(i) => (*i, 0),
            Row::Hit(i, j) => (*i, *j),
        };
        let f = &self.results[i];
        Some((f, f.hits.get(j)?))
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        self.ensure_visible();
    }

    pub fn select(&mut self, index: usize) {
        if index < self.rows.len() {
            self.selected = index;
            self.ensure_visible();
        }
    }

    /// Select the next (or previous) match line, skipping file headers and wrapping around.
    /// Returns false when there are no matches.
    pub fn step(&mut self, forward: bool) -> bool {
        let n = self.rows.len();
        if self.match_count == 0 || n == 0 {
            return false;
        }
        let mut i = self.selected.min(n - 1);
        for _ in 0..n {
            i = if forward {
                (i + 1) % n
            } else {
                (i + n - 1) % n
            };
            if matches!(self.rows[i], Row::Hit(..)) {
                self.select(i);
                return true;
            }
        }
        false
    }

    /// 1-based number of the selected match among all matches.
    pub fn match_number(&self) -> Option<usize> {
        match self.rows.get(self.selected)? {
            Row::Hit(..) => Some(
                self.rows[..=self.selected]
                    .iter()
                    .filter(|r| matches!(r, Row::Hit(..)))
                    .count(),
            ),
            Row::File(_) => None,
        }
    }

    pub fn ensure_visible(&mut self) {
        let rows = self.visible_rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.rows.len().saturating_sub(self.visible_rows.max(1));
        self.scroll = self.scroll.saturating_add_signed(delta).min(max);
    }

    /// Footer text: progress, totals, or the error.
    pub fn status(&self) -> String {
        if let Some(e) = &self.error {
            return e.clone();
        }
        if self.query.is_empty() {
            return "Type to search. Tab moves to the file filter.".into();
        }
        let files = self.results.len();
        let plural = |n: usize, s: &str| format!("{n} {s}{}", if n == 1 { "" } else { "es" });
        let files_s = format!("{files} file{}", if files == 1 { "" } else { "s" });
        if let Some(job) = self.job.as_ref().filter(|j| j.running()) {
            return format!(
                "Searching… {} in {files_s} ({} files searched)",
                plural(self.match_count, "match"),
                job.files_searched()
            );
        }
        if self.match_count == 0 {
            return "No matches".into();
        }
        let mut s = format!("{} in {files_s}", plural(self.match_count, "match"));
        if let Some(t) = self.elapsed {
            s.push_str(&format!(" · {:.2} s", t.as_secs_f32()));
        }
        if self.capped {
            s.push_str(&format!(
                " · stopped at {} matches",
                crate::workspace::grep::MAX_MATCHES
            ));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rel: &str, lines: &[usize]) -> FileResult {
        FileResult {
            abs: PathBuf::from("/w").join(rel),
            rel: rel.into(),
            hits: lines
                .iter()
                .map(|&line| Hit {
                    line,
                    start: 0,
                    end: 1,
                    preview: "x".into(),
                    preview_ranges: vec![(0, 1)],
                })
                .collect(),
        }
    }

    #[test]
    fn rows_stay_sorted_and_selection_follows() {
        let mut p = FindPanel {
            visible_rows: 10,
            ..Default::default()
        };
        p.add_results(vec![file("b.js", &[1, 5])]);
        assert_eq!(p.rows, [Row::File(0), Row::Hit(0, 0), Row::Hit(0, 1)]);
        assert_eq!(p.selected, 1); // first match
        p.move_selection(1); // b.js line 5
        p.add_results(vec![file("a.js", &[3])]);
        assert_eq!(p.results[0].rel, "a.js");
        assert_eq!(p.selected_hit().unwrap().1.line, 5);
        assert_eq!(p.match_count, 3);
        assert_eq!(p.match_number(), Some(3));
    }

    #[test]
    fn step_skips_headers_and_wraps() {
        let mut p = FindPanel {
            visible_rows: 10,
            ..Default::default()
        };
        p.add_results(vec![file("a.js", &[1]), file("b.js", &[2])]);
        // rows: File a, Hit a1, File b, Hit b2
        assert_eq!(p.selected, 1);
        assert!(p.step(true));
        assert_eq!(p.selected, 3);
        assert!(p.step(true));
        assert_eq!(p.selected, 1); // wrapped past the header
        assert!(p.step(false));
        assert_eq!(p.selected, 3);
        p.select(2);
        assert_eq!(p.selected_hit().unwrap().1.line, 2); // header -> first match
        p.clear_results();
        assert!(!p.step(true));
    }
}
