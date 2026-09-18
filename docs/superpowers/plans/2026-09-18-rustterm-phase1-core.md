# RustTerm Phase 1 Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a working TUI terminal multiplexer (RustTerm) with a flat Projects → Panes model: multiple real PTYs per project, laid out in a resizable grid, navigated via a tmux-style leader key, all inside a single terminal window with no GUI/webview.

**Architecture:** Single Rust binary. `portable-pty` spawns real shells; each pane's raw output is parsed into a `vt100::Parser` by a dedicated reader thread; `tui-term` renders the parser's `Screen` as a Ratatui widget each frame. Layout Rects are recomputed synchronously every frame on the main thread, and any pane whose computed size changed gets `set_size()` + `master.resize()` called immediately in that same pass — no async gap between measuring a size and informing the PTY of it.

**Tech Stack:** Rust (edition 2021), `ratatui` 0.30 (crossterm 0.29 backend), `portable-pty` 0.9, `vt100` 0.16, `tui-term` 0.3, `anyhow` 1.

**Spec:** `/home/mahadi/projects/RustTerm/docs/superpowers/specs/2026-09-18-rustterm-phase1-core-design.md`

## Global Constraints

- Model is flat: Projects → Panes. No Workspace layer.
- PTYs live only for the process lifetime — no daemon, no detach/reattach.
- Leader key is `Ctrl+B` (tmux convention); all other keys pass through to the focused pane's shell.
- No mouse-driven drag-to-resize in Phase 1 — split ratios adjust via leader `+`/`-`.
- Out of scope for Phase 1: file finder, code editor, LSP, git panel, command palette, agent-CLI detection, desktop notifications.
- `ratatui::init()` / `ratatui::restore()` are used for terminal setup/teardown (these install a panic hook automatically, so a panic mid-render still restores the user's shell to a usable state).

---

### Task 1: Project scaffolding

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `.gitignore`

**Interfaces:**
- Produces: a `rustterm` binary crate that builds and runs, printing nothing but exiting cleanly — proves the toolchain and dependency set resolve before any real code is written.

- [ ] **Step 1: Write Cargo.toml**

```toml
[package]
name = "rustterm"
version = "0.1.0"
edition = "2021"

[dependencies]
ratatui = "0.30"
crossterm = "0.29"
portable-pty = "0.9"
vt100 = "0.16"
tui-term = { version = "0.3", features = ["vt100"] }
anyhow = "1"
```

- [ ] **Step 2: Write a minimal main.rs**

```rust
fn main() -> anyhow::Result<()> {
    println!("rustterm scaffold ok");
    Ok(())
}
```

- [ ] **Step 3: Write .gitignore**

```
/target
```

- [ ] **Step 4: Verify it builds and runs**

Run: `cargo run`
Expected: prints `rustterm scaffold ok` and exits with code 0. (First run will also download and compile all dependencies — expect this to take a minute or two.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs .gitignore
git commit -m "chore: project scaffolding"
```

---

### Task 2: layout.rs — pane Rect computation

**Files:**
- Create: `src/layout.rs`
- Modify: `src/main.rs` (add `mod layout;`)

**Interfaces:**
- Produces: `pub fn pane_rects(area: Rect, count: usize, col_split: f32, row_split: f32) -> Vec<Rect>` — later used by `ui.rs` (Task 9) to place each pane, and consumed nowhere else in this plan.

- [ ] **Step 1: Write the failing tests**

```rust
// src/layout.rs
use ratatui::layout::Rect;

pub fn pane_rects(area: Rect, count: usize, col_split: f32, row_split: f32) -> Vec<Rect> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 100, 40)
    }

    #[test]
    fn zero_panes_is_empty() {
        assert_eq!(pane_rects(area(), 0, 0.5, 0.5), Vec::<Rect>::new());
    }

    #[test]
    fn one_pane_fills_area() {
        let rects = pane_rects(area(), 1, 0.5, 0.5);
        assert_eq!(rects, vec![area()]);
    }

    #[test]
    fn two_panes_split_by_col_split_with_gutter() {
        let rects = pane_rects(area(), 2, 0.5, 0.5);
        assert_eq!(rects.len(), 2);
        // Left pane starts at the area's left edge.
        assert_eq!(rects[0].x, 0);
        // Right pane ends at the area's right edge.
        assert_eq!(rects[1].x + rects[1].width, 100);
        // A 1-cell gutter separates them: right pane starts strictly after
        // the left pane ends.
        assert!(rects[1].x > rects[0].x + rects[0].width);
        // Both panes span the full height.
        assert_eq!(rects[0].height, 40);
        assert_eq!(rects[1].height, 40);
    }

    #[test]
    fn three_panes_two_top_one_bottom_spanning() {
        let rects = pane_rects(area(), 3, 0.5, 0.5);
        assert_eq!(rects.len(), 3);
        // Top two panes share the top row.
        assert_eq!(rects[0].y, rects[1].y);
        // Bottom pane spans the full width of the area.
        assert_eq!(rects[2].x, 0);
        assert_eq!(rects[2].x + rects[2].width, 100);
        // Bottom pane is below the top two.
        assert!(rects[2].y > rects[0].y);
    }

    #[test]
    fn four_panes_is_a_2x2_grid() {
        let rects = pane_rects(area(), 4, 0.5, 0.5);
        assert_eq!(rects.len(), 4);
        // Panes 0 and 1 share a row; panes 2 and 3 share a (lower) row.
        assert_eq!(rects[0].y, rects[1].y);
        assert_eq!(rects[2].y, rects[3].y);
        assert!(rects[2].y > rects[0].y);
    }

    #[test]
    fn five_panes_falls_back_to_wrapping_grid() {
        let rects = pane_rects(area(), 5, 0.5, 0.5);
        assert_eq!(rects.len(), 5);
        // All rects stay within the area bounds.
        for r in &rects {
            assert!(r.x + r.width <= area().x + area().width);
            assert!(r.y + r.height <= area().y + area().height);
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib layout`
Expected: compile error or panic from `todo!()` — every test fails.

- [ ] **Step 3: Implement pane_rects**

```rust
// src/layout.rs (replace the todo!() body)
use ratatui::layout::{Constraint, Direction, Layout, Rect};

const GUTTER: u16 = 1;

pub fn pane_rects(area: Rect, count: usize, col_split: f32, row_split: f32) -> Vec<Rect> {
    match count {
        0 => Vec::new(),
        1 => vec![area],
        2 => split_cols(area, col_split),
        3 => {
            let top = split_cols(top_half(area, row_split), col_split);
            let bottom = bottom_half(area, row_split);
            vec![top[0], top[1], bottom]
        }
        4 => {
            let top = split_cols(top_half(area, row_split), col_split);
            let bottom = split_cols(bottom_half(area, row_split), col_split);
            vec![top[0], top[1], bottom[0], bottom[1]]
        }
        _ => wrapping_grid(area, count),
    }
}

fn split_cols(area: Rect, col_split: f32) -> Vec<Rect> {
    let left_pct = (col_split * 100.0).round() as u16;
    let right_pct = 100u16.saturating_sub(left_pct);
    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(left_pct),
            Constraint::Length(GUTTER),
            Constraint::Percentage(right_pct),
        ])
        .split(area);
    vec![parts[0], parts[2]]
}

