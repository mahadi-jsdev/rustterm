use crate::app::{App, InputMode, LinePurpose};
use crate::keys::key_event_to_bytes;
use crate::notify;
use crate::palette::{self, Palette};
use crate::text_input::{EditResult, LineEdit};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

const SCROLL_LINES: usize = 3;

/// Route a mouse event. Wheel events scroll the scrollback of the pane
/// under the cursor — unless that pane's app enabled mouse reporting,
/// in which case they're forwarded to the PTY in its requested encoding.
pub fn handle_mouse(app: &mut App, mouse: MouseEvent, frame_area: Rect) {
    let up = match mouse.kind {
        MouseEventKind::ScrollUp => true,
        MouseEventKind::ScrollDown => false,
        _ => return,
    };
    let Some(project) = app.active_project_mut() else {
        return;
    };
    let (_, main, _) = crate::layout::frame_areas(frame_area);
    let rects = crate::layout::pane_rects(main, project.panes.len(), project.col_split, project.row_split);
    let pos = Position::new(mouse.column, mouse.row);
    let Some((pane, rect)) = project
        .panes
        .iter()
        .zip(rects.iter())
        .find(|(_, r)| r.contains(pos))
    else {
        return;
    };
    if pane.mouse_reporting() {
        // Forward only when the cursor is on a real app cell (inside the
        // border); coords are 1-based relative to the pane's inner area.
        let inner_right = rect.x + rect.width.saturating_sub(1);
        let inner_bottom = rect.y + rect.height.saturating_sub(1);
        if mouse.column > rect.x && mouse.column < inner_right
            && mouse.row > rect.y && mouse.row < inner_bottom
        {
            let x = mouse.column - rect.x;
            let y = mouse.row - rect.y;
            let _ = pane.write_input(&wheel_bytes(up, x, y, pane.mouse_sgr_encoding()));
        }
    } else if up {
        pane.scroll_up(SCROLL_LINES);
    } else {
        pane.scroll_down(SCROLL_LINES);
    }
}

