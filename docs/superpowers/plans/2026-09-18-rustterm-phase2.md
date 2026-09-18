# RustTerm Phase 2 Implementation Plan — Multi-Project, Agent Awareness, Command Palette

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Projects layer real (CLI args + in-app add/switch/close), port FLAME's taskWatcher as a screen-poll watcher with badges + desktop notifications, and add a fuzzy command palette with reopen history, agent launch, and pane rename.

**Architecture:** A pure per-pane `Watcher` state machine is fed (a) screen-text snapshots polled every 400ms in the existing main loop and (b) the same input bytes written to the PTY. It emits `Done`/`Waiting`/`Command` events routed through `notify::dispatch` to pane badges + `notify-rust` desktop notifications. A `Palette` command registry is rebuilt on open and fuzzy-filtered. The reader thread and layout/pty/keys modules are untouched.

**Tech Stack:** Rust (edition 2021), `ratatui` 0.30 (crossterm 0.29 backend), `portable-pty` 0.9, `vt100` 0.16, `tui-term` 0.3, `anyhow` 1 — plus new: `regex` 1, `notify-rust` 4.

**Spec:** `/home/mahadi/projects/RustTerm/docs/superpowers/specs/2026-09-18-rustterm-phase2-design.md`

## Global Constraints

- Model stays flat: Projects → Panes. No Workspace layer, no persistence.
- Leader key is `Ctrl+A` (rebound from Ctrl+B in Phase 1); all other keys pass through to the focused pane's PTY.
- The PTY reader thread is untouched — the watcher reads the already-parsed `vt100` screen, never raw bytes.
- New dependencies: `regex = "1"`, `notify-rust = "4"` only. Fuzzy matching, `~` expansion, and PATH scanning are hand-rolled.
- Every new module declares `pub mod X;` in `src/lib.rs` only — never also in `main.rs` (that double-declares the module and runs its tests twice).
- Tests live in `#[cfg(test)] mod tests` at the bottom of each source file, matching Phase 1 style.
- Work happens on a `phase2` worktree branched off `master` (which now contains all of Phase 1).
- Commit style: `feat:` / `fix:` / `test:` conventional commits, one commit per task.

---

### Task 1: watcher.rs — pure per-pane state machine

**Files:**
- Create: `src/watcher.rs`
- Modify: `src/lib.rs` (add `pub mod watcher;`)
- Modify: `Cargo.toml` (add `regex = "1"`)

**Interfaces:**
- Consumes: nothing project-internal.
- Produces:
  ```rust
  pub const POLL: Duration;            // 400ms — how often main loop calls update()
  pub const BUSY_MIN: Duration;        // 10s
  pub const QUIET: Duration;           // 8s
  pub const WAITING_QUIET: Duration;   // 1.5s
  pub const RUNNING_WINDOW: Duration;  // 1.5s
  pub const OUTPUT_TAIL_MAX: usize;    // 2000

  pub enum WatchEvent {
      Done { command: String },
      Waiting(bool),
      Command(String),
  }

  pub struct Watcher { /* private */ }
  impl Watcher {
      pub fn new() -> Watcher;
      pub fn on_input(&mut self, bytes: &[u8]) -> Vec<WatchEvent>;
      pub fn update(&mut self, now: Instant, screen_text: &str) -> Vec<WatchEvent>;
      pub fn is_waiting(&self) -> bool;
      pub fn is_running(&self) -> bool;
      pub fn last_command(&self) -> &str;
  }
  pub fn looks_like_prompt(text: &str) -> bool;
  ```

- [ ] **Step 1: Add the dependency**

In `Cargo.toml` `[dependencies]` add:

```toml
regex = "1"
```

- [ ] **Step 2: Write the failing tests**

Create `src/watcher.rs` containing only the test module first (implementation lands in step 4 — this is the test-first half). Put both in the same file write; the impl can be stub signatures that `panic!("unimplemented")` or `unimplemented!()`. Simpler: write the full file in step 4, and in this step write only `use` + tests won't compile until impl exists — instead do it the Phase 1 way: write tests into the file along with empty-struct stubs that compile but fail assertions.

Write `src/watcher.rs`:

```rust
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const POLL: Duration = Duration::from_millis(400);
pub const BUSY_MIN: Duration = Duration::from_secs(10);
pub const QUIET: Duration = Duration::from_secs(8);
pub const WAITING_QUIET: Duration = Duration::from_millis(1500);
pub const RUNNING_WINDOW: Duration = Duration::from_millis(1500);
pub const OUTPUT_TAIL_MAX: usize = 2000;

#[derive(Debug, PartialEq)]
pub enum WatchEvent {
    Done { command: String },
    Waiting(bool),
    Command(String),
}

pub struct Watcher {}

impl Watcher {
    pub fn new() -> Watcher { Watcher {} }
    pub fn on_input(&mut self, _bytes: &[u8]) -> Vec<WatchEvent> { Vec::new() }
    pub fn update(&mut self, _now: Instant, _screen_text: &str) -> Vec<WatchEvent> { Vec::new() }
    pub fn is_waiting(&self) -> bool { false }
    pub fn is_running(&self) -> bool { false }
    pub fn last_command(&self) -> &str { "" }
}

pub fn looks_like_prompt(_text: &str) -> bool { false }

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn input_line_commits_on_enter_as_command_event() {
        let mut w = Watcher::new();
        assert!(w.on_input(b"git sta").is_empty());
        let events = w.on_input(b"tus\r");
        assert_eq!(events, vec![WatchEvent::Command("git status".to_string())]);
        assert_eq!(w.last_command(), "git status");
    }

    #[test]
    fn backspace_and_ctrl_c_edit_the_input_line() {
        let mut w = Watcher::new();
        w.on_input(b"ab\x7fc");           // "ac"
        let events = w.on_input(b"\x03ignored\r"); // ctrl-C clears, then a fresh line
        assert_eq!(events, vec![WatchEvent::Command("ignored".to_string())]);
    }

    #[test]
    fn empty_enter_emits_no_command() {
        let mut w = Watcher::new();
        assert!(w.on_input(b"\r").is_empty());
        assert!(w.on_input(b"  \r").is_empty());
    }

    #[test]
    fn busy_then_quiet_emits_done() {
        let mut w = Watcher::new();
        w.on_input(b"claude\r");
        // Screen changes at t=0..11s (busy 11s >= BUSY_MIN)
        assert!(w.update(t(0), "a").is_empty());
        assert!(w.update(t(11_000), "ab").is_empty());
        // 8s of quiet after last change at t=11s
        let events = w.update(t(19_100), "ab");
        assert_eq!(events, vec![WatchEvent::Done { command: "claude".to_string() }]);
    }

    #[test]
    fn short_burst_then_quiet_emits_nothing() {
        let mut w = Watcher::new();
        w.update(t(0), "a");
        w.update(t(2_000), "ab"); // only 2s busy
        assert!(w.update(t(10_100), "ab").is_empty());
    }

    #[test]
    fn prompt_text_at_rest_emits_waiting() {
        let mut w = Watcher::new();
        w.update(t(0), "Do you want to proceed?");
        let events = w.update(t(1_600), "Do you want to proceed?");
        assert_eq!(events, vec![WatchEvent::Waiting(true)]);
        assert!(w.is_waiting());
    }

    #[test]
    fn waiting_clears_on_new_output_and_on_input() {
        let mut w = Watcher::new();
        w.update(t(0), "Do you want to proceed?");
        w.update(t(1_600), "Do you want to proceed?");
        let events = w.update(t(2_000), "Do you want to proceed? ok");
        assert_eq!(events, vec![WatchEvent::Waiting(false)]);

        let mut w2 = Watcher::new();
        w2.update(t(0), "Do you want to proceed?");
        w2.update(t(1_600), "Do you want to proceed?");
        let events = w2.on_input(b"y");
        assert_eq!(events, vec![WatchEvent::Waiting(false)]);
    }

    #[test]
    fn running_flag_tracks_recent_change() {
        let mut w = Watcher::new();
        w.update(t(0), "a");
        assert!(w.is_running());
        w.update(t(2_000), "a");
        assert!(!w.is_running());
    }

    #[test]
    fn looks_like_prompt_matches_ported_patterns() {
        for text in [
            "Overwrite? (y/n)",
            "Continue? [y/N]",
            "Do you want to proceed?",
            "Do you trust the files in this folder?",
            "Allow this command?",
            "press enter to continue",
            "1. Yes  2. No\nEnter to select",
            "esc to cancel",
            "Yes, and don't ask again",
            "(Y)es/(N)o",
            "Allow execution of this tool?",
            "yes, allow all",
        ] {
            assert!(looks_like_prompt(text), "expected prompt match: {text}");
        }
        for text in ["compiling project...", "$ cargo build", "yes allowlist", ""] {
            assert!(!looks_like_prompt(text), "unexpected match: {text}");
        }
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test watcher::` — but first add `pub mod watcher;` to `src/lib.rs` (keep the existing `pub mod` lines; insert alphabetically before `pub mod ui;` — actual order in lib.rs is `app, input, keys, layout, pane, project, pty, ui`; insert `watcher` after `ui`).
Expected: FAIL — `input_line_commits_on_enter_as_command_event` gets `[]` instead of `Command(...)`, `looks_like_prompt` tests fail on `false`, etc.

