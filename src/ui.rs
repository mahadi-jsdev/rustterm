use crate::app::{App, InputMode};
use crate::layout::{frame_areas, pane_rects};
use crate::pane::Pane;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;
use tui_term::widget::{Cursor, PseudoTerminal};

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let (sidebar_area, main_area, status_area) =
        frame_areas(area, app.sidebar_visible, app.config.sidebar_width);

    if app.sidebar_visible {
        draw_sidebar(frame, app, sidebar_area);
    }
    draw_panes(frame, app, main_area);
    // Floats overlay everything except the status bar.
    let overlay = Rect {
        height: area.height.saturating_sub(1),
        ..area
    };
    draw_floats(frame, app, overlay);
    draw_copy_overlay(frame, app, main_area, overlay);
    draw_mouse_sel_overlay(frame, app, main_area, overlay);
    draw_status_bar(frame, app, status_area);

    if matches!(app.mode, InputMode::Palette) {
        draw_palette(frame, app);
    }
    if matches!(app.mode, InputMode::Finder) {
        draw_finder(frame, app);
    }
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    // Projects get their rows + border, capped at 10 so git always shows.
    let project_h = (app.projects.len() as u16 + 2).min(10).min(area.height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(project_h), Constraint::Min(5)])
        .split(area);
    let items: Vec<ListItem> = app
        .projects
        .iter()
        .enumerate()
        .map(|(i, project)| {
            let badge = if project.panes.iter().any(|p| p.waiting) {
                " ●"
            } else if project.panes.iter().any(|p| p.attention) {
                " !"
            } else {
                ""
            };
            if i == app.active_project {
                let style = Style::default()
                    .fg(app.config.accent)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED);
                ListItem::new(format!("▸ {}{}", project.name, badge)).style(style)
            } else {
                ListItem::new(format!("  {}{}", project.name, badge))
            }
        })
        .collect();
    let projects_focused = matches!(app.mode, InputMode::Sidebar)
        && app.sidebar_focus == crate::app::SidebarSection::Projects;
    let projects_border = if projects_focused {
        Style::default().fg(app.config.accent)
    } else {
        Style::default()
    };
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(projects_border)
            .title("Projects"),
    );
    frame.render_widget(list, chunks[0]);
    draw_panel(frame, app, chunks[1]);
}

/// The sidebar's bottom panel — a file tree by default, or the git
/// section when panel_view is Git (leader g/e flip it).
fn draw_panel(frame: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.mode, InputMode::Sidebar)
        && app.sidebar_focus == crate::app::SidebarSection::Panel;
    let border = if focused {
        Style::default().fg(app.config.accent)
    } else {
        Style::default()
    };

    let (title, items) = match app.panel_view {
        crate::app::PanelView::Files => {
            let items: Vec<ListItem> = match app.files.as_ref() {
                Some(t) => t
                    .rows
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        let indent = "  ".repeat(r.depth);
                        let name = r
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let text = if r.is_dir {
                            format!("{}{} {}/", indent, if r.expanded { "▾" } else { "▸" }, name)
                        } else {
                            format!("{}  {}", indent, name)
                        };
                        sidebar_row(text, i, app.sidebar_sel, focused)
                    })
                    .collect(),
                None => vec![ListItem::new("  empty")],
            };
            ("Files".to_string(), items)
        }
        crate::app::PanelView::Git => {
            let title = app
                .git_status
                .as_ref()
                .map(|s| format!("⎇ {}", s.branch))
                .unwrap_or_else(|| "git".to_string());
            let items: Vec<ListItem> = if app.sidebar_branches {
                app.sidebar_branch_list
                    .iter()
                    .enumerate()
                    .map(|(i, b)| sidebar_row(b.clone(), i, app.sidebar_sel, focused))
                    .collect()
            } else {
                match app.git_status.as_ref() {
                    Some(s) => s
                        .files
                        .iter()
                        .enumerate()
                        .map(|(i, f)| {
                            // Staged rows show the letter green, unstaged
                            // dim — space toggles between the two states.
                            let mark = if f.staged_only() { "+" } else { " " };
                            sidebar_row(
                                format!("{}{} {}", mark, f.status, f.path),
                                i,
                                app.sidebar_sel,
                                focused,
                            )
                        })
                        .collect(),
                    None => vec![ListItem::new("  no repo")],
                }
            };
            (title, items)
        }
    };

    // Viewport scroll — keep the selection inside the panel's visible
    // rows (inside the border). `panel_scroll` persists between draws so
    // the list doesn't jump until the selection actually leaves view.
    let vis = area.height.saturating_sub(2) as usize;
    let len = items.len();
    let mut scroll = app.panel_scroll.get();
    if app.sidebar_sel < scroll {
        scroll = app.sidebar_sel;
    } else if vis > 0 && app.sidebar_sel >= scroll + vis {
        scroll = app.sidebar_sel + 1 - vis;
    }
    scroll = scroll.min(len.saturating_sub(vis));
    app.panel_scroll.set(scroll);
    let items: Vec<ListItem> = items.into_iter().skip(scroll).take(vis).collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(title);
    frame.render_widget(List::new(items).block(block), area);
}