/// Encode a wheel event for a pane whose app enabled mouse reporting:
/// SGR (1006) `\x1b[<btn;x;yM`, or legacy X10 `\x1b[M` + three +32 bytes.
fn wheel_bytes(up: bool, x: u16, y: u16, sgr: bool) -> Vec<u8> {
    let btn: u8 = if up { 64 } else { 65 };
    if sgr {
        format!("\x1b[<{btn};{x};{y}M").into_bytes()
    } else {
        vec![0x1b, b'[', b'M', btn + 32, (x.min(223) as u8) + 32, (y.min(223) as u8) + 32]
    }
}

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
                        pane.scroll_to_bottom();
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
                    let mut edit = LineEdit::new();
                    edit.suggestions = crate::completion::dir_candidates("");
                    app.line_input = Some(edit);
                    app.mode = InputMode::LineInput(LinePurpose::AddProject);
                }
                KeyCode::Char('g') => app.enter_sidebar(),
                KeyCode::Char('G') => app.spawn_pane(Some("lazygit")),
                KeyCode::Char('f') => app.open_finder(),
                KeyCode::Char('/') => {
                    app.line_input = Some(LineEdit::new());
                    app.mode = InputMode::LineInput(LinePurpose::Search);
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
        InputMode::Sidebar => {
            // Ctrl+A works from every non-text-entry mode — re-enter Leader
            // before any sidebar-local key handling.
            if key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.mode = InputMode::Leader;
                return;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('g') => {
                    app.mode = InputMode::Normal;
                }
                KeyCode::Char('j') | KeyCode::Down => app.sidebar_move(1),
                KeyCode::Char('k') | KeyCode::Up => app.sidebar_move(-1),
                KeyCode::Char('b') => app.sidebar_toggle_branches(),
                KeyCode::Char('c') => app.start_ai_commit(),
                KeyCode::Enter => {
                    if app.sidebar_branches {
                        if let (Some(root), Some(branch)) =
                            (app.active_root(), app.sidebar_selected_branch())
                        {
                            match crate::git::switch(&root, &branch) {
                                Ok(()) => {
                                    app.git_status = crate::git::status(&root);
                                    app.flash(format!("switched to {branch}"));
                                }
                                Err(e) => app.flash(format!("git switch: {e}")),
                            }
                        }
                    } else if let Some(file) = app.sidebar_selected_file() {
                        let cmd = format!(
                            "git --no-pager diff --color=always -- {}",
                            crate::app::shell_quote(&file)
                        );
                        app.spawn_pane(Some(&cmd));
                    }
                }
                _ => {}
            }
        }
        InputMode::Finder => {
            let Some(f) = app.finder.as_mut() else {
                app.mode = InputMode::Normal;
                return;
            };
            match key.code {
                KeyCode::Esc => {
                    app.finder = None;
                    app.mode = InputMode::Normal;
                }
                KeyCode::Enter => {
                    if let Some(path) = f.selected_path() {
                        // $VISUAL → $EDITOR → nvim (spec: nvim-primary, no vi).
                        let editor = std::env::var("VISUAL")
                            .ok()
                            .filter(|s| !s.is_empty())
                            .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.is_empty()))
                            .unwrap_or_else(|| "nvim".to_string());
                        let cmd =
                            format!("{} {}", editor, crate::app::shell_quote(&path.to_string_lossy()));
                        app.finder = None;
                        app.mode = InputMode::Normal;
                        app.spawn_pane(Some(&cmd));
                    }
                }
                KeyCode::Up => f.move_prev(),
                KeyCode::Down => f.move_next(),
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => f.move_prev(),
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => f.move_next(),
                KeyCode::Backspace => {
                    let mut q = f.query.clone();
                    q.pop();
                    f.set_query(q);
                }
                KeyCode::Char(c) => {
                    let mut q = f.query.clone();
                    q.push(c);
                    f.set_query(q);
                }
                _ => {}
            }
        }
        InputMode::Search => match key.code {
            KeyCode::Char('n') => app.search_next(),
            KeyCode::Char('N') => app.search_prev(),
            _ => app.exit_search(), // Esc, Enter, and ANY other key exits
        },
        InputMode::LineInput(purpose) => {
            let Some(edit) = app.line_input.as_mut() else {
                app.mode = InputMode::Normal;
                return;
            };
            if key.code == KeyCode::Tab && matches!(purpose, LinePurpose::AddProject) {
                let mut buf = edit.as_str().to_string();
                crate::completion::complete(&mut buf);
                edit.set_text(buf);
                edit.suggestions = crate::completion::dir_candidates(edit.as_str());
                return;
            }
            let result = edit.handle_key(key);
            if matches!(purpose, LinePurpose::AddProject) && matches!(result, EditResult::Editing) {
                edit.suggestions = crate::completion::dir_candidates(edit.as_str());
            }
            match result {
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
                    LinePurpose::CommitMsg => {
                        let msg = text.trim().to_string();
                        if let Some(root) = app.active_root() {
                            if msg.is_empty() {
                                app.flash("commit aborted: empty message");
                            } else {
                                match crate::git::commit(&root, &msg) {
                                    Ok(hash) => app.flash(format!("committed {hash}")),
                                    Err(e) => app.flash(format!("git commit: {e}")),
                                }
                            }
                        }
                        app.line_input = None;
                        app.mode = InputMode::Normal;
                    }
                    LinePurpose::Search => {
                        app.line_input = None;
                        app.start_search(text.trim());
                        // start_search sets mode=Search on match; ensure Normal on empty:
                        if !matches!(app.mode, InputMode::Search) {
                            app.mode = InputMode::Normal;
                        }
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
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
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

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn pane_scrollback(app: &App) -> usize {
        app.active_project().unwrap().panes[0]
            .parser
            .lock()
            .unwrap()
            .screen()
            .scrollback()
    }

    fn grow_scrollback(app: &App) {
        let pane = &app.active_project().unwrap().panes[0];
        let mut p = pane.parser.lock().unwrap();
        for _ in 0..40 {
            p.process(b"line\r\n");
        }
    }

    #[test]
    fn wheel_bytes_encodes_sgr_and_legacy() {
        assert_eq!(wheel_bytes(true, 5, 7, true), b"\x1b[<64;5;7M".to_vec());
        assert_eq!(wheel_bytes(false, 5, 7, true), b"\x1b[<65;5;7M".to_vec());
        assert_eq!(
            wheel_bytes(true, 5, 7, false),
            vec![0x1b, b'[', b'M', 96, 37, 39]
        );
    }

    #[test]
    fn wheel_scrolls_the_pane_under_the_cursor() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        grow_scrollback(&app);
        let frame = Rect::new(0, 0, 80, 24);
        // A single pane fills the main area (right of the 24-col sidebar).
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 40, 10), frame);
        assert_eq!(pane_scrollback(&app), SCROLL_LINES);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 40, 10), frame);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 40, 10), frame);
        assert_eq!(pane_scrollback(&app), SCROLL_LINES);
    }

    #[test]
    fn wheel_over_sidebar_or_status_bar_does_not_scroll() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        grow_scrollback(&app);
        let frame = Rect::new(0, 0, 80, 24);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 5, 10), frame); // sidebar
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 40, 23), frame); // status row
        assert_eq!(pane_scrollback(&app), 0);
    }

    #[test]
    fn wheel_is_forwarded_when_the_pane_app_reports_mouse() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            // App enables SGR mouse reporting; wheel events then go to the
            // PTY and must not move the local scrollback offset.
            pane.parser.lock().unwrap().process(b"\x1b[?1006h\x1b[?1000h");
        }
        grow_scrollback(&app);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 40, 10), Rect::new(0, 0, 80, 24));
        assert_eq!(pane_scrollback(&app), 0);
    }

    #[test]
    fn tab_completes_unique_dir_and_descends() {
        let root = std::env::temp_dir().join(format!("rustterm-input-compl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("projapp/src")).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();

        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('c'), KeyModifiers::NONE));
        for c in format!("{}/proj", root.display()).chars() {
            handle_key(&mut app, key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        handle_key(&mut app, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            app.line_input.as_ref().unwrap().as_str(),
            format!("{}/projapp/", root.display())
        );
        // Descend: suggestions now list projapp's subdirs.
        assert_eq!(app.line_input.as_ref().unwrap().suggestions, vec!["src"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tab_on_ambiguous_prefix_extends_to_common_prefix() {
        let root = std::env::temp_dir().join(format!("rustterm-input-amb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        std::fs::create_dir_all(root.join("alpine")).unwrap();

        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('c'), KeyModifiers::NONE));
        for c in format!("{}/al", root.display()).chars() {
            handle_key(&mut app, key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        handle_key(&mut app, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            app.line_input.as_ref().unwrap().as_str(),
            format!("{}/alp", root.display())
        );
        assert_eq!(
            app.line_input.as_ref().unwrap().suggestions,
            vec!["alpha", "alpine"]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn typing_snaps_pane_back_to_live_view() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        grow_scrollback(&app);
        app.active_project().unwrap().panes[0].scroll_up(5);
        assert_eq!(pane_scrollback(&app), 5);
        handle_key(&mut app, key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(pane_scrollback(&app), 0);
    }

    #[test]
    fn leader_g_enters_sidebar_and_h_exits() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('g'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Sidebar));
        handle_key(&mut app, key(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
    }

    #[test]
    fn leader_capital_g_spawns_lazygit_pane() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('G'), KeyModifiers::SHIFT));
        let pane = &app.active_project().unwrap().panes[0];
        assert_eq!(pane.startup_command.as_deref(), Some("lazygit"));
    }

    #[test]
    fn leader_f_opens_finder_and_esc_closes() {
        let mut app = app_with_one_project(); // root /tmp — has files
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('f'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Finder));
        assert!(app.finder.is_some());
        handle_key(&mut app, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(app.finder.is_none());
    }

    #[test]
    fn finder_enter_spawns_editor_pane() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('f'), KeyModifiers::NONE));
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        let pane = &app.active_project().unwrap().panes[0];
        assert!(pane.startup_command.is_some());
        let cmd = pane.startup_command.clone().unwrap();
        assert!(cmd.contains("host") || cmd.contains('/'), "expected editor command, got {cmd}");
    }

    #[test]
    fn leader_slash_opens_search_prompt_and_enter_searches() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            pane.parser.lock().unwrap().process(b"searchable\r\n");
        }
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::LineInput(LinePurpose::Search)));
        for c in "searchable".chars() {
            handle_key(&mut app, key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Search));
        handle_key(&mut app, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
    }

    #[test]
    fn search_mode_n_cycles_and_any_key_exits() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            pane.parser.lock().unwrap().process(b"x x x\r\n");
        }
        app.start_search("x");
        handle_key(&mut app, key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Search));
        handle_key(&mut app, key(KeyCode::Char('z'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
    }
}
