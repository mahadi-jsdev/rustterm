use crate::app::{App, InputMode, LinePurpose};
use crate::keys::key_event_to_bytes;
use crate::notify;
use crate::palette::{self, Palette};
use crate::text_input::{EditResult, LineEdit};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

/// Route a mouse event. Wheel scrolls the pane under the cursor (or
/// forwards to it when the app enabled mouse reporting). Left press
/// focuses panes and drives the sidebar AND anchors a text selection:
/// drag extends it (auto-scrolling scrollback at the pane's top/bottom
/// edge), release yanks it to the clipboard via OSC52. Apps with mouse
/// reporting get press/drag/release forwarded; Shift bypasses to local
/// select, same as alacritty/tmux.
pub fn handle_mouse(app: &mut App, mouse: MouseEvent, frame_area: Rect) {
    match mouse.kind {
        MouseEventKind::ScrollUp => wheel_scroll(app, mouse, frame_area, true),
        MouseEventKind::ScrollDown => wheel_scroll(app, mouse, frame_area, false),
        MouseEventKind::Down(MouseButton::Left) => mouse_down(app, mouse, frame_area),
        MouseEventKind::Drag(MouseButton::Left) => mouse_drag(app, mouse, frame_area),
        MouseEventKind::Up(MouseButton::Left) => mouse_up(app, mouse, frame_area),
        _ => {}
    }
}

/// (main grid area, float overlay area) — the two regions selection
/// coordinates can come from; the status bar is never selectable.
fn selectable_areas(app: &App, frame_area: Rect) -> (Rect, Rect) {
    let overlay = Rect {
        height: frame_area.height.saturating_sub(1),
        ..frame_area
    };
    let (_, main, _) =
        crate::layout::frame_areas(frame_area, app.sidebar_visible, app.config.sidebar_width);
    (main, overlay)
}

/// Left press: focus/sidebar via `click`, then anchor a mouse selection
/// on the pane under the cursor — or hand the press to its app when it
/// reports mouse events and Shift isn't held. Floats are modal: only
/// the top float's interior can anchor/select.
fn mouse_down(app: &mut App, mouse: MouseEvent, frame_area: Rect) {
    app.mouse_sel = None;
    app.mouse_dragging = false;
    app.mouse_app = None;
    if matches!(
        app.mode,
        InputMode::Palette | InputMode::Finder | InputMode::LineInput(_)
    ) {
        return;
    }
    let pos = Position::new(mouse.column, mouse.row);
    let (main, overlay) = selectable_areas(app, frame_area);

    // Copy mode: a click inside the copy pane places its cursor.
    if matches!(app.mode, InputMode::Copy) {
        if let Some(id) = app.copy.as_ref().map(|c| c.pane_id) {
            if let Some(rect) = app.pane_screen_rect(id, main, overlay) {
                if let Some(cell) = app.cell_in_pane(id, pos, rect) {
                    if let Some(copy) = app.copy.as_mut() {
                        copy.cursor = cell;
                    }
                }
            }
        }
        return;
    }

    let top_float = app
        .active_project()
        .and_then(|p| p.top_float())
        .map(|f| f.id);
    let target = if let Some(fid) = top_float {
        // Modal: the float interior is the only selectable target.
        app.pane_screen_rect(fid, main, overlay)
            .and_then(|r| app.cell_in_pane(fid, pos, r).map(|cell| (fid, r, cell)))
    } else {
        // Sidebar/status-bar presses are UI clicks — never selections.
        click(app, mouse, frame_area);
        let Some(project) = app.active_project() else { return };
        let render = project.render_indices();
        let rects =
            crate::layout::pane_rects(main, render.len(), project.col_split, project.row_split);
        rects
            .iter()
            .enumerate()
            .find(|(_, r)| r.contains(pos))
            .and_then(|(vi, r)| {
                let id = project.panes[render[vi]].id;
                app.cell_in_pane(id, pos, *r).map(|cell| (id, *r, cell))
            })
    };
    let Some((id, rect, cell)) = target else { return };
    let Some(pane) = app.pane_by_id(id) else { return };
    if pane.mouse_reporting() && !mouse.modifiers.contains(KeyModifiers::SHIFT) {
        // The app owns the mouse: forward the press, remember it owns
        // drag/release until the button comes up.
        let sgr = pane.mouse_sgr_encoding();
        let x = pos.x - rect.x;
        let y = pos.y - rect.y;
        let _ = pane.write_input(&btn_bytes(0, false, x, y, sgr));
        app.mouse_app = Some(id);
    } else {
        app.mouse_sel = Some(crate::copy::CopyState {
            pane_id: id,
            cursor: cell,
            anchor: Some(cell),
        });
        app.mouse_dragging = true;
    }
}

/// Left drag: extends an in-progress selection — overshooting the
/// pane's top/bottom edge scrolls it so long scrollback selections
/// work — or forwards motion to the mouse-reporting app that took the
/// press. In copy mode a drag moves the cursor (anchoring on first
/// move so drag-select works there too).
fn mouse_drag(app: &mut App, mouse: MouseEvent, frame_area: Rect) {
    let pos = Position::new(mouse.column, mouse.row);
    let (main, overlay) = selectable_areas(app, frame_area);
    if app.mouse_dragging {
        let Some(id) = app.mouse_sel.as_ref().map(|s| s.pane_id) else { return };
        let Some(rect) = app.pane_screen_rect(id, main, overlay) else {
            app.mouse_sel = None;
            app.mouse_dragging = false;
            return;
        };
        let inner = Rect {
            x: rect.x + 1,
            y: rect.y + 1,
            width: rect.width.saturating_sub(2),
            height: rect.height.saturating_sub(2),
        };
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        // Edge auto-scroll: overshoot above/below the content area
        // scrolls scrollback (capped so a wild flick doesn't jump far),
        // then the clamped position selects to the edge row.
        if let Some(pane) = app.pane_by_id(id) {
            if pos.y < inner.y {
                pane.scroll_up((inner.y - pos.y) as usize);
            } else if pos.y >= inner.y + inner.height {
                pane.scroll_down((pos.y - (inner.y + inner.height - 1)) as usize);
            }
        }
        let clamped = Position::new(
            pos.x.clamp(inner.x, inner.x + inner.width - 1),
            pos.y.clamp(inner.y, inner.y + inner.height - 1),
        );
        if let Some(cell) = app.cell_in_pane(id, clamped, rect) {
            if let Some(sel) = app.mouse_sel.as_mut() {
                sel.cursor = cell;
            }
        }
        return;
    }
    if let Some(id) = app.mouse_app {
        let Some(rect) = app.pane_screen_rect(id, main, overlay) else {
            app.mouse_app = None;
            return;
        };
        let Some(pane) = app.pane_by_id(id) else {
            app.mouse_app = None;
            return;
        };
        let x = pos.x.clamp(rect.x + 1, rect.x + rect.width - 2) - rect.x;
        let y = pos.y.clamp(rect.y + 1, rect.y + rect.height - 2) - rect.y;
        let _ = pane.write_input(&btn_bytes(32, false, x, y, pane.mouse_sgr_encoding()));
        return;
    }
    if matches!(app.mode, InputMode::Copy) {
        if let Some(id) = app.copy.as_ref().map(|c| c.pane_id) {
            if let Some(rect) = app.pane_screen_rect(id, main, overlay) {
                if let Some(cell) = app.cell_in_pane(id, pos, rect) {
                    if let Some(copy) = app.copy.as_mut() {
                        if copy.anchor.is_none() {
                            copy.anchor = Some(copy.cursor);
                        }
                        copy.cursor = cell;
                    }
                }
            }
        }
    }
}

