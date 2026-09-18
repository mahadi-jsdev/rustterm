use crate::app::{App, InputMode, LinePurpose};
use crate::keys::key_event_to_bytes;
use crate::notify;
use crate::palette::{self, Palette};
use crate::text_input::{EditResult, LineEdit};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match app.mode {
        InputMode::Normal => {
            if key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.mode = InputMode::Leader;
                return;
            }
            let mut pending = Vec::new();
            if let Some(project) = app.active_project_mut() {
                if let Some(pane) = project.active_pane_mut() {
                    let bytes = key_event_to_bytes(key);
                    if !bytes.is_empty() {
                        for e in pane.watcher.on_input(&bytes) {
                            pending.push((pane.id, e));
                        }
                        let _ = pane.write_input(&bytes);
                    }
                }
            }
            for (id, e) in pending {
                notify::dispatch(app, id, e);
            }
        }
        InputMode::Leader => {
            app.mode = InputMode::Normal;
            match key.code {
                KeyCode::Char('q') => app.should_quit = true,
                KeyCode::Char('n') => app.spawn_pane(None),
                KeyCode::Char('x') => app.close_active_pane(),
                KeyCode::Left | KeyCode::Char('h') => {
                    if let Some(p) = app.active_project_mut() {
                        p.prev_pane();
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    if let Some(p) = app.active_project_mut() {
                        p.next_pane();
                    }
                }
                KeyCode::Char('[') => app.prev_project(),
                KeyCode::Char(']') => app.next_project(),
                KeyCode::Char('+') => app.adjust_split(0.05),
                KeyCode::Char('-') => app.adjust_split(-0.05),
                KeyCode::Char(':') => {
                    app.palette = Some(Palette::open(app));
                    app.mode = InputMode::Palette;
                }
                KeyCode::Char('c') => {
                    app.line_input = Some(LineEdit::new());
                    app.mode = InputMode::LineInput(LinePurpose::AddProject);
                }
                _ => {}
            }
            // Project switch lands on ensure-pane.
            if matches!(key.code, KeyCode::Char('[') | KeyCode::Char(']')) {
                app.ensure_active_pane();
            }
        }
        InputMode::Palette => {
            let Some(pal) = app.palette.as_mut() else {
                app.mode = InputMode::Normal;
                return;
            };
            match key.code {
                KeyCode::Esc => {
                    app.palette = None;
                    app.mode = InputMode::Normal;
                }
                KeyCode::Enter => {
                    let cmd = pal.selected_command().map(|c| c.id.clone());
                    if let Some(id) = cmd {
                        palette::execute(app, &id);
                    }
                    if !matches!(app.mode, InputMode::LineInput(_)) {
                        app.mode = InputMode::Normal;
                    }
                    app.palette = None;
                }
                KeyCode::Up => pal.move_prev(),
                KeyCode::Down => pal.move_next(),
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => pal.move_prev(),
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => pal.move_next(),
                KeyCode::Backspace => {
                    let mut q = pal.query.clone();
                    q.pop();
                    pal.set_query(q);
                }
                KeyCode::Char(c) => {
                    let mut q = pal.query.clone();
                    q.push(c);
                    pal.set_query(q);
                }
                _ => {}
            }
        }
        InputMode::LineInput(purpose) => {
            let Some(edit) = app.line_input.as_mut() else {
                app.mode = InputMode::Normal;
                return;
            };
            match edit.handle_key(key) {
                EditResult::Editing => {}
                EditResult::Cancel => {
                    app.line_input = None;
                    app.mode = InputMode::Normal;
                }
                EditResult::Submit(text) => match purpose {
                    LinePurpose::AddProject => {
                        let path = crate::app::expand_tilde(text.trim());
                        if path.is_dir() {
                            app.line_input = None;
                            app.mode = InputMode::Normal;
                            app.add_project(path);
                        } else {
                            app.line_input
                                .as_mut()
                                .unwrap()
                                .set_error(format!("not a directory: {}", path.display()));
                        }
                    }
                    LinePurpose::RenamePane => {
                        if let Some(project) = app.active_project_mut() {
                            if let Some(pane) = project.active_pane_mut() {
                                pane.title = if text.trim().is_empty() {
                                    format!("pane-{}", pane.id)
                                } else {
                                    text.trim().to_string()
                                };
                            }
                        }
                        app.line_input = None;
                        app.mode = InputMode::Normal;
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn app_with_one_project() -> App {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_a_enters_leader_mode() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, InputMode::Leader));
    }

    #[test]
    fn leader_then_q_sets_should_quit() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(app.should_quit);
        assert!(matches!(app.mode, InputMode::Normal), "mode should revert after a leader command");
    }

    #[test]
    fn leader_then_bracket_switches_project() {
        let mut app = app_with_one_project();
        app.projects.push(Project::new("second".into(), PathBuf::from("/tmp")));

        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char(']'), KeyModifiers::NONE));
        assert_eq!(app.active_project, 1);
    }

    #[test]
    fn leader_then_n_spawns_a_pane_in_the_active_project() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('n'), KeyModifiers::NONE));

        let project = app.active_project().unwrap();
        assert_eq!(project.panes.len(), 1);
        assert_eq!(project.active_pane, 0);
    }

    #[test]
    fn leader_then_x_closes_the_active_pane() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(app.active_project().unwrap().panes.len(), 1);

        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.active_project().unwrap().panes.len(), 0);
    }

    #[test]
    fn normal_mode_plain_key_is_a_noop_with_no_active_pane() {
        let mut app = app_with_one_project();
        // Should not panic even though the active project has zero panes.
        handle_key(&mut app, key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
    }

    #[test]
    fn leader_colon_opens_palette_and_enter_executes() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char(':'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Palette));
        assert!(app.palette.is_some());

        // type "new" then Enter — "New pane" should be the top hit
        for c in "new".chars() {
            handle_key(&mut app, key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(app.active_project().unwrap().panes.len(), 1);
    }

    #[test]
    fn esc_closes_palette_without_executing() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char(':'), KeyModifiers::NONE));
        handle_key(&mut app, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(app.palette.is_none());
        assert!(app.active_project().unwrap().panes.is_empty());
    }

    #[test]
    fn leader_c_opens_project_path_input_and_submit_adds_project() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::LineInput(LinePurpose::AddProject)));

        // /etc not /tmp — the fixture project is already rooted at /tmp and
        // add_project dedupes on canonicalized root.
        for c in "/etc".chars() {
            handle_key(&mut app, key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(app.projects.len(), 2);
    }

    #[test]
    fn invalid_project_path_sets_error_and_stays_open() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('c'), KeyModifiers::NONE));
        for c in "/definitely/not/here".chars() {
            handle_key(&mut app, key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::LineInput(LinePurpose::AddProject)));
        assert!(app.line_input.as_ref().unwrap().error().is_some());
    }

    #[test]
    fn typed_input_is_fed_to_the_pane_watcher() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        handle_key(&mut app, key(KeyCode::Char('l'), KeyModifiers::NONE));
        handle_key(&mut app, key(KeyCode::Char('s'), KeyModifiers::NONE));
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        let pane = &app.active_project().unwrap().panes[0];
        assert_eq!(pane.watcher.last_command(), "ls");
    }
}
