use portable_pty::MasterPty;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

pub type PaneId = u32;

pub enum PaneEvent {
    Output(PaneId),
    Exited(PaneId),
}

pub struct Pane {
    pub id: PaneId,
    pub title: String,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub exited: Option<String>,
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

        Ok(Pane {
            id,
            title,
            parser,
            exited: None,
            writer: spawned.writer,
            master: spawned.master,
            child: spawned.child,
        })
    }

    pub fn write_input(&self, data: &[u8]) -> anyhow::Result<()> {
        self.writer.lock().unwrap().write_all(data)?;
        Ok(())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> anyhow::Result<()> {
        self.parser.lock().unwrap().screen_mut().set_size(rows, cols);
        self.master.resize(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    pub fn kill(&mut self) -> anyhow::Result<()> {
        self.child.kill()?;
        let _ = self.child.wait();
        Ok(())
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
        let pane = Pane::spawn(1, "test".into(), 24, 80, None, tx).unwrap();
        pane.write_input(b"echo rustterm-pane-ok\n").unwrap();
        assert!(
            drain_until(&rx, &pane.parser, "rustterm-pane-ok", Duration::from_secs(3)),
            "expected 'rustterm-pane-ok' to appear in the parsed screen"
        );
    }

    #[test]
    fn kill_terminates_the_child_and_reader_reports_exit() {
        let (tx, rx) = mpsc::channel();
        let mut pane = Pane::spawn(2, "test".into(), 24, 80, None, tx).unwrap();
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
}
