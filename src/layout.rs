use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::Borders;

/// Panes abut — neighbors share a single border line (see pane_borders).
const GUTTER: u16 = 0;

/// (sidebar, pane-grid, status-bar) regions of a frame. Rendering and
/// mouse hit-testing share this so they always agree on pane positions.
/// `sidebar_visible: false` collapses the sidebar — panes get the width.
pub fn frame_areas(area: Rect, sidebar_visible: bool) -> (Rect, Rect, Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);
    if !sidebar_visible {
        return (Rect::default(), rows[0], rows[1]);
    }
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

/// Which borders a pane should draw. Neighbors share a single divider:
/// a pane drops RIGHT when another rect abuts its right edge and BOTTOM
/// when one abuts its bottom edge — the right/bottom neighbor's own
/// LEFT/TOP border serves as the divider line. LEFT and TOP are always
/// drawn (outer frame edges or the pane's own divider duty).
pub fn pane_borders(rects: &[Rect], r: Rect) -> Borders {
    let mut b = Borders::ALL;
    let right_abuts = |o: &Rect| {
        o.x == r.x + r.width && o.y < r.y + r.height && o.y + o.height > r.y
    };
    let bottom_abuts = |o: &Rect| {
        o.y == r.y + r.height && o.x < r.x + r.width && o.x + o.width > r.x
    };
    if rects.iter().any(right_abuts) {
        b.remove(Borders::RIGHT);
    }
    if rects.iter().any(bottom_abuts) {
        b.remove(Borders::BOTTOM);
    }
    b
}

/// Floating popup pane: ~90% of `area`, centered; each deeper float in
/// the stack shifts 2 cols right / 1 row down so the pile stays visible.
/// Clamped to `area` so a float never overflows a small terminal.
pub fn float_rect(area: Rect, depth: usize) -> Rect {
    let w = (area.width * 9 / 10).clamp(20.min(area.width), area.width);
    let h = (area.height * 9 / 10).clamp(6.min(area.height), area.height);
    // Cascade shift capped at the slack left of the centered rect so a
    // deep stack can't push the float past the right/bottom edge.
    let dx = (depth as u16 * 2).min(area.width - w - (area.width - w) / 2);
    let dy = (depth as u16).min(area.height - h - (area.height - h) / 2);
    Rect {
        x: area.x + (area.width - w) / 2 + dx,
        y: area.y + (area.height - h) / 2 + dy,
        width: w,
        height: h,
    }
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
    fn two_panes_split_by_col_split_abutting() {
        let rects = pane_rects(area(), 2, 0.5, 0.5);
        assert_eq!(rects.len(), 2);
        // Left pane starts at the area's left edge.
        assert_eq!(rects[0].x, 0);
        // Right pane ends at the area's right edge.
        assert_eq!(rects[1].x + rects[1].width, 100);
        // No gutter: the right pane starts where the left pane ends —
        // the shared border line is the divider.
        assert_eq!(rects[1].x, rects[0].x + rects[0].width);
        // Both panes span the full height.
        assert_eq!(rects[0].height, 40);
        assert_eq!(rects[1].height, 40);
    }

    #[test]
    fn pane_borders_drop_facing_sides_so_neighbors_share_one_line() {
        let rects = pane_rects(area(), 4, 0.5, 0.5);
        // Top-left abuts right and bottom neighbors — keeps LEFT+TOP.
        assert_eq!(pane_borders(&rects, rects[0]), Borders::LEFT | Borders::TOP);
        // Top-right only abuts below.
        assert_eq!(
            pane_borders(&rects, rects[1]),
            Borders::LEFT | Borders::TOP | Borders::RIGHT
        );
        // Bottom-left only abuts right.
        assert_eq!(
            pane_borders(&rects, rects[2]),
            Borders::LEFT | Borders::TOP | Borders::BOTTOM
        );
        // Bottom-right owns all outer edges plus both divider lines.
        assert_eq!(pane_borders(&rects, rects[3]), Borders::ALL);
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
    fn hidden_sidebar_gives_main_the_full_width() {
        let a = area();
        let (sb, main, _status) = frame_areas(a, false);
        assert_eq!(sb.width, 0);
        assert_eq!(main.width, a.width);
        // Visible sidebar reserves 24 cols as before.
        let (sb2, main2, _) = frame_areas(a, true);
        assert_eq!(sb2.width, 24);
        assert_eq!(main2.width, a.width - 24);
    }

    #[test]
    fn float_rect_is_centered_cascading_and_bounded() {
        let a = area();
        let f0 = float_rect(a, 0);
        assert_eq!(f0.width, 90);
        assert_eq!(f0.height, 36);
        assert_eq!(f0.x, 5); // centered: (100-90)/2
        assert_eq!(f0.y, 2); // centered: (40-36)/2
        // Each deeper float shifts +2x/+1y.
        let f1 = float_rect(a, 1);
        assert_eq!(f1.x, f0.x + 2);
        assert_eq!(f1.y, f0.y + 1);
        // Never overflows the area even at silly depths/sizes.
        for d in [0usize, 3, 100] {
            let r = float_rect(a, d);
            assert!(r.x >= a.x && r.x + r.width <= a.x + a.width);
            assert!(r.y >= a.y && r.y + r.height <= a.y + a.height);
        }
        let tiny = Rect::new(0, 0, 12, 4);
        let r = float_rect(tiny, 0);
        assert!(r.x + r.width <= 12 && r.y + r.height <= 4);
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