- [ ] **Step 4: Implement the watcher**

Replace the stub bodies in `src/watcher.rs` (keep constants, enum, tests) with:

```rust
pub struct Watcher {
    input_buf: Vec<u8>,
    last_command: String,
    output_tail: String,
    last_hash: u64,
    last_change: Option<Instant>,
    busy_since: Option<Instant>,
    waiting: bool,
    running: bool,
}

impl Watcher {
    pub fn new() -> Watcher {
        Watcher {
            input_buf: Vec::new(),
            last_command: String::new(),
            output_tail: String::new(),
            last_hash: 0,
            last_change: None,
            busy_since: None,
            waiting: false,
            running: false,
        }
    }

    /// Feed the same bytes that were written to the pane's PTY. Returns a
    /// `Command` event when Enter commits a non-empty line, and
    /// `Waiting(false)` if the keystroke cleared a waiting state.
    pub fn on_input(&mut self, bytes: &[u8]) -> Vec<WatchEvent> {
        let mut events = Vec::new();
        if !bytes.is_empty() && self.waiting {
            self.waiting = false;
            events.push(WatchEvent::Waiting(false));
        }
        for &b in bytes {
            match b {
                b'\r' => {
                    let line = String::from_utf8_lossy(&self.input_buf).trim().to_string();
                    self.input_buf.clear();
                    if !line.is_empty() {
                        self.last_command = line.clone();
                        events.push(WatchEvent::Command(line));
                    }
                }
                0x7f | 0x08 => {
                    self.input_buf.pop();
                }
                0x03 => self.input_buf.clear(),
                b if b >= 0x20 => self.input_buf.push(b),
                _ => {}
            }
        }
        events
    }

    /// Poll with the pane's current visible screen text. Detects activity
    /// via a hash of the text, records the tail for prompt matching, and
    /// emits Done / Waiting transitions.
    pub fn update(&mut self, now: Instant, screen_text: &str) -> Vec<WatchEvent> {
        let mut events = Vec::new();

        let mut hasher = DefaultHasher::new();
        screen_text.hash(&mut hasher);
        let hash = hasher.finish();

        if hash != self.last_hash {
            self.last_hash = hash;
            self.last_change = Some(now);
            if self.busy_since.is_none() {
                self.busy_since = Some(now);
            }
            self.output_tail = tail_chars(screen_text, OUTPUT_TAIL_MAX).to_string();
            if self.waiting {
                self.waiting = false;
                events.push(WatchEvent::Waiting(false));
            }
        }

        self.running = self
            .last_change
            .map(|t| now.duration_since(t) < RUNNING_WINDOW)
            .unwrap_or(false);

        if !self.waiting {
            if let Some(t) = self.last_change {
                if now.duration_since(t) >= WAITING_QUIET && looks_like_prompt(&self.output_tail) {
                    self.waiting = true;
                    events.push(WatchEvent::Waiting(true));
                }
            }
        }

        // FLAME resets busyStart on ANY quiet >= quietMs, not only when Done
        // fires — otherwise a stale busy_since survives a short burst and a
        // later single-frame change looks like a >=10s busy period.
        if let (Some(busy_start), Some(last)) = (self.busy_since, self.last_change) {
            if now.duration_since(last) >= QUIET {
                let busy_for = last.duration_since(busy_start);
                self.busy_since = None;
                if busy_for >= BUSY_MIN {
                    events.push(WatchEvent::Done {
                        command: self.last_command.clone(),
                    });
                }
            }
        }

        events
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn last_command(&self) -> &str {
        &self.last_command
    }
}

fn tail_chars(text: &str, max: usize) -> &str {
    if text.len() <= max {
        text
    } else {
        let mut start = text.len() - max;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        &text[start..]
    }
}

/// Prompt patterns ported verbatim from FLAME's taskWatcher.ts.
fn prompt_patterns() -> &'static [regex::Regex] {
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"(?i)\(y/n\)",
            r"(?i)\[y/n\]",
            r"\[y/N\]",
            r"\[Y/n\]",
            r"(?i)\(yes/no\)",
            r"(?i)do you want to proceed",
            r"(?i)do you want to make this edit",
            r"(?i)do you want to create",
            r"(?i)do you trust the files",
            r"(?i)trust this (folder|workspace|directory)",
            r"(?i)allow this (action|edit|command)",
            r"(?im)overwrite\?\s*$",
            r"(?im)continue\?\s*$",
            r"(?i)press enter to continue",
            r"(?i)\by/n\b",
            r"(?i)enter to (select|confirm)",
            r"(?i)esc(ape)? to cancel",
            r"(?i)\byes, and don't ask again\b",
            r"(?i)\(y\)es\b",
            r"(?i)\(n\)o\b",
            r"(?i)\(d\)on't ask",
            r"(?i)allow execution",
            r"(?i)yes, allow",
        ]
        .iter()
        .map(|p| regex::Regex::new(p).unwrap())
        .collect()
    })
}

pub fn looks_like_prompt(text: &str) -> bool {
    prompt_patterns().iter().any(|re| re.is_match(text))
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test watcher::`
Expected: all 10 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/watcher.rs
git commit -m "feat: watcher state machine — busy/done/waiting detection with FLAME prompt patterns"
```

---

### Task 2: agents.rs — agent registry, detection, PATH scan

**Files:**
- Create: `src/agents.rs`
- Modify: `src/lib.rs` (add `pub mod agents;`)

**Interfaces:**
- Consumes: `regex` (Task 1 dep).
- Produces:
  ```rust
  pub struct AgentSpec {
      pub name: &'static str,
      pub binary: &'static str,
      pub color: ratatui::style::Color,
  }
  pub static AGENTS: &[AgentSpec];
  pub fn detect(command: &str) -> Option<&'static AgentSpec>;
  pub fn installed() -> Vec<&'static AgentSpec>;
  ```

- [ ] **Step 1: Write the failing tests**

Create `src/agents.rs` with stub impls + tests:

```rust
use ratatui::style::Color;
use std::sync::OnceLock;