fn sidebar_row(text: String, i: usize, sel: usize, focused: bool) -> ListItem<'static> {
    let style = if focused && i == sel {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    ListItem::new(text).style(style)
}

fn draw_panes(frame: &mut Frame, app: &App, area: Rect) {
    let Some(project) = app.active_project() else {
        return;
    };
    let render = project.render_indices();
    let zoomed = project.zoomed.is_some() && render.len() == 1;
    let visible: Vec<(usize, &crate::pane::Pane)> =
        render.iter().map(|&i| (i, &project.panes[i])).collect();
    let rects = pane_rects(area, visible.len(), project.col_split, project.row_split);
    for ((index, pane), rect) in visible.iter().zip(rects.iter()) {
        let index = *index;
        let mut title = pane.title.clone();
        if pane.waiting {
            title.push_str(" ●");
        } else if pane.running {
            title.push_str(" ▸");
        }
        if pane.attention {
            title.push_str(" !");
        }
        if zoomed {
            title.push_str(" [Z]");
        }
        match &pane.status {
            crate::pane::PaneStatus::Exited(code) => {
                // -1 means "exit code unavailable" (wait failed) — showing a
                // bare [exited] is less confusing than a bogus code.
                if *code < 0 {
                    title.push_str(" [exited]")
                } else {
                    title.push_str(&format!(" [exited {code}]"))
                }
            }
            crate::pane::PaneStatus::Failed(_) => title.push_str(" [failed]"),
            crate::pane::PaneStatus::Running => {}
        }
        let is_active = index == project.active_pane;
        let border_style = if pane.waiting {
            Style::default().fg(Color::Yellow)
        } else if is_active {
            Style::default().fg(app.config.accent)
        } else if let Some(c) = pane.color {
            Style::default().fg(c)
        } else {
            Style::default()
        };
        let borders = crate::layout::pane_borders(&rects, *rect);
        let block = Block::default()
            .borders(borders)
            .border_style(border_style)
            .title(title);
        sync_pane_size(*pane, block.inner(*rect));
        let cursor = Cursor::default().visibility(is_active);
        let parser = pane.parser.lock().unwrap();
        let screen = parser.screen();
        let widget = PseudoTerminal::new(screen).block(block).cursor(cursor);
        frame.render_widget(widget, *rect);
        drop(parser); // highlight_matches re-locks for the scrollback math
        highlight_matches(frame, *pane, *rect, borders);
    }
}

/// Post-render pass: REVERSED on each visible search-match cell. Match rows
/// index the full grid (scrollback + visible); visible view row r shows grid
/// line (total - offset + r).
fn highlight_matches(frame: &mut Frame, pane: &Pane, rect: Rect, borders: Borders) {
    let Some(s) = &pane.search else { return };
    let (total, offset, height) = match pane.parser.lock() {
        Ok(mut p) => {
            let sc = p.screen_mut();
            (
                crate::search::scrollback_len(sc),
                sc.scrollback(),
                sc.size().0 as usize,
            )
        }
        Err(_) => return,
    };
    // Dropped facing borders are content cells — only clamp off a side
    // whose border line was actually drawn.
    let right = rect.x + rect.width - u16::from(borders.contains(Borders::RIGHT));
    let bottom = rect.y + rect.height - u16::from(borders.contains(Borders::BOTTOM));
    for m in &s.matches {
        let view_row = m.row as i64 - (total as i64 - offset as i64);
        if view_row < 0 || view_row >= height as i64 {
            continue;
        }
        let y = rect.y + 1 + view_row as u16; // +1 for the border
        for dx in 0..m.len {
            let x = rect.x + 1 + (m.col + dx) as u16;
            if x < right && y < bottom {
                frame.buffer_mut()[(x, y)].modifier |= Modifier::REVERSED;
            }
        }
    }
}

