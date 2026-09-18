use crate::app::App;
use crate::layout::pane_rects;
use crate::pane::Pane;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(0)])
        .split(area);
    let sidebar_area = cols[0];
    let main_area = cols[1];

    draw_sidebar(frame, app, sidebar_area);
    draw_panes(frame, app, main_area);
    draw_status_bar(frame, app, Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1));
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
            ListItem::new(project.name.clone()).style(style)
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
    for (pane, rect) in project.panes.iter().zip(rects.iter()) {
        sync_pane_size(pane, *rect);
        let title = if pane.exited.is_some() {
            format!("{} [exited]", pane.title)
        } else {
            pane.title.clone()
        };
        let parser = pane.parser.lock().unwrap();
        let screen = parser.screen();
        let widget =
            PseudoTerminal::new(screen).block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(widget, *rect);
    }
}

fn sync_pane_size(pane: &Pane, rect: Rect) {
    let rows = rect.height.saturating_sub(2).max(1);
    let cols = rect.width.saturating_sub(2).max(1);
    let _ = pane.resize(rows, cols);
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let text = match app.mode {
        crate::app::InputMode::Normal => "Ctrl+B for commands".to_string(),
        crate::app::InputMode::Leader => "n new  x close  h/l switch  [ ] project  +/- split  q quit".to_string(),
    };
    frame.render_widget(Paragraph::new(text), area);
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
        let pane = Pane::spawn(1, "my-pane-title".into(), 24, 80, None, pane_tx).unwrap();
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
}
