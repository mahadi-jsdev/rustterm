use crate::agents;
use crate::app::{App, InputMode, LinePurpose};
use crate::text_input::LineEdit;

#[derive(Debug, Clone, PartialEq)]
pub enum CmdId {
    NewPane,
    ClosePane,
    NextPane,
    PrevPane,
    JumpPane(usize),
    IncSplit,
    DecSplit,
    AddProject,
    SwitchProject(usize),
    CloseProject,
    ReopenPane,
    RunAgent(&'static str),
    RenamePane,
    Quit,
}

pub struct Command {
    pub id: CmdId,
    pub label: String,
}

pub struct Palette {
    pub query: String,
    pub selected: usize,
    items: Vec<Command>,
}

impl Palette {
    pub fn open(app: &App) -> Palette {
        let mut items: Vec<Command> = Vec::new();
        items.push(Command { id: CmdId::NewPane, label: "New pane".into() });
        items.push(Command { id: CmdId::ClosePane, label: "Close pane".into() });
        items.push(Command { id: CmdId::NextPane, label: "Next pane".into() });
        items.push(Command { id: CmdId::PrevPane, label: "Previous pane".into() });
        if let Some(project) = app.active_project() {
            for (i, pane) in project.panes.iter().enumerate() {
                items.push(Command {
                    id: CmdId::JumpPane(i),
                    label: format!("Jump to pane {}: {}", i + 1, pane.title),
                });
            }
        }
        items.push(Command { id: CmdId::IncSplit, label: "Increase split".into() });
        items.push(Command { id: CmdId::DecSplit, label: "Decrease split".into() });
        items.push(Command { id: CmdId::AddProject, label: "Add project…".into() });
        for (i, project) in app.projects.iter().enumerate() {
            items.push(Command {
                id: CmdId::SwitchProject(i),
                label: format!("Switch to project: {}", project.name),
            });
        }
        items.push(Command { id: CmdId::CloseProject, label: "Close project".into() });
        if !app.closed_panes.is_empty() {
            items.push(Command {
                id: CmdId::ReopenPane,
                label: "Reopen last closed pane".into(),
            });
        }
        for spec in agents::installed() {
            items.push(Command {
                id: CmdId::RunAgent(spec.binary),
                label: format!("Run {}", spec.name),
            });
        }
        items.push(Command { id: CmdId::RenamePane, label: "Rename pane…".into() });
        items.push(Command { id: CmdId::Quit, label: "Quit".into() });
        Palette { query: String::new(), selected: 0, items }
    }

    pub fn set_query(&mut self, q: String) {
        self.query = q;
        self.selected = 0;
    }

    pub fn filtered(&self) -> Vec<&Command> {
        let mut scored: Vec<(f64, &Command)> = self
            .items
            .iter()
            .filter_map(|c| fuzzy_score(&self.query, &c.label).map(|s| (s, c)))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().map(|(_, c)| c).collect()
    }

    pub fn move_next(&mut self) {
        let n = self.filtered().len();
        if n > 0 {
            self.selected = (self.selected + 1) % n;
        }
    }

    pub fn move_prev(&mut self) {
        let n = self.filtered().len();
        if n > 0 {
            self.selected = if self.selected == 0 { n - 1 } else { self.selected - 1 };
        }
    }

