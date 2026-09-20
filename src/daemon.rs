//! Fork-keeper daemon — `leader d` detach / `rustterm -a` reattach.
//! See docs/superpowers/specs/2026-09-19-rustterm-detach-design.md.
//!
//! `fork()` duplicates the whole process — App, PTY masters, parsers,
//! watcher — so the keeper needs no serialization. Attach passes the
//! client's stdin/stdout fds via SCM_RIGHTS; the keeper pumps stdin
//! through a permanent internal pty on its own fd 0 (crossterm's
//! global event source binds fd 0 once and must stay isatty-stable).

use crate::app::{App, AppEvent};
use crate::pane::PaneEvent;
use nix::sys::socket::{recvmsg, sendmsg, ControlMessage, ControlMessageOwned, MsgFlags};
use nix::unistd::{dup2, fork, setsid, ForkResult};
use std::io::{IoSlice, IoSliceMut, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, MutexGuard};
use std::time::Duration;

pub const MSG_ATTACH: u8 = 0x01;
pub const MSG_KILL: u8 = 0x02;
pub const REPLY_OK: u8 = b'o';
pub const REPLY_DETACH: u8 = b'd';
pub const REPLY_QUIT: u8 = b'q';
pub const REPLY_KILL: u8 = b'k';
#[allow(dead_code)]
pub const REPLY_BUSY: u8 = b'b'; // protocol reserve — accepts serialize attach

pub fn socket_path() -> PathBuf {
    // RUSTTERM_SOCK overrides the default — parallel/ISOLATED sessions
    // and tests get their own attach socket.
    if let Ok(p) = std::env::var("RUSTTERM_SOCK") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    crate::session::data_dir().join("attach.sock")
}

fn io_err(e: nix::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e)
}

// ---------- socket + fd passing ----------

/// Probe: a live keeper accepts connections.
pub fn keeper_alive_at(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

/// Bind the listen socket. Refuses if a live keeper exists; removes
/// stale socket files otherwise. Caller forks only after this succeeds.
pub fn bind_at(path: &Path) -> std::io::Result<UnixListener> {
    if keeper_alive_at(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "a detached session already exists",
        ));
    }
    let _ = std::fs::remove_file(path); // stale file or nonexistent
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let l = UnixListener::bind(path)?;
    l.set_nonblocking(true)?;
    Ok(l)
}

/// Send a 1-byte payload plus optional SCM_RIGHTS fd bundle.
pub fn send_with_fds(fd: RawFd, payload: u8, pass: &[RawFd]) -> std::io::Result<()> {
    let data = [payload];
    let iov = [IoSlice::new(&data)];
    if pass.is_empty() {
        sendmsg::<()>(fd, &iov, &[], MsgFlags::empty(), None).map_err(io_err)?;
    } else {
        let cmsg = [ControlMessage::ScmRights(pass)];
        sendmsg::<()>(fd, &iov, &cmsg, MsgFlags::empty(), None).map_err(io_err)?;
    }
    Ok(())
}

/// Receive the 1-byte payload and any SCM_RIGHTS fds.
pub fn recv_with_fds(fd: RawFd) -> std::io::Result<(u8, Vec<RawFd>)> {
    let mut data = [0u8; 1];
    let mut iov = [IoSliceMut::new(&mut data)];
    let mut cmsg = nix::cmsg_space!([RawFd; 4]);
    let msg = recvmsg::<()>(fd, &mut iov, Some(cmsg.as_mut_slice()), MsgFlags::empty())
        .map_err(io_err)?;
    if msg.bytes == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "peer closed",
        ));
    }
    let mut fds = Vec::new();
    for c in msg.cmsgs().map_err(io_err)? {
        if let ControlMessageOwned::ScmRights(list) = c {
            fds.extend(list);
        }
    }
    Ok((data[0], fds))
}

// ---------- detach / fork ----------