fn top_half(area: Rect, row_split: f32) -> Rect {
    split_rows(area, row_split)[0]
}

fn bottom_half(area: Rect, row_split: f32) -> Rect {
    split_rows(area, row_split)[1]
}

fn split_rows(area: Rect, row_split: f32) -> Vec<Rect> {
    let top_pct = (row_split * 100.0).round() as u16;
    let bottom_pct = 100u16.saturating_sub(top_pct);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(top_pct),
            Constraint::Length(GUTTER),
            Constraint::Percentage(bottom_pct),
        ])
        .split(area);
    vec![parts[0], parts[2]]
}

fn wrapping_grid(area: Rect, count: usize) -> Vec<Rect> {
    let cols = 3usize;
    let rows = count.div_ceil(cols);
    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Percentage((100 / rows.max(1)) as u16))
        .collect();
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    let mut rects = Vec::with_capacity(count);
    for r in 0..rows {
        let remaining = count - r * cols;
        let this_row_cols = remaining.min(cols);
        let col_constraints: Vec<Constraint> = (0..this_row_cols)
            .map(|_| Constraint::Percentage((100 / this_row_cols) as u16))
            .collect();
        let col_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints)
            .split(row_areas[r]);
        for c in 0..this_row_cols {
            rects.push(col_areas[c]);
        }
    }
    rects
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib layout`
Expected: all 6 tests pass.

- [ ] **Step 5: Wire the module into main.rs**

```rust
// src/main.rs — add near the top
mod layout;

