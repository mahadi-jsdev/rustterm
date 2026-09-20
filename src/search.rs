pub struct SearchMatch {
    /// Row index across the full grid (scrollback + visible rows).
    pub row: usize,
    /// Byte column of the match start inside that line.
    pub col: usize,
    pub len: usize,
}

pub struct SearchState {
    pub query: String,
    pub matches: Vec<SearchMatch>,
    pub idx: usize,
}

impl SearchState {
    pub fn next(&mut self) {
        if !self.matches.is_empty() {
            self.idx = (self.idx + 1) % self.matches.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.matches.is_empty() {
            self.idx = if self.idx == 0 { self.matches.len() - 1 } else { self.idx - 1 };
        }
    }

    pub fn current(&self) -> Option<&SearchMatch> {
        self.matches.get(self.idx)
    }
}

/// All grid rows (scrollback + visible), enumerated by stepping the
/// viewport. Saves and restores the current scroll offset; transient
/// mutation is invisible under the parser lock. Shared by search and
/// copy-mode extraction.
pub(crate) fn grid_lines(screen: &mut vt100::Screen) -> Vec<String> {
    let h = screen.size().0 as usize;
    if h == 0 {
        return Vec::new();
    }
    let saved = screen.scrollback();
    screen.set_scrollback(usize::MAX);
    let s = screen.scrollback();
    let mut rows: Vec<String> = Vec::with_capacity(s + h);
    let mut top = 0usize;
    while top < s + h {
        screen.set_scrollback(s.saturating_sub(top));
        let base = s - screen.scrollback(); // actual viewport top after clamping
        // `rows()` yields exactly h Strings — one per visible grid row,
        // unmerged (unlike `contents()`, which joins wrapped rows).
        for (i, l) in screen.rows(0, screen.size().1).enumerate() {
            let r = base + i;
            if r >= rows.len() && r < s + h {
                while rows.len() < r {
                    rows.push(String::new());
                }
                rows.push(l);
            }
        }
        top = base + h;
    }
    screen.set_scrollback(saved);
    rows
}

/// Lines currently in scrollback (total, not just visible).
pub fn scrollback_len(screen: &mut vt100::Screen) -> usize {
    let saved = screen.scrollback();
    screen.set_scrollback(usize::MAX);
    let s = screen.scrollback();
    screen.set_scrollback(saved);
    s
}

/// Case-insensitive search over the full grid (scrollback + visible).
pub fn find_matches(screen: &mut vt100::Screen, query: &str) -> Vec<SearchMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let q = query.to_lowercase();
    grid_lines(screen)
        .iter()
        .enumerate()
        .flat_map(|(row, line)| {
            let lower = line.to_lowercase();
            lower
                .match_indices(q.as_str())
                .map(move |(col, _)| SearchMatch { row, col, len: query.len() })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Scroll offset that puts `row` (index across scrollback+visible
/// lines) at the top of the view; 0 keeps the live view.
pub fn offset_for_row(scrollback: usize, row: usize) -> usize {
    scrollback.saturating_sub(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_with(lines: &[&str]) -> vt100::Parser {
        let mut p = vt100::Parser::new(5, 40, 100);
        for l in lines {
            p.process(format!("{l}\r\n").as_bytes());
        }
        p
    }

    #[test]
    fn finds_all_case_insensitive_matches_with_positions() {
        let mut p = screen_with(&["alpha", "beta", "ALPHA again", "none"]);
        let m = find_matches(p.screen_mut(), "alpha");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].row, 0);
        assert_eq!(m[0].col, 0);
        assert_eq!(m[1].row, 2);
        assert_eq!(m[1].col, 0);
        assert_eq!(m[0].len, 5);
    }

    #[test]
    fn empty_query_finds_nothing() {
        let mut p = screen_with(&["x"]);
        assert!(find_matches(p.screen_mut(), "").is_empty());
    }

    #[test]
    fn scrollback_rows_are_counted() {
        // 10 lines into a 5-row screen → ≥5 scrollback lines.
        let lines: Vec<String> = (0..10).map(|i| format!("line{i}")).collect();
        let mut p = vt100::Parser::new(5, 40, 100);
        for l in &lines {
            p.process(format!("{l}\r\n").as_bytes());
        }
        assert!(scrollback_len(p.screen_mut()) >= 5);
    }

    #[test]
    fn finds_matches_in_scrollback_rows() {
        // 10 lines into a 5-row screen: "line2" sits in scrollback.
        let lines: Vec<String> = (0..10).map(|i| format!("line{i}")).collect();
        let mut p = vt100::Parser::new(5, 40, 100);
        for l in &lines {
            p.process(format!("{l}\r\n").as_bytes());
        }
        let m = find_matches(p.screen_mut(), "line2");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].row, 2);
        assert_eq!(m[0].col, 0);
        // Grid row 2 → scroll offset S−2 = 4 puts it at the top.
        assert_eq!(offset_for_row(scrollback_len(p.screen_mut()), m[0].row), 4);
    }

    #[test]
    fn wrapped_lines_count_as_separate_grid_rows() {
        // 10-col screen: 16 chars wrap onto two grid rows.
        let mut p = vt100::Parser::new(5, 10, 100);
        p.process(b"0123456789abcdef\r\n");
        p.process(b"target\r\n");
        let m = find_matches(p.screen_mut(), "target");
        assert_eq!(m.len(), 1);
        // `contents()` would merge the wrap into one line and report row 1;
        // counting real grid rows puts "target" on row 2.
        assert_eq!(m[0].row, 2);
    }

    #[test]
    fn find_matches_restores_scroll_offset() {
        let mut p = vt100::Parser::new(5, 40, 100);
        for i in 0..10 {
            p.process(format!("line{i}\r\n").as_bytes());
        }
        p.screen_mut().set_scrollback(3);
        let _ = find_matches(p.screen_mut(), "line");
        assert_eq!(p.screen().scrollback(), 3);
    }

    #[test]
    fn matches_span_partial_scrollback_and_live_rows() {
        // 7 lines into a 5-row screen → partial scrollback (0 < s < h).
        let mut p = vt100::Parser::new(5, 40, 100);
        for i in 0..7 {
            p.process(format!("line{i}\r\n").as_bytes());
        }
        let s = scrollback_len(p.screen_mut());
        assert!(s > 0 && s < 5, "expected partial scrollback, got {s}");
        // Match inside scrollback.
        let m = find_matches(p.screen_mut(), "line0");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].row, 0);
        // Match inside the live region.
        let m = find_matches(p.screen_mut(), "line5");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].row, 5);
    }

    #[test]
    fn offset_for_row_maps_scrollback_line_to_view() {
        // 8 scrollback lines; match on line 3 → offset 5 puts it at top.
        assert_eq!(offset_for_row(8, 3), 5);
        // Match inside the visible region → stay at live view.
        assert_eq!(offset_for_row(8, 10), 0);
    }

    #[test]
    fn next_prev_wrap() {
        let mut s = SearchState {
            query: "x".into(),
            matches: vec![
                SearchMatch { row: 0, col: 0, len: 1 },
                SearchMatch { row: 1, col: 0, len: 1 },
                SearchMatch { row: 2, col: 0, len: 1 },
            ],
            idx: 2,
        };
        s.next();
        assert_eq!(s.idx, 0);
        s.prev();
        assert_eq!(s.idx, 2);
    }
}
