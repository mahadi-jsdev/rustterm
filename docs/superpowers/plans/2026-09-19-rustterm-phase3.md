# RustTerm Phase 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn RustTerm into a workspace tool — git sidebar + lazygit, file finder → nvim, AI auto-commit, pane lifecycle hardening, vim-style terminal search.

**Architecture:** Four new leaf modules (`git.rs` shell-out, `finder.rs` ignore-walk, `ai_commit.rs` ureq+OpenAI, `search.rs` vt100 match engine) feed an expanded `App` (new input modes + a second `AppEvent` channel for worker-thread results). Input wiring and UI follow Phase 2's established patterns.

**Tech Stack:** Rust 2021, ratatui 0.30, crossterm 0.29, vt100 0.16, portable-pty 0.9. New deps: `ignore = "0.4"`, `ureq = "2"`, `serde_json = "1"`.

**Spec:** `docs/superpowers/specs/2026-09-19-rustterm-phase3-design.md`

## Global Constraints

- Rust edition 2021; warnings must stay at zero (`cargo build` clean).
- Leader key stays `Ctrl+A`; in Normal mode all other keys pass to the focused pane's PTY (and its watcher).
- New leader keys: `g` (git sidebar focus), `G` (lazygit pane), `f` (file finder), `/` (search prompt). All currently unassigned.
- `Ctrl+A` → Leader must work from every non-text-entry mode (Sidebar, Search) — not from Palette/Finder/LineInput (text entry owns keys there).
- No synchronous blocking work on the render loop beyond ~50ms: git polls and the OpenAI call run on worker threads posting through `AppEvent`.
- Tests in `#[cfg(test)] mod tests` at the bottom of each file, matching existing style. Temp-dir fixtures must use unique per-test names (parallel tests).
- Follow the existing conventions: status flashes via `app.flash`, pane spawning via `app.spawn_pane`, borrows released before dispatch.

---

### Task 1: git.rs — shell-out git queries and actions

**Files:**
- Create: `src/git.rs`
- Modify: `src/lib.rs` (add `pub mod git;` after `pub mod finder;` — insert in the new-module block, e.g. line 3 before `pub mod layout;`)

**Interfaces:**
- Consumes: nothing (std::process::Command only)
- Produces: `GitStatus { pub branch: String, pub files: Vec<ChangedFile> }`, `ChangedFile { pub status: char, pub path: String }`, `status(&Path) -> Option<GitStatus>`, `branches(&Path) -> Vec<String>`, `switch(&Path, &str) -> Result<(), String>`, `add_all(&Path) -> Result<(), String>`, `staged_diff(&Path) -> Result<String, String>`, `commit(&Path, &str) -> Result<String, String>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    // Unique per test — parallel tests each create/remove their own repo.
    fn repo(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rustterm-git-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| {
            Command::new("git").arg("-C").arg(&root).args(args).output().unwrap()
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), b"one").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-m", "init"]);
        root
    }

    #[test]
    fn status_reports_branch_and_changes() {
        let root = repo("status");
        std::fs::write(root.join("a.txt"), b"two").unwrap();   // M
        std::fs::write(root.join("b.txt"), b"new").unwrap();    // ??
        let s = status(&root).unwrap();
        assert_eq!(s.branch, "main");
        assert!(s.files.iter().any(|f| f.path == "a.txt" && f.status == 'M'));
        assert!(s.files.iter().any(|f| f.path == "b.txt" && f.status == '?'));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn status_returns_none_outside_a_repo() {
        let root = std::env::temp_dir().join(format!("rustterm-git-norepo-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(status(&root).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn branches_lists_current_first_then_others() {
        let root = repo("branches");
        Command::new("git").arg("-C").arg(&root).args(["branch", "feature"]).output().unwrap();
        let bs = branches(&root);
        assert_eq!(bs[0], "main");
        assert!(bs.contains(&"feature".to_string()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn switch_and_staged_diff_and_commit_roundtrip() {
        let root = repo("switch");
        switch(&root, &branches(&root)[0]).unwrap(); // no-op switch succeeds
        std::fs::write(root.join("c.txt"), b"x").unwrap();
        add_all(&root).unwrap();
        assert!(staged_diff(&root).unwrap().contains("c.txt"));
        let hash = commit(&root, "test commit").unwrap();
        assert_eq!(hash.len(), 7);
        assert!(staged_diff(&root).unwrap().trim().is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test git::`
Expected: FAIL — `unresolved import crate::git` / file not found.

- [ ] **Step 3: Write the implementation**

```rust
use std::path::Path;
use std::process::Command;

pub struct GitStatus {
    pub branch: String,
    pub files: Vec<ChangedFile>,
}

pub struct ChangedFile {
    pub status: char,
    pub path: String,
}

fn git(root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))
}

pub fn status(root: &Path) -> Option<GitStatus> {
    let out = git(root, &["status", "--porcelain=v1", "-z"]).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(GitStatus {
        branch: branch_name(root),
        files: parse_porcelain(&out.stdout),
    })
}

fn branch_name(root: &Path) -> String {
    let named = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    named.unwrap_or_else(|| {
        git(root, &["rev-parse", "--short", "HEAD"])
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    })
}

/// porcelain -z: "XY path\0" per entry; renames emit a second nul
/// field holding the source path.
fn parse_porcelain(raw: &[u8]) -> Vec<ChangedFile> {
    let text = String::from_utf8_lossy(raw);
    let mut entries = text.split('\0').filter(|s| !s.is_empty());
    let mut files = Vec::new();
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let x = entry.as_bytes()[0] as char;
        let y = entry.as_bytes()[1] as char;
        files.push(ChangedFile {
            status: display_letter(x, y),
            path: entry[3..].to_string(),
        });
        if x == 'R' || y == 'R' {
            entries.next(); // consume the rename source field
        }
    }
    files
}

fn display_letter(x: char, y: char) -> char {
    if x == '?' || y == '?' {
        '?'
    } else if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        'U'
    } else if x == 'R' || y == 'R' {
        'R'
    } else if x == 'A' {
        'A'
    } else if x == 'D' || y == 'D' {
        'D'
    } else {
        'M'
    }
}

pub fn branches(root: &Path) -> Vec<String> {
    let current = branch_name(root);
    let mut names: Vec<String> = git(root, &["branch", "--format=%(refname:short)"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|s| !s.is_empty() && s != &current)
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut out = Vec::new();
    if !current.is_empty() {
        out.push(current);
    }
    out.extend(names);
    out
}

pub fn switch(root: &Path, branch: &str) -> Result<(), String> {
    let out = git(root, &["switch", branch])?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn add_all(root: &Path) -> Result<(), String> {
    let out = git(root, &["add", "-A"])?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn staged_diff(root: &Path) -> Result<String, String> {
    let out = git(root, &["diff", "--cached"])?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Returns the new commit's short hash.
pub fn commit(root: &Path, msg: &str) -> Result<String, String> {
    let out = git(root, &["commit", "-m", msg])?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(git(root, &["rev-parse", "--short", "HEAD"])
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default())
}
```

