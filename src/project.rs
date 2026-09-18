use crate::pane::Pane;
use std::path::PathBuf;

pub struct Project {
    pub name: String,
    pub root: PathBuf,
    pub panes: Vec<Pane>,
    pub active_pane: usize,
    pub col_split: f32,
    pub row_split: f32,
}

impl Project {
    pub fn new(name: String, root: PathBuf) -> Project {
        Project {
            name,
            root,
            panes: Vec::new(),
            active_pane: 0,
            col_split: 0.5,
            row_split: 0.5,
        }
    }

    pub fn active_pane(&self) -> Option<&Pane> {
        self.panes.get(self.active_pane)
    }

    pub fn next_pane(&mut self) {
        if !self.panes.is_empty() {
            self.active_pane = (self.active_pane + 1) % self.panes.len();
        }
    }

    pub fn prev_pane(&mut self) {
        if !self.panes.is_empty() {
            self.active_pane = if self.active_pane == 0 {
                self.panes.len() - 1
            } else {
                self.active_pane - 1
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::PaneEvent;
    use std::sync::mpsc;

    fn dummy_pane(id: u32) -> Pane {
        let (tx, _rx) = mpsc::channel::<PaneEvent>();
        Pane::spawn(id, format!("pane-{id}"), 24, 80, None, tx).unwrap()
    }

    #[test]
    fn new_project_has_no_panes_and_default_splits() {
        let p = Project::new("demo".into(), PathBuf::from("/tmp"));
        assert_eq!(p.panes.len(), 0);
        assert_eq!(p.active_pane, 0);
        assert_eq!(p.col_split, 0.5);
        assert_eq!(p.row_split, 0.5);
    }

    #[test]
    fn active_pane_is_none_when_empty() {
        let p = Project::new("demo".into(), PathBuf::from("/tmp"));
        assert!(p.active_pane().is_none());
    }

    #[test]
    fn next_and_prev_pane_wrap_around() {
        let mut p = Project::new("demo".into(), PathBuf::from("/tmp"));
        p.panes.push(dummy_pane(1));
        p.panes.push(dummy_pane(2));
        p.panes.push(dummy_pane(3));

        assert_eq!(p.active_pane, 0);
        p.next_pane();
        assert_eq!(p.active_pane, 1);
        p.next_pane();
        assert_eq!(p.active_pane, 2);
        p.next_pane();
        assert_eq!(p.active_pane, 0, "next_pane should wrap from last to first");

        p.prev_pane();
        assert_eq!(p.active_pane, 2, "prev_pane should wrap from first to last");
    }

    #[test]
    fn next_prev_pane_on_empty_project_does_not_panic() {
        let mut p = Project::new("demo".into(), PathBuf::from("/tmp"));
        p.next_pane();
        p.prev_pane();
        assert_eq!(p.active_pane, 0);
    }
}