/// Lock every pane parser — quiesce before `fork()`. `parser` is the
/// ONLY cross-thread mutex (reader threads); writer/watcher/mpsc are
/// main-thread or lock-free. Hold guards across fork; child drops them.
pub fn quiesce(app: &App) -> Vec<MutexGuard<'_, vt100::Parser>> {
    app.projects
        .iter()
        .flat_map(|p| p.panes.iter())
        .map(|pane| pane.parser.lock().unwrap())
        .collect()
}

/// Entry point after `run()` breaks on `detach_requested` and the
/// terminal is restored. Saves, binds, quiesces, forks; the parent
/// releases the user's tty and exits, the child daemonizes into
/// `run_keeper` (never returns). fd 0 is already the internal slave —
/// installed at startup — so the keeper's crossterm reader stays bound
/// to the same live description.
pub fn detach(
    app: &mut App,
    events_rx: &mpsc::Receiver<PaneEvent>,
    app_rx: &mpsc::Receiver<AppEvent>,
    mut relay: Option<&mut InputRelay>,
) -> anyhow::Result<()> {
    println!("rustterm: detached — 'rustterm -a' to reattach");
    let _ = std::io::stdout().flush();
    if let Err(e) = crate::session::save(app) {
        eprintln!("rustterm: session save failed: {e}");
    }
    let listener = bind_at(&socket_path()).map_err(|e| anyhow::anyhow!("detach failed: {e}"))?;
    let guards = quiesce(app);
    match unsafe { fork() } {
        Ok(ForkResult::Parent { .. }) => {
            if let Some(r) = relay.as_deref_mut() {
                r.release_tty();
            }
            drop(guards);
            std::process::exit(0);
        }
        Ok(ForkResult::Child) => {
            let _ = setsid();
            // fd 0 is already the internal slave — the reader's epoll
            // registration follows its file description across fork.
            let input_master = match relay.as_deref_mut() {
                Some(r) => {
                    r.abandon_tty();
                    r.master_fd()
                }
                None => match setup_input_pty() {
                    Ok(m) => m,
                    Err(_) => std::process::exit(1),
                },
            };
            stdio_to_devnull(1);
            stdio_to_devnull(2);
            drop(guards); // unlock the inherited mutexes
            app.daemonize(); // respawn readers, clear in-flight flags
            run_keeper(app, events_rx, app_rx, listener, input_master);
        }
        Err(e) => Err(anyhow::anyhow!("fork failed: {e}")),
    }
}

/// `dup2(src → target)` — nix 0.30 wants the target as `&mut OwnedFd`;
/// claim the number briefly, then leak it back so the fd keeps its new
/// referent after return.
fn dup2_onto<Fd: AsFd>(src: Fd, target: RawFd) -> std::io::Result<()> {
    let mut owned = unsafe { OwnedFd::from_raw_fd(target) };
    dup2(src, &mut owned).map_err(io_err)?;
    let _ = owned.into_raw_fd(); // don't close `target` on drop
    Ok(())
}

/// Internal pty pair: raw-mode slave becomes fd 0 permanently; the
/// master is returned for input pumps.
fn setup_input_pty() -> std::io::Result<RawFd> {
    let pty = nix::pty::openpty(None, None).map_err(io_err)?;
    let mut tio = nix::sys::termios::tcgetattr(&pty.slave).map_err(io_err)?;
    nix::sys::termios::cfmakeraw(&mut tio);
    nix::sys::termios::tcsetattr(&pty.slave, nix::sys::termios::SetArg::TCSANOW, &tio)
        .map_err(io_err)?;
    dup2_onto(&pty.slave, 0)?;
    Ok(pty.master.into_raw_fd()) // leak — lives for the process lifetime
}