/// Left release: a real drag (anchor ≠ cursor) yanks via OSC52 and the
/// highlight lingers; a same-cell release was just a click, so the
/// press-time anchor is dropped. A mouse-app press gets its release.
fn mouse_up(app: &mut App, mouse: MouseEvent, frame_area: Rect) {
    if let Some(id) = app.mouse_app.take() {
        if let (Some(rect), Some(pane)) = (
            app.pane_screen_rect(id, selectable_areas(app, frame_area).0,
                selectable_areas(app, frame_area).1),
            app.pane_by_id(id),
        ) {
            let pos = Position::new(mouse.column, mouse.row);
            let x = pos.x.clamp(rect.x + 1, rect.x + rect.width - 2) - rect.x;
            let y = pos.y.clamp(rect.y + 1, rect.y + rect.height - 2) - rect.y;
            let _ = pane.write_input(&btn_bytes(0, true, x, y, pane.mouse_sgr_encoding()));
        }
    }
    if app.mouse_dragging {
        app.mouse_dragging = false;
        let dragged = app
            .mouse_sel
            .as_ref()
            .map(|s| s.anchor != Some(s.cursor))
            .unwrap_or(false);
        if dragged {
            app.mouse_yank();
        }
        // The selection is done — copied or not, release clears it.
        app.mouse_sel = None;
    }
}

fn wheel_scroll(app: &mut App, mouse: MouseEvent, frame_area: Rect, up: bool) {
    let sidebar_visible = app.sidebar_visible;
    // Hoisted before the project borrow — NLL rejects self reads inside it.
    let scroll = app.config.scroll_lines;
    let float_pct = app.config.float_pct;
    let sidebar_width = app.config.sidebar_width;
    let Some(project) = app.active_project_mut() else {
        return;
    };
    let pos = Position::new(mouse.column, mouse.row);
    // A float is modal — wheel inside it scrolls/forwards to the float,
    // wheel outside is swallowed rather than scrolling the grid behind.
    if let Some(float) = project.top_float() {
        let overlay = Rect {
            height: frame_area.height.saturating_sub(1),
            ..frame_area
        };
        let rect = crate::layout::float_rect(overlay, project.floats.len() - 1, float_pct);
        if !rect.contains(pos) {
            return;
        }
        if float.mouse_reporting() {
            if mouse.column > rect.x
                && mouse.column < rect.x + rect.width - 1
                && mouse.row > rect.y
                && mouse.row < rect.y + rect.height - 1
            {
                let x = mouse.column - rect.x;
                let y = mouse.row - rect.y;
                let _ = float.write_input(&wheel_bytes(up, x, y, float.mouse_sgr_encoding()));
            }
        } else if up {
            float.scroll_up(scroll);
        } else {
            float.scroll_down(scroll);
        }
        return;
    }
    let (_, main, _) = crate::layout::frame_areas(frame_area, sidebar_visible, sidebar_width);
    let render = project.render_indices();
    let visible: Vec<&crate::pane::Pane> = render.iter().map(|&i| &project.panes[i]).collect();
    let rects = crate::layout::pane_rects(main, visible.len(), project.col_split, project.row_split);
    let Some((pane, rect)) = visible
        .iter()
        .zip(rects.iter())
        .find(|(_, r)| r.contains(pos))
    else {
        return;
    };
    if pane.mouse_reporting() {
        // Forward only when the cursor is on a real app cell (inside the
        // border); coords are 1-based relative to the pane's inner area.
        // Dropped facing borders are content cells, not boundary.
        let borders = crate::layout::pane_borders(&rects, *rect);
        let inner_right =
            rect.x + rect.width - u16::from(borders.contains(ratatui::widgets::Borders::RIGHT));
        let inner_bottom =
            rect.y + rect.height - u16::from(borders.contains(ratatui::widgets::Borders::BOTTOM));
        if mouse.column > rect.x && mouse.column < inner_right
            && mouse.row > rect.y && mouse.row < inner_bottom
        {
            let x = mouse.column - rect.x;
            let y = mouse.row - rect.y;
            let _ = pane.write_input(&wheel_bytes(up, x, y, pane.mouse_sgr_encoding()));
        }
    } else if up {
        pane.scroll_up(scroll);
    } else {
        pane.scroll_down(scroll);
    }
}

/// Left-click: a pane focuses it (and exits Sidebar/Leader back to
/// Normal); the sidebar drives selection/activation. Modal overlays
/// (palette/finder/line input) swallow clicks — Esc dismisses those.
fn click(app: &mut App, mouse: MouseEvent, frame_area: Rect) {
    if matches!(
        app.mode,
        InputMode::Palette | InputMode::Finder | InputMode::LineInput(_)
    ) {
        return;
    }
    // Floats are modal — clicks can't reach the grid or sidebar behind.
    if app
        .active_project()
        .map(|p| p.top_float().is_some())
        .unwrap_or(false)
    {
        return;
    }
    let pos = Position::new(mouse.column, mouse.row);
    let (sidebar, main, _) =
        crate::layout::frame_areas(frame_area, app.sidebar_visible, app.config.sidebar_width);
    if app.sidebar_visible && sidebar.contains(pos) {
        click_sidebar(app, pos, sidebar);
        return;
    }
    if !main.contains(pos) {
        return;
    }
    let Some(project) = app.active_project_mut() else {
        return;
    };
    let visible: Vec<usize> = project.render_indices();
    let rects =
        crate::layout::pane_rects(main, visible.len(), project.col_split, project.row_split);
    if let Some((vi, _)) = rects.iter().enumerate().find(|(_, r)| r.contains(pos)) {
        project.active_pane = visible[vi];
        project.zoom_follow(); // a zoomed view tracks the clicked pane
    }
    app.mode = InputMode::Normal;
}

