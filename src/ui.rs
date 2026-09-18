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
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .projects
        .iter()
        .enumerate()
        .map(|(i, project)| {
            let style = if i == app.active_project {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let badge = if project.panes.iter().any(|p| p.waiting) {
                " ●"
            } else if project.panes.iter().any(|p| p.attention) {
                " !"
            } else {
                ""
            };
            ListItem::new(format!("{}{}", project.name, badge)).style(style)
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Projects"));
    frame.render_widget(list, area);
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
        if pane.exited.is_some() {
            title.push_str(" [exited]");
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
            InputMode::LineInput(purpose) => {
                let label = match purpose {
                    crate::app::LinePurpose::AddProject => "Add project: ",
                    crate::app::LinePurpose::RenamePane => "Rename pane: ",
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

fn draw_palette(frame: &mut Frame, app: &App) {
    let Some(pal) = app.palette.as_ref() else { return };
    let area = frame.area();
    // 3/5 of the screen but ≥30 cols; the clamp min is capped at area.width
    // because clamp() panics when min > max (e.g. a 20-col terminal).
    let width = (area.width * 3 / 5).clamp(30.min(area.width), area.width);
    // Height fits the filtered command count (border×2 + query row + items)
    // rather than the spec's fixed 14: on machines with agent CLIs on PATH,
    // "Run <agent>" entries push "Quit" past a 14-row overlay's visible rows.
    // Capped at area.height - y (after the 6-row floor) so the rect's bottom
    // edge stays on-screen given the y = h/6 top offset.
    let y = area.height / 6;
    let height = (pal.filtered().len() as u16 + 3)
        .max(6)
        .min(area.height.saturating_sub(y));
    let rect = Rect {
        x: (area.width - width) / 2,
        y,
        width,
        height,
    };
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
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
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
    fn active_pane_title_is_rendered_as_a_block_title() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
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
        let (tx, _rx) = mpsc::channel();
        let app = App::new(tx);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
    }

    #[test]
    fn waiting_pane_shows_dot_in_title() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
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
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        app.flash("can't close the last project");
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(terminal.backend().buffer(), "can't close the last project"));
    }

    #[test]
    fn palette_overlay_lists_matching_commands() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
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
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
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
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
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
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.mode = crate::app::InputMode::LineInput(crate::app::LinePurpose::AddProject);
        app.line_input = Some(crate::text_input::LineEdit::from_str("/tmp/fo"));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(terminal.backend().buffer(), "/tmp/fo"));
    }
}
