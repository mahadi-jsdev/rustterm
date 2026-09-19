//! Workspace persistence — projects and pane specs survive `leader q`.
//! Processes do NOT persist (PTY children die on exit; a live PTY can't
//! be reattached). Restore = fresh shells in saved cwds replaying each
//! pane's `startup_command`.

use crate::app::App;
use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub active_project: usize,
    pub sidebar_visible: bool,
    pub projects: Vec<SessionProject>,
}

#[derive(Serialize, Deserialize)]
pub struct SessionProject {
    pub name: String,
    pub root: PathBuf,
    pub active_pane: usize,
    pub col_split: f32,
    pub row_split: f32,
    pub panes: Vec<SessionPane>,
}

#[derive(Serialize, Deserialize)]
pub struct SessionPane {
    pub title: String,
    pub cwd: PathBuf,
    pub startup_command: Option<String>,
    /// "rrggbb" hex — pane colors only come from agent tagging (Rgb).
    pub color: Option<String>,
    pub hidden: bool,
}

pub fn capture(app: &App) -> Session {
    Session {
        version: VERSION,
        active_project: app.active_project,
        sidebar_visible: app.sidebar_visible,
        projects: app
            .projects
            .iter()
            .map(|p| SessionProject {
                name: p.name.clone(),
                root: p.root.clone(),
                active_pane: p.active_pane,
                col_split: p.col_split,
                row_split: p.row_split,
                panes: p
                    .panes
                    .iter()
                    .map(|pane| SessionPane {
                        title: pane.title.clone(),
                        cwd: pane.cwd.clone(),
                        startup_command: pane.startup_command.clone(),
                        color: pane.color.and_then(color_to_str),
                        hidden: pane.hidden,
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("rustterm")
}

pub fn session_path() -> PathBuf {
    data_dir().join("session.json")
}

pub fn save(app: &App) -> std::io::Result<()> {
    save_to(app, &session_path())
}

pub fn load() -> Option<Session> {
    load_from(&session_path())
}

pub fn save_to(app: &App, path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(&capture(app))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, json)
}

/// Missing, corrupt, wrong-version, or empty session → `None` (fresh
/// start) — restore never crashes a launch.
pub fn load_from(path: &Path) -> Option<Session> {
    let text = std::fs::read_to_string(path).ok()?;
    let s: Session = serde_json::from_str(&text).ok()?;
    if s.version == VERSION && !s.projects.is_empty() {
        Some(s)
    } else {
        None
    }
}

pub fn color_to_str(c: Color) -> Option<String> {
    match c {
        Color::Rgb(r, g, b) => Some(format!("{r:02x}{g:02x}{b:02x}")),
        _ => None,
    }
}

pub fn color_from_str(s: &str) -> Option<Color> {
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color::Rgb(
        ((v >> 16) & 0xff) as u8,
        ((v >> 8) & 0xff) as u8,
        (v & 0xff) as u8,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use std::sync::mpsc;

    fn app_fixture() -> App {
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        app.projects
            .push(Project::new("one".into(), PathBuf::from("/tmp")));
        app.projects
            .push(Project::new("two".into(), PathBuf::from("/etc")));
        app
    }

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rustterm-session-{name}-{}", std::process::id()))
    }

    #[test]
    fn round_trip_preserves_structure() {
        let mut app = app_fixture();
        app.sidebar_visible = false;
        app.spawn_pane(None);
        app.spawn_pane(None);
        app.active_project_mut().unwrap().panes[1].hidden = true;
        app.active_project_mut().unwrap().panes[0].title = "api".into();
        app.active_project_mut().unwrap().panes[0].color =
            Some(Color::Rgb(0xff, 0xb2, 0x38));
        app.active_project_mut().unwrap().col_split = 0.3;
        app.active_project_mut().unwrap().active_pane = 0;

        let json = serde_json::to_string(&capture(&app)).unwrap();
        let s: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(s.version, VERSION);
        assert_eq!(s.active_project, 0);
        assert!(!s.sidebar_visible);
        assert_eq!(s.projects.len(), 2);
        assert_eq!(s.projects[0].name, "one");
        assert_eq!(s.projects[0].panes.len(), 2);
        assert!(!s.projects[0].panes[0].hidden);
        assert!(s.projects[0].panes[1].hidden);
        assert_eq!(s.projects[0].panes[0].title, "api");
        assert_eq!(
            s.projects[0].panes[0].color.as_deref(),
            Some("ffb238")
        );
        assert!((s.projects[0].col_split - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn save_and_load_via_file() {
        let path = tmp_path("file.json");
        let _ = std::fs::remove_file(&path);
        let mut app = app_fixture();
        app.spawn_pane(None);
        save_to(&app, &path).unwrap();
        let s = load_from(&path).expect("saved session must load");
        assert_eq!(s.projects.len(), 2);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn load_missing_file_returns_none() {
        assert!(load_from(&tmp_path("does-not-exist.json")).is_none());
    }

    #[test]
    fn load_corrupt_file_returns_none() {
        let path = tmp_path("corrupt.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_from(&path).is_none());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn load_wrong_version_or_empty_projects_returns_none() {
        let path = tmp_path("version.json");
        let s = Session {
            version: 99,
            active_project: 0,
            sidebar_visible: true,
            projects: vec![SessionProject {
                name: "x".into(),
                root: PathBuf::from("/tmp"),
                active_pane: 0,
                col_split: 0.5,
                row_split: 0.5,
                panes: vec![],
            }],
        };
        std::fs::write(&path, serde_json::to_string(&s).unwrap()).unwrap();
        assert!(load_from(&path).is_none(), "version 99 rejected");

        let s = Session {
            version: VERSION,
            active_project: s.active_project,
            sidebar_visible: s.sidebar_visible,
            projects: vec![],
        };
        std::fs::write(&path, serde_json::to_string(&s).unwrap()).unwrap();
        assert!(load_from(&path).is_none(), "empty projects rejected");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn color_hex_round_trip() {
        let c = Color::Rgb(0xff, 0xb2, 0x38);
        assert_eq!(color_to_str(c).as_deref(), Some("ffb238"));
        assert_eq!(color_from_str("ffb238"), Some(c));
        assert_eq!(color_from_str("zzzzzz"), None);
        assert_eq!(color_from_str("fff"), None);
        assert_eq!(color_to_str(Color::Red), None, "non-Rgb drops");
    }
}
