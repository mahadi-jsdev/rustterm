use crate::pane::PaneEvent;
use crate::project::Project;
use std::sync::mpsc;

pub enum InputMode {
    Normal,
    Leader,
}

pub struct App {
    pub projects: Vec<Project>,
    pub active_project: usize,
    pub mode: InputMode,
    pub next_pane_id: u32,
    pub should_quit: bool,
    pub events_tx: mpsc::Sender<PaneEvent>,
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
}
