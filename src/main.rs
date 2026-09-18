use rustterm::app::App;
use rustterm::pane::{Pane, PaneEvent};
use rustterm::project::Project;
use rustterm::{input, ui};
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let (events_tx, events_rx) = mpsc::channel::<PaneEvent>();

    let roots = project_roots_from_args();
    let mut app = App::new(events_tx.clone());
    for root in &roots {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());
        app.projects.push(Project::new(name, root.clone()));
    }

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    // Only the first project gets an initial pane; others spawn on activation.
    if let Some(first) = roots.first() {
        let first_pane = Pane::spawn(
            app.alloc_pane_id(),
            "pane-0".to_string(),
            rows,
            cols,
            Some(first),
            events_tx,
            None,
        )?;
        app.projects[0].panes.push(first_pane);
    }

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &events_rx);
    ratatui::restore();
    result
}

fn project_roots_from_args() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    for arg in std::env::args().skip(1) {
        match std::fs::canonicalize(&arg) {
            Ok(p) if p.is_dir() => roots.push(p),
            _ => eprintln!("rustterm: skipping {arg:?} — not a directory"),
        }
    }
    if roots.is_empty() {
        roots.push(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    }
    roots
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
                            pane.waiting = false;
                            pane.running = false;
                        }
                    }
                }
            }
        }

        if app.last_watch_poll.elapsed() >= rustterm::watcher::POLL {
            app.last_watch_poll = Instant::now();
            let now = Instant::now();
            let mut pending = Vec::new();
            for project in app.projects.iter_mut() {
                for pane in project.panes.iter_mut() {
                    if pane.exited.is_some() {
                        continue;
                    }
                    let text = match pane.parser.lock() {
                        Ok(p) => p.screen().contents(),
                        Err(_) => continue,
                    };
                    for e in pane.watcher.update(now, &text) {
                        pending.push((pane.id, e));
                    }
                    pane.running = pane.watcher.is_running();
                }
            }
            for (id, e) in pending {
                rustterm::notify::dispatch(app, id, e);
            }
        }

        app.clear_focused_badges();

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
