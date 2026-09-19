use crate::watcher::Watcher;
use portable_pty::MasterPty;
use ratatui::style::Color;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub type PaneId = u32;

pub enum PaneEvent {
    Output(PaneId),
    Exited(PaneId),
}

pub enum PaneStatus {
    Running,
    Exited(i32),
    Failed(String),
}

pub struct Pane {
    pub id: PaneId,
    pub title: String,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub status: PaneStatus,
    pub cwd: PathBuf,
    pub color: Option<Color>,
    pub agent_tagged: bool,
    pub running: bool,
    pub waiting: bool,
    pub attention: bool,
    pub last_notify_at: Option<Instant>,
    pub watcher: Watcher,
    pub startup_command: Option<String>,
    pub search: Option<crate::search::SearchState>,
    /// Backgrounded panes keep running (PTY alive, watcher + badges
    /// active) but drop out of the render grid and pane navigation.
    pub hidden: bool,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Pane {
    pub fn spawn(
        id: PaneId,
        title: String,
        rows: u16,
        cols: u16,
        cwd: Option<&Path>,
        events_tx: mpsc::Sender<PaneEvent>,
        startup_command: Option<&str>,
    ) -> anyhow::Result<Pane> {
        let spawned = crate::pty::spawn(rows, cols, cwd)?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 10_000)));

        let mut reader = spawned.reader;
        let reader_parser = Arc::clone(&parser);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = events_tx.send(PaneEvent::Exited(id));
                        break;
                    }
                    Ok(n) => {
                        reader_parser.lock().unwrap().process(&buf[..n]);
                        if events_tx.send(PaneEvent::Output(id)).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = events_tx.send(PaneEvent::Exited(id));
                        break;
                    }
                }
            }
        });

        let pane = Pane {
            id,
            title,
            parser,
            status: PaneStatus::Running,
            cwd: cwd.map(|p| p.to_path_buf()).unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            }),
            color: None,
            agent_tagged: false,
            running: false,
            waiting: false,
            attention: false,
            last_notify_at: None,
            watcher: Watcher::new(),
            startup_command: startup_command.map(String::from),
            search: None,
            hidden: false,
            writer: spawned.writer,
            master: spawned.master,
            child: spawned.child,
        };
        if let Some(cmd) = startup_command {
            // PTY input is buffered: the shell reads it whenever it's ready.
            let _ = pane.write_input(format!("{cmd}\r").as_bytes());
        }
        Ok(pane)
    }

    pub fn write_input(&self, data: &[u8]) -> anyhow::Result<()> {
        self.writer.lock().unwrap().write_all(data)?;
        Ok(())
    }

    /// Scroll the view `n` rows up into scrollback history. vt100 clamps
    /// the offset to the available scrollback; 0 is the live view.
    pub fn scroll_up(&self, n: usize) {
        if let Ok(mut p) = self.parser.lock() {
            let s = p.screen_mut();
            let pos = s.scrollback().saturating_add(n);
            s.set_scrollback(pos);
        }
    }

    pub fn scroll_down(&self, n: usize) {
        if let Ok(mut p) = self.parser.lock() {
            let s = p.screen_mut();
            let pos = s.scrollback().saturating_sub(n);
            s.set_scrollback(pos);
        }
    }

    /// Snap back to the live view — called on input so typing never
    /// leaves the user looking at scrollback.
    pub fn scroll_to_bottom(&self) {
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_scrollback(0);
        }
    }

    /// Absolute scroll offset — 0 is the live view; vt100 clamps to
    /// the available scrollback.
    pub fn set_scroll(&self, off: usize) {
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_scrollback(off);
        }
    }

    /// Whether the app in this pane enabled xterm mouse reporting — if
    /// so, wheel events are forwarded to it instead of scrolling locally.
    pub fn mouse_reporting(&self) -> bool {
        self.parser
            .lock()
            .map(|p| p.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None)
            .unwrap_or(false)
    }

    /// Whether the app asked for SGR (1006) encoding vs. legacy X10.
    pub fn mouse_sgr_encoding(&self) -> bool {
        self.parser
            .lock()
            .map(|p| p.screen().mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr)
            .unwrap_or(true)
    }

    pub fn resize(&self, rows: u16, cols: u16) -> anyhow::Result<()> {
        if self.parser.lock().unwrap().screen().size() == (rows, cols) {
            return Ok(());
        }
        self.parser.lock().unwrap().screen_mut().set_size(rows, cols);
        self.master.resize(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Best-effort: a failed kill() must NOT skip wait() — the early `?`
    /// used to leave the child unreaped (zombie) on error.
    pub fn kill(&mut self) -> anyhow::Result<()> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }

    /// Reap the child after the reader thread reports PTY EOF; captures the
    /// real exit code (-1 when unavailable). Idempotent.
    pub fn reap(&mut self) {
        if matches!(self.status, PaneStatus::Running) {
            // portable_pty::ExitStatus::exit_code() -> u32; -1 when wait fails.
            let code = self.child.wait().map(|s| s.exit_code() as i32).unwrap_or(-1);
            self.status = PaneStatus::Exited(code);
        }
    }

    pub fn is_dead(&self) -> bool {
        !matches!(self.status, PaneStatus::Running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn drain_until(
        rx: &mpsc::Receiver<PaneEvent>,
        parser: &Arc<Mutex<vt100::Parser>>,
        needle: &str,
        timeout: Duration,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if rx.recv_timeout(Duration::from_millis(100)).is_ok() {
                let screen = parser.lock().unwrap();
                let contents = screen.screen().contents();
                if contents.contains(needle) {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn write_input_is_visible_in_parsed_screen() {
        let (tx, rx) = mpsc::channel();
        let pane = Pane::spawn(1, "test".into(), 24, 80, None, tx, None).unwrap();
        pane.write_input(b"echo rustterm-pane-ok\n").unwrap();
        assert!(
            drain_until(&rx, &pane.parser, "rustterm-pane-ok", Duration::from_secs(3)),
            "expected 'rustterm-pane-ok' to appear in the parsed screen"
        );
    }

    #[test]
    fn scroll_offsets_move_through_scrollback_and_clamp() {
        let (tx, _rx) = mpsc::channel();
        let pane = Pane::spawn(1, "s".into(), 5, 20, None, tx, None).unwrap();
        {
            let mut p = pane.parser.lock().unwrap();
            for _ in 0..40 {
                p.process(b"line\r\n");
            }
        }
        let offset = || pane.parser.lock().unwrap().screen().scrollback();
        pane.scroll_up(3);
        assert_eq!(offset(), 3);
        pane.scroll_up(10_000); // clamps at the scrollback size
        let clamped = offset();
        assert!(clamped >= 35, "expected deep scrollback, got {clamped}");
        pane.scroll_down(4);
        assert_eq!(offset(), clamped - 4);
        pane.scroll_to_bottom();
        assert_eq!(offset(), 0);
        pane.scroll_down(10); // already at live view — stays
        assert_eq!(offset(), 0);
    }

    #[test]
    fn startup_command_is_written_to_the_pty() {
        let (tx, rx) = mpsc::channel();
        let pane = Pane::spawn(
            3,
            "test".into(),
            24,
            80,
            None,
            tx,
            Some("echo rustterm-startup-ok"),
        )
        .unwrap();
        assert!(
            drain_until(&rx, &pane.parser, "rustterm-startup-ok", Duration::from_secs(3)),
            "expected startup command output on screen"
        );
        assert_eq!(pane.startup_command.as_deref(), Some("echo rustterm-startup-ok"));
    }

    #[test]
    fn kill_terminates_the_child_and_reader_reports_exit() {
        let (tx, rx) = mpsc::channel();
        let mut pane = Pane::spawn(2, "test".into(), 24, 80, None, tx, None).unwrap();
        pane.kill().unwrap();

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saw_exit = false;
        while Instant::now() < deadline {
            if let Ok(PaneEvent::Exited(id)) = rx.recv_timeout(Duration::from_millis(100)) {
                assert_eq!(id, 2);
                saw_exit = true;
                break;
            }
        }
        assert!(saw_exit, "expected a PaneEvent::Exited after kill()");
    }

    #[test]
    fn reap_captures_the_exit_code() {
        let (tx, rx) = mpsc::channel();
        let mut pane = Pane::spawn(1, "s".into(), 24, 80, None, tx, None).unwrap();
        pane.write_input(b"exit 3\n").unwrap();
        // Wait for the reader thread to report PTY EOF.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saw_exit = false;
        while Instant::now() < deadline && !saw_exit {
            if let Ok(PaneEvent::Exited(_)) = rx.recv_timeout(Duration::from_millis(200)) {
                saw_exit = true;
            }
        }
        assert!(saw_exit, "pane never reported exit");
        pane.reap();
        assert!(matches!(pane.status, PaneStatus::Exited(3)));
    }

    #[test]
    fn is_dead_reflects_status() {
        let (tx, _rx) = mpsc::channel();
        let mut pane = Pane::spawn(1, "s".into(), 24, 80, None, tx, None).unwrap();
        assert!(!pane.is_dead());
        pane.status = PaneStatus::Exited(0);
        assert!(pane.is_dead());
        pane.status = PaneStatus::Failed("boom".into());
        assert!(pane.is_dead());
    }
}