/// Click inside the sidebar: the "Projects" title row collapses the
/// panel, project rows switch projects, the git block's title row
/// toggles files↔branches, and git rows select — a click on the
/// already-selected row activates it (same as Enter).
fn click_sidebar(app: &mut App, pos: Position, sidebar: Rect) {
    if pos.y == sidebar.y {
        app.toggle_sidebar();
        return;
    }
    let project_h = (app.projects.len() as u16 + 2).min(10).min(sidebar.height);
    let git_top = sidebar.y + project_h;
    if pos.y < git_top {
        let row = pos.y - sidebar.y - 1;
        if (row as usize) < app.projects.len() {
            app.set_active_project(row as usize);
        }
        app.enter_sidebar();
        // A project click focuses the Projects section — the git panel
        // isn't what you clicked on.
        app.sidebar_focus = crate::app::SidebarSection::Projects;
        return;
    }
    if pos.y == git_top {
        app.enter_sidebar();
        app.sidebar_toggle_branches();
        return;
    }
    let row = (pos.y - git_top - 1) as usize;
    if row < app.sidebar_items_len() {
        if matches!(app.mode, InputMode::Sidebar)
            && app.sidebar_focus == crate::app::SidebarSection::Git
            && app.sidebar_sel == row
        {
            app.sidebar_activate();
        } else {
            if !matches!(app.mode, InputMode::Sidebar) {
                app.enter_sidebar();
            }
            app.sidebar_focus = crate::app::SidebarSection::Git;
            app.sidebar_sel = row;
        }
    } else {
        app.enter_sidebar();
    }
}

