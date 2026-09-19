use crate::pane::{Pane, PaneEvent, PaneId};
use crate::project::Project;
use crate::text_input::LineEdit;
use ratatui::style::Color;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

pub enum InputMode {
    Normal,
    Leader,
    Palette,
    Sidebar,
    Finder,
    Search,
    LineInput(LinePurpose),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LinePurpose {
    AddProject,
    RenamePane,
    CommitMsg,
    Search,
}

pub enum AppEvent {
    GitStatus { root: PathBuf, status: Option<crate::git::GitStatus> },
    /// `root` is the project root polled when the worker started — the
    /// commit must target THAT repo even if the user switched projects
    /// while the request was in flight.
    AiMessage { root: PathBuf, result: Result<String, String> },
}

pub struct App {
    pub projects: Vec<Project>,
    pub active_project: usize,
    pub mode: InputMode,
    pub next_pane_id: u32,
    pub should_quit: bool,
    pub events_tx: mpsc::Sender<PaneEvent>,
    pub closed_panes: VecDeque<ClosedPane>,
    pub status_msg: Option<(String, Instant)>,
    pub line_input: Option<LineEdit>,
    pub palette: Option<crate::palette::Palette>,
    pub last_watch_poll: Instant,
    pub app_tx: mpsc::Sender<AppEvent>,
    pub sidebar_sel: usize,
    pub sidebar_branches: bool,
    /// Cached branch list — populated on enter_sidebar/toggle so the
    /// renderer and len helpers never shell out to git per frame.
    pub sidebar_branch_list: Vec<String>,
    pub git_status: Option<crate::git::GitStatus>,
    pub git_poll_in_flight: bool,
    pub last_git_poll: Instant,
    pub finder: Option<crate::finder::FinderState>,
    /// Root captured when the AI-commit worker started — the CommitMsg
    /// prompt commits HERE, not the currently-active project.
    pub commit_root: Option<PathBuf>,
    /// One AI-commit worker at a time — set before spawn, cleared when
    /// the AppEvent::AiMessage result is drained.
    pub ai_in_flight: bool,
    /// Leader `b` toggles the sidebar; hidden = panes take the width.
    pub sidebar_visible: bool,
}

pub struct ClosedPane {
    pub title: String,
    pub cwd: PathBuf,
    pub startup_command: Option<String>,
    pub color: Option<Color>,
    /// Root of the project the pane was closed from — NOT an index into
    /// `projects` (indices go stale when a lower-indexed project closes).
    pub project: PathBuf,
}

impl App {
    pub fn new(events_tx: mpsc::Sender<PaneEvent>, app_tx: mpsc::Sender<AppEvent>) -> App {
        App {
            projects: Vec::new(),
            active_project: 0,
            mode: InputMode::Normal,
            next_pane_id: 0,
            should_quit: false,
            events_tx,
            closed_panes: VecDeque::new(),
            status_msg: None,
            line_input: None,
            palette: None,
            last_watch_poll: Instant::now(),
            app_tx,
            sidebar_sel: 0,
            sidebar_branches: false,
            sidebar_branch_list: vec![],
            git_status: None,
            git_poll_in_flight: false,
            last_git_poll: Instant::now(),
            finder: None,
            commit_root: None,
            sidebar_visible: true,
            ai_in_flight: false,
        }
    }

    pub fn active_project(&self) -> Option<&Project> {
        self.projects.get(self.active_project)
    }

    pub fn active_project_mut(&mut self) -> Option<&mut Project> {
        self.projects.get_mut(self.active_project)
    }

    pub fn next_project(&mut self) {
        if !self.projects.is_empty() {
            self.active_project = (self.active_project + 1) % self.projects.len();
            self.clear_git_cache();
        }
    }

    pub fn prev_project(&mut self) {
        if !self.projects.is_empty() {
            self.active_project = if self.active_project == 0 {
                self.projects.len() - 1
            } else {
                self.active_project - 1
            };
            self.clear_git_cache();
        }
    }

    /// Drop the cached git view on project switch — it belongs to the
    /// previously-active root. The next 1s poll repopulates it.
    fn clear_git_cache(&mut self) {
        self.git_status = None;
        self.sidebar_branch_list.clear();
    }