    pub fn selected_command(&self) -> Option<&Command> {
        self.filtered().get(self.selected).copied()
    }
}

/// Direct port of FLAME's fuzzyScore: in-order subsequence match;
/// consecutive-run bonus, word/path-boundary bonus, small length penalty.
pub fn fuzzy_score(query: &str, target: &str) -> Option<f64> {
    if query.is_empty() {
        return Some(0.0);
    }
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = target.to_lowercase().chars().collect();
    let mut qi = 0;
    let mut score = 0i64;
    let mut consecutive = 0i64;
    let mut last_match: isize = -1;

    for ti in 0..t.len() {
        if qi >= q.len() {
            break;
        }
        if t[ti] != q[qi] {
            continue;
        }
        score += 1;
        if last_match == ti as isize - 1 {
            consecutive += 1;
            score += consecutive * 2;
        } else {
            consecutive = 0;
        }
        if ti == 0 || "/.-_ ".contains(t[ti - 1]) {
            score += 3;
        }
        last_match = ti as isize;
        qi += 1;
    }
    if qi < q.len() {
        return None;
    }
    Some(score as f64 - t.len() as f64 * 0.01)
}

pub fn execute(app: &mut App, id: &CmdId) {
    match id {
        CmdId::NewPane => app.spawn_pane(None),
        CmdId::ClosePane => app.close_active_pane(),
        CmdId::NextPane => {
            if let Some(p) = app.active_project_mut() {
                p.next_pane();
            }
        }
        CmdId::PrevPane => {
            if let Some(p) = app.active_project_mut() {
                p.prev_pane();
            }
        }
        CmdId::JumpPane(i) => {
            if let Some(p) = app.active_project_mut() {
                if *i < p.panes.len() {
                    p.active_pane = *i;
                }
            }
        }
        CmdId::IncSplit => app.adjust_split(0.05),
        CmdId::DecSplit => app.adjust_split(-0.05),
        CmdId::AddProject => {
            app.mode = InputMode::LineInput(LinePurpose::AddProject);
            app.line_input = Some(LineEdit::new());
        }
        CmdId::SwitchProject(i) => app.set_active_project(*i),
        CmdId::CloseProject => app.close_active_project(),
        CmdId::ReopenPane => app.reopen_last_pane(),
        CmdId::RunAgent(binary) => app.spawn_pane(Some(binary)),
        CmdId::RenamePane => {
            let current = app
                .active_project()
                .and_then(|p| p.active_pane())
                .map(|p| p.title.clone())
                .unwrap_or_default();
            app.mode = InputMode::LineInput(LinePurpose::RenamePane);
            app.line_input = Some(LineEdit::from_str(&current));
        }
        CmdId::Quit => app.should_quit = true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn app_with_projects(names: &[&str]) -> App {
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        for n in names {
            app.projects.push(Project::new((*n).into(), PathBuf::from("/tmp")));
        }
        app
    }

    #[test]
    fn fuzzy_subsequence_match_with_boundary_bonus() {
        assert!(fuzzy_score("np", "New pane").is_some());
        assert!(fuzzy_score("xyz", "New pane").is_none());
        // "gp"-style: boundary matches beat mid-word matches
        let word = fuzzy_score("cp", "Close pane").unwrap();
        let mid = fuzzy_score("cp", "accept pepper").unwrap();
        assert!(word > mid);
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(fuzzy_score("", "anything"), Some(0.0));
    }

    #[test]
    fn palette_lists_core_commands_and_projects() {
        let app = app_with_projects(&["alpha", "beta"]);
        let p = Palette::open(&app);
        let labels: Vec<&str> = p.filtered().iter().map(|c| c.label.as_str()).collect();
        assert!(labels.iter().any(|l| l.contains("New pane")));
        assert!(labels.iter().any(|l| l.contains("alpha")));
        assert!(labels.iter().any(|l| l.contains("beta")));
        assert!(labels.iter().any(|l| l.contains("Quit")));
    }

    #[test]
    fn query_filters_items() {
        let app = app_with_projects(&["alpha"]);
        let mut p = Palette::open(&app);
        p.set_query("quit".into());
        let labels: Vec<&str> = p.filtered().iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["Quit"]);
    }

    #[test]
    fn navigation_wraps_and_selection_tracks_filter() {
        let app = app_with_projects(&["alpha"]);
        let mut p = Palette::open(&app);
        p.set_query("quit".into());
        p.move_next(); // wraps on a 1-item list
        assert_eq!(p.selected_command().map(|c| &c.id), Some(&CmdId::Quit));
        p.move_prev();
        assert_eq!(p.selected_command().map(|c| &c.id), Some(&CmdId::Quit));
    }
}
