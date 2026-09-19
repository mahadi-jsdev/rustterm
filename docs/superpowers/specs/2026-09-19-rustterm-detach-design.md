# RustTerm — Detach / Reattach (fork-keeper daemon)

**Date:** 2026-09-19
**Status:** Design — approved approach, pending spec review
**Scope:** `leader d` detaches the whole workspace into a background process that keeps every pane alive; `rustterm -a` reattaches to it with byte-for-byte state. `leader q` quit semantics unchanged. Single detached session (v1).

## Context

Session persistence (`session.json`, commit `6af397a`) rebuilds the workspace on next launch but cannot keep processes alive — PTY children die when the process holding their master exits. True detach requires a live process holding the PTYs. `portable-pty`'s `MasterPty` trait exposes no raw fd, so masters cannot be handed between processes; the only way to preserve them across a process boundary is `fork()` inheritance.

The fork-keeper design exploits this: `leader d` forks; the parent exits while the child — a complete in-memory copy of the running RustTerm, PTYs included — daemonizes and waits on a Unix socket. Reattach passes the client's tty fds to the keeper via SCM_RIGHTS; the keeper `dup2`s them onto its own stdin/stdout and resumes the unmodified TUI loop. No state serialization, no client/server protocol split.

## Goals / non-goals

**Goals**

- `leader d` → detach: all panes keep running; shell prompt returns immediately.
- `rustterm -a` → reattach to the detached session with full state: scrollback, watcher state, hidden panes, modes, everything.
- While detached: watcher, badges, exit reaping, git poll, and desktop notifications continue.
- `rustterm -k` → kill a detached session (send a byte; keeper exits).
- `session.json` still saved on detach and quit (crash/reboot fallback).
- No external runtime dependencies. One new crate: `nix`.

**Non-goals (v1)**

- Multiple detached sessions (one socket, one keeper — refuse second detach).
- Multiple simultaneous attached clients (refuse second attach).
- Detached session surviving reboot (session.json covers layout restore only).
- Scrollback persistence to disk (already decided out of scope).
- Windows support (fork/SCM_RIGHTS are Unix-only; this feature is Unix-only).

## Architecture

### Process model

```
attached TUI (run loop)                detached keeper (daemon loop)
┌────────────────────────┐   fork()   ┌────────────────────────┐
│ App + panes + parsers  │ ─────────► │ SAME App/panes/parsers │
│ PTY masters, watchers  │  parent    │ PTY masters (inherited)│
│                        │  exits     │ reader threads respawn │
│ owns tty               │            │ attach.sock listener   │
└────────────────────────┘            │ tick(): watcher/git/   │
                                      │ reap/notify, no render │
                                      └──────────┬─────────────┘
                          SCM_RIGHTS fds 0,1     │ accept
┌────────────────────────┐  ───────────────────► │
│ rustterm -a (client)   │                       │ dup2→0,1
│ blocks on socket read  │ ◄─ 'd'/'q'/'k'/'b' ── │ run() TUI resumes
└────────────────────────┘                       └────────────────┘
```

### Detach sequence (`leader d`)

1. `input.rs` leader `d` → `app.detach_requested = true`; run loop breaks like `should_quit`.
2. `main.rs` sees the flag: restore terminal (leave alt-screen, cooked mode), print `rustterm: detached — 'rustterm -a' to reattach`.
3. Save `session.json` (crash insurance) — before fork, via existing `session::save`.
4. **Quiesce**: the detaching thread locks every pane mutex — `parser` then `writer`, in pane order across all projects — collecting `MutexGuard`s into a `Vec`. No other mutexes exist in the codebase. Guards are held across the fork.
5. `fork()`:
   - **Parent**: drop guards, `exit(0)`. User sees the printed message.
   - **Child**: `setsid()` (new session — immune to controlling-tty SIGHUP); `dup2` stdin/stdout/stderr to `/dev/null`; drop the quiesce guards (mutexes now free — dead threads are gone); respawn each pane's reader thread via `Pane::respawn_reader()`; clear `ai_in_flight`/`git_poll_in_flight` (in-flight workers died at fork; next poll refires); bind `attach.sock`; enter the daemon loop.
6. If `attach.sock` already accepts a connection (live keeper): refuse before forking — flash "detached session already exists". Stale socket file → remove and proceed.

### Keeper daemon loop (detached)

Runs the non-UI part of `run()` — refactored as `tick(app, events_rx, app_rx)`:

- Drain `events_rx` → `pane.reap()` on `Exited`.
- Drain `app_rx` → `GitStatus`/`AiMessage` handling (unchanged).
- Watcher poll (`POLL` cadence) → `notify::dispatch` — notifications fire while detached.
- Git-status poll worker spawn — unchanged.
- `accept()` on the listen socket, nonblocking, per iteration → attach handshake.
- Sleep ~50ms per iteration.

AI/git worker threads are transient per-poll spawns — they die at fork but the channels and `app_tx` survive, so new workers post results normally.

### Attach protocol (`rustterm -a`)

Single `SOCK_STREAM` Unix socket at `session::data_dir()/attach.sock`. The wire protocol is two bytes total plus fd passing:

- **Client → keeper, first message**: `sendmsg` with `SCM_RIGHTS` carrying fds `[0, 1]` plus a 1-byte payload: `0x01` = attach, `0x02` = kill (`rustterm -k`). Kill carries no fds.
- **Keeper → client, response**: one byte — `b'o'` ok (attach accepted), `b'b'` busy (already attached), `b'd'` detaching (clean client exit), `b'q'` quitting (clean client exit), `b'k'` killed. Any EOF = exit too.

**Client (`rustterm -a`)**: connect (failure → remove stale socket, print `rustterm: no detached session`, exit 1) → `enable_raw_mode` on its own tty (keeps it raw for the whole attach — Ctrl+C becomes a byte, never SIGINT) → sendmsg attach + fds → block on read → on any byte or EOF: restore its own tty (`disable_raw_mode`, `LeaveAlternateScreen`, `DisableMouseCapture` on its own stdout — idempotent), exit 0 (`b'q'`/`b'k'`/EOF) or print + exit 1 (`b'b'` → "session busy"). A killed client leaves a raw tty — same failure class as any TUI crash (`reset`).

**Keeper on attach**: `recvmsg` → `dup2(recv_stdout → 1)` → write `b'o'` → spawn input pump (`read(recv_stdin)` → `write(input_pty_master)`, EOF sets the client-dead flag) → manual terminal init (see below) → enter the normal `run()` attached loop. Terminal size needs no protocol — `TIOCGWINSZ` on the dup'd fd reports the client's real size; ratatui's per-frame `autoresize` handles resizes with zero socket traffic.

**While attached**, the pump thread's `read` hitting EOF means the client vanished → `client_dead` AtomicBool → `run()` treats it as detach; no other client→keeper traffic exists.

**AMENDMENT — input relay via internal PTY (verified against crossterm 0.29 source)**: crossterm's `INTERNAL_EVENT_READER` is a global static created once; `tty_fd()` picks fd 0 only when `isatty(0)`, else `/dev/tty` (unopenable for a ctty-less daemon — `TIOCSCTTY` on a foreign tty needs CAP_SYS_ADMIN). `dup2(client_stdin → 0)` per attach would (a) stale the epoll registration on every reattach and (b) only satisfy isatty while attached. Instead: at daemonize, the keeper opens an **internal pty pair** (`nix::pty::openpty`), sets the slave raw once, and `dup2`s it onto fd 0 **permanently** — fd 0 is then `isatty`-true and epoll-stable for the process lifetime. Per attach, a pump thread copies `recvfd → master`; keystrokes arrive on the keeper's fd 0 unchanged. `enable_raw_mode` calls land on the internal pty (harmless no-ops); the *client* owns real tty termios — it `enable_raw_mode`s its own fds at attach start, which also makes Ctrl+C a byte (never SIGINT) for the whole attach. `ratatui::init()` is skipped in keeper mode — manual `Terminal::new(CrosstermBackend::new(stdout()))` + `EnterAlternateScreen`/`EnableMouseCapture` escapes on fd 1.

**Detach while attached** (`leader d` again): restore terminal on the borrowed fds → send `b'd'` → dup2 stdio back to `/dev/null` → close client socket → return to daemon loop.
**Quit while attached** (`leader q`/palette): normal quit path — kill panes on `App` drop, `session::save`, send `b'q'`, unlink socket, `exit(0)`.
**Kill (`rustterm -k`)**: connect + send `0x02` (no fds) → keeper sends `b'k'`, runs the quit path (saves session.json), exits.

### Module changes