fn main() -> anyhow::Result<()> {
    println!("rustterm scaffold ok");
    Ok(())
}
```

- [ ] **Step 6: Verify the whole crate still builds**

Run: `cargo build`
Expected: succeeds (an `unused` warning on `layout` is fine at this stage; later tasks consume it).

- [ ] **Step 7: Commit**

```bash
git add src/layout.rs src/main.rs
git commit -m "feat: pane Rect layout math for 1/2/3/4/5+ panes"
```

---

### Task 3: pty.rs — spawn a real PTY

**Files:**
- Create: `src/pty.rs`
- Modify: `src/main.rs` (add `mod pty;`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `pub struct PtySpawnResult { pub master: Box<dyn portable_pty::MasterPty + Send>, pub writer: Arc<Mutex<Box<dyn Write + Send>>>, pub child: Box<dyn portable_pty::Child + Send + Sync>, pub reader: Box<dyn Read + Send> }` and `pub fn spawn(rows: u16, cols: u16, cwd: Option<&Path>) -> anyhow::Result<PtySpawnResult>` — consumed by `pane.rs` (Task 4).

- [ ] **Step 1: Write the failing test**

```rust
// src/pty.rs
use portable_pty::{Child, MasterPty};
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
    todo!()
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
```

Note: this test intentionally leaks the spawned shell process (no explicit kill) — acceptable for a short-lived `sh`/`bash` invocation in a test run; the OS reclaims it when the test process exits. `Pane`'s tests in Task 4 do exercise explicit kill.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib pty`
Expected: fails on `todo!()`.

- [ ] **Step 3: Implement spawn**

```rust
// src/pty.rs (replace the todo!() body)
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib pty`
Expected: `spawned_shell_echoes_written_input` passes (may take up to a few seconds due to shell startup).

- [ ] **Step 5: Wire the module into main.rs**

```rust
// src/main.rs — add near the top, alongside mod layout;
mod pty;
```

- [ ] **Step 6: Commit**

```bash
git add src/pty.rs src/main.rs
git commit -m "feat: spawn real PTYs via portable-pty"
```

---

### Task 4: pane.rs — Pane wiring PTY output into a vt100 parser

**Files:**
- Create: `src/pane.rs`
- Modify: `src/main.rs` (add `mod pane;`)

**Interfaces:**
- Consumes: `pty::spawn(rows, cols, cwd) -> anyhow::Result<pty::PtySpawnResult>` (Task 3).
- Produces: `pub type PaneId = u32`, `pub enum PaneEvent { Output(PaneId), Exited(PaneId) }`, `pub struct Pane { pub id: PaneId, pub title: String, pub parser: Arc<Mutex<vt100::Parser>>, pub exited: Option<String>, .. }` with `pub fn spawn(id: PaneId, title: String, rows: u16, cols: u16, cwd: Option<&Path>, events_tx: mpsc::Sender<PaneEvent>) -> anyhow::Result<Pane>`, `pub fn write_input(&self, data: &[u8]) -> anyhow::Result<()>`, `pub fn resize(&self, rows: u16, cols: u16) -> anyhow::Result<()>`, `pub fn kill(&mut self) -> anyhow::Result<()>` — consumed by `project.rs` (Task 5), `input.rs` (Task 8), `ui.rs` (Task 9), `main.rs` (Task 10).

- [ ] **Step 1: Write the failing tests**

```rust
// src/pane.rs
use portable_pty::MasterPty;
use std::io::Write;
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
        todo!()
    }

    pub fn write_input(&self, data: &[u8]) -> anyhow::Result<()> {
        todo!()
    }

    pub fn resize(&self, rows: u16, cols: u16) -> anyhow::Result<()> {
        todo!()
    }

    pub fn kill(&mut self) -> anyhow::Result<()> {
        todo!()
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib pane`
Expected: fails on `todo!()`.

- [ ] **Step 3: Implement Pane**

```rust
// src/pane.rs — replace the todo!() bodies
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
        self.parser.lock().unwrap().set_size(rows, cols);
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
        Ok(())
    }
}
```