/// The startup input relay. Installed BEFORE `ratatui::init()` so
/// crossterm's one-shot event source binds the internal slave's file
/// description — which `fork()` inherits intact, keeping attached-mode
/// input alive across any number of attach cycles. fd 0 never changes
/// after install; only fd 1 flips per attach.
pub struct InputRelay {
    master: RawFd,
    orig_in: Option<OwnedFd>,
    orig_termios: Option<nix::sys::termios::Termios>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl InputRelay {
    /// Replace fd 0 with a raw internal-pty slave and pump the original
    /// stdin through it. Call first thing in main (after arg dispatch,
    /// before `ratatui::init`).
    pub fn install() -> std::io::Result<InputRelay> {
        let stdin = unsafe { BorrowedFd::borrow_raw(0) };
        let orig_termios = if nix::unistd::isatty(stdin).unwrap_or(false) {
            let saved = nix::sys::termios::tcgetattr(stdin).map_err(io_err)?;
            let mut raw = saved.clone();
            nix::sys::termios::cfmakeraw(&mut raw);
            nix::sys::termios::tcsetattr(stdin, nix::sys::termios::SetArg::TCSANOW, &raw)
                .map_err(io_err)?;
            Some(saved)
        } else {
            None
        };
        let orig_in = nix::unistd::dup(stdin).map_err(io_err)?;
        let master = setup_input_pty()?;
        let stop = Arc::new(AtomicBool::new(false));
        let dead = Arc::new(AtomicBool::new(false));
        let join = spawn_input_pump(orig_in.as_raw_fd(), master, stop.clone(), dead);
        Ok(InputRelay {
            master,
            orig_in: Some(orig_in),
            orig_termios,
            stop,
            join: Some(join),
        })
    }

    pub fn master_fd(&self) -> RawFd {
        self.master
    }

    /// Foreground exit / detach-parent: stop reading the user's tty and
    /// restore its termios. Joins the pump first so the fd number can't
    /// be reused under a blocked read.
    pub fn release_tty(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        if let (Some(fd), Some(tio)) = (self.orig_in.as_ref(), self.orig_termios.take()) {
            let _ = nix::sys::termios::tcsetattr(fd, nix::sys::termios::SetArg::TCSANOW, &tio);
        }
        self.orig_in = None;
    }

    /// Post-fork keeper side — pump threads don't exist here; just drop
    /// the inherited tty handle. fd 0 stays the internal slave.
    pub fn abandon_tty(&mut self) {
        self.join = None;
        self.orig_in = None;
    }
}

fn stdio_to_devnull(fd: RawFd) {
    if let Ok(devnull) = nix::fcntl::open(
        "/dev/null",
        nix::fcntl::OFlag::O_RDWR,
        nix::sys::stat::Mode::empty(),
    ) {
        let _ = dup2_onto(devnull, fd);
    }
}

// ---------- keeper ----------

enum Handshake {
    Attach { stdin_fd: RawFd, stdout_fd: RawFd },
    Kill,
    Bad,
}

fn handshake(conn: &UnixStream) -> Handshake {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
    match recv_with_fds(conn.as_raw_fd()) {
        Ok((MSG_ATTACH, mut fds)) if fds.len() >= 2 => {
            for extra in fds.drain(2..) {
                let _ = nix::unistd::close(extra);
            }
            Handshake::Attach {
                stdin_fd: fds[0],
                stdout_fd: fds[1],
            }
        }
        Ok((MSG_KILL, mut fds)) => {
            for f in fds.drain(..) {
                let _ = nix::unistd::close(f);
            }
            Handshake::Kill
        }
        _ => Handshake::Bad,
    }
}

/// Copy client stdin → internal pty master. Polls so `stop` ends it
/// promptly — the keeper then joins and closes the fd, avoiding any
/// fd-number reuse race. `dead` flags client EOF/read/write errors.
fn spawn_input_pump(
    from_fd: RawFd,
    to_fd: RawFd,
    stop: Arc<AtomicBool>,
    dead: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let from = unsafe { BorrowedFd::borrow_raw(from_fd) };
        let to = unsafe { BorrowedFd::borrow_raw(to_fd) };
        let mut buf = [0u8; 4096];
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let mut pfd = nix::poll::PollFd::new(from, nix::poll::PollFlags::POLLIN);
            match nix::poll::poll(std::slice::from_mut(&mut pfd), 100u16) {
                Ok(0) => continue, // timeout — re-check stop
                Ok(_) => match nix::unistd::read(from, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if nix::unistd::write(to, &buf[..n]).is_err() {
                            break;
                        }
                    }
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(_) => break,
                },
                Err(nix::errno::Errno::EINTR) => continue,
                Err(_) => break,
            }
        }
        dead.store(true, Ordering::SeqCst);
    })
}

