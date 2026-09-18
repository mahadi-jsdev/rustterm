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

    let (sidebar_area, main_area, status_area) = frame_areas(area);

    draw_sidebar(frame, app, sidebar_area);
    draw_panes(frame, app, main_area);
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
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED);
                ListItem::new(format!("▸ {}{}", project.name, badge)).style(style)
            } else {
                ListItem::new(format!("  {}{}", project.name, badge))
            }
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Projects"));
    frame.render_widget(list, chunks[0]);
    draw_git_section(frame, app, chunks[1]);
}

fn draw_git_section(frame: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.mode, InputMode::Sidebar);
    let border = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let title = app
        .git_status
        .as_ref()
        .map(|s| format!("⎇ {}", s.branch))
        .unwrap_or_else(|| "git".to_string());
    let block = Block::default().borders(Borders::ALL).border_style(border).title(title);

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
                .map(|(i, f)| sidebar_row(format!("{} {}", f.status, f.path), i, app.sidebar_sel, focused))
                .collect(),
            None => vec![ListItem::new("  no repo")],
        }
    };
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
    let rects = pane_rects(area, project.panes.len(), project.col_split, project.row_split);
    for (index, (pane, rect)) in project.panes.iter().zip(rects.iter()).enumerate() {
        sync_pane_size(pane, *rect);
        let mut title = pane.title.clone();
        if pane.waiting {
            title.push_str(" ●");
        } else if pane.running {
            title.push_str(" ▸");
        }
        if pane.attention {
            title.push_str(" !");
        }
        match &pane.status {
            crate::pane::PaneStatus::Exited(code) => title.push_str(&format!(" [exited {code}]")),
            crate::pane::PaneStatus::Failed(_) => title.push_str(" [failed]"),
            crate::pane::PaneStatus::Running => {}
        }
        let is_active = index == project.active_pane;
        let border_style = if pane.waiting {
            Style::default().fg(Color::Yellow)
        } else if is_active {
            Style::default().fg(Color::Cyan)
        } else if let Some(c) = pane.color {
            Style::default().fg(c)
        } else {
            Style::default()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(title);
        let cursor = Cursor::default().visibility(is_active);
        let parser = pane.parser.lock().unwrap();
        let screen = parser.screen();
        let widget = PseudoTerminal::new(screen).block(block).cursor(cursor);
        frame.render_widget(widget, *rect);
        drop(parser); // highlight_matches re-locks for the scrollback math
        highlight_matches(frame, pane, *rect);
    }
}

/// Post-render pass: REVERSED on each visible search-match cell. Match rows
/// index the full grid (scrollback + visible); visible view row r shows grid
/// line (total - offset + r).
fn highlight_matches(frame: &mut Frame, pane: &Pane, rect: Rect) {
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
    for m in &s.matches {
        let view_row = m.row as i64 - (total as i64 - offset as i64);
        if view_row < 0 || view_row >= height as i64 {
            continue;
        }
        let y = rect.y + 1 + view_row as u16; // +1 for the border
        for dx in 0..m.len {
            let x = rect.x + 1 + (m.col + dx) as u16;
            // A match near a row's right edge can overshoot — clamp inside
            // the block's borders.
            if x < rect.x + rect.width - 1 && y < rect.y + rect.height - 1 {
                frame.buffer_mut()[(x, y)].modifier |= Modifier::REVERSED;
            }
        }
    }
}

fn sync_pane_size(pane: &Pane, rect: Rect) {
    let rows = rect.height.saturating_sub(2).max(1);
    let cols = rect.width.saturating_sub(2).max(1);
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
                format!("Ctrl+A for commands{state}")
            }
            InputMode::Leader => {
                "n new  x close  h/l switch  [ ] project  +/- split  : palette  c add-project  q quit"
                    .to_string()
            }
            InputMode::Palette => "type to filter  ↑/↓ move  enter run  esc cancel".to_string(),
            InputMode::Sidebar => {
                "j/k move  enter open  b branches  c ai-commit  esc back".to_string()
            }
            InputMode::Finder => "type to filter  ↑/↓ move  enter open  esc cancel".to_string(),
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
    let Some(pal) = app.palette.as_ref() else { return };
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
    fn sidebar_shows_project_names() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects.push(Project::new("alpha".into(), PathBuf::from("/tmp")));
        app.projects.push(Project::new("beta".into(), PathBuf::from("/tmp")));

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
        app.projects.push(Project::new("alpha".into(), PathBuf::from("/tmp")));
        app.projects.push(Project::new("beta".into(), PathBuf::from("/tmp")));

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
    fn active_pane_title_is_rendered_as_a_block_title() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        let mut project = Project::new("demo".into(), PathBuf::from("/tmp"));
        let (pane_tx, _pane_rx) = mpsc::channel();
        let pane = Pane::spawn(1, "my-pane-title".into(), 24, 80, None, pane_tx, None).unwrap();
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
        let mut pane = Pane::spawn(1, "work".into(), 24, 80, None, ptx, None).unwrap();
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
        assert!(buffer_contains(terminal.backend().buffer(), "can't close the last project"));
    }

    #[test]
    fn palette_overlay_lists_matching_commands() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
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
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.mode = crate::app::InputMode::Palette;
        app.palette = Some(crate::palette::Palette::open(&app));

        // 20 cols is below the 30-col minimum width: the width clamp must not
        // panic, and the palette's bottom edge must land inside the buffer.
        let backend = TestBackend::new(20, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let last_row = buffer.area.height - 1;
        let bottom_edge_on_screen = (0..buffer.area.width)
            .any(|x| buffer[(x, last_row)].symbol() == "└");
        assert!(bottom_edge_on_screen);
    }

    #[test]
    fn line_input_shows_completion_hints() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.mode = crate::app::InputMode::LineInput(crate::app::LinePurpose::AddProject);
        let mut edit = crate::text_input::LineEdit::from_str("/tmp/fo");
        edit.suggestions = vec!["foo".into(), "foobar".into()];
        app.line_input = Some(edit);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(terminal.backend().buffer(), "foo/  foobar/"));
    }

    #[test]
    fn line_input_prompt_shows_buffer() {
        let (tx, atx) = two_channels();
        let mut app = App::new(tx, atx);
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
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
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.git_status = Some(crate::git::GitStatus {
            branch: "main".into(),
            files: vec![crate::git::ChangedFile { status: 'M', path: "src/app.rs".into() }],
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
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
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
        let mut pane = Pane::spawn(1, "work".into(), 24, 80, None, ptx, None).unwrap();
        pane.status = crate::pane::PaneStatus::Exited(3);
        project.panes.push(pane);
        app.projects.push(project);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(terminal.backend().buffer(), "[exited 3]"));
    }
}