pub struct AgentSpec {
    pub name: &'static str,
    pub binary: &'static str,
    pub color: Color,
}

pub static AGENTS: &[AgentSpec] = &[];

pub fn detect(_command: &str) -> Option<&'static AgentSpec> {
    None
}

pub fn installed() -> Vec<&'static AgentSpec> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_known_agents_in_commands() {
        for cmd in ["claude", "claude --help", "codex exec", "sudo gemini chat", "aider src/main.rs"] {
            assert!(detect(cmd).is_some(), "expected agent in: {cmd}");
        }
    }

    #[test]
    fn detect_returns_name_and_color() {
        let spec = detect("claude --continue").unwrap();
        assert_eq!(spec.name, "claude");
        assert_eq!(spec.color, Color::Rgb(0xff, 0xb2, 0x38));
    }

    #[test]
    fn ignores_non_agent_commands() {
        for cmd in ["git status", "declared -x foo", "ls -la", "vim"] {
            assert!(detect(cmd).is_none(), "unexpected agent in: {cmd}");
        }
    }

    #[test]
    fn installed_only_returns_binaries_on_path() {
        // `sh` is not in the agent table; every returned spec must exist on
        // PATH. We can't assert which agents are installed (environment-
        // dependent), only that the filter works: mock by checking each
        // returned binary resolves.
        for spec in installed() {
            assert!(AGENTS.iter().any(|a| a.binary == spec.binary));
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod agents;` to `src/lib.rs` (before `pub mod app;` — alphabetical). Run: `cargo test agents::`
Expected: FAIL — `detects_known_agents_in_commands` fails on `None`.

- [ ] **Step 3: Implement agents.rs**

Replace the stub table/functions:

```rust
pub static AGENTS: &[AgentSpec] = &[
    AgentSpec { name: "claude",        binary: "claude",        color: Color::Rgb(0xff, 0xb2, 0x38) },
    AgentSpec { name: "codex",         binary: "codex",         color: Color::Rgb(0x8b, 0xb4, 0xe8) },
    AgentSpec { name: "devin",         binary: "devin",         color: Color::Rgb(0xff, 0x6b, 0x52) },
    AgentSpec { name: "gemini",        binary: "gemini",        color: Color::Rgb(0x7e, 0xc9, 0xc9) },
    AgentSpec { name: "aider",         binary: "aider",         color: Color::Rgb(0xff, 0xcb, 0x6b) },
    AgentSpec { name: "cursor-agent",  binary: "cursor-agent",  color: Color::Rgb(0xc9, 0xa8, 0x77) },
    AgentSpec { name: "opencode",      binary: "opencode",      color: Color::Rgb(0xe0, 0x89, 0x4a) },
    AgentSpec { name: "copilot",       binary: "copilot",       color: Color::Rgb(0x8c, 0x81, 0x72) },
];

fn agent_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(claude|codex|devin|gemini|aider|cursor-agent|opencode|copilot)\b")
            .unwrap()
    })
}

pub fn detect(command: &str) -> Option<&'static AgentSpec> {
    let m = agent_regex().find(command)?;
    let name = m.as_str().to_lowercase();
    AGENTS.iter().find(|a| a.name == name)
}

pub fn installed() -> Vec<&'static AgentSpec> {
    AGENTS.iter().filter(|a| binary_on_path(a.binary)).collect()
}

#[cfg(unix)]
fn binary_on_path(binary: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|dir| {
        let p = dir.join(binary);
        p.is_file() && p.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    })
}

#[cfg(not(unix))]
fn binary_on_path(binary: &str) -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| dir.join(binary).is_file())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test agents::`
Expected: all 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/agents.rs
git commit -m "feat: agent registry — known CLI names, colors, command detection, PATH scan"
```

---

### Task 3: text_input.rs — shared single-line edit buffer

**Files:**
- Create: `src/text_input.rs`
- Modify: `src/lib.rs` (add `pub mod text_input;`)

**Interfaces:**
- Consumes: `crossterm` KeyEvent types (already a dep).
- Produces:
  ```rust
  pub enum EditResult { Editing, Submit(String), Cancel }
  pub struct LineEdit { /* private buf, pub error */ }
  impl LineEdit {
      pub fn new() -> LineEdit;
      pub fn from_str(s: &str) -> LineEdit;
      pub fn as_str(&self) -> &str;
      pub fn error(&self) -> Option<&str>;
      pub fn set_error(&mut self, msg: String);
      pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> EditResult;
  }
  ```

- [ ] **Step 1: Write the failing tests + stubs**

Create `src/text_input.rs`:

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub enum EditResult {
    Editing,
    Submit(String),
    Cancel,
}

pub struct LineEdit {}

