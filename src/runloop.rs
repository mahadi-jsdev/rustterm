//! The interactive run loop — shared by the foreground process and the
//! keeper's attached phase. `tick()` is the non-UI half; the detached
//! daemon loop calls it alone.

use crate::app::{App, AppEvent};
use crate::pane::PaneEvent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

pub fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    events_rx: &mpsc::Receiver<PaneEvent>,
    app_rx: &mpsc::Receiver<AppEvent>,
    client_dead: Option<Arc<AtomicBool>>,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| crate::ui::draw(frame, app))?;

        if crossterm::event::poll(Duration::from_millis(50))? {
            match crossterm::event::read()? {
                crossterm::event::Event::Key(key) => {
                    if key.kind == crossterm::event::KeyEventKind::Press {
                        crate::input::handle_key(app, key);
                    }
                }
                crossterm::event::Event::Mouse(mouse) => {
                    if let Ok(size) = terminal.size() {
                        let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                        crate::input::handle_mouse(app, mouse, area);
                    }
                }
                _ => {}
            }
        }

        tick(app, events_rx, app_rx);

        if app.should_quit
            || app.detach_requested
            || client_dead
                .as_ref()
                .map(|d| d.load(Ordering::SeqCst))
                .unwrap_or(false)
        {
            break;
        }
    }
    Ok(())
}

/// Non-UI work per loop turn — PTY event reaping, AppEvent dispatch,
/// watcher poll, git-status poll, badge clearing. The keeper's detached
/// phase runs exactly this (no draw, no input).
pub fn tick(
    app: &mut App,
    events_rx: &mpsc::Receiver<PaneEvent>,
    app_rx: &mpsc::Receiver<AppEvent>,
) {
    while let Ok(event) = events_rx.try_recv() {
        if let PaneEvent::Exited(id) = event {
            for project in app.projects.iter_mut() {
                for pane in project.panes.iter_mut() {
                    if pane.id == id {
                        pane.reap();
                        pane.waiting = false;
                        pane.running = false;
                    }
                }
            }
        }
    }

    while let Ok(event) = app_rx.try_recv() {
        match event {
            AppEvent::GitStatus { root, status } => {
                app.git_poll_in_flight = false;
                if app.active_root().as_ref() == Some(&root) {
                    app.git_status = status;
                }
            }
            AppEvent::AiMessage { root, result } => {
                app.ai_in_flight = false;
                match result {
                    Ok(msg) => {
                        // A prompt is already open (AddProject / RenamePane /
                        // Search) — don't clobber its buffer. The user can
                        // re-run ai-commit once it's closed.
                        if app.line_input.is_some() {
                            app.flash("ai commit ready — re-run after current prompt");
                        } else {
                            if let Some(project) = app.active_project_mut() {
                                if let Some(pane) = project.active_pane_mut() {
                                    pane.search = None; // clear orphaned search state
                                }
                            }
                            if matches!(
                                app.mode,
                                crate::app::InputMode::Search
                                    | crate::app::InputMode::Finder
                                    | crate::app::InputMode::Sidebar
                            ) {
                                app.finder = None;
                                app.mode = crate::app::InputMode::Normal;
                            }
                            // The commit targets the polled root — the user
                            // may have switched projects mid-request.
                            app.commit_root = Some(root);
                            app.line_input = Some(crate::text_input::LineEdit::from_str(&msg));
                            app.mode = crate::app::InputMode::LineInput(
                                crate::app::LinePurpose::CommitMsg,
                            );
                        }
                    }
                    Err(e) => app.flash(format!("ai commit: {e}")),
                }
            }
        }
    }

    if app.last_watch_poll.elapsed() >= crate::watcher::POLL {
        app.last_watch_poll = Instant::now();
        let now = Instant::now();
        let mut pending = Vec::new();
        for project in app.projects.iter_mut() {
            for pane in project.panes.iter_mut() {
                if pane.is_dead() {
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
            crate::notify::dispatch(app, id, e);
        }
    }

    // 1s git-status poll — one worker in flight at a time; the result
    // arrives on app_rx and is applied only if the root still matches.
    if app.last_git_poll.elapsed() >= Duration::from_secs(1) && !app.git_poll_in_flight {
        app.last_git_poll = Instant::now();
        if let Some(root) = app.active_root() {
            app.git_poll_in_flight = true;
            let tx = app.app_tx.clone();
            std::thread::spawn(move || {
                let status = crate::git::status(&root);
                let _ = tx.send(AppEvent::GitStatus { root, status });
            });
        }
    }

    app.clear_focused_badges();
}
