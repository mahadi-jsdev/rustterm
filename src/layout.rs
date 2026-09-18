use ratatui::layout::{Constraint, Direction, Layout, Rect};

const GUTTER: u16 = 1;

/// (sidebar, pane-grid, status-bar) regions of a frame. Rendering and
/// mouse hit-testing share this so they always agree on pane positions.
pub fn frame_areas(area: Rect) -> (Rect, Rect, Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(0)])
        .split(rows[0]);
    (cols[0], cols[1], rows[1])
}

pub fn pane_rects(area: Rect, count: usize, col_split: f32, row_split: f32) -> Vec<Rect> {
    match count {
        0 => Vec::new(),
        1 => vec![area],
        2 => split_cols(area, col_split),
        3 => {
            let top = split_cols(top_half(area, row_split), col_split);
            let bottom = bottom_half(area, row_split);
            vec![top[0], top[1], bottom]
        }
        4 => {
            let top = split_cols(top_half(area, row_split), col_split);
            let bottom = split_cols(bottom_half(area, row_split), col_split);
            vec![top[0], top[1], bottom[0], bottom[1]]
        }
        _ => wrapping_grid(area, count),
    }
}

fn split_cols(area: Rect, col_split: f32) -> Vec<Rect> {
    let left_pct = (col_split * 100.0).round() as u16;
    let right_pct = 100u16.saturating_sub(left_pct);
    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(left_pct),
            Constraint::Length(GUTTER),
            Constraint::Percentage(right_pct),
        ])
        .split(area);
    vec![parts[0], parts[2]]
}

fn top_half(area: Rect, row_split: f32) -> Rect {
    split_rows(area, row_split)[0]
}

fn bottom_half(area: Rect, row_split: f32) -> Rect {
    split_rows(area, row_split)[1]
}

fn split_rows(area: Rect, row_split: f32) -> Vec<Rect> {
    let top_pct = (row_split * 100.0).round() as u16;
    let bottom_pct = 100u16.saturating_sub(top_pct);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(top_pct),
            Constraint::Length(GUTTER),
            Constraint::Percentage(bottom_pct),
        ])
        .split(area);
    vec![parts[0], parts[2]]
}

fn wrapping_grid(area: Rect, count: usize) -> Vec<Rect> {
    let cols = 3usize;
    let rows = count.div_ceil(cols);
    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Percentage((100 / rows.max(1)) as u16))
        .collect();
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    let mut rects = Vec::with_capacity(count);
    for r in 0..rows {
        let remaining = count - r * cols;
        let this_row_cols = remaining.min(cols);
        let col_constraints: Vec<Constraint> = (0..this_row_cols)
            .map(|_| Constraint::Percentage((100 / this_row_cols) as u16))
            .collect();
        let col_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints)
            .split(row_areas[r]);
        for c in 0..this_row_cols {
            rects.push(col_areas[c]);
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 100, 40)
    }

    #[test]
    fn zero_panes_is_empty() {
        assert_eq!(pane_rects(area(), 0, 0.5, 0.5), Vec::<Rect>::new());
    }

    #[test]
    fn one_pane_fills_area() {
        let rects = pane_rects(area(), 1, 0.5, 0.5);
        assert_eq!(rects, vec![area()]);
    }

    #[test]
    fn two_panes_split_by_col_split_with_gutter() {
        let rects = pane_rects(area(), 2, 0.5, 0.5);
        assert_eq!(rects.len(), 2);
        // Left pane starts at the area's left edge.
        assert_eq!(rects[0].x, 0);
        // Right pane ends at the area's right edge.
        assert_eq!(rects[1].x + rects[1].width, 100);
        // A 1-cell gutter separates them: right pane starts strictly after
        // the left pane ends.
        assert!(rects[1].x > rects[0].x + rects[0].width);
        // Both panes span the full height.
        assert_eq!(rects[0].height, 40);
        assert_eq!(rects[1].height, 40);
    }

    #[test]
    fn three_panes_two_top_one_bottom_spanning() {
        let rects = pane_rects(area(), 3, 0.5, 0.5);
        assert_eq!(rects.len(), 3);
        // Top two panes share the top row.
        assert_eq!(rects[0].y, rects[1].y);
        // Bottom pane spans the full width of the area.
        assert_eq!(rects[2].x, 0);
        assert_eq!(rects[2].x + rects[2].width, 100);
        // Bottom pane is below the top two.
        assert!(rects[2].y > rects[0].y);
    }

    #[test]
    fn four_panes_is_a_2x2_grid() {
        let rects = pane_rects(area(), 4, 0.5, 0.5);
        assert_eq!(rects.len(), 4);
        // Panes 0 and 1 share a row; panes 2 and 3 share a (lower) row.
        assert_eq!(rects[0].y, rects[1].y);
        assert_eq!(rects[2].y, rects[3].y);
        assert!(rects[2].y > rects[0].y);
    }

    #[test]
    fn five_panes_falls_back_to_wrapping_grid() {
        let rects = pane_rects(area(), 5, 0.5, 0.5);
        assert_eq!(rects.len(), 5);
        // All rects stay within the area bounds.
        for r in &rects {
            assert!(r.x + r.width <= area().x + area().width);
            assert!(r.y + r.height <= area().y + area().height);
        }
    }
}