impl LineEdit {
    pub fn new() -> LineEdit { LineEdit {} }
    pub fn from_str(_s: &str) -> LineEdit { LineEdit {} }
    pub fn as_str(&self) -> &str { "" }
    pub fn error(&self) -> Option<&str> { None }
    pub fn set_error(&mut self, _msg: String) {}
    pub fn handle_key(&mut self, _key: KeyEvent) -> EditResult { EditResult::Editing }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn chars_append_and_backspace_pops() {
        let mut e = LineEdit::new();
        for c in ['a', 'b', 'c'] {
            e.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(e.as_str(), "abc");
        e.handle_key(key(KeyCode::Backspace));
        assert_eq!(e.as_str(), "ab");
    }

    #[test]
    fn enter_submits_and_esc_cancels() {
        let mut e = LineEdit::from_str("/tmp/foo");
        match e.handle_key(key(KeyCode::Enter)) {
            EditResult::Submit(s) => assert_eq!(s, "/tmp/foo"),
            _ => panic!("expected Submit"),
        }
        let mut e = LineEdit::new();
        assert!(matches!(e.handle_key(key(KeyCode::Esc)), EditResult::Cancel));
    }

    #[test]
    fn ctrl_u_clears() {
        let mut e = LineEdit::from_str("junk");
        e.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(e.as_str(), "");
    }

    #[test]
    fn typing_clears_error() {
        let mut e = LineEdit::new();
        e.set_error("bad".into());
        assert_eq!(e.error(), Some("bad"));
        e.handle_key(key(KeyCode::Char('x')));
        assert_eq!(e.error(), None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod text_input;` to `src/lib.rs` (after `pub mod pty;`, before `ui`). Run: `cargo test text_input::`
Expected: FAIL — `chars_append_and_backspace_pops` asserts `"abc"` against `""`.

- [ ] **Step 3: Implement**

```rust
pub struct LineEdit {
    buf: String,
    error: Option<String>,
}

impl LineEdit {
    pub fn new() -> LineEdit {
        LineEdit { buf: String::new(), error: None }
    }

    pub fn from_str(s: &str) -> LineEdit {
        LineEdit { buf: s.to_string(), error: None }
    }

    pub fn as_str(&self) -> &str {
        &self.buf
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EditResult {
        self.error = None;
        match key.code {
            KeyCode::Enter => EditResult::Submit(std::mem::take(&mut self.buf)),
            KeyCode::Esc => EditResult::Cancel,
            KeyCode::Backspace => {
                self.buf.pop();
                EditResult::Editing
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.buf.clear();
                EditResult::Editing
            }
            KeyCode::Char(c) => {
                self.buf.push(c);
                EditResult::Editing
            }
            _ => EditResult::Editing,
        }
    }
}
```

(Delete the earlier stub `impl` block and stub struct — the test module stays.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test text_input::`
Expected: all 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/text_input.rs
git commit -m "feat: LineEdit single-line input buffer for prompts"
```

---

### Task 4: pane.rs — new fields + startup_command

**Files:**
- Modify: `src/pane.rs`
- Modify: `src/input.rs` (update the one `Pane::spawn` call site — arg count changes)
- Modify: `src/main.rs` (same)

**Interfaces:**
- Consumes: `crate::watcher::Watcher` (Task 1), `ratatui::style::Color`.
- Produces (fields consumed by Tasks 5–10):
  ```rust
  // New Pane fields:
  pub cwd: PathBuf,
  pub color: Option<Color>,
  pub agent_tagged: bool,
  pub running: bool,
  pub waiting: bool,
  pub attention: bool,
  pub last_notify_at: Option<Instant>,
  pub watcher: Watcher,
  pub startup_command: Option<String>,

  // New spawn signature:
  pub fn spawn(
      id: PaneId,
      title: String,
      rows: u16,
      cols: u16,
      cwd: Option<&Path>,
      events_tx: mpsc::Sender<PaneEvent>,
      startup_command: Option<&str>,
  ) -> anyhow::Result<Pane>
  ```

- [ ] **Step 1: Write the failing test**

In `src/pane.rs` tests, add:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test pane::tests::startup_command` 
Expected: FAIL to compile — `spawn` takes 6 args, test passes 7.

- [ ] **Step 3: Update Pane struct + spawn**

In `src/pane.rs` add imports and fields:

```rust
use crate::watcher::Watcher;
use ratatui::style::Color;
use std::path::PathBuf;
use std::time::Instant;
```

Extend `Pane`:

```rust
pub struct Pane {
    pub id: PaneId,
    pub title: String,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub exited: Option<String>,
    pub cwd: PathBuf,
    pub color: Option<Color>,
    pub agent_tagged: bool,
    pub running: bool,
    pub waiting: bool,
    pub attention: bool,
    pub last_notify_at: Option<Instant>,
    pub watcher: Watcher,
    pub startup_command: Option<String>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}
```

Update `spawn` signature (add `startup_command: Option<&str>`) and construct:

```rust
        let mut pane = Pane {
            id,
            title,
            parser,
            exited: None,
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
            writer: spawned.writer,
            master: spawned.master,
            child: spawned.child,
        };
        if let Some(cmd) = startup_command {
            // PTY input is buffered: the shell reads it whenever it's ready.
            let _ = pane.write_input(format!("{cmd}\r").as_bytes());
        }
        Ok(pane)
```

Fix the two existing call sites to pass `None`:
- `src/input.rs` leader-`n`: `Pane::spawn(id, format!("pane-{id}"), 24, 80, Some(&cwd), events_tx, None)` — Task 8 replaces this whole arm; for now just add the arg.
- `src/main.rs` first pane: `Pane::spawn(..., events_tx, None)`.
- `src/project.rs` test helper `dummy_pane` and `src/ui.rs` test `Pane::spawn` calls: add `None`.
- `src/pane.rs` existing tests: add `None` as the 7th arg.

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all existing tests pass + `startup_command_is_written_to_the_pty` PASS.

- [ ] **Step 5: Commit**

```bash
git add src/pane.rs src/input.rs src/main.rs src/project.rs src/ui.rs
git commit -m "feat: pane gains watcher/badge/agent fields and startup_command"
```

---

### Task 5: app.rs — new modes, project management, closed-pane history

**Files:**
- Modify: `src/app.rs`
- Modify: `src/project.rs` (add `active_pane_mut` accessor)

**Interfaces:**
- Consumes: `Pane` (Task 4 fields), `crate::palette::Palette` (Task 7 — but field type needed now; see note), `crate::text_input::LineEdit` (Task 3).
- Produces:
  ```rust
  pub enum InputMode { Normal, Leader, Palette, LineInput(LinePurpose) }
  pub enum LinePurpose { AddProject, RenamePane }

  pub struct ClosedPane {
      pub title: String, pub cwd: PathBuf,
      pub startup_command: Option<String>,
      pub color: Option<Color>, pub project: usize,
  }

  impl App {
      pub closed_panes: VecDeque<ClosedPane>,
      pub status_msg: Option<(String, Instant)>,
      pub line_input: Option<LineEdit>,
      pub last_watch_poll: Instant,

      pub fn flash(&mut self, msg: impl Into<String>);
      pub fn add_project(&mut self, root: PathBuf);
      pub fn set_active_project(&mut self, idx: usize);
      pub fn close_active_project(&mut self);
      pub fn ensure_active_pane(&mut self);
      pub fn spawn_pane(&mut self, startup_command: Option<&str>);
      pub fn close_active_pane(&mut self);
      pub fn reopen_last_pane(&mut self);
      pub fn adjust_split(&mut self, delta: f32);
      pub fn clear_focused_badges(&mut self);
      pub fn focused_pane_id(&self) -> Option<PaneId>;
  }
  pub fn expand_tilde(path: &str) -> PathBuf;
  ```
  Note on `palette` field: `App` does **not** hold the `Palette` — `input.rs` owns it as a transient while `mode == Palette` (see Task 8). This keeps `App` free of a forward dependency on Task 7. If the implementer prefers `app.palette: Option<Palette>`, that also works — pick one and be consistent; this plan uses the input.rs-owned variant.

- [ ] **Step 1: Write the failing tests**

In `src/app.rs`, extend `InputMode` + add stubs, then tests:

```rust
pub enum InputMode {
    Normal,
    Leader,
    Palette,
    LineInput(LinePurpose),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LinePurpose {
    AddProject,
    RenamePane,
}
```

New tests in `app.rs` test module:

```rust
    #[test]
    fn add_project_pushes_activates_and_spawns() {
        let mut app = app_with_projects(&["one"]);
        // Note: app_with_projects already roots its projects at /tmp — use a
        // different existing dir or dedupe will switch instead of adding.
        app.add_project(PathBuf::from("/etc"));
        assert_eq!(app.projects.len(), 2);
        assert_eq!(app.active_project, 1);
        assert_eq!(app.projects[1].name, "etc");
        assert_eq!(app.projects[1].panes.len(), 1, "new project spawns a pane");
    }

    #[test]
    fn add_project_with_duplicate_root_switches_instead() {
        let mut app = app_with_projects(&["one"]);
        app.projects[0].root = std::fs::canonicalize("/tmp").unwrap();
        app.add_project(PathBuf::from("/tmp"));
        assert_eq!(app.projects.len(), 1);
        assert_eq!(app.active_project, 0);
    }

    #[test]
    fn switching_to_empty_project_spawns_a_pane() {
        let mut app = app_with_projects(&["a", "b"]);
        assert!(app.projects[1].panes.is_empty());
        app.set_active_project(1);
        assert_eq!(app.projects[1].panes.len(), 1);
    }

    #[test]
    fn close_last_project_is_refused() {
        let mut app = app_with_projects(&["only"]);
        app.close_active_project();
        assert_eq!(app.projects.len(), 1);
        assert!(app.status_msg.is_some());
    }

    #[test]
    fn close_project_kills_panes_and_activates_neighbor() {
        let mut app = app_with_projects(&["a", "b"]);
        app.set_active_project(1); // spawns a pane in b
        app.close_active_project();
        assert_eq!(app.projects.len(), 1);
        assert_eq!(app.projects[0].name, "a");
        assert_eq!(app.active_project, 0);
    }

    #[test]
    fn closed_panes_history_caps_at_five_and_reopen_respawns() {
        let mut app = app_with_projects(&["demo"]);
        app.spawn_pane(None);
        for i in 0..6 {
            app.spawn_pane(None);
            app.active_project_mut().unwrap().panes.last_mut().unwrap().title = format!("t{i}");
            app.close_active_pane();
        }
        assert_eq!(app.closed_panes.len(), 5);
        assert_eq!(app.closed_panes[0].title, "t5", "most recent first");
        app.reopen_last_pane();
        assert_eq!(app.closed_panes.len(), 4);
        let panes = &app.active_project().unwrap().panes;
        assert!(panes.iter().any(|p| p.title == "t5"));
    }

    #[test]
    fn adjust_split_clamps() {
        let mut app = app_with_projects(&["demo"]);
        app.adjust_split(1.0);
        assert_eq!(app.projects[0].col_split, 0.85);
        app.adjust_split(-2.0);
        assert_eq!(app.projects[0].col_split, 0.15);
    }

    #[test]
    fn expand_tilde_replaces_leading_tilde() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/x"), PathBuf::from(format!("{home}/x")));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
    }
```

Note: `app_with_projects` helper already exists. `spawn_pane` in tests spawns real PTYs — consistent with existing `dummy_pane` tests. `close_active_project` will kill real shell children — fine in tests (fast).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test app::`
Expected: compile FAIL — methods don't exist.

- [ ] **Step 3: Implement app.rs changes**

Imports: add

```rust
use crate::pane::{Pane, PaneId};
use crate::text_input::LineEdit;
use ratatui::style::Color;
use std::collections::VecDeque;
use std::time::Instant;
```

Extend `App` struct + `new()`:

```rust
pub struct App {
    pub projects: Vec<Project>,
    pub active_project: usize,
    pub mode: InputMode,
    pub next_pane_id: u32,
    pub should_quit: bool,
    pub events_tx: mpsc::Sender<PaneEvent>,
    pub closed_panes: VecDeque<ClosedPane>,
    pub status_msg: Option<(String, Instant)>,
    pub line_input: Option<LineEdit>,
    pub last_watch_poll: Instant,
}

pub struct ClosedPane {
    pub title: String,
    pub cwd: PathBuf,
    pub startup_command: Option<String>,
    pub color: Option<Color>,
    pub project: usize,
}
```

`App::new` initializes: `closed_panes: VecDeque::new(), status_msg: None, line_input: None, last_watch_poll: Instant::now()`.

New methods:

```rust
    pub fn flash(&mut self, msg: impl Into<String>) {
        self.status_msg = Some((msg.into(), Instant::now()));
    }

    pub fn focused_pane_id(&self) -> Option<PaneId> {
        self.active_project()?.active_pane().map(|p| p.id)
    }

    pub fn add_project(&mut self, root: PathBuf) {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        if let Some(idx) = self.projects.iter().position(|p| p.root == root) {
            self.set_active_project(idx);
            return;
        }
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());
        self.projects.push(Project::new(name, root));
        self.active_project = self.projects.len() - 1;
        self.ensure_active_pane();
    }

    pub fn set_active_project(&mut self, idx: usize) {
        if idx < self.projects.len() {
            self.active_project = idx;
            self.ensure_active_pane();
        }
    }

    pub fn ensure_active_pane(&mut self) {
        let needs = self
            .active_project()
            .map(|p| p.panes.is_empty())
            .unwrap_or(false);
        if needs {
            self.spawn_pane(None);
        }
    }

    /// The shared spawn path — leader `n`, palette New pane, agent runs,
    /// and ensure-on-switch all come through here. Spawn size is 24x80;
    /// the first rendered frame's sync-resize corrects it.
    pub fn spawn_pane(&mut self, startup_command: Option<&str>) {
        let id = self.alloc_pane_id();
        let events_tx = self.events_tx.clone();
        if let Some(project) = self.active_project_mut() {
            let cwd = project.root.clone();
            match Pane::spawn(id, format!("pane-{id}"), 24, 80, Some(&cwd), events_tx, startup_command) {
                Ok(pane) => {
                    project.panes.push(pane);
                    project.active_pane = project.panes.len() - 1;
                }
                Err(e) => self.flash(format!("spawn failed: {e}")),
            }
        }
    }

    pub fn close_active_pane(&mut self) {
        if let Some(project) = self.active_project_mut() {
            if project.panes.is_empty() {
                return;
            }
            let idx = project.active_pane;
            let mut pane = project.panes.remove(idx);
            let _ = pane.kill();
            self.closed_panes.push_front(ClosedPane {
                title: pane.title,
                cwd: pane.cwd,
                startup_command: pane.startup_command,
                color: pane.color,
                project: self.active_project,
            });
            self.closed_panes.truncate(5);
            if project.active_pane >= project.panes.len() && project.active_pane > 0 {
                project.active_pane -= 1;
            }
        }
    }

    pub fn reopen_last_pane(&mut self) {
        let Some(closed) = self.closed_panes.pop_front() else {
            return;
        };
        let target = if closed.project < self.projects.len() {
            closed.project
        } else {
            self.active_project
        };
        self.active_project = target;
        let id = self.alloc_pane_id();
        let events_tx = self.events_tx.clone();
        if let Some(project) = self.active_project_mut() {
            match Pane::spawn(
                id,
                closed.title,
                24,
                80,
                Some(&closed.cwd),
                events_tx,
                closed.startup_command.as_deref(),
            ) {
                Ok(mut pane) => {
                    pane.color = closed.color;
                    project.panes.push(pane);
                    project.active_pane = project.panes.len() - 1;
                }
                Err(e) => self.flash(format!("spawn failed: {e}")),
            }
        }
    }

    pub fn close_active_project(&mut self) {
        if self.projects.len() <= 1 {
            self.flash("can't close the last project");
            return;
        }
        let idx = self.active_project;
        let mut project = self.projects.remove(idx);
        for pane in project.panes.iter_mut() {
            let _ = pane.kill();
        }
        self.active_project = idx.min(self.projects.len() - 1);
        self.ensure_active_pane();
    }

    pub fn adjust_split(&mut self, delta: f32) {
        if let Some(project) = self.active_project_mut() {
            project.col_split = (project.col_split + delta).clamp(0.15, 0.85);
        }
    }

    pub fn clear_focused_badges(&mut self) {
        if let Some(project) = self.active_project_mut() {
            if let Some(pane) = project.active_pane_mut() {
                pane.attention = false;
            }
        }
    }
```

In `src/project.rs` add:

```rust
    pub fn active_pane_mut(&mut self) -> Option<&mut Pane> {
        self.panes.get_mut(self.active_pane)
    }
```

At bottom of `app.rs`:

```rust
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            p.push(rest);
            return p;
        }
    } else if path == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(path)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all PASS including the 8 new ones.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/project.rs
git commit -m "feat: app state — input modes, project add/close/switch, closed-pane history, status flash"
```

---

### Task 6: notify.rs — badge + desktop notification routing

**Files:**
- Create: `src/notify.rs`
- Modify: `src/lib.rs` (add `pub mod notify;`)
- Modify: `Cargo.toml` (add `notify-rust = "4"`)

**Interfaces:**
- Consumes: `App`/`PaneId` (Task 5), `WatchEvent` (Task 1), `agents::detect` (Task 2).
- Produces:
  ```rust
  pub const NOTIFY_COOLDOWN: Duration; // 30s
  pub fn should_notify(pane_is_focused: bool) -> bool;
  pub fn dispatch(app: &mut App, pane_id: PaneId, event: WatchEvent);
  fn decide(app: &App, pane_id: PaneId, event: &WatchEvent) -> Option<(String, String)>;
  ```

- [ ] **Step 1: Add the dependency**

In `Cargo.toml` `[dependencies]` add:

```toml
notify-rust = "4"
```

Run `cargo build` once — pulls `notify-rust` + dbus deps. If the system lacks dbus dev libraries, surface that to the user before continuing (on this Linux desktop it should be present).

- [ ] **Step 2: Write the failing tests + stubs**

Create `src/notify.rs`:

```rust
use crate::app::App;
use crate::pane::PaneId;
use crate::watcher::WatchEvent;
use std::time::Duration;

pub const NOTIFY_COOLDOWN: Duration = Duration::from_secs(30);

pub fn should_notify(pane_is_focused: bool) -> bool {
    !pane_is_focused
}

pub fn dispatch(_app: &mut App, _pane_id: PaneId, _event: WatchEvent) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use crate::pane::Pane;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn app_with_pane() -> (App, PaneId) {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
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
```

- [ ] **Step 3: Run tests to verify they fail**

Add `pub mod notify;` to `src/lib.rs` (before `pub mod pane;`). Run: `cargo test notify::`
Expected: FAIL — badges never set.

- [ ] **Step 4: Implement notify.rs**

```rust
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
```

- [ ] **Step 5: Run tests**

Run: `cargo test notify::`
Expected: all 5 tests PASS. (The desktop-notify path runs `notify_rust` only inside `dispatch` when `decide` returns Some — in tests, `Waiting(true)` will attempt a real D-Bus notification. That's harmless on a desktop session; if CI is headless it's ignored by design. If it proves flaky, tests can call `decide` directly instead — acceptable deviation.)

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/notify.rs
git commit -m "feat: notify — badge state + desktop notifications with cooldown and auto-tag"
```

---

### Task 7: palette.rs — fuzzy match + command registry

**Files:**
- Create: `src/palette.rs`
- Modify: `src/lib.rs` (add `pub mod palette;`)

**Interfaces:**
- Consumes: `App`/`InputMode`/`LinePurpose`/`LineEdit` (Tasks 3, 5), `agents::installed` (Task 2).
- Produces:
  ```rust
  pub enum CmdId {
      NewPane, ClosePane, NextPane, PrevPane, JumpPane(usize),
      IncSplit, DecSplit,
      AddProject, SwitchProject(usize), CloseProject,
      ReopenPane, RunAgent(&'static str), RenamePane, Quit,
  }
  pub struct Command { pub id: CmdId, pub label: String }
  pub struct Palette { pub query: String, pub selected: usize, /* private items */ }
  impl Palette {
      pub fn open(app: &App) -> Palette;
      pub fn set_query(&mut self, q: String);
      pub fn filtered(&self) -> Vec<&Command>;
      pub fn move_next(&mut self);
      pub fn move_prev(&mut self);
      pub fn selected_command(&self) -> Option<&Command>;
  }
  pub fn fuzzy_score(query: &str, target: &str) -> Option<f64>;
  pub fn execute(app: &mut App, id: &CmdId);
  ```

- [ ] **Step 1: Write the failing tests + stubs**

Create `src/palette.rs`:

```rust
use crate::app::App;

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
    pub fn open(_app: &App) -> Palette {
        Palette { query: String::new(), selected: 0, items: Vec::new() }
    }
    pub fn set_query(&mut self, _q: String) {}
    pub fn filtered(&self) -> Vec<&Command> { Vec::new() }
    pub fn move_next(&mut self) {}
    pub fn move_prev(&mut self) {}
    pub fn selected_command(&self) -> Option<&Command> { None }
}

pub fn fuzzy_score(_query: &str, _target: &str) -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Project;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn app_with_projects(names: &[&str]) -> App {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
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
```

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod palette;` to `src/lib.rs` (before `pub mod pane;`). Run: `cargo test palette::`
Expected: FAIL — empty items, `None` scores.

- [ ] **Step 3: Implement palette.rs**

```rust
use crate::agents;
use crate::app::{App, InputMode, LinePurpose};
use crate::text_input::LineEdit;

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
```

- [ ] **Step 4: Run tests**

Run: `cargo test palette::`
Expected: all 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/palette.rs
git commit -m "feat: command palette — fuzzy scoring + command registry"
```

---

### Task 8: input.rs — new modes, leader keys, watcher input tap

**Files:**
- Modify: `src/input.rs`
- Modify: `src/app.rs` — `App` needs to own the open `Palette` after all (reconsidered: `handle_key` is the only place palette state is mutated, but it must persist between keypresses; `App` is the natural owner). Add `pub palette: Option<crate::palette::Palette>` to `App`, init `None`. (Overrides the Task 5 note — this is the consistent choice since `App` already owns `line_input`.)

**Interfaces:**
- Consumes: `Watcher::on_input` (Task 1), `notify::dispatch` (Task 6), `Palette`/`execute`/`CmdId` (Task 7), `LineEdit`/`EditResult` (Task 3), all `App` methods (Task 5).
- Produces: keymap additions —
  - Normal: input bytes tap `pane.watcher.on_input` before `write_input`; events drained into `notify::dispatch`.
  - Leader: `:` → open palette (`mode = Palette`, `app.palette = Some(Palette::open(app))`); `c` → `LineInput(AddProject)`; `[`/`]` → `set_active_project` wraps (uses ensure); `n` → `spawn_pane(None)`; `x` → `close_active_pane()`; `+`/`-` → `adjust_split(±0.05)`.
  - Palette mode: `Char(c)` → `query.push` via `set_query`; `Backspace`; `Up`/`Ctrl-p` → `move_prev`; `Down`/`Ctrl-n` → `move_next`; `Enter` → `execute` selected, then `mode = Normal` + `app.palette = None` **unless** execute switched to `LineInput`; `Esc` → close.
  - LineInput mode: delegate to `LineEdit::handle_key`; `Submit` → match `LinePurpose` (AddProject: `expand_tilde` → `is_dir` check → `add_project` or `set_error` + stay; RenamePane: empty → restore `pane-{id}` default, else set title); `Cancel` → `mode = Normal`, `line_input = None`.

- [ ] **Step 1: Write the failing tests**

Add to `src/input.rs` tests:

```rust
    #[test]
    fn leader_colon_opens_palette_and_enter_executes() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char(':'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Palette));
        assert!(app.palette.is_some());

        // type "new" then Enter — "New pane" should be the top hit
        for c in "new".chars() {
            handle_key(&mut app, key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(app.active_project().unwrap().panes.len(), 1);
    }

    #[test]
    fn esc_closes_palette_without_executing() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char(':'), KeyModifiers::NONE));
        handle_key(&mut app, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(app.palette.is_none());
        assert!(app.active_project().unwrap().panes.is_empty());
    }

    #[test]
    fn leader_c_opens_project_path_input_and_submit_adds_project() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::LineInput(LinePurpose::AddProject)));

        // /etc not /tmp — the fixture project is already rooted at /tmp and
        // add_project dedupes on canonicalized root.
        for c in "/etc".chars() {
            handle_key(&mut app, key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(app.projects.len(), 2);
    }

    #[test]
    fn invalid_project_path_sets_error_and_stays_open() {
        let mut app = app_with_one_project();
        handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        handle_key(&mut app, key(KeyCode::Char('c'), KeyModifiers::NONE));
        for c in "/definitely/not/here".chars() {
            handle_key(&mut app, key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::LineInput(LinePurpose::AddProject)));
        assert!(app.line_input.as_ref().unwrap().error().is_some());
    }

    #[test]
    fn typed_input_is_fed_to_the_pane_watcher() {
        let mut app = app_with_one_project();
        app.spawn_pane(None);
        handle_key(&mut app, key(KeyCode::Char('l'), KeyModifiers::NONE));
        handle_key(&mut app, key(KeyCode::Char('s'), KeyModifiers::NONE));
        handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
        let pane = &app.active_project().unwrap().panes[0];
        assert_eq!(pane.watcher.last_command(), "ls");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

First add to `src/app.rs`: `pub palette: Option<crate::palette::Palette>` field + `palette: None` in `new()`. Run: `cargo test input::`
Expected: FAIL — mode/keys unhandled.

- [ ] **Step 3: Implement input.rs**

New imports:

```rust
use crate::app::{App, InputMode, LinePurpose};
use crate::notify;
use crate::palette::{self, Palette};
use crate::text_input::{EditResult, LineEdit};
```

Rewrite `handle_key`:

```rust
pub fn handle_key(app: &mut App, key: KeyEvent) {
    match app.mode {
        InputMode::Normal => {
            if key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.mode = InputMode::Leader;
                return;
            }
            let mut pending = Vec::new();
            if let Some(project) = app.active_project_mut() {
                if let Some(pane) = project.active_pane_mut() {
                    let bytes = key_event_to_bytes(key);
                    if !bytes.is_empty() {
                        for e in pane.watcher.on_input(&bytes) {
                            pending.push((pane.id, e));
                        }
                        let _ = pane.write_input(&bytes);
                    }
                }
            }
            for (id, e) in pending {
                notify::dispatch(app, id, e);
            }
        }
        InputMode::Leader => {
            app.mode = InputMode::Normal;
            match key.code {
                KeyCode::Char('q') => app.should_quit = true,
                KeyCode::Char('n') => app.spawn_pane(None),
                KeyCode::Char('x') => app.close_active_pane(),
                KeyCode::Left | KeyCode::Char('h') => {
                    if let Some(p) = app.active_project_mut() {
                        p.prev_pane();
                    }
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    if let Some(p) = app.active_project_mut() {
                        p.next_pane();
                    }
                }
                KeyCode::Char('[') => app.prev_project(),
                KeyCode::Char(']') => app.next_project(),
                KeyCode::Char('+') => app.adjust_split(0.05),
                KeyCode::Char('-') => app.adjust_split(-0.05),
                KeyCode::Char(':') => {
                    app.palette = Some(Palette::open(app));
                    app.mode = InputMode::Palette;
                }
                KeyCode::Char('c') => {
                    app.line_input = Some(LineEdit::new());
                    app.mode = InputMode::LineInput(LinePurpose::AddProject);
                }
                _ => {}
            }
            // Project switch lands on ensure-pane.
            if matches!(key.code, KeyCode::Char('[') | KeyCode::Char(']')) {
                app.ensure_active_pane();
            }
        }
        InputMode::Palette => {
            let Some(pal) = app.palette.as_mut() else {
                app.mode = InputMode::Normal;
                return;
            };
            match key.code {
                KeyCode::Esc => {
                    app.palette = None;
                    app.mode = InputMode::Normal;
                }
                KeyCode::Enter => {
                    let cmd = pal.selected_command().map(|c| c.id.clone());
                    if let Some(id) = cmd {
                        palette::execute(app, &id);
                    }
                    if !matches!(app.mode, InputMode::LineInput(_)) {
                        app.mode = InputMode::Normal;
                    }
                    app.palette = None;
                }
                KeyCode::Up => pal.move_prev(),
                KeyCode::Down => pal.move_next(),
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => pal.move_prev(),
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => pal.move_next(),
                KeyCode::Backspace => {
                    let mut q = pal.query.clone();
                    q.pop();
                    pal.set_query(q);
                }
                KeyCode::Char(c) => {
                    let mut q = pal.query.clone();
                    q.push(c);
                    pal.set_query(q);
                }
                _ => {}
            }
        }
        InputMode::LineInput(purpose) => {
            let Some(edit) = app.line_input.as_mut() else {
                app.mode = InputMode::Normal;
                return;
            };
            match edit.handle_key(key) {
                EditResult::Editing => {}
                EditResult::Cancel => {
                    app.line_input = None;
                    app.mode = InputMode::Normal;
                }
                EditResult::Submit(text) => match purpose {
                    LinePurpose::AddProject => {
                        let path = crate::app::expand_tilde(text.trim());
                        if path.is_dir() {
                            app.line_input = None;
                            app.mode = InputMode::Normal;
                            app.add_project(path);
                        } else {
                            app.line_input
                                .as_mut()
                                .unwrap()
                                .set_error(format!("not a directory: {}", path.display()));
                        }
                    }
                    LinePurpose::RenamePane => {
                        if let Some(project) = app.active_project_mut() {
                            if let Some(pane) = project.active_pane_mut() {
                                pane.title = if text.trim().is_empty() {
                                    format!("pane-{}", pane.id)
                                } else {
                                    text.trim().to_string()
                                };
                            }
                        }
                        app.line_input = None;
                        app.mode = InputMode::Normal;
                    }
                },
            }
        }
    }
}
```

(Delete the old `match app.mode` body; the old inline `n`/`x`/`[`/`]`/`+`/`-` code is replaced by the App methods. Note `prev_project`/`next_project` stay on App — `[`/`]` call them then `ensure_active_pane`. The old `use crate::pane::Pane;` import is now unused — remove it or `cargo` will warn.)

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all PASS including 5 new input tests.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/input.rs
git commit -m "feat: input wiring — palette ':' + line-input 'c' modes, watcher input tap"
```

---

### Task 9: main.rs — CLI args + watcher poll loop

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `watcher::POLL` (Task 1), `notify::dispatch` (Task 6), `App` methods (Task 5).
- Produces: `rustterm [dir…]` startup + the 400ms watcher tick + exited-pane badge cleanup.

- [ ] **Step 1: Refactor `main()` for args**

Replace the top of `main()`:

```rust
fn main() -> anyhow::Result<()> {
    let (events_tx, events_rx) = mpsc::channel::<PaneEvent>();

    let roots = project_roots_from_args();
    let mut app = App::new(events_tx.clone());
    for root in &roots {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string());
        app.projects.push(Project::new(name, root.clone()));
    }

    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    // Only the first project gets an initial pane; others spawn on activation.
    if let Some(first) = roots.first() {
        let first_pane = Pane::spawn(
            app.alloc_pane_id(),
            "pane-0".to_string(),
            rows,
            cols,
            Some(first),
            events_tx,
            None,
        )?;
        app.projects[0].panes.push(first_pane);
    }

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &events_rx);
    ratatui::restore();
    result
}

fn project_roots_from_args() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    for arg in std::env::args().skip(1) {
        match std::fs::canonicalize(&arg) {
            Ok(p) if p.is_dir() => roots.push(p),
            _ => eprintln!("rustterm: skipping {arg:?} — not a directory"),
        }
    }
    if roots.is_empty() {
        roots.push(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    }
    roots
}
```

- [ ] **Step 2: Add the watcher poll + badge cleanup to `run()`**

Inside the `run` loop, after the pane-event `try_recv` drain:

```rust
        while let Ok(event) = events_rx.try_recv() {
            if let PaneEvent::Exited(id) = event {
                for project in app.projects.iter_mut() {
                    for pane in project.panes.iter_mut() {
                        if pane.id == id {
                            pane.exited = Some("exited".to_string());
                            pane.waiting = false;
                            pane.running = false;
                        }
                    }
                }
            }
        }

        if app.last_watch_poll.elapsed() >= rustterm::watcher::POLL {
            app.last_watch_poll = Instant::now();
            let now = Instant::now();
            let mut pending = Vec::new();
            for project in app.projects.iter_mut() {
                for pane in project.panes.iter_mut() {
                    if pane.exited.is_some() {
                        continue;
                    }
                    let text = match pane.parser.lock() {
                        Ok(p) => p.screen().contents(),
                        Err(_) => continue,
                    };
                    for e in pane.watcher.update(now, &text) {
                        pending.push((pane.id, e));
                    }
                    pane.running = pane.watcher.is_running();
                }
            }
            for (id, e) in pending {
                rustterm::notify::dispatch(app, id, e);
            }
        }

        app.clear_focused_badges();
```

Add `use std::time::Instant;` to imports.

- [ ] **Step 3: Build and smoke-test**

Run: `cargo build`
Expected: compiles clean.

Then a manual smoke check (not automated — the TUI needs a real terminal):

```bash
cargo install --path .
rustterm /tmp /home/mahadi/projects/RustTerm   # two projects; [ ] switches
# inside: Ctrl+A : → palette opens; type "new", Enter → pane spawns
#         Ctrl+A c → path prompt; type a dir, Enter → project added
#         run `sleep 12` in a pane → after ~10s quiet a Done notification/badge
```

- [ ] **Step 4: Run full test suite**

Run: `cargo test`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat: multi-project CLI args + 400ms watcher poll driving notifications"
```

---

### Task 10: ui.rs — badges, palette overlay, prompts, status flash

**Files:**
- Modify: `src/ui.rs`

**Interfaces:**
- Consumes: all Pane badge fields + `App.mode`/`palette`/`line_input`/`status_msg`.
- Produces: rendered badge/palette/prompt UI (visual layer — no new logic exported).

- [ ] **Step 1: Write the failing tests**

Add to `src/ui.rs` tests:

```rust
    #[test]
    fn waiting_pane_shows_dot_in_title() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        let mut project = Project::new("demo".into(), PathBuf::from("/tmp"));
        let (ptx, _prx) = mpsc::channel();
        let mut pane = Pane::spawn(1, "work".into(), 24, 80, None, ptx, None).unwrap();
        pane.waiting = true;
        project.panes.push(pane);
        app.projects.push(project);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(terminal.backend().buffer(), "work ●"));
    }

    #[test]
    fn status_flash_replaces_hint_text() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        app.flash("can't close the last project");
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(terminal.backend().buffer(), "can't close the last project"));
    }

    #[test]
    fn palette_overlay_lists_matching_commands() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.mode = crate::app::InputMode::Palette;
        app.palette = Some(crate::palette::Palette::open(&app));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(buffer_contains(buffer, "New pane"));
        assert!(buffer_contains(buffer, "Quit"));
    }

    #[test]
    fn line_input_prompt_shows_buffer() {
        let (tx, _rx) = mpsc::channel();
        let mut app = App::new(tx);
        app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
        app.mode = crate::app::InputMode::LineInput(crate::app::LinePurpose::AddProject);
        app.line_input = Some(crate::text_input::LineEdit::from_str("/tmp/fo"));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_contains(terminal.backend().buffer(), "/tmp/fo"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test ui::`
Expected: FAIL — no badges/overlay/flash rendered.

- [ ] **Step 3: Implement ui.rs changes**

Imports: add

```rust
use crate::app::InputMode;
use ratatui::widgets::Clear;
```

`draw()` — add after `draw_status_bar`:

```rust
    if matches!(app.mode, InputMode::Palette) {
        draw_palette(frame, app);
    }
```

`draw_sidebar` — badge suffix per project:

```rust
            let badge = if project.panes.iter().any(|p| p.waiting) {
                " ●"
            } else if project.panes.iter().any(|p| p.attention) {
                " !"
            } else {
                ""
            };
            ListItem::new(format!("{}{}", project.name, badge)).style(style)
```

`draw_panes` — title suffixes + border priority:

```rust
        let mut title = pane.title.clone();
        if pane.waiting {
            title.push_str(" ●");
        } else if pane.running {
            title.push_str(" ▸");
        }
        if pane.attention {
            title.push_str(" !");
        }
        if pane.exited.is_some() {
            title.push_str(" [exited]");
        }
        let border_style = if pane.waiting {
            Style::default().fg(Color::Yellow)
        } else if is_active {
            Style::default().fg(Color::Cyan)
        } else if let Some(c) = pane.color {
            Style::default().fg(c)
        } else {
            Style::default()
        };
```

(Replace the old `title`/`border_style` blocks; the `exited` suffix moves into the new title builder.)

`draw_status_bar` — flash + new modes + pane state:

```rust
fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let fresh_flash = app
        .status_msg
        .as_ref()
        .filter(|(_, at)| at.elapsed() < std::time::Duration::from_secs(3));
    let text = if let Some((msg, _)) = fresh_flash {
        msg.clone()
    } else {
        match app.mode {
            InputMode::Normal => {
                let state = app
                    .active_project()
                    .and_then(|p| p.active_pane())
                    .map(|p| {
                        if p.waiting {
                            "  ● waiting for input"
                        } else if p.running {
                            "  ▸ running"
                        } else {
                            ""
                        }
                    })
                    .unwrap_or("");
                format!("Ctrl+A for commands{state}")
            }
            InputMode::Leader => {
                "n new  x close  h/l switch  [ ] project  +/- split  : palette  c add-project  q quit"
                    .to_string()
            }
            InputMode::Palette => "type to filter  ↑/↓ move  enter run  esc cancel".to_string(),
            InputMode::LineInput(purpose) => {
                let label = match purpose {
                    crate::app::LinePurpose::AddProject => "Add project: ",
                    crate::app::LinePurpose::RenamePane => "Rename pane: ",
                };
                let (buf, err) = app
                    .line_input
                    .as_ref()
                    .map(|e| (e.as_str(), e.error().unwrap_or("")))
                    .unwrap_or(("", ""));
                if err.is_empty() {
                    format!("{label}{buf}█")
                } else {
                    format!("{label}{buf}█  — {err}")
                }
            }
        }
    };
    frame.render_widget(Paragraph::new(text), area);
}
```

New `draw_palette`:

```rust
fn draw_palette(frame: &mut Frame, app: &App) {
    let Some(pal) = app.palette.as_ref() else { return };
    let area = frame.area();
    let width = (area.width * 3 / 5).clamp(30, area.width);
    let height = 14u16.min(area.height.saturating_sub(2)).max(6);
    let rect = Rect {
        x: (area.width - width) / 2,
        y: area.height / 6,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title("Command");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);
    frame.render_widget(Paragraph::new(format!("> {}", pal.query)), rows[0]);

    let visible = rows[1].height as usize;
    let items: Vec<ListItem> = pal
        .filtered()
        .iter()
        .enumerate()
        .take(visible)
        .map(|(i, c)| {
            let style = if i == pal.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(c.label.clone()).style(style)
        })
        .collect();
    frame.render_widget(List::new(items), rows[1]);
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all PASS including 4 new ui tests.

- [ ] **Step 5: Commit**

```bash
git add src/ui.rs
git commit -m "feat: ui — waiting/attention/running badges, palette overlay, line-input prompt, status flash"
```

---

## Self-Review Notes

- **Spec coverage:** multi-project (T5/T8/T9), watcher+prompts (T1), agents+auto-tag (T2/T6), notifications+badges+cooldown (T6/T9/T10), palette+fuzzy+reopen+rename+run-agent (T7/T8), startup_command (T4), status flash (T5/T10), LineEdit (T3/T8/T10). Deferred items in spec stay deferred.
- **Known soft spots to verify during review:** `notify::dispatch` fires a real `notify_rust` call inside tests (harmless on desktop, ignored on headless — flagged in Task 6 Step 5); the `project.rs` change is one accessor beyond the spec's "unchanged" note; `App` owns `Option<Palette>` (Task 8 overrides the Task 5 interface note — documented inline).