And `pub mod git;` in `src/lib.rs` (after `pub mod completion;`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: all pass, including 4 new git tests.

- [ ] **Step 5: Commit**

```bash
git add src/git.rs src/lib.rs
git commit -m "feat: git.rs — porcelain status, branches, switch, staged diff, commit"
```

---

### Task 2: pane.rs — PaneStatus lifecycle + reap()

**Files:**
- Modify: `src/pane.rs` (field + enum + `reap()` + tests)
- Modify: `src/main.rs:87-90` (Exited event → `reap()`)
- Modify: `src/main.rs:104` (poll skip → `status != Running`)
- Modify: `src/ui.rs:67-68` (title → `[exited N]`)

**Interfaces:**
- Consumes: `child.wait()` (portable-pty `Child`), existing `PaneEvent::Exited(PaneId)`
- Produces: `PaneStatus { Running, Exited(i32), Failed(String) }`, `pane.status`, `pane.reap()`, `pane.is_dead()` — later tasks rely on `status` replacing `exited` everywhere.

- [ ] **Step 1: Write the failing tests** (append to `pane.rs` test module)

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test pane::`
Expected: FAIL — `PaneStatus`/`reap`/`is_dead` don't exist.

- [ ] **Step 3: Write the implementation**

In `src/pane.rs`, replace `pub exited: Option<String>` with:

```rust
pub enum PaneStatus {
    Running,
    Exited(i32),
    Failed(String),
}
```

Field: `pub status: PaneStatus` (init `PaneStatus::Running` in `spawn`).

Methods (near `kill`):

```rust
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
```

Call-site updates:

`src/main.rs` Exited drain — replace `pane.exited = Some("exited".to_string());` with `pane.reap();` (reap also sets `waiting`/`running`? keep the two lines that already clear them).

Poll skip — `if pane.exited.is_some()` → `if pane.is_dead()`.

`src/ui.rs` title — replace:

```rust
if pane.exited.is_some() {
    title.push_str(" [exited]");
}
```

with:

```rust
match &pane.status {
    crate::pane::PaneStatus::Exited(code) => title.push_str(&format!(" [exited {code}]")),
    crate::pane::PaneStatus::Failed(_) => title.push_str(" [failed]"),
    crate::pane::PaneStatus::Running => {}
}
```

Grep the whole tree for remaining `.exited` references — `src/notify.rs` and tests may use it; update all to `status`/`is_dead()`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: all pass incl. `reap_captures_the_exit_code` (exit code 3 must actually appear — the spawned shell is interactive `$SHELL`; `exit 3` sets code 3).

- [ ] **Step 5: Commit**

```bash
git add src/pane.rs src/main.rs src/ui.rs src/notify.rs
git commit -m "feat: pane lifecycle — PaneStatus, real exit codes, zombie reaping"
```

---

### Task 3: search.rs — scrollback match engine + pane.search

**Files:**
- Create: `src/search.rs`
- Modify: `src/lib.rs` (`pub mod search;`)
- Modify: `src/pane.rs` (`pub search: Option<SearchState>` + `set_scroll()`)

**Interfaces:**
- Consumes: `vt100::Screen` (`contents()`, `size()`), `pane.parser`, `pane.set_scroll`
- Produces: `SearchMatch { pub row: usize, pub col: usize, pub len: usize }`, `SearchState { pub query: String, pub matches: Vec<SearchMatch>, pub idx: usize }`, `find_matches(&Screen, &str) -> Vec<SearchMatch>`, `scrollback_len(&Screen) -> usize`, `offset_for_row(usize, usize) -> usize`, `SearchState::{next, prev, current}`, `Pane::set_scroll(usize)`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn screen_with(lines: &[&str]) -> vt100::Parser {
        let mut p = vt100::Parser::new(5, 40, 100);
        for l in lines {
            p.process(format!("{l}\r\n").as_bytes());
        }
        p
    }

    #[test]
    fn finds_all_case_insensitive_matches_with_positions() {
        let p = screen_with(&["alpha", "beta", "ALPHA again", "none"]);
        let m = find_matches(p.screen(), "alpha");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].row, 0);
        assert_eq!(m[0].col, 0);
        assert_eq!(m[1].row, 2);
        assert_eq!(m[1].col, 0);
        assert_eq!(m[0].len, 5);
    }

    #[test]
    fn empty_query_finds_nothing() {
        let p = screen_with(&["x"]);
        assert!(find_matches(p.screen(), "").is_empty());
    }

    #[test]
    fn scrollback_rows_are_counted() {
        // 10 lines into a 5-row screen → ≥5 scrollback lines.
        let lines: Vec<String> = (0..10).map(|i| format!("line{i}")).collect();
        let mut p = vt100::Parser::new(5, 40, 100);
        for l in &lines {
            p.process(format!("{l}\r\n").as_bytes());
        }
        assert!(scrollback_len(p.screen()) >= 5);
    }

    #[test]
    fn offset_for_row_maps_scrollback_line_to_view() {
        // 8 scrollback lines; match on line 3 → offset 5 puts it at top.
        assert_eq!(offset_for_row(8, 3), 5);
        // Match inside the visible region → stay at live view.
        assert_eq!(offset_for_row(8, 10), 0);
    }

    #[test]
    fn next_prev_wrap() {
        let mut s = SearchState {
            query: "x".into(),
            matches: vec![
                SearchMatch { row: 0, col: 0, len: 1 },
                SearchMatch { row: 1, col: 0, len: 1 },
                SearchMatch { row: 2, col: 0, len: 1 },
            ],
            idx: 2,
        };
        s.next();
        assert_eq!(s.idx, 0);
        s.prev();
        assert_eq!(s.idx, 2);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test search::`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Write the implementation**

```rust
pub struct SearchMatch {
    /// Row index across scrollback+visible lines of `screen.contents()`.
    pub row: usize,
    /// Byte column of the match start inside that line.
    pub col: usize,
    pub len: usize,
}

pub struct SearchState {
    pub query: String,
    pub matches: Vec<SearchMatch>,
    pub idx: usize,
}

impl SearchState {
    pub fn next(&mut self) {
        if !self.matches.is_empty() {
            self.idx = (self.idx + 1) % self.matches.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.matches.is_empty() {
            self.idx = if self.idx == 0 { self.matches.len() - 1 } else { self.idx - 1 };
        }
    }

    pub fn current(&self) -> Option<&SearchMatch> {
        self.matches.get(self.idx)
    }
}

/// Case-insensitive substring search over the pane's full text
/// (scrollback + visible rows).
pub fn find_matches(screen: &vt100::Screen, query: &str) -> Vec<SearchMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let q = query.to_lowercase();
    screen
        .contents()
        .lines()
        .enumerate()
        .flat_map(|(row, line)| {
            let lower = line.to_lowercase();
            lower
                .match_indices(q.as_str())
                .map(move |(col, _)| SearchMatch { row, col, len: query.len() })
        })
        .collect()
}

/// Lines in scrollback = total text lines − screen height.
pub fn scrollback_len(screen: &vt100::Screen) -> usize {
    screen
        .contents()
        .lines()
        .count()
        .saturating_sub(screen.size().0 as usize)
}

/// Scrollback offset that puts `match_row` at the top of the view
/// (0 = live view; used when the row is already inside it).
pub fn offset_for_row(scrollback_len: usize, match_row: usize) -> usize {
    scrollback_len.saturating_sub(match_row)
}
```

In `src/pane.rs` — field + helper:

```rust
pub search: Option<crate::search::SearchState>,  // init None in spawn

/// Jump directly to a scrollback offset (search navigation).
pub fn set_scroll(&self, off: usize) {
    if let Ok(mut p) = self.parser.lock() {
        p.screen_mut().set_scrollback(off);
    }
}
```

`pub mod search;` in `src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: all pass. If `scrollback_rows_are_counted` fails because `contents()` excludes scrollback, switch `find_matches`/`scrollback_len` to iterate `screen` rows via `screen.cell()` over `scrollback+size` — but verify `contents()` first (it should include scrollback).

- [ ] **Step 5: Commit**

```bash
git add src/search.rs src/lib.rs src/pane.rs
git commit -m "feat: search.rs — scrollback match engine + pane search state"
```

---

### Task 4: finder.rs — file index + fuzzy filter

**Files:**
- Create: `src/finder.rs`
- Modify: `src/lib.rs` (`pub mod finder;`)
- Modify: `Cargo.toml` (`ignore = "0.4"`)
- Modify: `src/palette.rs` (`fuzzy_score` → `pub fn`)
- Modify: `src/app.rs` (`shell_quote` helper — used by finder Enter and later sidebar diff)

**Interfaces:**
- Consumes: `crate::palette::fuzzy_score(query, target) -> Option<f64>` (Task 7 Phase 2 — now made `pub`)
- Produces: `FileIndex { pub root: PathBuf, pub files: Vec<PathBuf> }`, `FinderState { pub index: FileIndex, pub query: String, pub selected: usize }`, `build(&Path) -> FileIndex`, `filter(&FileIndex, &str) -> Vec<&Path>`, `FinderState::{open, filtered, set_query, move_next, move_prev, selected_path}`, `app::shell_quote(&str) -> String`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rustterm-find-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for f in ["src/main.rs", "src/lib.rs", "docs/spec.md", "ignored.log"] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
        }
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), b"x").unwrap();
        std::fs::write(root.join(".hidden"), b"x").unwrap();
        std::fs::write(root.join(".gitignore"), b"ignored.log\n").unwrap();
        root
    }

    #[test]
    fn build_skips_gitignored_hidden_and_dotgit() {
        let root = tree("build");
        // .gitignore only applies inside a git repo — init one.
        std::process::Command::new("git").arg("-C").arg(&root).args(["init", "-q"]).output().unwrap();
        let idx = build(&root);
        let names: Vec<String> = idx.files.iter().map(|p| p.to_string_lossy().to_string()).collect();
        assert!(names.contains(&"src/main.rs".into()));
        assert!(!names.iter().any(|n| n.contains("ignored.log")));
        assert!(!names.iter().any(|n| n.starts_with(".git") || n.contains(".git/")));
        assert!(!names.contains(&".hidden".to_string()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn filter_fuzzy_ranks_and_caps() {
        let root = tree("filter");
        let idx = build(&root);
        let hits = filter(&idx, "main");
        assert_eq!(hits[0].to_string_lossy(), "src/main.rs");
        // Empty query returns everything (alphabetical), capped at 50.
        assert_eq!(filter(&idx, "").len(), idx.files.len().min(50));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn state_nav_wraps_and_tracks_selection() {
        let root = tree("state");
        let mut s = FinderState::open(&root);
        assert_eq!(s.selected, 0);
        s.move_prev();
        assert_eq!(s.selected, s.filtered().len().saturating_sub(1).min(49));
        s.move_next();
        assert_eq!(s.selected, 0);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test finder::`
Expected: FAIL — module + `ignore` dep don't exist.

- [ ] **Step 3: Write the implementation**

`Cargo.toml` deps: `ignore = "0.4"`.

`src/palette.rs`: `fn fuzzy_score` → `pub fn fuzzy_score`.

`src/finder.rs`:

```rust
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 10_000;
const MAX_RESULTS: usize = 50;

pub struct FileIndex {
    pub root: PathBuf,
    pub files: Vec<PathBuf>, // relative paths
}

/// Walk `root` respecting .gitignore/.git-exclude, skipping hidden files
/// and .git itself, capped at MAX_FILES.
pub fn build(root: &Path) -> FileIndex {
    let mut files = Vec::new();
    for entry in ignore::WalkBuilder::new(root).build().flatten() {
        if files.len() >= MAX_FILES {
            break;
        }
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(root) {
            files.push(rel.to_path_buf());
        }
    }
    files.sort();
    FileIndex { root: root.to_path_buf(), files }
}

/// Fuzzy-filter relative paths, best score first, top MAX_RESULTS.
/// Empty query → all files in index order (fuzzy_score(“”, _) = 0).
pub fn filter<'a>(index: &'a FileIndex, query: &str) -> Vec<&'a Path> {
    let mut scored: Vec<(f64, &'a Path)> = index
        .files
        .iter()
        .filter_map(|p| {
            crate::palette::fuzzy_score(query, &p.to_string_lossy()).map(|s| (s, p.as_path()))
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(MAX_RESULTS).map(|(_, p)| p).collect()
}

/// Finder UI state — same shape as `palette::Palette`.
pub struct FinderState {
    pub index: FileIndex,
    pub query: String,
    pub selected: usize,
}

impl FinderState {
    pub fn open(root: &Path) -> FinderState {
        FinderState { index: build(root), query: String::new(), selected: 0 }
    }

    pub fn filtered(&self) -> Vec<&Path> {
        filter(&self.index, &self.query)
    }

    pub fn set_query(&mut self, q: String) {
        self.query = q;
        self.selected = 0;
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

    /// Absolute path of the current selection.
    pub fn selected_path(&self) -> Option<PathBuf> {
        self.filtered()
            .get(self.selected)
            .map(|p| self.index.root.join(p))
    }
}
```

`src/app.rs` — quote helper near `expand_tilde`:

```rust
/// Single-quote a path for embedding in a shell command line.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
```

`pub mod finder;` in `src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: all pass. If `.gitignore` isn't honored because the fixture isn't a repo, the `git init -q` in the test handles it (ignore's default `require_git(true)`).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/finder.rs src/lib.rs src/palette.rs src/app.rs
git commit -m "feat: finder.rs — ignore-aware file index + fuzzy filter state"
```

---

### Task 5: ai_commit.rs — OpenAI commit-message generation

**Files:**
- Create: `src/ai_commit.rs`
- Modify: `src/lib.rs` (`pub mod ai_commit;`)
- Modify: `Cargo.toml` (`ureq = "2"`, `serde_json = "1"`)

**Interfaces:**
- Consumes: `OPENAI_API_KEY` env var (read by the caller, Task 6/7)
- Produces: `generate_message(&str, &str) -> Result<String, String>`, `prompt(&str) -> String` (pure, testable)

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_contains_instructions_and_the_diff() {
        let p = prompt("diff --git a/x b/x");
        assert!(p.contains("conventional commit"));
        assert!(p.contains("diff --git a/x b/x"));
        assert!(p.contains("72"));
    }

    #[test]
    fn prompt_truncates_huge_diffs_on_char_boundary() {
        let big = "é".repeat(20_000); // multi-byte: must not split mid-char
        let p = prompt(&big);
        assert!(p.len() < 20_000);
        assert!(p.len() > 8_000); // instructions + ~8k of diff
    }

    #[test]
    fn parse_response_extracts_trimmed_content() {
        let body = r#"{"choices":[{"message":{"content":"  feat: add thing\n"}}]}"#;
        assert_eq!(parse_response(body).unwrap(), "feat: add thing");
    }

    #[test]
    fn parse_response_rejects_empty_or_missing_content() {
        assert!(parse_response(r#"{"choices":[]}"#).is_err());
        assert!(parse_response(r#"{"choices":[{"message":{"content":"  "}}]}"#).is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test ai_commit::`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Write the implementation**

`Cargo.toml` deps: `ureq = "2"`, `serde_json = "1"`.

`src/ai_commit.rs`:

```rust
const MODEL: &str = "gpt-4o-mini";
const MAX_DIFF_CHARS: usize = 8_000;

/// Generate a conventional-commit subject line for a staged diff.
/// Runs synchronously — callers must spawn it on a worker thread.
pub fn generate_message(diff: &str, api_key: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "model": MODEL,
        "messages": [{"role": "user", "content": prompt(diff)}],
        "max_tokens": 60,
        "temperature": 0.2,
    });
    let resp = ureq::post("https://api.openai.com/v1/chat/completions")
        .set("Authorization", &format!("Bearer {api_key}"))
        .send_json(body)
        .map_err(|e| format!("openai: {e}"))?;
    let text = resp.into_string().map_err(|e| format!("openai read: {e}"))?;
    parse_response(&text)
}

/// Extract the trimmed message content; Err on empty/missing.
fn parse_response(body: &str) -> Result<String, String> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("openai json: {e}"))?;
    json["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "openai: empty response".to_string())
}

/// Conventional-commit prompt; diff truncated to MAX_DIFF_CHARS on a
/// char boundary so multi-byte content never panics.
fn prompt(diff: &str) -> String {
    let truncated: String = diff.chars().take(MAX_DIFF_CHARS).collect();
    format!(
        "Write a conventional commit message subject line (type: summary, \
         \u{2264}72 chars, imperative, no body, no quotes) for this staged diff. \
         Output ONLY the subject line.\n\n{truncated}"
    )
}
```

(Note the `\u{2264}` escape — or write `≤` directly, both fine.)

`pub mod ai_commit;` in `src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: all pass (prompt + parse_response tests only — no network).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/ai_commit.rs src/lib.rs
git commit -m "feat: ai_commit.rs — OpenAI commit-message generation"
```

---

### Task 6: app.rs — modes, AppEvent, sidebar/finder/search state

**Files:**
- Modify: `src/app.rs`
- Modify: `src/input.rs` (placeholder arms for the new `InputMode` variants — real wiring is Task 7)
- Modify: `src/ui.rs` (placeholder arm — real rendering is Task 8)
- Modify: `src/main.rs` (`App::new` call — real events wired in Task 9)
- Modify: all `App::new(tx)` test call sites → `App::new(tx, tx2)` (~15 sites across app/input/ui/notify tests)

**Interfaces:**
- Consumes: `git::GitStatus`, `finder::FinderState`, `search::SearchState`, `ai_commit::generate_message`, `text_input::LineEdit`
- Produces: `InputMode::{Sidebar, Finder, Search}` + `LinePurpose::{CommitMsg, Search}`; `AppEvent::{GitStatus{root,status}, AiMessage(Result)}`; `App::new(pane_tx, app_tx)`; fields `app_tx`, `sidebar_sel`, `sidebar_branches`, `git_status`, `git_poll_in_flight`, `last_git_poll`, `finder`; methods `enter_sidebar`, `sidebar_items_len`, `sidebar_selected_file`, `sidebar_selected_branch`, `sidebar_move`, `open_finder`, `start_ai_commit`, `start_search`, `search_next`, `search_prev`, `exit_search`

- [ ] **Step 1: Write the failing tests** (append to `app.rs` test module)

```rust
#[test]
fn sidebar_items_len_follows_files_or_branches() {
    let mut app = app_with_one_project();
    app.git_status = Some(crate::git::GitStatus {
        branch: "main".into(),
        files: vec![
            crate::git::ChangedFile { status: 'M', path: "a".into() },
            crate::git::ChangedFile { status: '?', path: "b".into() },
        ],
    });
    assert_eq!(app.sidebar_items_len(), 2);
    app.sidebar_branches = true;
    app.sidebar_branch_list = vec!["main".into(), "dev".into()];
    assert_eq!(app.sidebar_items_len(), 2);
}

#[test]
fn sidebar_move_clamps_selection() {
    let mut app = app_with_one_project();
    app.git_status = Some(crate::git::GitStatus {
        branch: "m".into(),
        files: vec![crate::git::ChangedFile { status: 'M', path: "a".into() }],
    });
    app.sidebar_move(1);
    assert_eq!(app.sidebar_sel, 0); // wraps or clamps to len-1
    app.sidebar_move(-1);
    assert_eq!(app.sidebar_sel, 0);
}

#[test]
fn start_search_populates_pane_state_and_mode() {
    let mut app = app_with_one_project();
    app.spawn_pane(None);
    {
        let pane = &app.active_project().unwrap().panes[0];
        pane.parser.lock().unwrap().process(b"hello world\r\n");
    }
    app.start_search("hello");
    assert!(matches!(app.mode, InputMode::Search));
    let pane = &app.active_project().unwrap().panes[0];
    assert_eq!(pane.search.as_ref().unwrap().matches.len(), 1);
}

#[test]
fn start_search_with_no_matches_flashes_and_stays_normal() {
    let mut app = app_with_one_project();
    app.spawn_pane(None);
    app.start_search("zzz-no-match");
    assert!(matches!(app.mode, InputMode::Normal));
    assert!(app.status_msg.is_some());
}

#[test]
fn exit_search_clears_pane_state_and_mode() {
    let mut app = app_with_one_project();
    app.spawn_pane(None);
    {
        let pane = &app.active_project().unwrap().panes[0];
        pane.parser.lock().unwrap().process(b"needle\r\n");
    }
    app.start_search("needle");
    app.exit_search();
    assert!(matches!(app.mode, InputMode::Normal));
    assert!(app.active_project().unwrap().panes[0].search.is_none());
}
```

(Plus mechanical edits converting every `App::new(tx)` to `App::new(tx, tx2)` in tests — include a compile fix as part of this step.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test app::`
Expected: FAIL — variants/fields/methods don't exist.

- [ ] **Step 3: Write the implementation**

`src/app.rs` top:

```rust
pub enum InputMode {
    Normal,
    Leader,
    Palette,
    Sidebar,
    Finder,
    Search,
    LineInput(LinePurpose),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LinePurpose {
    AddProject,
    RenamePane,
    CommitMsg,
    Search,
}

pub enum AppEvent {
    GitStatus { root: PathBuf, status: Option<crate::git::GitStatus> },
    AiMessage(Result<String, String>),
}
```

`App` gains:

```rust
    pub app_tx: mpsc::Sender<AppEvent>,
    pub sidebar_sel: usize,
    pub sidebar_branches: bool,
    /// Cached branch list — populated on enter_sidebar/toggle so the
    /// renderer and len helpers never shell out to git per frame.
    pub sidebar_branch_list: Vec<String>,
    pub git_status: Option<crate::git::GitStatus>,
    pub git_poll_in_flight: bool,
    pub last_git_poll: Instant,
    pub finder: Option<crate::finder::FinderState>,
```

`App::new(events_tx, app_tx)` — init the new fields (`sidebar_sel: 0`, `sidebar_branches: false`, `sidebar_branch_list: vec![]`, `git_status: None`, `git_poll_in_flight: false`, `last_git_poll: Instant::now()`, `finder: None`).

Methods:

```rust
    pub fn active_root(&self) -> Option<PathBuf> {
        self.active_project().map(|p| p.root.clone())
    }

    pub fn enter_sidebar(&mut self) {
        if let Some(root) = self.active_root() {
            self.git_status = crate::git::status(&root); // instant refresh
            self.sidebar_branch_list = crate::git::branches(&root);
        }
        self.sidebar_sel = 0;
        self.sidebar_branches = false;
        self.mode = InputMode::Sidebar;
    }

    /// Toggle file↔branch list; refreshes the cached branch list.
    pub fn sidebar_toggle_branches(&mut self) {
        self.sidebar_branches = !self.sidebar_branches;
        self.sidebar_sel = 0;
        if self.sidebar_branches {
            if let Some(root) = self.active_root() {
                self.sidebar_branch_list = crate::git::branches(&root);
            }
        }
    }

    /// Rows in the active sidebar list (files or branches).
    pub fn sidebar_items_len(&self) -> usize {
        if self.sidebar_branches {
            self.sidebar_branch_list.len()
        } else {
            self.git_status.as_ref().map(|s| s.files.len()).unwrap_or(0)
        }
    }

    /// Move the sidebar selection, clamped to the current list.
    pub fn sidebar_move(&mut self, delta: i32) {
        let n = self.sidebar_items_len();
        if n == 0 {
            self.sidebar_sel = 0;
            return;
        }
        let cur = self.sidebar_sel as i32;
        self.sidebar_sel = (cur + delta).clamp(0, n as i32 - 1) as usize;
    }

    pub fn sidebar_selected_file(&self) -> Option<String> {
        self.git_status
            .as_ref()?
            .files
            .get(self.sidebar_sel)
            .map(|f| f.path.clone())
    }

    pub fn sidebar_selected_branch(&self) -> Option<String> {
        self.sidebar_branch_list.get(self.sidebar_sel).cloned()
    }

    pub fn open_finder(&mut self) {
        if let Some(root) = self.active_root() {
            self.finder = Some(crate::finder::FinderState::open(&root));
            self.mode = InputMode::Finder;
        }
    }

    /// Stage everything, snapshot the diff, and kick off the worker that
    /// posts AppEvent::AiMessage. Fast-fails inline (flash) when there's
    /// nothing to send or no API key.
    pub fn start_ai_commit(&mut self) {
        let Some(root) = self.active_root() else { return };
        if let Err(e) = crate::git::add_all(&root) {
            self.flash(format!("git add: {e}"));
            return;
        }
        let diff = match crate::git::staged_diff(&root) {
            Ok(d) => d,
            Err(e) => {
                self.flash(format!("git diff: {e}"));
                return;
            }
        };
        if diff.trim().is_empty() {
            self.flash("nothing to commit");
            return;
        }
        let key = match std::env::var("OPENAI_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                self.flash("OPENAI_API_KEY not set");
                return;
            }
        };
        let tx = self.app_tx.clone();
        std::thread::spawn(move || {
            let r = crate::ai_commit::generate_message(&diff, &key);
            let _ = tx.send(AppEvent::AiMessage(r));
        });
        self.flash("generating commit message…");
    }

    /// vim-style / — find matches in the focused pane, jump to the last.
    pub fn start_search(&mut self, query: &str) {
        let Some(project) = self.active_project_mut() else { return };
        let Some(pane) = project.active_pane_mut() else { return };
        let matches = pane
            .parser
            .lock()
            .map(|mut p| crate::search::find_matches(p.screen_mut(), query))
            .unwrap_or_default();
        if matches.is_empty() {
            self.flash(format!("no matches: {query}"));
            return;
        }
        let idx = matches.len() - 1;
        pane.search = Some(crate::search::SearchState {
            query: query.to_string(),
            matches,
            idx,
        });
        self.apply_search_scroll();
        self.mode = InputMode::Search;
    }

    pub fn search_next(&mut self) {
        if let Some(project) = self.active_project_mut() {
            if let Some(pane) = project.active_pane_mut() {
                if let Some(s) = pane.search.as_mut() {
                    s.next();
                }
            }
        }
        self.apply_search_scroll();
    }

    pub fn search_prev(&mut self) {
        if let Some(project) = self.active_project_mut() {
            if let Some(pane) = project.active_pane_mut() {
                if let Some(s) = pane.search.as_mut() {
                    s.prev();
                }
            }
        }
        self.apply_search_scroll();
    }

    /// Exit search mode AND clear the pane's search state (any exit path).
    pub fn exit_search(&mut self) {
        if let Some(project) = self.active_project_mut() {
            if let Some(pane) = project.active_pane_mut() {
                pane.search = None;
            }
        }
        self.mode = InputMode::Normal;
    }

    fn apply_search_scroll(&mut self) {
        if let Some(project) = self.active_project() {
            if let Some(pane) = project.active_pane() {
                if let Some(s) = &pane.search {
                    if let Some(m) = s.current() {
                        let total = pane
                            .parser
                            .lock()
                            .map(|mut p| crate::search::scrollback_len(p.screen_mut()))
                            .unwrap_or(0);
                        let off = crate::search::offset_for_row(total, m.row);
                        pane.set_scroll(off);
                    }
                }
            }
        }
    }
```

Placeholder arms so the tree compiles (same pattern as Phase 2 Task 5):

- `src/input.rs` `handle_key`: `InputMode::Sidebar | InputMode::Finder | InputMode::Search => {}` — and `LinePurpose::{CommitMsg, Search}` inside the Submit match → `LinePurpose::CommitMsg | LinePurpose::Search => {}`.
- `src/ui.rs` `draw_status_bar` match: `InputMode::Sidebar | InputMode::Finder | InputMode::Search => "…".to_string()` placeholder text.
- `src/main.rs`: `App::new(events_tx.clone(), app_tx)` — but `app_tx` doesn't exist yet there; for Task 6 create the channel in main WITHOUT wiring dispatch: `let (app_tx, _app_rx) = mpsc::channel::<rustterm::app::AppEvent>();` then `App::new(events_tx.clone(), app_tx.clone())`. Task 9 wires the receiver.
- Every `App::new(tx)` in tests → `let (tx, _rx) = mpsc::channel(); let (atx, _arx) = mpsc::channel(); App::new(tx, atx)` — a `two_channels()` test helper in each test module is cleaner; adjust per-file.

`pane.search` field comes from Task 3 — verify it's set before this task compiles.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: all pass (5 new app tests + suite).

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/input.rs src/ui.rs src/main.rs
git commit -m "feat: app state — sidebar/finder/search modes, AppEvent channel, AI/search orchestration"
```

---

### Task 7: input.rs — leader keys + new mode arms

**Files:**
- Modify: `src/input.rs`

**Interfaces:**
- Consumes: Task-6 App methods (`enter_sidebar`, `sidebar_move`, `sidebar_selected_file/branch`, `sidebar_branches`, `open_finder`, `start_ai_commit`, `start_search`, `search_next/prev`, `exit_search`), `crate::git::{switch, status}`, `crate::app::shell_quote`, `crate::finder::FinderState` methods, `LinePurpose::{CommitMsg, Search}`
- Produces: leader `g`/`G`/`f`/`/` map; `InputMode::{Sidebar, Finder, Search}` full handling; `LinePurpose::{CommitMsg, Search}` submit behavior

- [ ] **Step 1: Write the failing tests** (append to `input.rs` tests)

```rust
#[test]
fn leader_g_enters_sidebar_and_h_exits() {
    let mut app = app_with_one_project();
    handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    handle_key(&mut app, key(KeyCode::Char('g'), KeyModifiers::NONE));
    assert!(matches!(app.mode, InputMode::Sidebar));
    handle_key(&mut app, key(KeyCode::Char('h'), KeyModifiers::NONE));
    assert!(matches!(app.mode, InputMode::Normal));
}

#[test]
fn leader_capital_g_spawns_lazygit_pane() {
    let mut app = app_with_one_project();
    handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    handle_key(&mut app, key(KeyCode::Char('G'), KeyModifiers::SHIFT));
    let pane = &app.active_project().unwrap().panes[0];
    assert_eq!(pane.startup_command.as_deref(), Some("lazygit"));
}

#[test]
fn leader_f_opens_finder_and_esc_closes() {
    let mut app = app_with_one_project(); // root /tmp — has files
    handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    handle_key(&mut app, key(KeyCode::Char('f'), KeyModifiers::NONE));
    assert!(matches!(app.mode, InputMode::Finder));
    assert!(app.finder.is_some());
    handle_key(&mut app, key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, InputMode::Normal));
    assert!(app.finder.is_none());
}

#[test]
fn finder_enter_spawns_editor_pane() {
    let mut app = app_with_one_project();
    handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    handle_key(&mut app, key(KeyCode::Char('f'), KeyModifiers::NONE));
    handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
    let pane = &app.active_project().unwrap().panes[0];
    assert!(pane.startup_command.is_some());
    let cmd = pane.startup_command.clone().unwrap();
    assert!(cmd.contains("host") || cmd.contains('/'), "expected editor command, got {cmd}");
}

#[test]
fn leader_slash_opens_search_prompt_and_enter_searches() {
    let mut app = app_with_one_project();
    app.spawn_pane(None);
    {
        let pane = &app.active_project().unwrap().panes[0];
        pane.parser.lock().unwrap().process(b"searchable\r\n");
    }
    handle_key(&mut app, key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    handle_key(&mut app, key(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(matches!(app.mode, InputMode::LineInput(LinePurpose::Search)));
    for c in "searchable".chars() {
        handle_key(&mut app, key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    handle_key(&mut app, key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, InputMode::Search));
    handle_key(&mut app, key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, InputMode::Normal));
}

#[test]
fn search_mode_n_cycles_and_any_key_exits() {
    let mut app = app_with_one_project();
    app.spawn_pane(None);
    {
        let pane = &app.active_project().unwrap().panes[0];
        pane.parser.lock().unwrap().process(b"x x x\r\n");
    }
    app.start_search("x");
    handle_key(&mut app, key(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(matches!(app.mode, InputMode::Search));
    handle_key(&mut app, key(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(matches!(app.mode, InputMode::Normal));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test input::`
Expected: FAIL — leader keys/mode arms are placeholders.

- [ ] **Step 3: Write the implementation**

Leader arm — add inside the `match key.code` block (before `_ => {}`):

```rust
KeyCode::Char('g') => app.enter_sidebar(),
KeyCode::Char('G') => app.spawn_pane(Some("lazygit")),
KeyCode::Char('f') => app.open_finder(),
KeyCode::Char('/') => {
    app.line_input = Some(LineEdit::new());
    app.mode = InputMode::LineInput(LinePurpose::Search);
}
```

Note: `Char('G')` arrives with `SHIFT` modifier — match `KeyCode::Char('G')` (code alone, no modifier guard needed since `'g'` with shift IS `'G'`).

Replace the Task-6 placeholder arm with real arms:

```rust
InputMode::Sidebar => {
    if key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.mode = InputMode::Leader;
        return;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('g') => {
            app.mode = InputMode::Normal;
        }
        KeyCode::Char('j') | KeyCode::Down => app.sidebar_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.sidebar_move(-1),
        KeyCode::Char('b') => app.sidebar_toggle_branches(),
        KeyCode::Char('c') => app.start_ai_commit(),
        KeyCode::Enter => {
            if app.sidebar_branches {
                if let (Some(root), Some(branch)) =
                    (app.active_root(), app.sidebar_selected_branch())
                {
                    match crate::git::switch(&root, &branch) {
                        Ok(()) => {
                            app.git_status = crate::git::status(&root);
                            app.flash(format!("switched to {branch}"));
                        }
                        Err(e) => app.flash(format!("git switch: {e}")),
                    }
                }
            } else if let Some(file) = app.sidebar_selected_file() {
                let cmd = format!(
                    "git --no-pager diff --color=always -- {}",
                    crate::app::shell_quote(&file)
                );
                app.spawn_pane(Some(&cmd));
            }
        }
        _ => {}
    }
}

InputMode::Finder => {
    let Some(f) = app.finder.as_mut() else {
        app.mode = InputMode::Normal;
        return;
    };
    match key.code {
        KeyCode::Esc => {
            app.finder = None;
            app.mode = InputMode::Normal;
        }
        KeyCode::Enter => {
            if let Some(path) = f.selected_path() {
                let editor = std::env::var("VISUAL")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.is_empty()))
                    .unwrap_or_else(|| "nvim".to_string());
                let cmd = format!("{} {}", editor, crate::app::shell_quote(&path.to_string_lossy()));
                app.finder = None;
                app.mode = InputMode::Normal;
                app.spawn_pane(Some(&cmd));
            }
        }
        KeyCode::Up => f.move_prev(),
        KeyCode::Down => f.move_next(),
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => f.move_prev(),
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => f.move_next(),
        KeyCode::Backspace => {
            let mut q = f.query.clone();
            q.pop();
            f.set_query(q);
        }
        KeyCode::Char(c) => {
            let mut q = f.query.clone();
            q.push(c);
            f.set_query(q);
        }
        _ => {}
    }
}

InputMode::Search => match key.code {
    KeyCode::Char('n') => app.search_next(),
    KeyCode::Char('N') => app.search_prev(),
    _ => app.exit_search(), // Esc, Enter, and ANY other key exits
},
```

Editor fallback chain note: `nvim` is the default when neither env var is set; if nvim isn't installed the pane's shell reports it (same convention as lazygit). If you want a `vi` fallback instead, append `.unwrap_or_else(|| "vi".to_string())` — but spec says nvim-primary, so keep `nvim`.

`LinePurpose` Submit arms — extend the existing `match purpose`:

```rust
LinePurpose::CommitMsg => {
    let msg = text.trim().to_string();
    if let Some(root) = app.active_root() {
        if msg.is_empty() {
            app.flash("commit aborted: empty message");
        } else {
            match crate::git::commit(&root, &msg) {
                Ok(hash) => app.flash(format!("committed {hash}")),
                Err(e) => app.flash(format!("git commit: {e}")),
            }
        }
    }
    app.line_input = None;
    app.mode = InputMode::Normal;
}
LinePurpose::Search => {
    app.line_input = None;
    app.start_search(text.trim());
    // start_search sets mode=Search on match; ensure Normal on empty:
    if !matches!(app.mode, InputMode::Search) {
        app.mode = InputMode::Normal;
    }
}
```

Wait — `start_search` leaves mode unchanged on no-match (stays whatever it was — LineInput). The tail ensures Normal. But `start_search` flashes "no matches" — keep both lines.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: all pass incl. the 6 new input tests.

- [ ] **Step 5: Commit**

```bash
git add src/input.rs
git commit -m "feat: input — leader g/G/f//, sidebar nav, finder, search mode, commit-msg submit"
```

---

### Task 8: ui.rs — git section, finder overlay, search highlight, status titles

**Files:**
- Modify: `src/ui.rs`

**Interfaces:**
- Consumes: `app.git_status`, `app.sidebar_sel`, `app.sidebar_branches`, `app.finder`, `pane.search`, `InputMode::{Sidebar,Finder,Search}`, `PaneStatus`, `git::branches`
- Produces: rendered sidebar split, finder overlay, match highlight — the user-facing Phase 3 surfaces.

- [ ] **Step 1: Write the failing tests** (append to `ui.rs` tests)

```rust
#[test]
fn sidebar_shows_branch_and_changed_files() {
    let (tx, _rx) = mpsc::channel();
    let (atx, _arx) = mpsc::channel();
    let mut app = App::new(tx, atx);
    app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
    app.git_status = Some(crate::git::GitStatus {
        branch: "main".into(),
        files: vec![crate::git::ChangedFile { status: 'M', path: "src/app.rs".into() }],
    });
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    assert!(buffer_contains(buf, "main"));
    assert!(buffer_contains(buf, "M src/app.rs"));
}

#[test]
fn finder_overlay_lists_files() {
    let (tx, _rx) = mpsc::channel();
    let (atx, _arx) = mpsc::channel();
    let mut app = App::new(tx, atx);
    app.projects.push(Project::new("demo".into(), PathBuf::from("/tmp")));
    app.mode = InputMode::Finder;
    app.finder = Some(crate::finder::FinderState::open(&PathBuf::from("/tmp")));
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, &app)).unwrap();
    assert!(buffer_contains(terminal.backend().buffer(), "Find file"));
}

#[test]
fn exited_pane_title_shows_the_code() {
    let (tx, _rx) = mpsc::channel();
    let (atx, _arx) = mpsc::channel();
    let mut app = App::new(tx, atx);
    let mut project = Project::new("demo".into(), PathBuf::from("/tmp"));
    let (ptx, _prx) = mpsc::channel();
    let mut pane = Pane::spawn(1, "work".into(), 24, 80, None, ptx, None).unwrap();
    pane.status = crate::pane::PaneStatus::Exited(3);
    project.panes.push(pane);
    app.projects.push(project);
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, &app)).unwrap();
    assert!(buffer_contains(terminal.backend().buffer(), "[exited 3]"));
}
```

Note: existing `App::new(tx)` calls in ui tests must become `App::new(tx, atx)` — Task 6 already did the mechanical pass; these tests assume it.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test ui::`
Expected: FAIL — git section/finder overlay not rendered.

- [ ] **Step 3: Write the implementation**

`draw_sidebar` — split the sidebar rect into projects + git sections:

```rust
fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    // Projects get their rows + border, capped at 10 so git always shows.
    let project_h = (app.projects.len() as u16 + 2).min(10).min(area.height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(project_h), Constraint::Min(5)])
        .split(area);
    // … existing project List rendered into chunks[0] (unchanged code) …
    draw_git_section(frame, app, chunks[1]);
}

fn draw_git_section(frame: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.mode, InputMode::Sidebar);
    let border = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let title = app
        .git_status
        .as_ref()
        .map(|s| format!("⎇ {}", s.branch))
        .unwrap_or_else(|| "git".to_string());
    let block = Block::default().borders(Borders::ALL).border_style(border).title(title);

    let items: Vec<ListItem> = if app.sidebar_branches {
        app.sidebar_branch_list
            .iter()
            .enumerate()
            .map(|(i, b)| sidebar_row(b.clone(), i, app.sidebar_sel, focused))
            .collect()
    } else {
        match app.git_status.as_ref() {
            Some(s) => s
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| sidebar_row(format!("{} {}", f.status, f.path), i, app.sidebar_sel, focused))
                .collect(),
            None => vec![ListItem::new("  no repo")],
        }
    };
    frame.render_widget(List::new(items).block(block), area);
}

fn sidebar_row(text: String, i: usize, sel: usize, focused: bool) -> ListItem<'static> {
    let style = if focused && i == sel {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    ListItem::new(text).style(style)
}
```

`draw_finder` — mirror `draw_palette` (extract the shared rect math):

```rust
fn centered_rect(area: Rect, items: usize) -> Rect {
    let width = (area.width * 3 / 5).clamp(30.min(area.width), area.width);
    let y = area.height / 6;
    let height = (items as u16 + 3).max(6).min(area.height.saturating_sub(y));
    Rect { x: (area.width - width) / 2, y, width, height }
}
```

Refactor `draw_palette` to use `centered_rect(area, pal.filtered().len())`, then:

```rust
fn draw_finder(frame: &mut Frame, app: &App) {
    let Some(f) = app.finder.as_ref() else { return };
    let filtered = f.filtered();
    let rect = centered_rect(frame.area(), filtered.len());
    frame.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title("Find file");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);
    frame.render_widget(Paragraph::new(format!("> {}", f.query)), rows[0]);
    let visible = rows[1].height as usize;
    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .take(visible)
        .map(|(i, p)| {
            let style = if i == f.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(p.to_string_lossy().to_string()).style(style)
        })
        .collect();
    frame.render_widget(List::new(items), rows[1]);
}
```

Hook it in `draw` after the palette arm:

```rust
if matches!(app.mode, InputMode::Finder) {
    draw_finder(frame, app);
}
```

Search highlight — in `draw_panes`, after `frame.render_widget(widget, *rect)` per pane, add:

```rust
highlight_matches(frame, pane, *rect);
```

```rust
fn highlight_matches(frame: &mut Frame, pane: &Pane, rect: Rect) {
    let Some(s) = &pane.search else { return };
    let (total, offset, height) = match pane.parser.lock() {
        Ok(mut p) => {
            let sc = p.screen_mut();
            (
                crate::search::scrollback_len(sc),
                sc.scrollback(),
                sc.size().0 as usize,
            )
        }
        Err(_) => return,
    };
    // Match row → view row: visible row r shows grid line (total - offset + r).
    for m in &s.matches {
        let view_row = m.row as i64 - (total as i64 - offset as i64);
        if view_row < 0 || view_row >= height as i64 {
            continue;
        }
        let y = rect.y + 1 + view_row as u16; // +1 for the border
        for dx in 0..m.len {
            let x = rect.x + 1 + (m.col + dx) as u16;
            if x < rect.x + rect.width - 1 && y < rect.y + rect.height - 1 {
                frame.buffer_mut()[(x, y)].set_modifier(Modifier::REVERSED);
            }
        }
    }
}
```

(`frame.buffer_mut()` — ratatui 0.30: `Frame::buffer_mut() -> &mut Buffer`. Cells support `.set_modifier` via `Cell::set_style`? Verify: `buffer[(x,y)]` returns `&Cell`; use `frame.buffer_mut()[(x,y)].set_style(Style::default().add_modifier(Modifier::REVERSED))` — but that overwrites existing style. Prefer `cell.modifier |= Modifier::REVERSED`? `Cell` fields: `symbol`, `fg`, `bg`, `modifier`, `skip`. In ratatui 0.30 `Cell::modifier` is a pub field → `frame.buffer_mut()[(x, y)].modifier |= Modifier::REVERSED;` Try that first; fall back to set_style if the field isn't pub.)

Also update `draw_status_bar`'s mode match — replace the placeholder:

```rust
InputMode::Sidebar => "j/k move  enter open  b branches  c ai-commit  esc back".to_string(),
InputMode::Finder => "type to filter  ↑/↓ move  enter open  esc cancel".to_string(),
InputMode::Search => "n next  N prev  any key to exit".to_string(),
```

And `LinePurpose::{CommitMsg, Search}` labels in the LineInput arm:

```rust
crate::app::LinePurpose::CommitMsg => "Commit message: ",
crate::app::LinePurpose::Search => "Search: ",
```

(`AddProject`/`RenamePane` unchanged; add the two new variants to the existing `match purpose` — it's `#[derive(PartialEq)]` so match must be exhaustive.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: all pass incl. 3 new ui tests.

- [ ] **Step 5: Commit**

```bash
git add src/ui.rs
git commit -m "feat: ui — git sidebar section, finder overlay, search highlight, exited titles"
```

---

### Task 9: main.rs — AppEvent channel + git poll + AI dispatch

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `AppEvent`, `git::status`, `app.app_tx`, `app.git_poll_in_flight`, `app.last_git_poll`, `LinePurpose::CommitMsg` flow
- Produces: the wired event loop — live git status + AI message delivery.

- [ ] **Step 1: Refactor**

Replace the Task-6 placeholder channel with real wiring. In `main()`:

```rust
let (events_tx, events_rx) = mpsc::channel::<PaneEvent>();
let (app_tx, app_rx) = mpsc::channel::<rustterm::app::AppEvent>();
let mut app = App::new(events_tx.clone(), app_tx.clone());
```

Pass `&app_rx` into `run` (add a param — update the signature).

In `run()`, after the existing `PaneEvent` drain:

```rust
while let Ok(event) = app_rx.try_recv() {
    match event {
        rustterm::app::AppEvent::GitStatus { root, status } => {
            app.git_poll_in_flight = false;
            if app.active_root().as_ref() == Some(&root) {
                app.git_status = status;
            }
        }
        rustterm::app::AppEvent::AiMessage(result) => {
            match result {
                Ok(msg) => {
                    app.line_input = Some(crate::text_input::LineEdit::from_str(&msg));
                    app.mode = rustterm::app::InputMode::LineInput(
                        rustterm::app::LinePurpose::CommitMsg,
                    );
                }
                Err(e) => app.flash(format!("ai commit: {e}")),
            }
        }
    }
}
```

Git poll — after the watcher poll block (sender cloned from the app; the
only new `run` param is `app_rx`):

```rust
if app.last_git_poll.elapsed() >= Duration::from_secs(1) && !app.git_poll_in_flight {
    app.last_git_poll = Instant::now();
    if let Some(root) = app.active_root() {
        app.git_poll_in_flight = true;
        let tx = app.app_tx.clone();
        std::thread::spawn(move || {
            let status = rustterm::git::status(&root);
            let _ = tx.send(rustterm::app::AppEvent::GitStatus { root, status });
        });
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test`
Expected: all pass (main.rs has no unit tests — verified by the suite + build).

- [ ] **Step 3: Build + smoke checklist**

Run: `cargo build` — zero warnings.
Manual smoke (documented for the human): `rustterm ~/projects/RustTerm` → sidebar shows `⎇ master` + changed files; `Ctrl+A g` focuses section, `j/k` moves, `b` shows branches, `Enter` on a file opens its diff pane; `Ctrl+A G` opens lazygit; `Ctrl+A f` finds files, Enter opens nvim; `Ctrl+A /` searches pane output; sidebar `c` runs the AI commit flow (needs `OPENAI_API_KEY`).

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: main — AppEvent channel, 1s git-status poll, AI message delivery"
```

---

## Self-review notes (resolved during planning)

- `App::new` signature change ripples to ~15 test call sites — Task 6 owns the mechanical pass.
- `contents()` includes scrollback rows in vt100 0.16 — Task 3 has a fallback note if it doesn't.
- `Cell::modifier` pub-field access — Task 8 has a `set_style` fallback note.
- `enter_sidebar` does a synchronous `git::status` + `git::branches` for instant data — poll owns subsequent refreshes.
- Branch list is CACHED (`sidebar_branch_list`) — never call `git::branches` from the render path; it would spawn a git subprocess per frame.
- Search exit clears `pane.search` on ALL paths (Esc, Enter, any-key) — consistent single `exit_search`.
- `shell_quote` lives in `app.rs` beside `expand_tilde` — shared by sidebar-diff and finder-open commands.