/// Floating popup panes — drawn in stack order, each offset so the pile
/// is visible. The top float gets the cyan focus border and the cursor;
/// deeper floats render dim. Modal: while any exist they own input.
fn draw_floats(frame: &mut Frame, app: &App, area: Rect) {
    let Some(project) = app.active_project() else {
        return;
    };
    let last = project.floats.len().saturating_sub(1);
    for (depth, pane) in project.floats.iter().enumerate() {
        let rect = crate::layout::float_rect(area, depth, app.config.float_pct);
        frame.render_widget(Clear, rect);
        let is_top = depth == last;
        let border_style = if is_top {
            Style::default().fg(app.config.accent)
        } else {
            Style::default()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(pane.title.clone());
        sync_pane_size(pane, block.inner(rect));
        let cursor = Cursor::default().visibility(is_top);
        let parser = pane.parser.lock().unwrap();
        let screen = parser.screen();
        let widget = PseudoTerminal::new(screen).block(block).cursor(cursor);
        frame.render_widget(widget, rect);
    }
}

/// Copy-mode overlay: REVERSED selection cells + an accent cursor cell.
fn draw_copy_overlay(frame: &mut Frame, app: &App, main_area: Rect, overlay: Rect) {
    if let Some(copy) = &app.copy {
        draw_selection_overlay(frame, app, main_area, overlay, copy, true);
    }
}

/// Mouse drag-selection overlay — same reversed-cells rendering; no
/// cursor cell (the drag head isn't a text cursor).
fn draw_mouse_sel_overlay(frame: &mut Frame, app: &App, main_area: Rect, overlay: Rect) {
    if let Some(sel) = &app.mouse_sel {
        draw_selection_overlay(frame, app, main_area, overlay, sel, false);
    }
}

/// REVERSED-cell rendering of a selection over a pane's rendered rect —
/// a grid pane (respecting zoom) or the top float — mapping absolute
/// grid coords through the pane's current scroll offset. Shared by
/// copy mode and mouse drag-select.
fn draw_selection_overlay(
    frame: &mut Frame,
    app: &App,
    main_area: Rect,
    overlay: Rect,
    sel: &crate::copy::CopyState,
    show_cursor: bool,
) {
    let Some(project) = app.active_project() else {
        return;
    };
    let rect = if project.top_float().map(|f| f.id) == Some(sel.pane_id) {
        crate::layout::float_rect(overlay, project.floats.len() - 1, app.config.float_pct)
    } else {
        let render = project.render_indices();
        let Some(vi) = render
            .iter()
            .position(|&i| project.panes[i].id == sel.pane_id)
        else {
            return;
        };
        let rects = pane_rects(
            main_area,
            render.len(),
            project.col_split,
            project.row_split,
        );
        rects[vi]
    };
    // Grid panes always draw LEFT+TOP (only RIGHT/BOTTOM are shared), so
    // content starts one cell in from the rect origin either way.
    let Some(pane) = app.pane_by_id(sel.pane_id) else {
        return;
    };
    let (view_top, h) = match pane.parser.lock() {
        Ok(mut p) => {
            let s = p.screen_mut();
            (
                crate::search::scrollback_len(s) - s.scrollback(),
                s.size().0 as usize,
            )
        }
        Err(_) => return,
    };
    let to_xy = |(r, c): (usize, usize)| -> Option<(u16, u16)> {
        if r < view_top || r >= view_top + h {
            return None;
        }
        let x = rect.x + 1 + c as u16;
        let y = rect.y + 1 + (r - view_top) as u16;
        if x < rect.x + rect.width - 1 && y < rect.y + rect.height - 1 {
            Some((x, y))
        } else {
            None
        }
    };
    if let Some(anchor) = sel.anchor {
        let (a, b) = if anchor <= sel.cursor {
            (anchor, sel.cursor)
        } else {
            (sel.cursor, anchor)
        };
        let inner_w = rect.width.saturating_sub(2) as usize;
        for r in a.0..=b.0 {
            let c0 = if r == a.0 { a.1 } else { 0 };
            // Middle rows select to the pane's right edge — never past
            // it (a usize::MAX bound would iterate billions of cells).
            let c1 = if r == b.0 { b.1.min(inner_w) } else { inner_w };
            for c in c0..=c1 {
                if let Some((x, y)) = to_xy((r, c)) {
                    frame.buffer_mut()[(x, y)].set_bg(app.config.selection);
                }
            }
        }
    }
    if show_cursor {
        if let Some((x, y)) = to_xy(sel.cursor) {
            let cell = &mut frame.buffer_mut()[(x, y)];
            cell.set_bg(app.config.accent);
            cell.set_fg(Color::Black);
        }
    }
}

fn sync_pane_size(pane: &Pane, inner: Rect) {
    let rows = inner.height.max(1);
    let cols = inner.width.max(1);
    // Discarding the error is intentional: a resize failure here just means
    // the pane keeps its previous size for this one frame. It isn't a
    // permanent loss — the next frame calls resize() again with the current
    // rect and will succeed once whatever transient condition caused this
    // failure clears.
    let _ = pane.resize(rows, cols);
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let fresh_flash = app
        .status_msg
        .as_ref()
        .filter(|(_, at)| at.elapsed() < std::time::Duration::from_secs(3));
    let text = if let Some((msg, _)) = fresh_flash {
        msg.clone()
    } else {
        match app.mode {
            InputMode::Normal => {
                let state = app
                    .active_project()
                    .and_then(|p| p.active_pane())
                    .map(|p| {
                        if p.waiting {
                            "  ● waiting for input"
                        } else if p.running {
                            "  ▸ running"
                        } else {
                            ""
                        }
                    })
                    .unwrap_or("");
                let hidden = app
                    .active_project()
                    .map(|p| p.panes.iter().filter(|p| p.hidden).count())
                    .unwrap_or(0);
                let bg = if hidden > 0 {
                    format!("  +{hidden} hidden")
                } else {
                    String::new()
                };
                let floats = app
                    .active_project()
                    .map(|p| p.floats.len())
                    .unwrap_or(0);
                let fl = if floats > 0 {
                    format!("  ⬚{floats} popup")
                } else {
                    String::new()
                };
                format!(
                    "Ctrl+{} for commands{state}{bg}{fl}",
                    app.config.leader_char.to_ascii_uppercase()
                )
            }
            InputMode::Leader => {
                "n new  x close  h/l switch  z zoom  . flag  H hide  [ ] project  +/- split  :/p palette  c add  b side  g git  e files  G lazygit  L log  f find  / search  d detach  q quit"
                    .to_string()
            }
            InputMode::Palette => "type to filter  ↑/↓ move  enter run  esc cancel".to_string(),
            InputMode::Sidebar => match app.sidebar_focus {
                crate::app::SidebarSection::Projects => {
                    "j/k switch  enter open  tab panel  esc back".to_string()
                }
                crate::app::SidebarSection::Panel => match app.panel_view {
                    crate::app::PanelView::Git => {
                        "j/k move  enter open  space stage  b branches  c ai-commit  tab projects  esc back"
                            .to_string()
                    }
                    crate::app::PanelView::Files => {
                        "j/k move  h/l fold  enter open  . hidden  tab projects  esc back"
                            .to_string()
                    }
                },
            },
            InputMode::Finder => "type to filter  ↑/↓ move  enter open  esc cancel".to_string(),
            InputMode::Copy => {
                "hjkl move  v select  y copy  PgUp/PgDn page  g/G ends  esc cancel".to_string()
            }
            InputMode::Search => "n next  N prev  any key to exit".to_string(),
            InputMode::LineInput(purpose) => {
                let label = match purpose {
                    crate::app::LinePurpose::AddProject => "Add project: ",
                    crate::app::LinePurpose::RenamePane => "Rename pane: ",
                    crate::app::LinePurpose::CommitMsg => "Commit message: ",
                    crate::app::LinePurpose::Search => "Search: ",
                };
                let Some(edit) = app.line_input.as_ref() else {
                    return;
                };
                let buf = edit.as_str();
                if let Some(err) = edit.error() {
                    format!("{label}{buf}█  — {err}")
                } else if !edit.suggestions.is_empty() {
                    let shown = edit
                        .suggestions
                        .iter()
                        .take(5)
                        .map(|n| format!("{n}/"))
                        .collect::<Vec<_>>()
                        .join("  ");
                    let more = if edit.suggestions.len() > 5 { "  …" } else { "" };
                    format!("{label}{buf}█    {shown}{more}")
                } else {
                    format!("{label}{buf}█")
                }
            }
        }
    };
    frame.render_widget(Paragraph::new(text), area);
}

/// Overlay rect shared by palette + finder: 3/5 of the screen but ≥30 cols
/// (the clamp min is capped at area.width because clamp() panics when
/// min > max, e.g. a 20-col terminal). Height fits `items` (border×2 + query
/// row + items) rather than a fixed size: on machines with agent CLIs on
/// PATH, "Run <agent>" entries push "Quit" past a 14-row overlay's visible
/// rows. Capped at area.height - y (after the 6-row floor) so the rect's
/// bottom edge stays on-screen given the y = h/6 top offset.
fn centered_rect(area: Rect, items: usize) -> Rect {
    let width = (area.width * 3 / 5).clamp(30.min(area.width), area.width);
    let y = area.height / 6;
    let height = (items as u16 + 3).max(6).min(area.height.saturating_sub(y));
    Rect {
        x: (area.width - width) / 2,
        y,
        width,
        height,
    }
}

fn draw_palette(frame: &mut Frame, app: &App) {
    let Some(pal) = app.palette.as_ref() else {
        return;
    };
    let rect = centered_rect(frame.area(), pal.filtered().len());
    frame.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title("Command");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);
    frame.render_widget(Paragraph::new(format!("> {}", pal.query)), rows[0]);

    let visible = rows[1].height as usize;
    let items: Vec<ListItem> = pal
        .filtered()
        .iter()
        .enumerate()
        .take(visible)
        .map(|(i, c)| {
            let style = if i == pal.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(c.label.clone()).style(style)
        })
        .collect();
    frame.render_widget(List::new(items), rows[1]);
}