/// Encode a button event for a mouse-reporting app: SGR
/// `\x1b[<code;x;yM` (release uses `m`), or legacy X10 `\x1b[M` +
/// btn+32 bytes (release always btn 3; drag adds the 32 motion bit).
/// `code` is 0 for press/release, 32 for left-drag motion.
fn btn_bytes(code: u8, release: bool, x: u16, y: u16, sgr: bool) -> Vec<u8> {
    if sgr {
        let tail = if release { 'm' } else { 'M' };
        format!("\x1b[<{code};{x};{y}{tail}").into_bytes()
    } else {
        let btn = 32 + if release { 3 } else { code };
        vec![0x1b, b'[', b'M', btn, (x as u8).saturating_add(32), (y as u8).saturating_add(32)]
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
            if app.config.is_leader(&key) {
                app.mode = InputMode::Leader;
                return;
            }
            let mut pending = Vec::new();
            if let Some(project) = app.active_project_mut() {
                // A float is modal — it owns keystrokes while it's open.
                // (Two-step lookup: NLL rejects or_else reborrowing project.)
                let pane = if project.top_float().is_some() {
                    project.top_float_mut()
                } else {
                    project.active_pane_mut()
                };
                if let Some(pane) = pane {
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
                // Floats pop first — the popup is the obvious close target.
                KeyCode::Char('x') => {
                    if !app.close_float() {
                        app.close_active_pane();
                    }
                }
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
                KeyCode::Char(':') | KeyCode::Char('p') => {
                    app.palette = Some(Palette::open(app));
                    app.mode = InputMode::Palette;
                }
                KeyCode::Char('c') => {
                    let mut edit = LineEdit::new();
                    edit.suggestions = crate::completion::dir_candidates("");
                    app.line_input = Some(edit);
                    app.mode = InputMode::LineInput(LinePurpose::AddProject);
                }
                KeyCode::Char('b') => app.toggle_sidebar(),
                KeyCode::Char('g') => app.enter_sidebar(),
                // exec'd: `q` in lazygit exits the shell → popup auto-closes.
                KeyCode::Char('G') => app.spawn_float("lazygit", "exec lazygit"),
                KeyCode::Char('L') => app.open_git_log(),
                KeyCode::Char('H') => app.hide_active_pane(),
                KeyCode::Char('z') => {
                    if let Some(p) = app.active_project_mut() {
                        p.zoom_toggle();
                    }
                }
                KeyCode::Char('.') => app.jump_next_flagged(),
                // v like vim's visual mode — keyboard selection + yank.
                KeyCode::Char('v') => app.enter_copy(),
                KeyCode::Char('d') => {
                    // A keeper's own socket is always alive — only refuse
                    // when a FOREIGN keeper holds it.
                    if !app.is_keeper && crate::daemon::keeper_alive_at(&crate::daemon::socket_path()) {
                        app.flash("a detached session already exists");
                    } else {
                        app.detach_requested = true;
                    }
                }
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
            // The leader works from every non-text-entry mode — re-enter
            // Leader before any sidebar-local key handling.
            if app.config.is_leader(&key) {
                app.mode = InputMode::Leader;
                return;
            }
            // Tab flips focus between the Projects and Git sections.
            if key.code == KeyCode::Tab {
                app.sidebar_toggle_focus();
                return;
            }
            match app.sidebar_focus {
                crate::app::SidebarSection::Projects => match key.code {
                    KeyCode::Esc => {
                        app.mode = InputMode::Normal;
                    }
                    KeyCode::Char('h') | KeyCode::Char('g') if key.modifiers.is_empty() => {
                        app.mode = InputMode::Normal;
                    }
                    // Moving switches projects live — the row itself is
                    // the switch (same as clicking through the list).
                    KeyCode::Char('j') if key.modifiers.is_empty() => {
                        app.sidebar_project_move(1)
                    }
                    KeyCode::Down => app.sidebar_project_move(1),
                    KeyCode::Char('k') if key.modifiers.is_empty() => {
                        app.sidebar_project_move(-1)
                    }
                    KeyCode::Up => app.sidebar_project_move(-1),
                    // Enter commits to the chosen project — back to panes.
                    KeyCode::Enter => {
                        app.mode = InputMode::Normal;
                    }
                    _ => {}
                },
                crate::app::SidebarSection::Git => match key.code {
                    KeyCode::Esc => {
                        app.mode = InputMode::Normal;
                    }
                    // Plain-char bindings require NO modifiers — otherwise
                    // e.g. Ctrl+C would run `git add -A` via the 'c' binding.
                    // (The leader is intercepted above and can't reach here.)
                    KeyCode::Char('h') | KeyCode::Char('g') if key.modifiers.is_empty() => {
                        app.mode = InputMode::Normal;
                    }
                    KeyCode::Char('j') if key.modifiers.is_empty() => app.sidebar_move(1),
                    KeyCode::Down => app.sidebar_move(1),
                    KeyCode::Char('k') if key.modifiers.is_empty() => app.sidebar_move(-1),
                    KeyCode::Up => app.sidebar_move(-1),
                    KeyCode::Char('b') if key.modifiers.is_empty() => app.sidebar_toggle_branches(),
                    KeyCode::Char('c') if key.modifiers.is_empty() => app.start_ai_commit(),
                    KeyCode::Char(' ') => app.sidebar_toggle_stage(),
                    KeyCode::Enter => app.sidebar_activate(),
                    _ => {}
                },
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
                        // config.editor → $VISUAL → $EDITOR → nvim.
                        let editor = app
                            .config
                            .editor
                            .clone()
                            .or_else(|| {
                                std::env::var("VISUAL").ok().filter(|s| !s.is_empty())
                            })
                            .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.is_empty()))
                            .unwrap_or_else(|| "nvim".to_string());
                        let cmd = format!(
                            "exec {} {}",
                            editor,
                            crate::app::shell_quote(&path.to_string_lossy())
                        );
                        let title = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| editor.clone());
                        app.finder = None;
                        app.mode = InputMode::Normal;
                        app.spawn_float(&title, &cmd);
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
        InputMode::Search => {
            // The leader exits search AND re-enters Leader — consistent
            // with every other non-text-entry mode.
            if app.config.is_leader(&key) {
                app.exit_search();
                app.mode = InputMode::Leader;
                return;
            }
            match key.code {
                KeyCode::Char('n') => app.search_next(),
                KeyCode::Char('N') => app.search_prev(),
                _ => app.exit_search(), // Esc, Enter, and ANY other key exits
            }
        }
        InputMode::Copy => {
            // The leader exits copy mode into Leader — consistent escape.
            if app.config.is_leader(&key) {
                app.copy_exit();
                app.mode = InputMode::Leader;
                return;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => app.copy_exit(),
                KeyCode::Enter | KeyCode::Char('y') => app.copy_yank(),
                KeyCode::Char('v') => app.copy_toggle_anchor(),
                KeyCode::Char('h') | KeyCode::Left => app.copy_move(0, -1),
                KeyCode::Char('l') | KeyCode::Right => app.copy_move(0, 1),
                KeyCode::Char('k') | KeyCode::Up => app.copy_move(-1, 0),
                KeyCode::Char('j') | KeyCode::Down => app.copy_move(1, 0),
                KeyCode::PageUp => app.copy_page(true),
                KeyCode::PageDown => app.copy_page(false),
                KeyCode::Char('g') => app.copy_move(i64::MIN / 2, 0), // clamps to row 0
                KeyCode::Char('G') => app.copy_move(i64::MAX / 2, 0), // to last row
                KeyCode::Char('0') => app.copy_move(0, i64::MIN / 2),
                KeyCode::Char('$') => app.copy_move(0, i64::MAX / 2),
                _ => {}
            }
        }
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
                    app.commit_root = None;
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
                        // Commit to the root the AI worker polled — the user
                        // may have switched projects while it generated the
                        // message. Fall back to the active root for manually
                        // opened prompts.
                        let root = app
                            .commit_root
                            .take()
                            .unwrap_or_else(|| app.active_root().unwrap_or_default());
                        if msg.is_empty() {
                            app.flash("commit aborted: empty message");
                        } else {
                            match crate::git::commit(&root, &msg) {
                                Ok(hash) => app.flash(format!("committed {hash}")),
                                Err(e) => app.flash(format!("git commit: {e}")),
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

/// Terminal paste (bracketed-paste `Event::Paste`). Text-entry modes take
/// the text literally (newlines collapse to spaces inside `insert_str`);
/// pane modes forward to the top float or active pane wrapped in
/// `200~`/`201~` when the app opted in — so a multi-line paste lands as
/// ONE paste instead of executing line-by-line. Leader/Copy swallow it.
pub fn handle_paste(app: &mut App, text: String) {
    match app.mode {
        InputMode::LineInput(_) => {
            if let Some(edit) = app.line_input.as_mut() {
                edit.insert_str(&text);
            }
        }
        InputMode::Palette => {
            if let Some(pal) = app.palette.as_mut() {
                let q = format!("{}{}", pal.query, text.replace(['\r', '\n'], " "));
                pal.set_query(q);
            }
        }
        InputMode::Finder => {
            if let Some(f) = app.finder.as_mut() {
                let q = format!("{}{}", f.query, text.replace(['\r', '\n'], " "));
                f.set_query(q);
            }
        }
        InputMode::Normal | InputMode::Sidebar | InputMode::Search => {
            let mut pending = Vec::new();
            if let Some(project) = app.active_project_mut() {
                let pane = if project.top_float().is_some() {
                    project.top_float_mut()
                } else {
                    project.active_pane_mut()
                };
                if let Some(pane) = pane {
                    // The watcher sees raw text — the 200~/201~ wrap is
                    // transport framing, not input.
                    for e in pane.watcher.on_input(text.as_bytes()) {
                        pending.push((pane.id, e));
                    }
                    pane.scroll_to_bottom();
                    let _ = pane.write_input(&paste_bytes(pane, &text));
                }
            }
            for (id, e) in pending {
                notify::dispatch(app, id, e);
            }
        }
        InputMode::Leader | InputMode::Copy => {}
    }
}

/// Wraps `text` in bracketed-paste markers when the pane's app enabled
/// DECSET 2004; a literal `200~`/`201~` inside the content is stripped so
/// it can't break out of (or nest) the frame.
fn paste_bytes(pane: &crate::pane::Pane, text: &str) -> Vec<u8> {
    if !pane.bracketed_paste() {
        return text.as_bytes().to_vec();
    }
    let safe = text.replace("\x1b[201~", "").replace("\x1b[200~", "");
    let mut v = Vec::with_capacity(safe.len() + 12);
    v.extend_from_slice(b"\x1b[200~");
    v.extend_from_slice(safe.as_bytes());
    v.extend_from_slice(b"\x1b[201~");
    v
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
    fn leader_then_d_requests_detach() {
        // Isolate from any live keeper on the default socket — a real
        // detached session would (correctly) refuse this leader d.
        let sock = std::env::temp_dir()
            .join(format!("rustterm-test-sock-{}", std::process::id()));
        std::env::set_var("RUSTTERM_SOCK", &sock);
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(app.detach_requested);
        // The keeper itself is exempt from the live-socket refusal —
        // re-detach from an attached session must always work.
        let mut app = app_with_one_project();
        app.is_keeper = true;
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(app.detach_requested);
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
    fn leader_h_backgrounds_active_pane() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_pane(None); // active = pane 1
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('H'), KeyModifiers::NONE));
        let p = app.active_project().unwrap();
        assert!(p.panes[1].hidden);
        assert_eq!(p.active_pane, 0);
        assert_eq!(p.panes.len(), 2, "PTY stays alive in the vec");
        assert!(matches!(app.mode, InputMode::Normal));
    }

    #[test]
    fn leader_b_toggles_sidebar_visibility() {
        let mut app = app_with_one_project();
        assert!(app.sidebar_visible);
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(!app.sidebar_visible);
        assert!(matches!(app.mode, InputMode::Normal)); // leader consumed
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(app.sidebar_visible);
    }

    #[test]
    fn hiding_while_focused_in_sidebar_returns_to_normal() {
        let mut app = app_with_one_project();
        app.enter_sidebar();
        assert!(matches!(app.mode, InputMode::Sidebar));
        app.toggle_sidebar();
        assert!(!app.sidebar_visible);
        assert!(matches!(app.mode, InputMode::Normal));
        // leader g un-hides it again
        app.toggle_sidebar();
        assert!(app.sidebar_visible);
    }

    #[test]
    fn leader_p_opens_palette() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Palette));
        assert!(app.palette.is_some());
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
        assert_eq!(pane_scrollback(&app), app.config.scroll_lines);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 40, 10), frame);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 40, 10), frame);
        assert_eq!(pane_scrollback(&app), app.config.scroll_lines);
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
    fn wheel_maps_visible_rects_to_real_pane_indices() {
        // Two panes side by side; hide pane 0 → pane 1 takes the whole main
        // area. A wheel event anywhere must scroll pane 1, not hit a rect
        // still reserved for the hidden pane.
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_pane(None);
        app.active_project_mut().unwrap().panes[0].hidden = true;
        {
            let pane = &app.active_project().unwrap().panes[1];
            let mut p = pane.parser.lock().unwrap();
            for _ in 0..40 {
                p.process(b"line\r\n");
            }
        }
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::ScrollUp, 40, 10),
            Rect::new(0, 0, 80, 24),
        );
        let s = app.active_project().unwrap().panes[1]
            .parser
            .lock()
            .unwrap()
            .screen()
            .scrollback();
        assert_eq!(s, app.config.scroll_lines);
    }

    fn frame80() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

    fn click_at(column: u16, row: u16) -> MouseEvent {
        mouse(MouseEventKind::Down(MouseButton::Left), column, row)
    }

    /// Temp git repo with one modified file — enter_sidebar()'s real
    /// `git status` call must find something to list.
    fn git_repo(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("rustterm-click-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap()
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), b"one").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-m", "init"]);
        std::fs::write(root.join("a.txt"), b"two").unwrap();
        root
    }

    #[test]
    fn click_pane_focuses_it_and_exits_sidebar_mode() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_pane(None);
        // Two panes split the main area (x24..80): left ~24..51, right ~52..80.
        handle_mouse(&mut app, click_at(70, 10), frame80());
        assert_eq!(app.active_project().unwrap().active_pane, 1);

        app.enter_sidebar();
        handle_mouse(&mut app, click_at(30, 10), frame80());
        assert_eq!(app.active_project().unwrap().active_pane, 0);
        assert!(matches!(app.mode, InputMode::Normal));
    }

    #[test]
    fn click_project_row_switches_project_and_focuses_sidebar() {
        let mut app = app_with_one_project();
        app.projects.push(Project::new("second".into(), PathBuf::from("/tmp")));
        // Project rows live at sidebar.y+1 — row 1 is the second project.
        handle_mouse(&mut app, click_at(5, 2), frame80());
        assert_eq!(app.active_project, 1);
        assert!(matches!(app.mode, InputMode::Sidebar));
    }

    #[test]
    fn click_projects_title_row_collapses_sidebar() {
        let mut app = app_with_one_project();
        handle_mouse(&mut app, click_at(5, 0), frame80());
        assert!(!app.sidebar_visible);
    }

    #[test]
    fn click_git_title_row_toggles_branches() {
        let mut app = app_with_one_project();
        // project_h = 1 project + 2 borders = 3 → git title row is y3.
        handle_mouse(&mut app, click_at(5, 3), frame80());
        assert!(matches!(app.mode, InputMode::Sidebar));
        assert!(app.sidebar_branches);
    }

    #[test]
    fn click_git_row_selects_then_activates() {
        let root = git_repo("row");
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        app.projects.push(Project::new("demo".into(), root.clone()));
        app.spawn_pane(None);

        // Git rows start at y4 — first click selects (enters Sidebar).
        handle_mouse(&mut app, click_at(5, 4), frame80());
        assert!(matches!(app.mode, InputMode::Sidebar));
        assert_eq!(app.sidebar_sel, 0);
        assert_eq!(app.active_project().unwrap().panes.len(), 1);

        // Clicking the selected row activates it — opens the diff float.
        handle_mouse(&mut app, click_at(5, 4), frame80());
        let project = app.active_project().unwrap();
        assert_eq!(project.panes.len(), 1, "diff is a popup, not a grid pane");
        assert_eq!(project.floats.len(), 1);
        assert!(project.floats[0]
            .startup_command
            .as_deref()
            .unwrap()
            .contains("git --no-pager diff HEAD"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clicks_are_ignored_while_palette_is_open() {
        let mut app = app_with_one_project();
        app.projects.push(Project::new("second".into(), PathBuf::from("/tmp")));
        app.palette = Some(crate::palette::Palette::open(&app));
        app.mode = InputMode::Palette;
        handle_mouse(&mut app, click_at(5, 2), frame80()); // project row
        assert_eq!(app.active_project, 0);
        assert!(matches!(app.mode, InputMode::Palette));
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
    fn leader_capital_g_spawns_lazygit_float() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('G'), KeyModifiers::SHIFT));
        let project = app.active_project().unwrap();
        assert_eq!(project.floats.len(), 1);
        assert!(project.panes.is_empty(), "popup must not join the grid");
        // exec'd so lazygit's own `q` exits the shell → popup auto-closes.
        assert_eq!(project.floats[0].startup_command.as_deref(), Some("exec lazygit"));
        assert_eq!(project.floats[0].title, "lazygit");
    }

    #[test]
    fn leader_x_pops_float_before_closing_grid_pane() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_float("nvim", "exec nvim foo.rs");
        let project = app.active_project().unwrap();
        assert_eq!(project.panes.len(), 1);
        assert_eq!(project.floats.len(), 1);

        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('x'), KeyModifiers::NONE));
        let project = app.active_project().unwrap();
        assert!(project.floats.is_empty(), "x pops the popup first");
        assert_eq!(project.panes.len(), 1, "grid pane survives");

        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.active_project().unwrap().panes.len(), 0);
    }

    #[test]
    fn leader_z_zooms_follows_focus_and_unzooms() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_pane(None); // active = pane 1
        let leader = |app: &mut App, c: char| {
            handle_key(app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
            handle_key(app, key(KeyCode::Char(c), KeyModifiers::NONE));
        };
        leader(&mut app, 'z');
        let project = app.active_project().unwrap();
        assert_eq!(project.render_indices().len(), 1, "zoomed view renders one pane");

        // Focus moves → zoom follows to the newly active pane.
        leader(&mut app, 'h');
        let project = app.active_project().unwrap();
        assert_eq!(project.active_pane, 0);
        assert_eq!(project.render_indices(), vec![0]);

        leader(&mut app, 'z');
        assert_eq!(app.active_project().unwrap().render_indices().len(), 2);
    }

    #[test]
    fn zoom_drops_when_zoomed_pane_is_hidden_or_closed() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_pane(None);
        app.active_project_mut().unwrap().zoom_toggle();
        assert!(app.active_project().unwrap().zoomed.is_some());
        app.close_active_pane(); // closes the zoomed (active) pane
        assert!(app.active_project().unwrap().zoomed.is_none());
    }

    #[test]
    fn leader_dot_jumps_to_flagged_pane_across_projects() {
        let mut app = app_with_one_project();
        app.projects.push(Project::new("second".into(), PathBuf::from("/tmp")));
        app.spawn_pane(None);
        app.spawn_pane(None);
        // Flag pane 0 in the CURRENT project and pane 1 — jump from pane 1
        // (active) should land on pane 0? No: flagged[0]=(0,0) < cur → wrap.
        app.active_project_mut().unwrap().panes[0].waiting = true;
        app.active_project_mut().unwrap().panes[1].attention = true;
        // cur = (0,1); flagged = [(0,0),(0,1)] — find > (0,1) → none → wrap (0,0).
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('.'), KeyModifiers::NONE));
        assert_eq!(app.active_project().unwrap().active_pane, 0);
        // Again → wraps to (0,1).
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('.'), KeyModifiers::NONE));
        assert_eq!(app.active_project().unwrap().active_pane, 1);
    }

    #[test]
    fn leader_dot_surfaces_hidden_flagged_pane() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_pane(None);
        app.active_project_mut().unwrap().panes[0].hidden = true;
        app.active_project_mut().unwrap().panes[0].waiting = true;
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('.'), KeyModifiers::NONE));
        let project = app.active_project().unwrap();
        assert_eq!(project.active_pane, 0);
        assert!(!project.panes[0].hidden, "jump unhides the flagged pane");
    }

    #[test]
    fn leader_capital_l_spawns_git_log_float() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('L'), KeyModifiers::SHIFT));
        let project = app.active_project().unwrap();
        assert_eq!(project.floats.len(), 1);
        assert_eq!(project.floats[0].title, "git log");
        assert!(project.floats[0]
            .startup_command
            .as_deref()
            .unwrap()
            .contains("git log"));
    }

    #[test]
    fn sidebar_space_stages_then_unstages() {
        let root = git_repo("stage");
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        app.projects.push(Project::new("demo".into(), root.clone()));
        app.enter_sidebar(); // real git status — one modified file
        assert_eq!(app.sidebar_items_len(), 1);

        handle_key(&mut app, key(KeyCode::Char(' '), KeyModifiers::NONE));
        let f = &app.git_status.as_ref().unwrap().files[0];
        assert!(f.staged_only(), "space staged the modified file");
        let staged = std::process::Command::new("git")
            .arg("-C").arg(&root).args(["diff", "--cached", "--name-only"]).output().unwrap();
        assert!(String::from_utf8_lossy(&staged.stdout).contains("a.txt"));

        handle_key(&mut app, key(KeyCode::Char(' '), KeyModifiers::NONE));
        let f = &app.git_status.as_ref().unwrap().files[0];
        assert!(f.has_unstaged(), "second space unstaged the file");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn leader_v_enters_copy_mode_and_esc_exits() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            pane.parser.lock().unwrap().process(b"hello\r\nworld\r\n");
        }
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('v'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Copy));
        assert!(app.copy.is_some());
        handle_key(&mut app, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(app.copy.is_none());
    }

    #[test]
    fn copy_cursor_moves_and_scrolls_to_stay_visible() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            // Fill a 24-row pane with scrollback.
            let pane = &app.active_project().unwrap().panes[0];
            let mut p = pane.parser.lock().unwrap();
            for i in 0..60 {
                p.process(format!("line{i}\r\n").as_bytes());
            }
        }
        app.enter_copy();
        // Cursor starts at the pane cursor (bottom of live view). k×40
        // walks it deep into scrollback — the viewport must follow.
        for _ in 0..40 {
            handle_key(&mut app, key(KeyCode::Char('k'), KeyModifiers::NONE));
        }
        let copy = app.copy.as_ref().unwrap();
        let row = copy.cursor.0;
        let pane = &app.active_project().unwrap().panes[0];
        let (sb, offset, h) = {
            let mut p = pane.parser.lock().unwrap();
            let s = p.screen_mut();
            (crate::search::scrollback_len(s), s.scrollback(), s.size().0 as usize)
        };
        let view_top = sb - offset;
        assert!(row >= view_top && row < view_top + h, "cursor {row} outside view {view_top}..{}", view_top + h);
        assert!(offset > 0, "scrolled into scrollback");
    }

    #[test]
    fn copy_yank_clears_state_and_restores_scroll() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            let mut p = pane.parser.lock().unwrap();
            for i in 0..30 {
                p.process(format!("yankme{i}\r\n").as_bytes());
            }
        }
        app.enter_copy();
        handle_key(&mut app, key(KeyCode::Char('v'), KeyModifiers::NONE)); // anchor
        handle_key(&mut app, key(KeyCode::Char('k'), KeyModifiers::NONE)); // extend up
        handle_key(&mut app, key(KeyCode::Char('y'), KeyModifiers::NONE)); // yank → osc52 to stdout
        assert!(app.copy.is_none());
        assert!(matches!(app.mode, InputMode::Normal));
        let pane = &app.active_project().unwrap().panes[0];
        assert_eq!(pane.parser.lock().unwrap().screen().scrollback(), 0, "view restored to live");
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
        let project = app.active_project().unwrap();
        assert_eq!(project.floats.len(), 1, "finder opens the editor as a popup");
        let cmd = project.floats[0].startup_command.clone().unwrap();
        assert!(cmd.starts_with("exec "), "expected exec'd editor command, got {cmd}");
    }

    #[test]
    fn typing_goes_to_the_top_float_not_the_grid() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_float("nvim", "exec nvim foo.rs");
        // Give BOTH panes scrollback; a keypress snaps only the input
        // target back to the live view — that identifies the float.
        {
            let project = app.active_project().unwrap();
            for p in project.panes.iter().chain(project.floats.iter()) {
                let mut parser = p.parser.lock().unwrap();
                for _ in 0..30 {
                    parser.process(b"line\r\n");
                }
            }
            project.panes[0].scroll_up(5);
            project.floats[0].scroll_up(5);
        }
        handle_key(&mut app, key(KeyCode::Char('i'), KeyModifiers::NONE));
        let project = app.active_project().unwrap();
        assert_eq!(project.floats[0].parser.lock().unwrap().screen().scrollback(), 0);
        assert_eq!(project.panes[0].parser.lock().unwrap().screen().scrollback(), 5);
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

    #[test]
    fn search_mode_ctrl_a_goes_to_leader() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            pane.parser.lock().unwrap().process(b"needle\r\n");
        }
        app.start_search("needle");
        assert!(matches!(app.mode, InputMode::Search));
        assert!(app.active_project().unwrap().panes[0].search.is_some());

        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, InputMode::Leader));
        assert!(
            app.active_project().unwrap().panes[0].search.is_none(),
            "Ctrl+A must clear the pane's search state, not orphan it"
        );
    }

    #[test]
    fn sidebar_ctrl_c_does_not_trigger_ai_commit() {
        let mut app = app_with_one_project();
        app.enter_sidebar();
        // Ctrl+C must not hit the plain 'c' binding — git add -A is a
        // destructive side effect for a key users hit reflexively.
        handle_key(&mut app, key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!app.ai_in_flight);
        assert!(app.status_msg.is_none(), "no ai-commit path should have run");
        assert!(matches!(app.mode, InputMode::Sidebar), "mode unchanged");
    }

    // ---- mouse select + copy ----
    // Single pane: frame 80x24 → sidebar 24 wide, pane rect (24,0,56,23),
    // content inner (25,1)..(78,21).

    #[test]
    fn drag_selects_and_release_copies() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            pane.parser.lock().unwrap().process(b"hello world\r\n");
        }
        let frame = Rect::new(0, 0, 80, 24);
        handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 26, 1), frame);
        assert!(app.mouse_dragging);
        let sel = app.mouse_sel.as_ref().expect("press anchors a selection");
        assert_eq!(sel.anchor, Some((0, 1))); // abs row 0, col 1 ('e')
        handle_mouse(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), 30, 1), frame);
        assert_eq!(app.mouse_sel.as_ref().unwrap().cursor, (0, 5));
        handle_mouse(&mut app, mouse(MouseEventKind::Up(MouseButton::Left), 30, 1), frame);
        assert!(!app.mouse_dragging);
        assert!(app.mouse_sel.is_none(), "release clears the highlight");
        let (msg, _) = app.status_msg.as_ref().expect("yank flashes");
        assert!(msg.starts_with("copied"), "expected copy flash, got {msg}");
    }

    #[test]
    fn click_without_drag_selects_nothing() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        let frame = Rect::new(0, 0, 80, 24);
        handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 30, 5), frame);
        handle_mouse(&mut app, mouse(MouseEventKind::Up(MouseButton::Left), 30, 5), frame);
        assert!(app.mouse_sel.is_none(), "same-cell release is a click, not a selection");
        assert!(app.status_msg.is_none());
    }

    #[test]
    fn press_on_border_or_sidebar_anchors_nothing() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        let frame = Rect::new(0, 0, 80, 24);
        // Pane's left border column (x=24) is not content.
        handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 24, 5), frame);
        assert!(app.mouse_sel.is_none());
        assert!(!app.mouse_dragging);
        // Sidebar content row — a UI click, not text.
        handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 5, 5), frame);
        assert!(app.mouse_sel.is_none());
    }

    #[test]
    fn drag_past_top_edge_scrolls_scrollback() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        grow_scrollback(&app);
        let frame = Rect::new(0, 0, 80, 24);
        handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 30, 1), frame);
        handle_mouse(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), 30, 0), frame);
        let off = pane_scrollback(&app);
        assert!(off > 0, "overshoot above the pane scrolled up");
        // The cursor tracked into scrollback: it's above the anchor row.
        let sel = app.mouse_sel.as_ref().unwrap();
        assert!(sel.cursor.0 < sel.anchor.unwrap().0);
    }

    #[test]
    fn reporting_app_gets_press_and_release() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            // App enables mouse reporting + SGR encoding.
            pane.parser.lock().unwrap().process(b"\x1b[?1000h\x1b[?1006h");
        }
        let frame = Rect::new(0, 0, 80, 24);
        handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 30, 5), frame);
        assert!(app.mouse_sel.is_none(), "no local selection for a reporting app");
        assert!(app.mouse_app.is_some(), "press handed to the app");
        handle_mouse(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), 35, 6), frame);
        handle_mouse(&mut app, mouse(MouseEventKind::Up(MouseButton::Left), 35, 6), frame);
        assert!(app.mouse_app.is_none(), "release ends app capture");
        assert!(app.status_msg.is_none(), "no copy flash");
    }

    #[test]
    fn shift_drag_bypasses_app_capture_to_select() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            pane.parser.lock().unwrap().process(b"\x1b[?1000h\x1b[?1006hshift me\r\n");
        }
        let frame = Rect::new(0, 0, 80, 24);
        let ev = |kind| MouseEvent {
            kind,
            column: 30,
            row: 5,
            modifiers: KeyModifiers::SHIFT,
        };
        handle_mouse(&mut app, ev(MouseEventKind::Down(MouseButton::Left)), frame);
        assert!(app.mouse_dragging, "shift forces local selection");
        assert!(app.mouse_app.is_none());
        handle_mouse(&mut app, ev(MouseEventKind::Up(MouseButton::Left)), frame);
    }

    #[test]
    fn click_project_row_focuses_projects_section() {
        let mut app = app_with_one_project();
        app.projects.push(Project::new("second".into(), PathBuf::from("/tmp")));
        handle_mouse(&mut app, click_at(5, 2), frame80());
        assert_eq!(app.active_project, 1);
        assert!(matches!(app.mode, InputMode::Sidebar));
        assert_eq!(app.sidebar_focus, crate::app::SidebarSection::Projects,
            "clicking a project focuses Projects, not Git");
    }

    #[test]
    fn projects_focus_jk_switches_projects_live() {
        let mut app = app_with_one_project();
        app.projects.push(Project::new("second".into(), PathBuf::from("/tmp")));
        app.projects.push(Project::new("third".into(), PathBuf::from("/tmp")));
        app.enter_sidebar();
        app.sidebar_focus = crate::app::SidebarSection::Projects;
        handle_key(&mut app, key(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.active_project, 1);
        handle_key(&mut app, key(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.active_project, 2);
        handle_key(&mut app, key(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.active_project, 1);
        // Git keys do nothing while Projects is focused.
        handle_key(&mut app, key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(app.status_msg.is_none());
        // Enter commits to the project — back to panes.
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(app.active_project, 1);
    }

    #[test]
    fn tab_flips_sidebar_focus_and_git_keys_stay_git() {
        let mut app = app_with_one_project();
        app.projects.push(Project::new("second".into(), PathBuf::from("/tmp")));
        app.enter_sidebar();
        assert_eq!(app.sidebar_focus, crate::app::SidebarSection::Git,
            "leader g enters at Git");
        handle_key(&mut app, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.sidebar_focus, crate::app::SidebarSection::Projects);
        // j navigates projects while Projects-focused, not git rows.
        handle_key(&mut app, key(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.active_project, 1);
        assert_eq!(app.sidebar_sel, 0);
        handle_key(&mut app, key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.sidebar_focus, crate::app::SidebarSection::Git);
    }

    #[test]
    fn click_git_row_focuses_git_section() {
        let root = git_repo("gfocus");
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        app.projects.push(Project::new("demo".into(), root.clone()));
        app.projects.push(Project::new("other".into(), PathBuf::from("/tmp")));
        app.spawn_pane(None);
        // Start Projects-focused, then click a git row — focus follows.
        app.enter_sidebar();
        app.sidebar_focus = crate::app::SidebarSection::Projects;
        handle_mouse(&mut app, click_at(5, 5), frame80()); // first git row
        assert_eq!(app.sidebar_focus, crate::app::SidebarSection::Git);
    }

    #[test]
    fn float_interior_selects_but_outside_is_swallowed() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_float("pop", "echo hi");
        let frame = Rect::new(0, 0, 80, 24);
        let fid = app.active_project().unwrap().top_float().unwrap().id;
        // Float interior: centered 90% of 80x23 → inner ~(5,2)..(75,20).
        handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 30, 10), frame);
        let sel = app.mouse_sel.as_ref().expect("press inside float anchors");
        assert_eq!(sel.pane_id, fid, "selection targets the float, not the grid");
        // Outside the float — modal swallow, no selection.
        handle_mouse(&mut app, mouse(MouseEventKind::Up(MouseButton::Left), 30, 10), frame);
        handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 2, 2), frame);
        assert!(app.mouse_sel.is_none(), "outside a modal float selects nothing");
    }

    #[test]
    fn copy_mode_click_places_cursor_and_drag_selects() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            pane.parser.lock().unwrap().process(b"copy target\r\n");
        }
        app.enter_copy();
        let frame = Rect::new(0, 0, 80, 24);
        handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 30, 1), frame);
        assert!(matches!(app.mode, InputMode::Copy), "click stays in copy mode");
        assert_eq!(app.copy.as_ref().unwrap().cursor, (0, 5));
        assert!(app.copy.as_ref().unwrap().anchor.is_none());
        handle_mouse(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), 34, 1), frame);
        let copy = app.copy.as_ref().unwrap();
        assert_eq!(copy.anchor, Some((0, 5)), "first drag anchors at pre-drag cursor");
        assert_eq!(copy.cursor, (0, 9));
    }

    fn pane_screen_contains(pane: &crate::pane::Pane, needle: &str, wait_ms: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        while std::time::Instant::now() < deadline {
            if pane.parser.lock().unwrap().screen().contents().contains(needle) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    /// Unlike `app_with_one_project`, keeps the event receivers alive —
    /// spawn_reader exits on a failed send, so a dropped receiver leaves
    /// the pane's parser frozen after the first output chunk.
    fn app_with_live_events() -> (App, mpsc::Receiver<crate::pane::PaneEvent>) {
        let (tx, rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
        (app, rx)
    }

    /// Blocks until the pane's shell is reading input — fish can take a
    /// while to init under test load, and writes queued before it starts
    /// are fine but slow to echo.
    fn wait_pane_ready(pane: &crate::pane::Pane) {
        let _ = pane.write_input(b"echo RDY-MARK\n");
        assert!(
            pane_screen_contains(pane, "RDY-MARK", 4000),
            "pane never became ready"
        );
    }

    #[test]
    fn paste_bytes_wraps_only_when_pane_enabled_bracketed() {
        let (tx, _rx) = mpsc::channel();
        let pane =
            crate::pane::Pane::spawn(1, "t".into(), 24, 80, None, tx, None, 10_000).unwrap();
        // DECSET 2004 off by default → raw passthrough.
        assert_eq!(paste_bytes(&pane, "a\nb"), b"a\nb");
        pane.parser.lock().unwrap().process(b"\x1b[?2004h");
        assert_eq!(paste_bytes(&pane, "a\nb"), b"\x1b[200~a\nb\x1b[201~");
        // A literal terminator in the clipboard can't break the frame.
        assert_eq!(paste_bytes(&pane, "x\x1b[201~y"), b"\x1b[200~xy\x1b[201~");
        pane.parser.lock().unwrap().process(b"\x1b[?2004l");
        assert_eq!(paste_bytes(&pane, "a\nb"), b"a\nb");
    }

    #[test]
    fn paste_to_line_input_collapses_newlines() {
        let mut app = app_with_one_project();
        app.line_input = Some(LineEdit::new());
        app.mode = InputMode::LineInput(LinePurpose::Search);
        handle_paste(&mut app, "alpha\r\nbeta\ngamma".into());
        assert_eq!(app.line_input.as_ref().unwrap().as_str(), "alpha beta gamma");
    }

    #[test]
    fn paste_appends_to_palette_and_finder_queries() {
        let mut app = app_with_one_project();
        app.palette = Some(Palette::open(&app));
        app.mode = InputMode::Palette;
        handle_paste(&mut app, "qui\nt".into());
        assert_eq!(app.palette.as_ref().unwrap().query, "qui t");
        app.palette = None;

        app.finder = Some(crate::finder::FinderState::open(&app.active_root().unwrap()));
        app.mode = InputMode::Finder;
        handle_paste(&mut app, "src\nmain".into());
        assert_eq!(app.finder.as_ref().unwrap().query, "src main");
        app.finder = None;
        app.mode = InputMode::Normal;
    }

    #[test]
    fn paste_routes_to_active_pane_and_float_wins() {
        let (mut app, _rx) = app_with_live_events();
        // `cat` echoes input — a deterministic sink that can't "execute".
        app.spawn_pane(Some("cat"));
        let pane_id = app.active_project().unwrap().panes[0].id;
        wait_pane_ready(&app.active_project().unwrap().panes[0]);
        handle_paste(&mut app, "GRID-MARK".into());
        let pane = &app.active_project().unwrap().panes[0];
        assert!(
            pane_screen_contains(pane, "GRID-MARK", 1500),
            "paste should reach the active pane"
        );

        app.spawn_float("cat", "cat");
        wait_pane_ready(app.active_project().unwrap().top_float().unwrap());
        handle_paste(&mut app, "FLOAT-MARK".into());
        let project = app.active_project().unwrap();
        let float = project.top_float().unwrap();
        assert!(
            pane_screen_contains(float, "FLOAT-MARK", 1500),
            "paste should reach the top float"
        );
        let grid = project.panes.iter().find(|p| p.id == pane_id).unwrap();
        assert!(
            !pane_screen_contains(grid, "FLOAT-MARK", 300),
            "grid pane must not see the float's paste"
        );
    }

    #[test]
    fn paste_in_leader_or_copy_mode_is_swallowed() {
        let (mut app, _rx) = app_with_live_events();
        app.spawn_pane(Some("cat"));
        wait_pane_ready(&app.active_project().unwrap().panes[0]);
        app.mode = InputMode::Leader;
        handle_paste(&mut app, "SWALLOW-L".into());
        app.mode = InputMode::Copy;
        handle_paste(&mut app, "SWALLOW-C".into());
        let pane = &app.active_project().unwrap().panes[0];
        assert!(!pane_screen_contains(pane, "SWALLOW-L", 300));
        assert!(!pane_screen_contains(pane, "SWALLOW-C", 300));
    }
}
