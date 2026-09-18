use rustterm::app::App;
use rustterm::pane::{Pane, PaneEvent};
use rustterm::project::Project;
use rustterm::{input, ui};
use std::sync::mpsc;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let (events_tx, events_rx) = mpsc::channel::<PaneEvent>();

    let cwd = std::env::current_dir()?;
    let project_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    let mut app = App::new(events_tx.clone());
    let mut project = Project::new(project_name, cwd.clone());

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let first_pane = Pane::spawn(
        app.alloc_pane_id(),
        "pane-0".to_string(),
        rows,
        cols,
        Some(&cwd),
        events_tx,
        None,
    )?;
    project.panes.push(first_pane);
    app.projects.push(project);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &events_rx);
    ratatui::restore();
    result
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    events_rx: &mpsc::Receiver<PaneEvent>,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if crossterm::event::poll(Duration::from_millis(50))? {
            if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                if key.kind == crossterm::event::KeyEventKind::Press {
                    input::handle_key(app, key);
                }
            }
        }

        while let Ok(event) = events_rx.try_recv() {
            if let PaneEvent::Exited(id) = event {
                for project in app.projects.iter_mut() {
                    for pane in project.panes.iter_mut() {
                        if pane.id == id {
                            pane.exited = Some("exited".to_string());
                        }
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