/// The daemon loop — never returns. Detached: `tick` + accept.
/// Attached: full `run()` on the client's borrowed stdout.
pub fn run_keeper(
    app: &mut App,
    events_rx: &mpsc::Receiver<PaneEvent>,
    app_rx: &mpsc::Receiver<AppEvent>,
    listener: UnixListener,
    input_master: RawFd,
) -> ! {
    loop {
        let conn = loop {
            match listener.accept() {
                Ok((c, _)) => break c,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => {}
            }
            crate::runloop::tick(app, events_rx, app_rx);
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut w = &conn;
        match handshake(&conn) {
            Handshake::Kill => {
                let _ = w.write_all(&[REPLY_KILL]);
                quit_keeper(app);
            }
            Handshake::Bad => {}
            Handshake::Attach {
                stdin_fd,
                stdout_fd,
            } => {
                let client_out = unsafe { BorrowedFd::borrow_raw(stdout_fd) };
                if dup2_onto(client_out, 1).is_err() {
                    let _ = nix::unistd::close(stdin_fd);
                    let _ = nix::unistd::close(stdout_fd);
                    continue;
                }
                let _ = w.write_all(&[REPLY_OK]);
                let stop = Arc::new(AtomicBool::new(false));
                let dead = Arc::new(AtomicBool::new(false));
                let pump = spawn_input_pump(stdin_fd, input_master, stop.clone(), dead.clone());
                if attached_phase(app, events_rx, app_rx, &conn, dead).is_quit() {
                    quit_keeper(app);
                }
                // detach path — release the borrowed fds, back to daemon
                stop.store(true, Ordering::SeqCst);
                let _ = pump.join();
                let _ = nix::unistd::close(stdin_fd);
                let _ = nix::unistd::close(stdout_fd);
                stdio_to_devnull(1);
                app.detach_requested = false;
            }
        }
    }
}

enum PostAttach {
    Detach,
    Quit,
}

impl PostAttach {
    fn is_quit(&self) -> bool {
        matches!(self, PostAttach::Quit)
    }
}

/// Run the TUI on the borrowed fds until detach/client-death/quit.
/// Sends the status byte before returning.
fn attached_phase(
    app: &mut App,
    events_rx: &mpsc::Receiver<PaneEvent>,
    app_rx: &mpsc::Receiver<AppEvent>,
    conn: &UnixStream,
    dead: Arc<AtomicBool>,
) -> PostAttach {
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut terminal = match ratatui::Terminal::new(backend) {
        Ok(t) => t,
        Err(_) => return PostAttach::Detach,
    };
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    );
    let _ = crate::runloop::run(&mut terminal, app, events_rx, app_rx, Some(dead.clone()));
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    );
    let _ = std::io::stdout().flush();
    let mut w = conn;
    if app.should_quit {
        let _ = w.write_all(&[REPLY_QUIT]);
        PostAttach::Quit
    } else {
        let _ = w.write_all(&[REPLY_DETACH]); // fails if client died — fine
        PostAttach::Detach
    }
}

fn quit_keeper(app: &App) -> ! {
    let _ = crate::session::save(app);
    let _ = std::fs::remove_file(socket_path());
    std::process::exit(0);
}

// ---------- client side ----------

