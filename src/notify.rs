use crate::agents;
use crate::app::App;
use crate::pane::{Pane, PaneId};
use crate::watcher::WatchEvent;
use std::time::{Duration, Instant};

pub const NOTIFY_COOLDOWN: Duration = Duration::from_secs(30);

/// Port of FLAME's shouldNotify minus the window-focus check (a TUI can't
/// detect outer-terminal focus portably): only the pane the user is
/// currently watching is exempt from desktop notifications.
pub fn should_notify(pane_is_focused: bool) -> bool {
    !pane_is_focused
}

pub fn dispatch(app: &mut App, pane_id: PaneId, event: WatchEvent) {
    // Badge mutations first.
    match &event {
        WatchEvent::Waiting(w) => {
            if let Some(pane) = find_pane_mut(app, pane_id) {
                pane.waiting = *w;
            }
        }
        WatchEvent::Done { .. } => {
            if let Some(pane) = find_pane_mut(app, pane_id) {
                pane.attention = true;
            }
        }
        WatchEvent::Command(cmd) => auto_tag(app, pane_id, cmd),
    }

    // Then maybe a desktop notification.
    let Some((title, body)) = decide(app, pane_id, &event) else {
        return;
    };
    let focused = app.focused_pane_id() == Some(pane_id);
    if !should_notify(focused) {
        return;
    }
    if let Some(pane) = find_pane_mut(app, pane_id) {
        let now = Instant::now();
        if pane
            .last_notify_at
            .map(|t| now.duration_since(t) < NOTIFY_COOLDOWN)
            .unwrap_or(false)
        {
            return;
        }
        pane.last_notify_at = Some(now);
        let _ = notify_rust::Notification::new()
            .summary(&title)
            .body(&body)
            .show(); // best-effort: headless/no-dbus failures are ignored
    }
}

/// Pure decision: what (if anything) to notify for this event. Title+body.
fn decide(app: &App, pane_id: PaneId, event: &WatchEvent) -> Option<(String, String)> {
    let pane = find_pane(app, pane_id)?;
    match event {
        WatchEvent::Done { command } => {
            let agent = agents::detect(command);
            let title = agent
                .map(|a| format!("{} finished", a.name))
                .unwrap_or_else(|| "Terminal task finished".to_string());
            let body = if command.is_empty() {
                "A task completed or is waiting for input".to_string()
            } else {
                command.clone()
            };
            Some((title, body))
        }
        WatchEvent::Waiting(true) => Some((
            format!("{} needs your input", pane.title),
            "Waiting on a prompt or confirmation".to_string(),
        )),
        _ => None,
    }
}

fn auto_tag(app: &mut App, pane_id: PaneId, command: &str) {
    let Some(spec) = agents::detect(command) else {
        return;
    };
    if let Some(pane) = find_pane_mut(app, pane_id) {
        if !pane.agent_tagged && pane.color.is_none() && pane.title == format!("pane-{}", pane.id) {
            pane.title = spec.name.to_string();
            pane.color = Some(spec.color);
            pane.agent_tagged = true;
        }
    }
}

fn find_pane(app: &App, pane_id: PaneId) -> Option<&Pane> {
    app.projects
        .iter()
        .flat_map(|p| p.panes.iter())
        .find(|p| p.id == pane_id)
}

fn find_pane_mut(app: &mut App, pane_id: PaneId) -> Option<&mut Pane> {
    app.projects
        .iter_mut()
        .flat_map(|p| p.panes.iter_mut())
        .find(|p| p.id == pane_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use crate::pane::Pane;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn app_with_pane() -> (App, PaneId) {
        let (tx, _rx) = mpsc::channel();
        let (atx, _arx) = mpsc::channel();
        let mut app = App::new(tx, atx);
        let mut project = Project::new("demo".into(), PathBuf::from("/tmp"));
        let (ptx, _prx) = mpsc::channel();
        let pane = Pane::spawn(7, "pane-7".into(), 24, 80, None, ptx, None).unwrap();
        let id = pane.id;
        project.panes.push(pane);
        app.projects.push(project);
        (app, id)
    }

    #[test]
    fn should_notify_skips_only_the_focused_pane() {
        assert!(!should_notify(true));
        assert!(should_notify(false));
    }

    #[test]
    fn waiting_event_sets_badge() {
        let (mut app, id) = app_with_pane();
        dispatch(&mut app, id, WatchEvent::Waiting(true));
        assert!(app.projects[0].panes[0].waiting);
        dispatch(&mut app, id, WatchEvent::Waiting(false));
        assert!(!app.projects[0].panes[0].waiting);
    }

    #[test]
    fn done_event_sets_attention() {
        let (mut app, id) = app_with_pane();
        dispatch(&mut app, id, WatchEvent::Done { command: "claude".into() });
        assert!(app.projects[0].panes[0].attention);
    }

    #[test]
    fn command_event_auto_tags_default_pane() {
        let (mut app, id) = app_with_pane();
        dispatch(&mut app, id, WatchEvent::Command("claude --continue".into()));
        let pane = &app.projects[0].panes[0];
        assert_eq!(pane.title, "claude");
        assert!(pane.agent_tagged);
    }

    #[test]
    fn command_event_does_not_retag_renamed_pane() {
        let (mut app, id) = app_with_pane();
        app.projects[0].panes[0].title = "my work".into();
        dispatch(&mut app, id, WatchEvent::Command("claude".into()));
        assert_eq!(app.projects[0].panes[0].title, "my work");
        assert!(!app.projects[0].panes[0].agent_tagged);
    }
}