| File | Change |
|---|---|
| `daemon.rs` (new, ~300 lines) | `socket_path()`, `bind()`, `accept` handshake, `send_fds`/`recv_fds` (SCM_RIGHTS via `nix::sys::socket::{sendmsg,recvmsg}`), `run_keeper(app, events_rx, app_rx, listener)` daemon loop, `run_attach_client()` for `-a`, `send_kill()` for `-k`, `quiesce_locks(app) -> Vec<MutexGuard>` |
| `main.rs` (~150 lines) | `-a`/`-k` arg handling before dir-args; detach branch after `run()` (restore, save, quiesce, `nix::unistd::fork`, parent exits / child daemonizes into `daemon::run_keeper`); factor `run()`'s non-UI body into `tick()` shared with the daemon loop |
| `pane.rs` (~30 lines) | `respawn_reader(events_tx)` — `master.try_clone_reader()` + spawn the existing reader loop; the spawn-time reader code moves into it (single call site shared by `spawn` and post-fork respawn) |
| `app.rs` (~40 lines) | `detach_requested: bool` field; `quiesce` helper can live in daemon.rs; `ai_in_flight`/`git_poll_in_flight` reset post-fork in keeper |
| `input.rs` (~10 lines) | leader `d` → `app.detach_requested = true`; leader hint text |
| `session.rs` (~10 lines) | export `data_dir()` alongside `session_path()` for the socket path |
| `Cargo.toml` | `nix = { version = "0.30", features = ["socket", "uio", "process", "fs", "term"] }` (fork, setsid, dup2, sendmsg/recvmsg) |
| `ui.rs` | leader hint adds `d detach` |

### The fork-lock discipline (the one subtle part)

`fork()` duplicates only the calling thread — reader threads and in-flight workers cease to exist in the child. A `Mutex` held by a dead thread stays locked forever → guaranteed deadlock on first access. Defense: the detaching (main) thread acquires every pane lock (`parser`, `writer`) in a fixed global order before forking, so no lock can be held by another thread at the fork instant; the child drops its inherited guards and proceeds with a consistent, unlocked set. After respawn, new reader threads acquire locks normally.

Post-fork child must not allocate/lock anything before dropping the guards — `setsid`, `dup2`, `/dev/null` open are fd ops, fine. Thread respawn happens *after* guard drop.

### Edge cases

- **Client death while attached**: pump thread's `read` sees EOF → `client_dead` flag → `run()` breaks → auto-detach (stdio→/dev/null, back to daemon loop). The tty was the client's — it's gone; no cleanup owed.
- **`rustterm -a` with dir args** (`rustterm -a foo`): error "attach takes no arguments", exit 2.
- **Bare `rustterm` while a keeper lives**: starts an independent instance (session.json last-writer-wins on quit — accepted caveat, same as today with two instances).
- **`leader d` in a second instance while keeper lives**: pre-fork probe connect succeeds → flash refusal.
- **Detach during modal states** (palette/search/sidebar): preserved verbatim on reattach — App survives whole.
- **Fork inside `run()`'s `terminal.draw`**: impossible — detach flag only checked between frames.
- **Socket dir missing**: `data_dir()` create_dir_all before bind (session.rs already does for the json file).
- **Second `-a` while attached**: keeper already has a client → reply `b'b'`, client prints "session busy", exit 1.
- **Keeper crashes**: stale socket; `-a`/`leader d`/`rustterm -k` connect-fail paths all remove it and report cleanly.

## Testing

- `daemon`: socketpair SCM_RIGHTS round-trip — send an fd through `sendmsg`/`recvmsg`, verify the received fd reads the same file. Protocol byte encode/decode. `socket_path()` under `XDG_DATA_HOME`.
- `pane`: `respawn_reader` is the same code path `spawn` already uses — spawn test still covers it.
- `app`: `detach_requested` set by leader `d` (input test, mirrors `leader_h_backgrounds_active_pane`).
- session.json saved before fork — covered by existing `session::save_to` tests.
- Manual smoke (documented, not automated): detach → `ps` shows processes alive → `-a` reattaches with scrollback intact → `leader d` again → `-k` kills → stale-socket cleanup. The tty-dependent path can't be unit-tested.

## Risks

- **Fork-in-multithreaded** is the load-bearing assumption — mitigated by quiesce ordering; the mutex inventory is exactly two per pane, enforced by code review.
- `nix` 0.30 `sendmsg`/`recvmsg` API churn across versions — pin the version; `passfd` crate is the fallback if the API fights us.
- A pane forked mid-`process()` writes a consistent parser (lock held by us) — safe by the same quiesce.
