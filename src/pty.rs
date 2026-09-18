use portable_pty::{Child, MasterPty, native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub struct PtySpawnResult {
    pub master: Box<dyn MasterPty + Send>,
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub child: Box<dyn Child + Send + Sync>,
    pub reader: Box<dyn Read + Send>,
}

pub fn spawn(rows: u16, cols: u16, cwd: Option<&Path>) -> anyhow::Result<PtySpawnResult> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    if let Some(cwd) = cwd {
        cmd.cwd(cwd);
    }

    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let master = pair.master;
    let writer = master.take_writer()?;
    let reader = master.try_clone_reader()?;

    Ok(PtySpawnResult {
        master,
        writer: Arc::new(Mutex::new(writer)),
        child,
        reader,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn spawned_shell_echoes_written_input() {
        let mut result = spawn(24, 80, None).expect("spawn should succeed");

        // Read in a background thread so a shell that never terminates
        // can't hang the test — the reader loop simply stops when the
        // test's own timeout below gives up waiting for the expected text.
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match result.reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        result
            .writer
            .lock()
            .unwrap()
            .write_all(b"echo rustterm-pty-ok\n")
            .unwrap();

        let mut collected = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
                collected.extend_from_slice(&chunk);
                if String::from_utf8_lossy(&collected).contains("rustterm-pty-ok") {
                    return;
                }
            }
        }
        panic!(
            "expected output to contain 'rustterm-pty-ok', got: {:?}",
            String::from_utf8_lossy(&collected)
        );
    }
}
