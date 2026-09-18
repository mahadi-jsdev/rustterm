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
    LineInput(LinePurpose),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LinePurpose {
    AddProject,
    RenamePane,
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
    pub fn new(events_tx: mpsc::Sender<PaneEvent>) -> App {
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
        }
    }

    pub fn prev_project(&mut self) {
        if !self.projects.is_empty() {
            self.active_project = if self.active_project == 0 {
                self.projects.len() - 1
            } else {
                self.active_project - 1
            };
        }
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
    }

    pub fn set_active_project(&mut self, idx: usize) {
        if idx < self.projects.len() {
            self.active_project = idx;
            self.ensure_active_pane();
        }
    }

    pub fn ensure_active_pane(&mut self) {
        let needs = self
            .active_project()
            .map(|p| p.panes.is_empty())
            .unwrap_or(false);
        if needs {
            self.spawn_pane(None);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn app_with_projects(names: &[&str]) -> App {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        for name in names {
            app.projects.push(Project::new((*name).into(), PathBuf::from("/tmp")));
        }
        app
    }

    #[test]
    fn new_app_has_no_projects_and_normal_mode() {
        let (tx, _rx) = mpsc::channel();
        let app = App::new(tx);
        assert_eq!(app.projects.len(), 0);
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(!app.should_quit);
    }

    #[test]
    fn active_project_is_none_when_empty() {
        let (tx, _rx) = mpsc::channel();
        let app = App::new(tx);
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
        let mut app = App::new(tx);
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