Also add the missing `use std::io::Read;` import at the top of `src/pane.rs` (needed for `reader.read(&mut buf)`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib pane`
Expected: both tests pass.

- [ ] **Step 5: Wire the module into main.rs**

```rust
// src/main.rs — add near the top
mod pane;
```

- [ ] **Step 6: Commit**

```bash
git add src/pane.rs src/main.rs
git commit -m "feat: Pane wiring PTY output into a vt100 parser"
```

---

### Task 5: project.rs — Project state and pane navigation

**Files:**
- Create: `src/project.rs`
- Modify: `src/main.rs` (add `mod project;`)

**Interfaces:**
- Consumes: `pane::{Pane, PaneEvent}` (Task 4), spawned in tests via `Pane::spawn(id, title, rows, cols, None, tx)`.
- Produces: `pub struct Project { pub name: String, pub root: PathBuf, pub panes: Vec<Pane>, pub active_pane: usize, pub col_split: f32, pub row_split: f32 }` with `pub fn new(name: String, root: PathBuf) -> Project`, `pub fn active_pane(&self) -> Option<&Pane>`, `pub fn next_pane(&mut self)`, `pub fn prev_pane(&mut self)` — consumed by `app.rs` (Task 6), `input.rs` (Task 8), `ui.rs` (Task 9), `main.rs` (Task 10).

- [ ] **Step 1: Write the failing tests**

```rust
// src/project.rs
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
        todo!()
    }

    pub fn active_pane(&self) -> Option<&Pane> {
        todo!()
    }

    pub fn next_pane(&mut self) {
        todo!()
    }

    pub fn prev_pane(&mut self) {
        todo!()
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib project`
Expected: fails on `todo!()`.

- [ ] **Step 3: Implement Project**

```rust
// src/project.rs — replace the todo!() bodies
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib project`
Expected: all 4 tests pass.

- [ ] **Step 5: Wire the module into main.rs**

```rust
// src/main.rs — add near the top
mod project;
```

- [ ] **Step 6: Commit**

```bash
git add src/project.rs src/main.rs
git commit -m "feat: Project state with pane navigation"
```

---

### Task 6: app.rs — App state and project navigation

**Files:**
- Create: `src/app.rs`
- Modify: `src/main.rs` (add `mod app;`)

**Interfaces:**
- Consumes: `project::Project` (Task 5).
- Produces: `pub enum InputMode { Normal, Leader }`, `pub struct App { pub projects: Vec<Project>, pub active_project: usize, pub mode: InputMode, pub next_pane_id: u32, pub should_quit: bool, pub events_tx: mpsc::Sender<PaneEvent> }` with `pub fn new(events_tx: mpsc::Sender<PaneEvent>) -> App`, `pub fn active_project(&self) -> Option<&Project>`, `pub fn active_project_mut(&mut self) -> Option<&mut Project>`, `pub fn next_project(&mut self)`, `pub fn prev_project(&mut self)`, `pub fn alloc_pane_id(&mut self) -> u32` — consumed by `input.rs` (Task 8), `ui.rs` (Task 9), `main.rs` (Task 10).

- [ ] **Step 1: Write the failing tests**

```rust
// src/app.rs
use crate::pane::PaneEvent;
use crate::project::Project;
use std::sync::mpsc;

pub enum InputMode {
    Normal,
    Leader,
}

pub struct App {
    pub projects: Vec<Project>,
    pub active_project: usize,
    pub mode: InputMode,
    pub next_pane_id: u32,
    pub should_quit: bool,
    pub events_tx: mpsc::Sender<PaneEvent>,
}

impl App {
    pub fn new(events_tx: mpsc::Sender<PaneEvent>) -> App {
        todo!()
    }

    pub fn active_project(&self) -> Option<&Project> {
        todo!()
    }

    pub fn active_project_mut(&mut self) -> Option<&mut Project> {
        todo!()
    }

    pub fn next_project(&mut self) {
        todo!()
    }

    pub fn prev_project(&mut self) {
        todo!()
    }

    pub fn alloc_pane_id(&mut self) -> u32 {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn app_with_projects(names: &[&str]) -> App {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        for name in names {
            app.projects.push(Project::new((*name).into(), PathBuf::from("/tmp")));
        }
        app
    }

    #[test]
    fn new_app_has_no_projects_and_normal_mode() {
        let (tx, _rx) = mpsc::channel();
        let app = App::new(tx);
        assert_eq!(app.projects.len(), 0);
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(!app.should_quit);
    }

    #[test]
    fn active_project_is_none_when_empty() {
        let (tx, _rx) = mpsc::channel();
        let app = App::new(tx);
        assert!(app.active_project().is_none());
    }

    #[test]
    fn next_and_prev_project_wrap_around() {
        let mut app = app_with_projects(&["a", "b", "c"]);

        assert_eq!(app.active_project, 0);
        app.next_project();
        assert_eq!(app.active_project, 1);
        app.next_project();
        assert_eq!(app.active_project, 2);
        app.next_project();
        assert_eq!(app.active_project, 0);

        app.prev_project();
        assert_eq!(app.active_project, 2);
    }

    #[test]
    fn alloc_pane_id_increments() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        assert_eq!(app.alloc_pane_id(), 0);
        assert_eq!(app.alloc_pane_id(), 1);
        assert_eq!(app.alloc_pane_id(), 2);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib app`
Expected: fails on `todo!()`.

- [ ] **Step 3: Implement App**

```rust
// src/app.rs — replace the todo!() bodies
impl App {
    pub fn new(events_tx: mpsc::Sender<PaneEvent>) -> App {
        App {
            projects: Vec::new(),
            active_project: 0,
            mode: InputMode::Normal,
            next_pane_id: 0,
            should_quit: false,
            events_tx,
        }
    }

    pub fn active_project(&self) -> Option<&Project> {
        self.projects.get(self.active_project)
    }

    pub fn active_project_mut(&mut self) -> Option<&mut Project> {
        self.projects.get_mut(self.active_project)
    }

    pub fn next_project(&mut self) {
        if !self.projects.is_empty() {
            self.active_project = (self.active_project + 1) % self.projects.len();
        }
    }

    pub fn prev_project(&mut self) {
        if !self.projects.is_empty() {
            self.active_project = if self.active_project == 0 {
                self.projects.len() - 1
            } else {
                self.active_project - 1
            };
        }
    }

    pub fn alloc_pane_id(&mut self) -> u32 {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        id
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib app`
Expected: all 4 tests pass.

- [ ] **Step 5: Wire the module into main.rs**

```rust
// src/main.rs — add near the top
mod app;
```

- [ ] **Step 6: Commit**

```bash
git add src/app.rs src/main.rs
git commit -m "feat: App state with project navigation"
```

---

### Task 7: keys.rs — translate crossterm key events to PTY bytes

**Files:**
- Create: `src/keys.rs`
- Modify: `src/main.rs` (add `mod keys;`)

**Interfaces:**
- Consumes: nothing from earlier tasks (pure function over `crossterm::event` types).
- Produces: `pub fn key_event_to_bytes(key: crossterm::event::KeyEvent) -> Vec<u8>` — consumed by `input.rs` (Task 8).

- [ ] **Step 1: Write the failing tests**

```rust
// src/keys.rs
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn key_event_to_bytes(key: KeyEvent) -> Vec<u8> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn plain_char_passes_through_as_utf8() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Char('a'), KeyModifiers::NONE)), b"a".to_vec());
    }

    #[test]
    fn ctrl_c_becomes_control_byte_3() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), vec![3u8]);
    }

    #[test]
    fn enter_becomes_carriage_return() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Enter, KeyModifiers::NONE)), vec![b'\r']);
    }

    #[test]
    fn backspace_becomes_del_byte() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Backspace, KeyModifiers::NONE)), vec![0x7f]);
    }

    #[test]
    fn arrow_keys_become_ansi_escape_sequences() {
        assert_eq!(key_event_to_bytes(key(KeyCode::Up, KeyModifiers::NONE)), b"\x1b[A".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::Down, KeyModifiers::NONE)), b"\x1b[B".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::Right, KeyModifiers::NONE)), b"\x1b[C".to_vec());
        assert_eq!(key_event_to_bytes(key(KeyCode::Left, KeyModifiers::NONE)), b"\x1b[D".to_vec());
    }

    #[test]
    fn unmapped_key_yields_empty_bytes() {
        assert_eq!(key_event_to_bytes(key(KeyCode::F(5), KeyModifiers::NONE)), Vec::<u8>::new());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib keys`
Expected: fails on `todo!()`.

- [ ] **Step 3: Implement key_event_to_bytes**

```rust
// src/keys.rs — replace the todo!() body
pub fn key_event_to_bytes(key: KeyEvent) -> Vec<u8> {
    match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let lower = c.to_ascii_lowercase();
                if lower.is_ascii_alphabetic() {
                    vec![(lower as u8) - b'a' + 1]
                } else {
                    vec![c as u8]
                }
            } else {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf).as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        _ => Vec::new(),
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib keys`
Expected: all 6 tests pass.

- [ ] **Step 5: Wire the module into main.rs**

```rust
// src/main.rs — add near the top
mod keys;
```

- [ ] **Step 6: Commit**

```bash
git add src/keys.rs src/main.rs
git commit -m "feat: translate crossterm key events to PTY input bytes"
```

---

### Task 8: input.rs — leader-key routing

**Files:**
- Create: `src/input.rs`
- Modify: `src/main.rs` (add `mod input;`)

**Interfaces:**
- Consumes: `app::{App, InputMode}` (Task 6), `keys::key_event_to_bytes` (Task 7), `pane::Pane` (Task 4), `project::Project` (Task 5).
- Produces: `pub fn handle_key(app: &mut App, key: crossterm::event::KeyEvent)` — consumed by `main.rs` (Task 10).

- [ ] **Step 1: Write the failing tests**

```rust
// src/input.rs
use crate::app::{App, InputMode};
use crate::keys::key_event_to_bytes;
use crate::pane::Pane;
use crate::project::Project;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn handle_key(app: &mut App, key: KeyEvent) {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn app_with_one_project() -> App {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_b_enters_leader_mode() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, InputMode::Leader));
    }

    #[test]
    fn leader_then_q_sets_should_quit() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(app.should_quit);
        assert!(matches!(app.mode, InputMode::Normal), "mode should revert after a leader command");
    }

    #[test]
    fn leader_then_bracket_switches_project() {
        let mut app = app_with_one_project();
        app.projects.push(Project::new("second".into(), PathBuf::from("/tmp")));

        handle_key(&mut app, key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char(']'), KeyModifiers::NONE));
        assert_eq!(app.active_project, 1);
    }

    #[test]
    fn leader_then_n_spawns_a_pane_in_the_active_project() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('n'), KeyModifiers::NONE));

        let project = app.active_project().unwrap();
        assert_eq!(project.panes.len(), 1);
        assert_eq!(project.active_pane, 0);
    }

    #[test]
    fn leader_then_x_closes_the_active_pane() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(app.active_project().unwrap().panes.len(), 1);

        handle_key(&mut app, key(KeyCode::Char('b'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(app.active_project().unwrap().panes.len(), 0);
    }

    #[test]
    fn normal_mode_plain_key_is_a_noop_with_no_active_pane() {
        let mut app = app_with_one_project();
        // Should not panic even though the active project has zero panes.
        handle_key(&mut app, key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib input`
Expected: fails on `todo!()`.

- [ ] **Step 3: Implement handle_key**

```rust
// src/input.rs — replace the todo!() body
pub fn handle_key(app: &mut App, key: KeyEvent) {
    match app.mode {
        InputMode::Normal => {
            if key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.mode = InputMode::Leader;
                return;
            }
            if let Some(project) = app.active_project() {
                if let Some(pane) = project.active_pane() {
                    let bytes = key_event_to_bytes(key);
                    if !bytes.is_empty() {
                        let _ = pane.write_input(&bytes);
                    }
                }
            }
        }
        InputMode::Leader => {
            app.mode = InputMode::Normal;
            match key.code {
                KeyCode::Char('q') => app.should_quit = true,
                KeyCode::Char('n') => {
                    let id = app.alloc_pane_id();
                    let events_tx = app.events_tx.clone();
                    if let Some(project) = app.active_project_mut() {
                        let cwd = project.root.clone();
                        // Deviation from the spec's "spawn failure renders
                        // error text into the pane": that needs a
                        // Running/Failed split on Pane that would cascade
                        // through every task in this plan. Scoped down for
                        // Phase 1 MVP to "no pane appears" on failure,
                        // which doesn't panic or corrupt state — revisit if
                        // spawn failures turn out to be common in practice.
                        if let Ok(pane) =
                            Pane::spawn(id, format!("pane-{id}"), 24, 80, Some(&cwd), events_tx)
                        {
                            project.panes.push(pane);
                            project.active_pane = project.panes.len() - 1;
                        }
                    }
                }
                KeyCode::Char('x') => {
                    if let Some(project) = app.active_project_mut() {
                        if !project.panes.is_empty() {
                            let idx = project.active_pane;
                            let _ = project.panes[idx].kill();
                            project.panes.remove(idx);
                            if project.active_pane >= project.panes.len() && project.active_pane > 0
                            {
                                project.active_pane -= 1;
                            }
                        }
                    }
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    if let Some(project) = app.active_project_mut() {
                        project.prev_pane();
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    if let Some(project) = app.active_project_mut() {
                        project.next_pane();
                    }
                }
                KeyCode::Char('[') => app.prev_project(),
                KeyCode::Char(']') => app.next_project(),
                KeyCode::Char('+') => {
                    if let Some(project) = app.active_project_mut() {
                        project.col_split = (project.col_split + 0.05).min(0.85);
                    }
                }
                KeyCode::Char('-') => {
                    if let Some(project) = app.active_project_mut() {
                        project.col_split = (project.col_split - 0.05).max(0.15);
                    }
                }
                _ => {}
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib input`
Expected: all 6 tests pass. (`leader_then_n_spawns_a_pane_in_the_active_project` and `leader_then_x_closes_the_active_pane` really spawn a shell via `Pane::spawn`, so allow a couple of seconds.)

- [ ] **Step 5: Wire the module into main.rs**

```rust
// src/main.rs — add near the top
mod input;
```

- [ ] **Step 6: Commit**

```bash
git add src/input.rs src/main.rs
git commit -m "feat: leader-key input routing (Ctrl+B n/x/h/l/[/]/+/-/q)"
```

---

### Task 9: ui.rs — draw the sidebar and pane grid

**Files:**
- Create: `src/ui.rs`
- Modify: `src/main.rs` (add `mod ui;`)

**Interfaces:**
- Consumes: `app::App` (Task 6), `layout::pane_rects` (Task 2), `pane::Pane` (Task 4).
- Produces: `pub fn draw(frame: &mut ratatui::Frame, app: &App)` — consumed by `main.rs` (Task 10).

- [ ] **Step 1: Write the failing tests**

```rust
// src/ui.rs
use crate::app::App;
use ratatui::Frame;

pub fn draw(frame: &mut Frame, app: &App) {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::Pane;
    use crate::project::Project;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn buffer_contains(buffer: &Buffer, needle: &str) -> bool {
        for y in 0..buffer.area.height {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            if row.contains(needle) {
                return true;
            }
        }
        false
    }

    #[test]
    fn sidebar_shows_project_names() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        app.projects.push(Project::new("alpha".into(), PathBuf::from("/tmp")));
        app.projects.push(Project::new("beta".into(), PathBuf::from("/tmp")));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!(buffer_contains(buffer, "alpha"));
        assert!(buffer_contains(buffer, "beta"));
    }

    #[test]
    fn active_pane_title_is_rendered_as_a_block_title() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        let mut project = Project::new("demo".into(), PathBuf::from("/tmp"));
        let (pane_tx, _pane_rx) = mpsc::channel();
        let pane = Pane::spawn(1, "my-pane-title".into(), 24, 80, None, pane_tx).unwrap();
        project.panes.push(pane);
        app.projects.push(project);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!(buffer_contains(buffer, "my-pane-title"));
    }

    #[test]
    fn draw_with_no_projects_does_not_panic() {
        let (tx, _rx) = mpsc::channel();
        let app = App::new(tx);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib ui`
Expected: fails on `todo!()`.

- [ ] **Step 3: Implement draw**

```rust
// src/ui.rs — replace the todo!() body, and add these imports at the top
use crate::layout::pane_rects;
use crate::pane::Pane;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem};
use tui_term::widget::PseudoTerminal;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(24), Constraint::Min(0)])
        .split(area);
    let sidebar_area = cols[0];
    let main_area = cols[1];

    draw_sidebar(frame, app, sidebar_area);
    draw_panes(frame, app, main_area);
    draw_status_bar(frame, app, Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1));
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .projects
        .iter()
        .enumerate()
        .map(|(i, project)| {
            let style = if i == app.active_project {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(project.name.clone()).style(style)
        })
        .collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Projects"));
    frame.render_widget(list, area);
}

fn draw_panes(frame: &mut Frame, app: &App, area: Rect) {
    let Some(project) = app.active_project() else {
        return;
    };
    let rects = pane_rects(area, project.panes.len(), project.col_split, project.row_split);
    for (pane, rect) in project.panes.iter().zip(rects.iter()) {
        sync_pane_size(pane, *rect);
        let title = if pane.exited.is_some() {
            format!("{} [exited]", pane.title)
        } else {
            pane.title.clone()
        };
        let parser = pane.parser.lock().unwrap();
        let screen = parser.screen();
        let widget =
            PseudoTerminal::new(screen).block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(widget, *rect);
    }
}

fn sync_pane_size(pane: &Pane, rect: Rect) {
    let rows = rect.height.saturating_sub(2).max(1);
    let cols = rect.width.saturating_sub(2).max(1);
    let _ = pane.resize(rows, cols);
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    use ratatui::widgets::Paragraph;
    let text = match app.mode {
        crate::app::InputMode::Normal => "Ctrl+B for commands".to_string(),
        crate::app::InputMode::Leader => "n new  x close  h/l switch  [ ] project  +/- split  q quit".to_string(),
    };
    frame.render_widget(Paragraph::new(text), area);
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib ui`
Expected: all 3 tests pass.

- [ ] **Step 5: Wire the module into main.rs**

```rust
// src/main.rs — add near the top
mod ui;
```

- [ ] **Step 6: Commit**

```bash
git add src/ui.rs src/main.rs
git commit -m "feat: draw sidebar, pane grid, and status bar; sync pane size to layout each frame"
```

---

### Task 10: main.rs — wire the real event loop

**Files:**
- Modify: `src/main.rs` (replace the placeholder `main` with the real event loop)

**Interfaces:**
- Consumes: everything from Tasks 2–9.
- Produces: the runnable `rustterm` binary. No further tasks consume this — it's the top of the dependency graph.

- [ ] **Step 1: Replace main.rs with the full event loop**

```rust
// src/main.rs — full replacement
mod app;
mod input;
mod keys;
mod layout;
mod pane;
mod project;
mod pty;
mod ui;

use app::App;
use pane::{Pane, PaneEvent};
use project::Project;
use std::sync::mpsc;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let (events_tx, events_rx) = mpsc::channel::<PaneEvent>();

    let cwd = std::env::current_dir()?;
    let project_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string());

    let mut app = App::new(events_tx.clone());
    let mut project = Project::new(project_name, cwd.clone());

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let first_pane = Pane::spawn(
        app.alloc_pane_id(),
        "pane-0".to_string(),
        rows,
        cols,
        Some(&cwd),
        events_tx,
    )?;
    project.panes.push(first_pane);
    app.projects.push(project);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &events_rx);
    ratatui::restore();
    result
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    events_rx: &mpsc::Receiver<PaneEvent>,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if crossterm::event::poll(Duration::from_millis(50))? {
            if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                if key.kind == crossterm::event::KeyEventKind::Press {
                    input::handle_key(app, key);
                }
            }
        }

        while let Ok(event) = events_rx.try_recv() {
            if let PaneEvent::Exited(id) = event {
                for project in app.projects.iter_mut() {
                    for pane in project.panes.iter_mut() {
                        if pane.id == id {
                            pane.exited = Some("exited".to_string());
                        }
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
```

- [ ] **Step 2: Build the full crate**

Run: `cargo build`
Expected: succeeds with no errors (warnings about unused items are fine if any remain).

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`
Expected: every test from Tasks 2–9 still passes.

- [ ] **Step 4: Manual verification**

Run: `cargo run`
Expected, checked by hand (this is a TUI — there is no automated way to assert the interactive experience):
- A single pane fills the main area with your default shell running in it; typing and pressing Enter works normally.
- `Ctrl+B` then `n` opens a second pane; the layout splits into two side-by-side panes.
- `Ctrl+B` then `h`/`l` moves focus between panes (only the focused pane's shell receives further keystrokes — verify by typing in each).
- `Ctrl+B` then `x` closes the focused pane.
- `Ctrl+B` then `+`/`-` changes the split ratio.
- Typing `exit` in a pane's shell causes that pane to show `[exited]` in its border title without crashing the app.
- Resizing the actual terminal window causes the panes to reflow to the new size (this exercises the per-frame `sync_pane_size` resize path).
- `Ctrl+B` then `q` quits cleanly and returns you to a normal, usable shell prompt (raw mode/alternate screen were properly restored).

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire the full RustTerm event loop"
```

---

## Post-plan note

Phase 1 ends here with a working terminal multiplexer. Phases 2 (agent-CLI
detection, notifications, command palette) and 3 (file finder, code editor,
LSP, git panel) are out of scope for this plan and should each get their
own brainstorming → spec → plan cycle when picked up.