fn draw_finder(frame: &mut Frame, app: &App) {
    let Some(f) = app.finder.as_ref() else { return };
    let filtered = f.filtered();
    let rect = centered_rect(frame.area(), filtered.len());
    frame.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title("Find file");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);
    frame.render_widget(Paragraph::new(format!("> {}", f.query)), rows[0]);
    let visible = rows[1].height as usize;
    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .take(visible)
        .map(|(i, p)| {
            let style = if i == f.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(p.to_string_lossy().to_string()).style(style)
        })
        .collect();
    frame.render_widget(List::new(items), rows[1]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::Pane;
    use crate::project::Project;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::Terminal;
    use std::path::PathBuf;
    use std::sync::mpsc;

    /// Senders for both app channels; the receivers are dropped — pane/app
    /// event sends are all `let _ =`, so tests need only the sender half.
    fn two_channels() -> (
        mpsc::Sender<crate::pane::PaneEvent>,
        mpsc::Sender<crate::app::AppEvent>,
    ) {
        (mpsc::channel().0, mpsc::channel().0)
    }

    fn buffer_contains(buffer: &Buffer, needle: &str) -> bool {
        for y in 0..buffer.area.height {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            if row.contains(needle) {
                return true;
            }
        }
        false
    }

    #[test]
    fn three_panes_share_border_lines_no_dead_gap() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects
            .push(Project::new("demo".into(), PathBuf::from("/tmp")));
        for _ in 0..3 {
            app.spawn_pane(None);
        }

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();

        // Pane-grid area: right of the 24-col sidebar, above the status bar.
        let grid = Rect::new(24, 0, 56, 23);
        for y in grid.y..grid.y + grid.height {
            assert!(
                (grid.x..grid.x + grid.width).any(|x| buffer[(x, y)].symbol() != " "),
                "row {y} is blank — a dead gap row leaked into the grid"
            );
        }
        for x in grid.x..grid.x + grid.width {
            assert!(
                (grid.y..grid.y + grid.height).any(|y| buffer[(x, y)].symbol() != " "),
                "col {x} is blank — a dead gap column leaked into the grid"
            );
        }
    }

    #[test]
    fn float_renders_centered_over_the_grid() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects
            .push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.spawn_pane(None);
        app.spawn_float("my-popup", "exec true");

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();

        // The float's title row: a bordered top edge somewhere mid-screen.
        let title_cell = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .find(|&(x, y)| buffer[(x, y)].symbol() == "m" && row_contains(buffer, y, "my-popup"))
            .expect("float title not rendered");
        assert!(title_cell.1 > 0, "float starts below the frame top");

        // The grid pane's own title is still drawn behind the float's
        // margins (float is 90% wide, so the outer columns stay visible).
        assert!(buffer_contains(buffer, "pane-"));
    }

    #[test]
    fn files_panel_scrolls_selection_into_view() {
        let dir = std::env::temp_dir().join(format!("rustterm-scroll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..40 {
            std::fs::write(dir.join(format!("f{i:02}.txt")), "").unwrap();
        }
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects.push(Project::new("demo".into(), dir.clone()));
        app.enter_sidebar();
        // Selection far below the fold — the draw must scroll to it.
        app.sidebar_sel = 30;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();

        assert!(app.panel_scroll.get() > 0, "viewport scrolled");
        assert!(buffer_contains(buffer, "f30.txt"), "selected row rendered");
        assert!(!buffer_contains(buffer, "f00.txt"), "top rows scrolled off");

        // Scrolling back up restores the top of the list.
        app.sidebar_sel = 0;
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(buffer_contains(buffer, "f00.txt"));
        assert_eq!(app.panel_scroll.get(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// All cells of row `y` joined — for substring checks on one row.
    fn row_contains(buffer: &Buffer, y: u16, needle: &str) -> bool {
        let row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        row.contains(needle)
    }

    #[test]
    fn sidebar_shows_project_names() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects
            .push(Project::new("alpha".into(), PathBuf::from("/tmp")));
        app.projects
            .push(Project::new("beta".into(), PathBuf::from("/tmp")));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!(buffer_contains(buffer, "alpha"));
        assert!(buffer_contains(buffer, "beta"));
    }

    #[test]
    fn active_project_row_is_highlighted() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects
            .push(Project::new("alpha".into(), PathBuf::from("/tmp")));
        app.projects
            .push(Project::new("beta".into(), PathBuf::from("/tmp")));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let marker = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .find(|&(x, y)| buffer[(x, y)].symbol() == "▸")
            .expect("active project marker not rendered");
        assert!(buffer[marker].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn mouse_selection_uses_selection_color_not_reverse() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        let mut project = Project::new("demo".into(), PathBuf::from("/tmp"));
        let (ptx, _prx) = mpsc::channel();
        let pane = Pane::spawn(1, "p".into(), 24, 80, None, ptx, None, 10_000).unwrap();
        pane.parser.lock().unwrap().process(b"select me now\r\n");
        let pane_id = pane.id;
        project.panes.push(pane);
        app.projects.push(project);
        app.mouse_sel = Some(crate::copy::CopyState {
            pane_id,
            cursor: (0, 8), // "select m" of "select me now"
            anchor: Some((0, 0)),
        });

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        // Selected cells carry the config.selection bg — never REVERSED.
        let selected: Vec<(u16, u16)> = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| buffer[(x, y)].bg == app.config.selection)
            .collect();
        assert!(!selected.is_empty(), "no selection-colored cells rendered");
        for pos in &selected {
            assert!(
                !buffer[*pos].modifier.contains(Modifier::REVERSED),
                "selection must be a color, not reverse video"
            );
        }
    }

    #[test]
    fn active_pane_title_is_rendered_as_a_block_title() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        let mut project = Project::new("demo".into(), PathBuf::from("/tmp"));
        let (pane_tx, _pane_rx) = mpsc::channel();
        let pane = Pane::spawn(
            1,
            "my-pane-title".into(),
            24,
            80,
            None,
            pane_tx,
            None,
            10_000,
        )
        .unwrap();
        project.panes.push(pane);
        app.projects.push(project);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!(buffer_contains(buffer, "my-pane-title"));
    }

    #[test]
    fn draw_with_no_projects_does_not_panic() {
        let (tx, atx) = two_channels();
        let app = App::new(tx, atx);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
    }

    #[test]
    fn waiting_pane_shows_dot_in_title() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        let mut project = Project::new("demo".into(), PathBuf::from("/tmp"));
        let (ptx, _prx) = mpsc::channel();
        let mut pane = Pane::spawn(1, "work".into(), 24, 80, None, ptx, None, 10_000).unwrap();
        pane.waiting = true;
        project.panes.push(pane);
        app.projects.push(project);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(terminal.backend().buffer(), "work ●"));
    }

    #[test]
    fn status_flash_replaces_hint_text() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.flash("can't close the last project");
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(
            terminal.backend().buffer(),
            "can't close the last project"
        ));
    }

    #[test]
    fn palette_overlay_lists_matching_commands() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects
            .push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.mode = crate::app::InputMode::Palette;
        app.palette = Some(crate::palette::Palette::open(&app));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(buffer_contains(buffer, "New pane"));
        assert!(buffer_contains(buffer, "Quit"));
    }

    #[test]
    fn palette_rect_stays_on_screen_on_tiny_terminal() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects
            .push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.mode = crate::app::InputMode::Palette;
        app.palette = Some(crate::palette::Palette::open(&app));

        // 20 cols is below the 30-col minimum width: the width clamp must not
        // panic, and the palette's bottom edge must land inside the buffer.
        let backend = TestBackend::new(20, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let last_row = buffer.area.height - 1;
        let bottom_edge_on_screen =
            (0..buffer.area.width).any(|x| buffer[(x, last_row)].symbol() == "└");
        assert!(bottom_edge_on_screen);
    }

    #[test]
    fn line_input_shows_completion_hints() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects
            .push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.mode = crate::app::InputMode::LineInput(crate::app::LinePurpose::AddProject);
        let mut edit = crate::text_input::LineEdit::from_str("/tmp/fo");
        edit.suggestions = vec!["foo".into(), "foobar".into()];
        app.line_input = Some(edit);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(
            terminal.backend().buffer(),
            "foo/  foobar/"
        ));
    }

    #[test]
    fn line_input_prompt_shows_buffer() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects
            .push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.mode = crate::app::InputMode::LineInput(crate::app::LinePurpose::AddProject);
        app.line_input = Some(crate::text_input::LineEdit::from_str("/tmp/fo"));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(terminal.backend().buffer(), "/tmp/fo"));
    }

    #[test]
    fn sidebar_shows_branch_and_changed_files() {
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        app.projects
            .push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.panel_view = crate::app::PanelView::Git;
        app.git_status = Some(crate::git::GitStatus {
            branch: "main".into(),
            files: vec![crate::git::ChangedFile {
                status: 'M',
                index: ' ',
                worktree: 'M',
                path: "src/app.rs".into(),
            }],
        });
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buf = terminal.backend().buffer();
        assert!(buffer_contains(buf, "main"));
        assert!(buffer_contains(buf, "M src/app.rs"));
    }

    #[test]
    fn finder_overlay_lists_files() {
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        app.projects
            .push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.mode = InputMode::Finder;
        app.finder = Some(crate::finder::FinderState::open(&PathBuf::from("/tmp")));
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(terminal.backend().buffer(), "Find file"));
    }

    #[test]
    fn exited_pane_title_shows_the_code() {
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        let mut project = Project::new("demo".into(), PathBuf::from("/tmp"));
        let (ptx, _prx) = mpsc::channel();
        let mut pane = Pane::spawn(1, "work".into(), 24, 80, None, ptx, None, 10_000).unwrap();
        pane.status = crate::pane::PaneStatus::Exited(3);
        project.panes.push(pane);
        app.projects.push(project);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(terminal.backend().buffer(), "[exited 3]"));
    }
}
