/// Scroll state of a document's viewport, in lines and display columns.
#[derive(Debug, Clone, Copy, Default)]
pub struct View {
    pub top_line: usize,
    pub left_col: usize,
}

/// Lines kept between the cursor and the top/bottom edge when scrolling.
const SCROLL_MARGIN: usize = 2;

impl View {
    /// Adjust scroll so that (line, col) is inside a `rows` x `cols` viewport.
    pub fn ensure_visible(&mut self, line: usize, col: usize, rows: usize, cols: usize) {
        let rows = rows.max(1);
        let margin = SCROLL_MARGIN.min(rows.saturating_sub(1) / 2);
        if line < self.top_line + margin {
            self.top_line = line.saturating_sub(margin);
        } else if line + margin >= self.top_line + rows {
            self.top_line = line + margin + 1 - rows;
        }

        let cols = cols.max(1);
        let hmargin = 4.min(cols.saturating_sub(1) / 2);
        if col < self.left_col + hmargin {
            self.left_col = col.saturating_sub(hmargin);
        } else if col + hmargin >= self.left_col + cols {
            self.left_col = col + hmargin + 1 - cols;
        }
    }

    pub fn scroll_lines(&mut self, delta: isize, total_lines: usize) {
        let max_top = total_lines.saturating_sub(1);
        self.top_line = self.top_line.saturating_add_signed(delta).min(max_top);
    }

    pub fn scroll_cols(&mut self, delta: isize) {
        self.left_col = self.left_col.saturating_add_signed(delta);
    }
}