/// `rustterm -a` — lend our tty to the keeper and wait for it to close.
pub fn run_attach_client() -> anyhow::Result<()> {
    let path = socket_path();
    let mut conn = match UnixStream::connect(&path) {
        Ok(c) => c,
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            eprintln!("rustterm: no detached session");
            std::process::exit(1);
        }
    };
    // Raw on OUR tty for the whole attach — Ctrl+C stays a byte, and the
    // tty can never be left cooked mid-handshake.
    crossterm::terminal::enable_raw_mode()?;
    if let Err(e) = send_with_fds(conn.as_raw_fd(), MSG_ATTACH, &[0, 1]) {
        client_cleanup();
        return Err(e.into());
    }
    // The keeper replies at once when free; a queued connect means it's
    // attached elsewhere — don't hang uninterruptibly in raw mode.
    let _ = conn.set_read_timeout(Some(Duration::from_secs(3)));
    let mut b = [0u8; 1];
    match conn.read(&mut b) {
        Ok(1) if b[0] == REPLY_OK => {}
        _ => {
            client_cleanup();
            eprintln!("rustterm: attach failed — session busy or dead");
            std::process::exit(1);
        }
    }
    // Block until the keeper says something or the socket dies.
    let _ = conn.set_read_timeout(None);
    let mut b = [0u8; 1];
    let _ = conn.read(&mut b);
    client_cleanup();
    if b[0] == REPLY_QUIT {
        println!("rustterm: session ended");
    }
    Ok(())
}

fn client_cleanup() {
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    );
    let _ = crossterm::terminal::disable_raw_mode();
}

/// `rustterm -k` — ask the keeper to save and exit.
pub fn send_kill() -> anyhow::Result<()> {
    let path = socket_path();
    let mut conn = match UnixStream::connect(&path) {
        Ok(c) => c,
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            eprintln!("rustterm: no detached session");
            std::process::exit(1);
        }
    };
    send_with_fds(conn.as_raw_fd(), MSG_KILL, &[])?;
    // The keeper accepts between attaches — a busy session queues us.
    conn.set_read_timeout(Some(Duration::from_secs(3)))?;
    let mut b = [0u8; 1];
    match conn.read(&mut b) {
        Ok(1) => {
            println!("rustterm: detached session killed");
            Ok(())
        }
        _ => {
            eprintln!("rustterm: session busy — an attached client is using it");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fds_roundtrip_through_socketpair() {
        // Send a real file fd through a Unix socketpair; the received fd
        // must read the same open file description.
        let (a, b) = nix::sys::socket::socketpair(
            nix::sys::socket::AddressFamily::Unix,
            nix::sys::socket::SockType::Stream,
            None,
            nix::sys::socket::SockFlag::empty(),
        )
        .unwrap();
        let tmp = std::env::temp_dir().join(format!("rustterm-fdpass-{}", std::process::id()));
        std::fs::write(&tmp, b"fd-payload").unwrap();
        let file = std::fs::File::open(&tmp).unwrap();
        let file2 = std::fs::File::open(&tmp).unwrap();
        send_with_fds(
            a.as_raw_fd(),
            MSG_ATTACH,
            &[file.as_raw_fd(), file2.as_raw_fd()],
        )
        .unwrap();

        let (byte, fds) = recv_with_fds(b.as_raw_fd()).unwrap();
        assert_eq!(byte, MSG_ATTACH);
        assert_eq!(fds.len(), 2);
        let bfd = unsafe { BorrowedFd::borrow_raw(fds[0]) };
        let mut buf = [0u8; 16];
        let n = nix::unistd::read(bfd, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"fd-payload");
        let _ = nix::unistd::close(fds[0]);
        let _ = nix::unistd::close(fds[1]);
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn kill_payload_needs_no_fds() {
        let (a, b) = nix::sys::socket::socketpair(
            nix::sys::socket::AddressFamily::Unix,
            nix::sys::socket::SockType::Stream,
            None,
            nix::sys::socket::SockFlag::empty(),
        )
        .unwrap();
        send_with_fds(a.as_raw_fd(), MSG_KILL, &[]).unwrap();
        let (byte, fds) = recv_with_fds(b.as_raw_fd()).unwrap();
        assert_eq!(byte, MSG_KILL);
        assert!(fds.is_empty());
    }

    #[test]
    fn bind_refuses_live_keeper_and_clears_stale() {
        let dir = std::env::temp_dir().join(format!("rustterm-sock-{}", std::process::id()));
        let path = dir.join("attach.sock");
        std::fs::create_dir_all(&dir).unwrap();
        let listener = bind_at(&path).unwrap();
        assert!(keeper_alive_at(&path), "bound listener is a live keeper");
        assert!(bind_at(&path).is_err(), "second bind refused while live");
        drop(listener);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