    pub fn alloc_pane_id(&mut self) -> u32 {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        id
    }

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.status_msg = Some((msg.into(), Instant::now()));
    }

    pub fn focused_pane_id(&self) -> Option<PaneId> {
        self.active_project()?.active_pane().map(|p| p.id)
    }

    pub fn add_project(&mut self, root: PathBuf) {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        if let Some(idx) = self.projects.iter().position(|p| p.root == root) {
            self.set_active_project(idx);
            return;
        }
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());
        self.projects.push(Project::new(name, root));
        self.active_project = self.projects.len() - 1;
        self.ensure_active_pane();
        self.clear_git_cache();
    }

    pub fn set_active_project(&mut self, idx: usize) {
        if idx < self.projects.len() {
            self.active_project = idx;
            self.ensure_active_pane();
            self.clear_git_cache();
        }
    }

    pub fn ensure_active_pane(&mut self) {
        if let Some(project) = self.active_project_mut() {
            if project.panes.is_empty() {
                // fall through to spawn
            } else if project.visible_count() == 0 {
                // All panes backgrounded — surface one rather than spawn.
                if let Some(i) = project.first_hidden() {
                    project.panes[i].hidden = false;
                    project.active_pane = i;
                }
                return;
            } else {
                return;
            }
        }
        self.spawn_pane(None);
    }

    /// Leader H — background the active pane. Keeps running (watcher,
    /// badges, scrollback); restore via palette Unhide entries.
    pub fn hide_active_pane(&mut self) {
        let Some(project) = self.active_project_mut() else {
            return;
        };
        if project.visible_count() <= 1 {
            self.flash("can't hide the last visible pane");
            return;
        }
        let idx = project.active_pane;
        let title = project.panes[idx].title.clone();
        project.panes[idx].hidden = true;
        if let Some(next) = project.nearest_visible(idx) {
            project.active_pane = next;
        }
        self.flash(format!("{title} backgrounded"));
    }

    /// Restore a hidden pane and focus it (palette Unhide entries).
    pub fn unhide_pane(&mut self, idx: usize) {
        if let Some(project) = self.active_project_mut() {
            if let Some(pane) = project.panes.get_mut(idx) {
                pane.hidden = false;
                project.active_pane = idx;
            }
        }
    }

    /// The shared spawn path — leader `n`, palette New pane, agent runs,
    /// and ensure-on-switch all come through here. Spawn size is 24x80;
    /// the first rendered frame's sync-resize corrects it.
    pub fn spawn_pane(&mut self, startup_command: Option<&str>) {
        let id = self.alloc_pane_id();
        let events_tx = self.events_tx.clone();
        if let Some(project) = self.active_project_mut() {
            let cwd = project.root.clone();
            match Pane::spawn(id, format!("pane-{id}"), 24, 80, Some(&cwd), events_tx, startup_command)
            {
                Ok(pane) => {
                    project.panes.push(pane);
                    project.active_pane = project.panes.len() - 1;
                }
                Err(e) => self.flash(format!("spawn failed: {e}")),
            }
        }
    }

    pub fn close_active_pane(&mut self) {
        let mut pane = None;
        let mut project_root = None;
        if let Some(project) = self.active_project_mut() {
            if project.panes.is_empty() {
                return;
            }
            let idx = project.active_pane;
            let mut removed = project.panes.remove(idx);
            let _ = removed.kill();
            if project.active_pane >= project.panes.len() && project.active_pane > 0 {
                project.active_pane -= 1;
            }
            // The clamp can leave active_pane on a hidden pane (a hidden
            // pane slides into the removed index). Snap to the nearest
            // visible; if none remain, surface a hidden one so the
            // project never ends up with zero visible panes.
            if !project.panes.is_empty() {
                match project.nearest_visible(project.active_pane) {
                    Some(v) => project.active_pane = v,
                    None => {
                        if let Some(i) = project.first_hidden() {
                            project.panes[i].hidden = false;
                            project.active_pane = i;
                        }
                    }
                }
            }
            project_root = Some(project.root.clone());
            pane = Some(removed);
        }
        // The `project` borrow ends above so the closed-pane history (a
        // separate field on self) can be updated here.
        if let Some(pane) = pane {
            self.closed_panes.push_front(ClosedPane {
                title: pane.title,
                cwd: pane.cwd,
                startup_command: pane.startup_command,
                color: pane.color,
                project: project_root.unwrap_or_default(),
            });
            self.closed_panes.truncate(5);
        }
    }

    pub fn reopen_last_pane(&mut self) {
        let Some(closed) = self.closed_panes.pop_front() else {
            return;
        };
        // Target the ORIGINATING project, matched by root — fall back to the
        // active project if it's gone (closed since). An index would go stale
        // whenever a lower-indexed project closed after the pane was recorded.
        let target = self
            .projects
            .iter()
            .position(|p| p.root == closed.project)
            .unwrap_or(self.active_project);
        self.active_project = target;
        let id = self.alloc_pane_id();
        let events_tx = self.events_tx.clone();
        if let Some(project) = self.active_project_mut() {
            match Pane::spawn(
                id,
                closed.title,
                24,
                80,
                Some(&closed.cwd),
                events_tx,
                closed.startup_command.as_deref(),
            ) {
                Ok(mut pane) => {
                    pane.color = closed.color;
                    project.panes.push(pane);
                    project.active_pane = project.panes.len() - 1;
                }
                Err(e) => self.flash(format!("spawn failed: {e}")),
            }
        }
    }

    pub fn close_active_project(&mut self) {
        if self.projects.len() <= 1 {
            self.flash("can't close the last project");
            return;
        }
        let idx = self.active_project;
        let mut project = self.projects.remove(idx);
        for pane in project.panes.iter_mut() {
            let _ = pane.kill();
        }
        self.active_project = idx.min(self.projects.len() - 1);
        self.ensure_active_pane();
    }

    pub fn adjust_split(&mut self, delta: f32) {
        if let Some(project) = self.active_project_mut() {
            project.col_split = (project.col_split + delta).clamp(0.15, 0.85);
        }
    }

    pub fn clear_focused_badges(&mut self) {
        if let Some(project) = self.active_project_mut() {
            if let Some(pane) = project.active_pane_mut() {
                pane.attention = false;
            }
        }
    }

    pub fn active_root(&self) -> Option<PathBuf> {
        self.active_project().map(|p| p.root.clone())
    }

    pub fn enter_sidebar(&mut self) {
        self.sidebar_visible = true; // focusing un-hides the panel
        if let Some(root) = self.active_root() {
            self.git_status = crate::git::status(&root); // instant refresh
            self.sidebar_branch_list = crate::git::branches(&root);
        }
        self.sidebar_sel = 0;
        self.sidebar_branches = false;
        self.mode = InputMode::Sidebar;
    }

    /// Leader `b` — hide/show the whole sidebar. Hiding while focused
    /// inside it returns focus to the panes.
    pub fn toggle_sidebar(&mut self) {
        self.sidebar_visible = !self.sidebar_visible;
        if !self.sidebar_visible && matches!(self.mode, InputMode::Sidebar) {
            self.mode = InputMode::Normal;
        }
    }

    /// Toggle file↔branch list; refreshes the cached branch list.
    pub fn sidebar_toggle_branches(&mut self) {
        self.sidebar_branches = !self.sidebar_branches;
        self.sidebar_sel = 0;
        if self.sidebar_branches {
            if let Some(root) = self.active_root() {
                self.sidebar_branch_list = crate::git::branches(&root);
            }
        }
    }

    /// Rows in the active sidebar list (files or branches).
    pub fn sidebar_items_len(&self) -> usize {
        if self.sidebar_branches {
            self.sidebar_branch_list.len()
        } else {
            self.git_status.as_ref().map(|s| s.files.len()).unwrap_or(0)
        }
    }

    /// Move the sidebar selection, clamped to the current list.
    pub fn sidebar_move(&mut self, delta: i32) {
        let n = self.sidebar_items_len();
        if n == 0 {
            self.sidebar_sel = 0;
            return;
        }
        let cur = self.sidebar_sel as i32;
        self.sidebar_sel = (cur + delta).clamp(0, n as i32 - 1) as usize;
    }

    pub fn sidebar_selected_file(&self) -> Option<String> {
        self.git_status
            .as_ref()?
            .files
            .get(self.sidebar_sel)
            .map(|f| f.path.clone())
    }

    pub fn sidebar_selected_branch(&self) -> Option<String> {
        self.sidebar_branch_list.get(self.sidebar_sel).cloned()
    }

    pub fn open_finder(&mut self) {
        if let Some(root) = self.active_root() {
            self.finder = Some(crate::finder::FinderState::open(&root));
            self.mode = InputMode::Finder;
        }
    }

    /// Stage everything, snapshot the diff, and kick off the worker that
    /// posts AppEvent::AiMessage. Fast-fails inline (flash) when there's
    /// nothing to send or no API key. Only one worker at a time —
    /// `ai_in_flight` is cleared when the result event is drained.
    pub fn start_ai_commit(&mut self) {
        if self.ai_in_flight {
            self.flash("ai commit already running");
            return;
        }
        let Some(root) = self.active_root() else { return };
        if let Err(e) = crate::git::add_all(&root) {
            self.flash(format!("git add: {e}"));
            return;
        }
        let diff = match crate::git::staged_diff(&root) {
            Ok(d) => d,
            Err(e) => {
                self.flash(format!("git diff: {e}"));
                return;
            }
        };
        if diff.trim().is_empty() {
            self.flash("nothing to commit");
            return;
        }
        let key = match std::env::var("OPENAI_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                self.flash("OPENAI_API_KEY not set");
                return;
            }
        };
        let tx = self.app_tx.clone();
        self.ai_in_flight = true;
        std::thread::spawn(move || {
            let r = crate::ai_commit::generate_message(&diff, &key);
            let _ = tx.send(AppEvent::AiMessage { root, result: r });
        });
        self.flash("generating commit message…");
    }

    /// vim-style / — find matches in the focused pane, jump to the last.
    pub fn start_search(&mut self, query: &str) {
        let Some(project) = self.active_project_mut() else { return };
        let Some(pane) = project.active_pane_mut() else { return };
        let matches = pane
            .parser
            .lock()
            .map(|mut p| crate::search::find_matches(p.screen_mut(), query))
            .unwrap_or_default();
        if matches.is_empty() {
            self.flash(format!("no matches: {query}"));
            return;
        }
        let idx = matches.len() - 1;
        pane.search = Some(crate::search::SearchState {
            query: query.to_string(),
            matches,
            idx,
        });
        self.apply_search_scroll();
        self.mode = InputMode::Search;
    }

    pub fn search_next(&mut self) {
        if let Some(project) = self.active_project_mut() {
            if let Some(pane) = project.active_pane_mut() {
                if let Some(s) = pane.search.as_mut() {
                    s.next();
                }
            }
        }
        self.apply_search_scroll();
    }

    pub fn search_prev(&mut self) {
        if let Some(project) = self.active_project_mut() {
            if let Some(pane) = project.active_pane_mut() {
                if let Some(s) = pane.search.as_mut() {
                    s.prev();
                }
            }
        }
        self.apply_search_scroll();
    }

    /// Exit search mode AND clear the pane's search state (any exit path).
    pub fn exit_search(&mut self) {
        if let Some(project) = self.active_project_mut() {
            if let Some(pane) = project.active_pane_mut() {
                pane.search = None;
            }
        }
        self.mode = InputMode::Normal;
    }

    fn apply_search_scroll(&mut self) {
        if let Some(project) = self.active_project() {
            if let Some(pane) = project.active_pane() {
                if let Some(s) = &pane.search {
                    if let Some(m) = s.current() {
                        let total = pane
                            .parser
                            .lock()
                            .map(|mut p| crate::search::scrollback_len(p.screen_mut()))
                            .unwrap_or(0);
                        let off = crate::search::offset_for_row(total, m.row);
                        pane.set_scroll(off);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn app_with_projects(names: &[&str]) -> App {
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        for name in names {
            app.projects.push(Project::new((*name).into(), PathBuf::from("/tmp")));
        }
        app
    }

    fn app_with_one_project() -> App {
        app_with_projects(&["demo"])
    }

    #[test]
    fn new_app_has_no_projects_and_normal_mode() {
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let app = App::new(tx, atx);
        assert_eq!(app.projects.len(), 0);
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(!app.should_quit);
    }

    #[test]
    fn active_project_is_none_when_empty() {
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let app = App::new(tx, atx);
        assert!(app.active_project().is_none());
    }

    #[test]
    fn next_and_prev_project_wrap_around() {
        let mut app = app_with_projects(&["a", "b", "c"]);

        assert_eq!(app.active_project, 0);
        app.next_project();
        assert_eq!(app.active_project, 1);
        app.next_project();
        assert_eq!(app.active_project, 2);
        app.next_project();
        assert_eq!(app.active_project, 0);

        app.prev_project();
        assert_eq!(app.active_project, 2);
    }

    #[test]
    fn alloc_pane_id_increments() {
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        assert_eq!(app.alloc_pane_id(), 0);
        assert_eq!(app.alloc_pane_id(), 1);
        assert_eq!(app.alloc_pane_id(), 2);
    }

    #[test]
    fn add_project_pushes_activates_and_spawns() {
        let mut app = app_with_projects(&["one"]);
        // Note: app_with_projects already roots its projects at /tmp — use a
        // different existing dir or dedupe will switch instead of adding.
        app.add_project(PathBuf::from("/etc"));
        assert_eq!(app.projects.len(), 2);
        assert_eq!(app.active_project, 1);
        assert_eq!(app.projects[1].name, "etc");
        assert_eq!(app.projects[1].panes.len(), 1, "new project spawns a pane");
    }

    #[test]
    fn add_project_with_duplicate_root_switches_instead() {
        let mut app = app_with_projects(&["one"]);
        app.projects[0].root = std::fs::canonicalize("/tmp").unwrap();
        app.add_project(PathBuf::from("/tmp"));
        assert_eq!(app.projects.len(), 1);
        assert_eq!(app.active_project, 0);
    }

    #[test]
    fn switching_to_empty_project_spawns_a_pane() {
        let mut app = app_with_projects(&["a", "b"]);
        assert!(app.projects[1].panes.is_empty());
        app.set_active_project(1);
        assert_eq!(app.projects[1].panes.len(), 1);
    }

    #[test]
    fn close_last_project_is_refused() {
        let mut app = app_with_projects(&["only"]);
        app.close_active_project();
        assert_eq!(app.projects.len(), 1);
        assert!(app.status_msg.is_some());
    }

    #[test]
    fn close_project_kills_panes_and_activates_neighbor() {
        let mut app = app_with_projects(&["a", "b"]);
        app.set_active_project(1); // spawns a pane in b
        app.close_active_project();
        assert_eq!(app.projects.len(), 1);
        assert_eq!(app.projects[0].name, "a");
        assert_eq!(app.active_project, 0);
    }

    #[test]
    fn closed_panes_history_caps_at_five_and_reopen_respawns() {
        let mut app = app_with_projects(&["demo"]);
        app.spawn_pane(None);
        for i in 0..6 {
            app.spawn_pane(None);
            app.active_project_mut().unwrap().panes.last_mut().unwrap().title = format!("t{i}");
            app.close_active_pane();
        }
        assert_eq!(app.closed_panes.len(), 5);
        assert_eq!(app.closed_panes[0].title, "t5", "most recent first");
        app.reopen_last_pane();
        assert_eq!(app.closed_panes.len(), 4);
        let panes = &app.active_project().unwrap().panes;
        assert!(panes.iter().any(|p| p.title == "t5"));
    }

    #[test]
    fn reopen_targets_originating_project_after_lower_index_close() {
        // Regression: ClosedPane.project used to store a Vec index, which went
        // stale when a lower-indexed project closed afterward — the pane then
        // respawned under the WRONG project. Now it's matched by root.
        let mut app = app_with_projects(&["a", "b", "c"]);
        app.projects[0].root = PathBuf::from("/tmp");
        app.projects[1].root = PathBuf::from("/etc");
        app.projects[2].root = PathBuf::from("/usr");

        // close a pane in project b (root /etc, index 1)
        app.set_active_project(1); // ensure-spawns a pane in b
        app.close_active_pane();
        assert_eq!(app.closed_panes[0].project, PathBuf::from("/etc"));

        // close project a (index 0) → b and c shift to indices 0 and 1
        app.set_active_project(0);
        app.close_active_project();
        assert_eq!(app.projects.len(), 2);
        assert_eq!(app.projects[0].root, PathBuf::from("/etc"));

        // reopen → lands in the ORIGINATING project (root /etc, now index 0),
        // NOT the stale index 1 (old c).
        app.reopen_last_pane();
        assert_eq!(app.active_project, 0);
        assert_eq!(app.projects[0].root, PathBuf::from("/etc"));
        assert!(
            !app.projects[0].panes.is_empty(),
            "pane respawned under the originating project"
        );
    }

    #[test]
    fn reopen_falls_back_to_active_project_when_origin_gone() {
        // Originating project closed entirely → respawn under the ACTIVE project.
        let mut app = app_with_projects(&["a", "b"]);
        app.projects[0].root = PathBuf::from("/tmp");
        app.projects[1].root = PathBuf::from("/etc");

        // close a pane in b (root /etc), then close project b itself
        app.set_active_project(1);
        app.close_active_pane();
        app.close_active_project(); // removes b; a stays, active → 0
        assert_eq!(app.projects.len(), 1);
        assert_eq!(app.projects[0].root, PathBuf::from("/tmp"));

        app.reopen_last_pane();
        assert_eq!(app.active_project, 0);
        assert!(
            !app.projects[0].panes.is_empty(),
            "pane respawned under the active (fallback) project"
        );
    }

    #[test]
    fn adjust_split_clamps() {
        let mut app = app_with_projects(&["demo"]);
        app.adjust_split(1.0);
        assert_eq!(app.projects[0].col_split, 0.85);
        app.adjust_split(-2.0);
        assert_eq!(app.projects[0].col_split, 0.15);
    }

    #[test]
    fn expand_tilde_replaces_leading_tilde() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/x"), PathBuf::from(format!("{home}/x")));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
    }

    #[test]
    fn sidebar_items_len_follows_files_or_branches() {
        let mut app = app_with_one_project();
        app.git_status = Some(crate::git::GitStatus {
            branch: "main".into(),
            files: vec![
                crate::git::ChangedFile { status: 'M', path: "a".into() },
                crate::git::ChangedFile { status: '?', path: "b".into() },
            ],
        });
        assert_eq!(app.sidebar_items_len(), 2);
        app.sidebar_branches = true;
        app.sidebar_branch_list = vec!["main".into(), "dev".into()];
        assert_eq!(app.sidebar_items_len(), 2);
    }

    #[test]
    fn sidebar_move_clamps_selection() {
        let mut app = app_with_one_project();
        app.git_status = Some(crate::git::GitStatus {
            branch: "m".into(),
            files: vec![crate::git::ChangedFile { status: 'M', path: "a".into() }],
        });
        app.sidebar_move(1);
        assert_eq!(app.sidebar_sel, 0); // wraps or clamps to len-1
        app.sidebar_move(-1);
        assert_eq!(app.sidebar_sel, 0);
    }

    #[test]
    fn start_search_populates_pane_state_and_mode() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            pane.parser.lock().unwrap().process(b"hello world\r\n");
        }
        app.start_search("hello");
        assert!(matches!(app.mode, InputMode::Search));
        let pane = &app.active_project().unwrap().panes[0];
        assert_eq!(pane.search.as_ref().unwrap().matches.len(), 1);
    }

    #[test]
    fn start_search_with_no_matches_flashes_and_stays_normal() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.start_search("zzz-no-match");
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(app.status_msg.is_some());
    }

    #[test]
    fn exit_search_clears_pane_state_and_mode() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        {
            let pane = &app.active_project().unwrap().panes[0];
            pane.parser.lock().unwrap().process(b"needle\r\n");
        }
        app.start_search("needle");
        app.exit_search();
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(app.active_project().unwrap().panes[0].search.is_none());
    }

    #[test]
    fn commit_root_survives_project_switch() {
        // The AI worker carries the root it polled; the CommitMsg submit path
        // must use THAT root even when the user switched projects mid-request.
        let mut app = app_with_projects(&["a", "b"]);
        app.projects[0].root = PathBuf::from("/tmp");
        app.projects[1].root = PathBuf::from("/etc");

        // AiMessage drain stores the polled root, then the user switches.
        app.commit_root = Some(PathBuf::from("/tmp"));
        app.set_active_project(1);
        assert_eq!(app.active_root(), Some(PathBuf::from("/etc")));

        // The submit path reads commit_root FIRST — same expression as
        // input.rs's CommitMsg arm — so the polled root wins.
        let root = app
            .commit_root
            .take()
            .unwrap_or_else(|| app.active_root().unwrap_or_default());
        assert_eq!(root, PathBuf::from("/tmp"));
        assert!(app.commit_root.is_none(), "take() consumes the stored root");

        // With no stored root the fallback is the active project.
        let root = app
            .commit_root
            .take()
            .unwrap_or_else(|| app.active_root().unwrap_or_default());
        assert_eq!(root, PathBuf::from("/etc"));
    }

    #[test]
    fn project_switch_clears_cached_git_view() {
        let mut app = app_with_projects(&["a", "b"]);
        app.git_status = Some(crate::git::GitStatus {
            branch: "main".into(),
            files: vec![crate::git::ChangedFile { status: 'M', path: "x".into() }],
        });
        app.sidebar_branch_list = vec!["main".into()];
        app.set_active_project(1);
        assert!(app.git_status.is_none());
        assert!(app.sidebar_branch_list.is_empty());
    }

    #[test]
    fn hide_active_pane_marks_hidden_and_moves_focus() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_pane(None);
        app.spawn_pane(None); // active = 2
        app.hide_active_pane();
        let p = app.active_project().unwrap();
        assert!(p.panes[2].hidden, "active pane was hidden");
        assert_eq!(p.active_pane, 1, "focus moved to nearest visible");
        assert_eq!(p.panes.len(), 3, "hidden pane stays in the vec");
        assert_eq!(p.hidden_count(), 1);
    }

    #[test]
    fn hide_last_visible_pane_is_refused() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.hide_active_pane();
        let p = app.active_project().unwrap();
        assert!(!p.panes[0].hidden, "can't hide the only pane");
        assert!(app.status_msg.is_some());
    }

    #[test]
    fn unhide_pane_restores_and_focuses() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_pane(None);
        app.hide_active_pane(); // hides pane 1
        app.unhide_pane(1);
        let p = app.active_project().unwrap();
        assert!(!p.panes[1].hidden);
        assert_eq!(p.active_pane, 1, "restored pane takes focus");
    }

    #[test]
    fn ensure_active_pane_surfaces_hidden_instead_of_spawning() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        // Force the all-hidden state (the leader guard normally prevents it).
        app.active_project_mut().unwrap().panes[0].hidden = true;
        app.ensure_active_pane();
        let p = app.active_project().unwrap();
        assert_eq!(p.panes.len(), 1, "no extra pane spawned");
        assert!(!p.panes[0].hidden, "the hidden pane was surfaced");
        assert_eq!(p.active_pane, 0);
    }

    #[test]
    fn close_pane_skips_focus_past_hidden() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_pane(None);
        app.spawn_pane(None);
        // Hide pane 0, keep active on pane 2, then close pane 2 — the clamp
        // lands on index 1 (visible), not the hidden pane 0.
        app.active_project_mut().unwrap().panes[0].hidden = true;
        app.close_active_pane();
        let p = app.active_project().unwrap();
        assert_eq!(p.panes.len(), 2);
        assert_eq!(p.active_pane, 1);
        assert!(!p.panes[p.active_pane].hidden);
    }

    #[test]
    fn closing_last_visible_pane_surfaces_a_hidden_one() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        app.spawn_pane(None);
        app.active_project_mut().unwrap().panes[0].hidden = true;
        app.close_active_pane(); // closes visible pane 1
        let p = app.active_project().unwrap();
        assert_eq!(p.panes.len(), 1);
        assert!(!p.panes[0].hidden, "the hidden pane was surfaced");
        assert_eq!(p.active_pane, 0);
    }
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            p.push(rest);
            return p;
        }
    } else if path == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(path)
}

/// Single-quote a path for embedding in a shell command line.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
