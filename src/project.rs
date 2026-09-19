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

    pub fn active_pane_mut(&mut self) -> Option<&mut Pane> {
        self.panes.get_mut(self.active_pane)
    }

    /// Indices of non-hidden panes, in order — what renders and what
    /// pane navigation walks.
    pub fn visible_indices(&self) -> Vec<usize> {
        self.panes
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.hidden)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn visible_count(&self) -> usize {
        self.panes.iter().filter(|p| !p.hidden).count()
    }

    pub fn hidden_count(&self) -> usize {
        self.panes.iter().filter(|p| p.hidden).count()
    }

    /// Index of the first hidden pane (used by ensure/unhide paths).
    pub fn first_hidden(&self) -> Option<usize> {
        self.panes.iter().position(|p| p.hidden)
    }

    /// Nearest visible pane index to `from` by distance — forward wins
    /// ties. `None` when every pane is hidden.
    pub fn nearest_visible(&self, from: usize) -> Option<usize> {
        let n = self.panes.len();
        for d in 0..n {
            for i in [from + d, from.wrapping_sub(d)] {
                if i < n && !self.panes[i].hidden {
                    return Some(i);
                }
            }
        }
        None
    }

    pub fn next_pane(&mut self) {
        let visible = self.visible_indices();
        if let Some(pos) = visible.iter().position(|&i| i == self.active_pane) {
            self.active_pane = visible[(pos + 1) % visible.len()];
        } else if let Some(&i) = visible.first() {
            self.active_pane = i;
        }
    }

    pub fn prev_pane(&mut self) {
        let visible = self.visible_indices();
        if let Some(pos) = visible.iter().position(|&i| i == self.active_pane) {
            self.active_pane = visible[if pos == 0 { visible.len() - 1 } else { pos - 1 }];
        } else if let Some(&i) = visible.first() {
            self.active_pane = i;
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
        Pane::spawn(id, format!("pane-{id}"), 24, 80, None, tx, None).unwrap()
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

    fn project_with_hidden(hidden: &[usize]) -> Project {
        let mut p = Project::new("demo".into(), PathBuf::from("/tmp"));
        for i in 0..4 {
            p.panes.push(dummy_pane(i as u32 + 1));
        }
        for &i in hidden {
            p.panes[i].hidden = true;
        }
        p
    }

    #[test]
    fn visible_indices_and_counts_skip_hidden() {
        let p = project_with_hidden(&[1, 3]);
        assert_eq!(p.visible_indices(), vec![0, 2]);
        assert_eq!(p.visible_count(), 2);
        assert_eq!(p.hidden_count(), 2);
        assert_eq!(p.first_hidden(), Some(1));
    }

    #[test]
    fn nearest_visible_returns_closest_by_distance() {
        let p = project_with_hidden(&[1, 2]);
        assert_eq!(p.nearest_visible(0), Some(0));
        assert_eq!(p.nearest_visible(1), Some(0), "back-1 beats forward-2");
        assert_eq!(p.nearest_visible(2), Some(3), "forward-1 beats back-2");
        assert_eq!(p.nearest_visible(3), Some(3));
        let all_hidden = project_with_hidden(&[0, 1, 2, 3]);
        assert_eq!(all_hidden.nearest_visible(0), None);
    }

    #[test]
    fn next_pane_skips_hidden() {
        let mut p = project_with_hidden(&[1]);
        assert_eq!(p.active_pane, 0);
        p.next_pane();
        assert_eq!(p.active_pane, 2, "skips hidden pane 1");
        p.next_pane();
        assert_eq!(p.active_pane, 3);
        p.next_pane();
        assert_eq!(p.active_pane, 0, "wraps over hidden pane 1");
    }

    #[test]
    fn prev_pane_skips_hidden() {
        let mut p = project_with_hidden(&[1, 2]);
        p.active_pane = 3;
        p.prev_pane();
        assert_eq!(p.active_pane, 0, "skips hidden panes 1-2");
        p.prev_pane();
        assert_eq!(p.active_pane, 3, "wraps back over hidden panes");
    }
}
