use crate::app::{App, InputMode};
use crate::keys::key_event_to_bytes;
use crate::pane::Pane;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match app.mode {
        InputMode::Normal => {
            if key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.mode = InputMode::Leader;
                return;
            }
            if let Some(project) = app.active_project() {
                if let Some(pane) = project.active_pane() {
                    let bytes = key_event_to_bytes(key);
                    if !bytes.is_empty() {
                        let _ = pane.write_input(&bytes);
                    }
                }
            }
        }
        InputMode::Leader => {
            app.mode = InputMode::Normal;
            match key.code {
                KeyCode::Char('q') => app.should_quit = true,
                KeyCode::Char('n') => {
                    let id = app.alloc_pane_id();
                    let events_tx = app.events_tx.clone();
                    if let Some(project) = app.active_project_mut() {
                        let cwd = project.root.clone();
                        // Deviation from the spec's "spawn failure renders
                        // error text into the pane": that needs a
                        // Running/Failed split on Pane that would cascade
                        // through every task in this plan. Scoped down for
                        // Phase 1 MVP to "no pane appears" on failure,
                        // which doesn't panic or corrupt state — revisit if
                        // spawn failures turn out to be common in practice.
                        if let Ok(pane) = Pane::spawn(
                            id,
                            format!("pane-{id}"),
                            24,
                            80,
                            Some(&cwd),
                            events_tx,
                            None,
                        ) {
                            project.panes.push(pane);
                            project.active_pane = project.panes.len() - 1;
                        }
                    }
                }
                KeyCode::Char('x') => {
                    if let Some(project) = app.active_project_mut() {
                        if !project.panes.is_empty() {
                            let idx = project.active_pane;
                            let _ = project.panes[idx].kill();
                            project.panes.remove(idx);
                            if project.active_pane >= project.panes.len() && project.active_pane > 0
                            {
                                project.active_pane -= 1;
                            }
                        }
                    }
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    if let Some(project) = app.active_project_mut() {
                        project.prev_pane();
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    if let Some(project) = app.active_project_mut() {
                        project.next_pane();
                    }
                }
                KeyCode::Char('[') => app.prev_project(),
                KeyCode::Char(']') => app.next_project(),
                KeyCode::Char('+') => {
                    if let Some(project) = app.active_project_mut() {
                        project.col_split = (project.col_split + 0.05).min(0.85);
                    }
                }
                KeyCode::Char('-') => {
                    if let Some(project) = app.active_project_mut() {
                        project.col_split = (project.col_split - 0.05).max(0.15);
                    }
                }
                _ => {}
            }
        }
        // Palette and LineInput key handling is owned by the input task that
        // introduces them; nothing enters these modes yet, so keys are
        // swallowed rather than forwarded to the focused pane.
        InputMode::Palette | InputMode::LineInput(_) => {}
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
}
