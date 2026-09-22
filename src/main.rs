use rustterm::app::App;
use rustterm::pane::{Pane, PaneEvent};
use rustterm::project::Project;
use std::sync::mpsc;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-a" || a == "--attach") {
        if args.len() != 1 {
            eprintln!("rustterm: -a takes no arguments");
            std::process::exit(2);
        }
        return rustterm::daemon::run_attach_client();
    }
    if args.iter().any(|a| a == "-k" || a == "--kill") {
        if args.len() != 1 {
            eprintln!("rustterm: -k takes no arguments");
            std::process::exit(2);
        }
        return rustterm::daemon::send_kill();
    }

    let (events_tx, events_rx) = mpsc::channel::<PaneEvent>();
    // AppEvent channel — drained in the run loop: git-status polls and
    // AI-commit worker results arrive here.
    let (app_tx, app_rx) = mpsc::channel::<rustterm::app::AppEvent>();

    let mut roots = explicit_roots_from_args();
    let mut app = App::new(events_tx.clone(), app_tx.clone());
    // ~/.config/rustterm/config.toml — loaded before any pane spawns
    // (scrollback size) and before session restore. A parse error falls
    // back to defaults and surfaces as a startup flash.
    let (config, config_err) = rustterm::config::load();
    app.config = config;
    if let Some(e) = config_err {
        app.flash(e);
    }
    // Bare `rustterm` (no args at all) restores the saved session; explicit
    // args always win — including the all-invalid-args → cwd fallback.
    if std::env::args().len() == 1 {
        if let Some(session) = rustterm::session::load() {
            app.restore_session(&session);
        }
    }
    if app.projects.is_empty() && roots.is_empty() {
        roots.push(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    }
    for root in &roots {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());
        app.projects.push(Project::new(name, root.clone()));
    }
    app.ensure_files();

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
            app.config.scrollback,
        )?;
        app.projects[0].panes.push(first_pane);
    }

    // Install BEFORE ratatui::init(): fd 0 becomes the internal pty
    // slave, so crossterm's one-shot event source binds the relay's
    // file description — which fork() inherits intact into the keeper.
    let mut relay = rustterm::daemon::InputRelay::install().ok();
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableBracketedPaste
    );
    let result = rustterm::runloop::run(&mut terminal, &mut app, &events_rx, &app_rx, None);
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableBracketedPaste,
        crossterm::event::DisableMouseCapture
    );
    ratatui::restore();

    if app.detach_requested {
        // Prints, saves, binds, quiesces, forks — parent exits, child
        // daemonizes into the keeper loop and never returns.
        return rustterm::daemon::detach(&mut app, &events_rx, &app_rx, relay.as_mut());
    }
    if let Some(r) = relay.as_mut() {
        r.release_tty();
    }
    if let Err(e) = rustterm::session::save(&app) {
        eprintln!("rustterm: session save failed: {e}");
    }
    result
}

fn explicit_roots_from_args() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    for arg in std::env::args().skip(1) {
        match std::fs::canonicalize(&arg) {
            Ok(p) if p.is_dir() => roots.push(p),
            _ => eprintln!("rustterm: skipping {arg:?} — not a directory"),
        }
    }
    roots
}
