# RustTerm Phase 3 — Workspace Tooling

**Status:** approved design, pre-implementation
**Date:** 2026-09-19
**Scope:** git sidebar section + lazygit keymap, file finder → nvim, AI auto-commit (OpenAI), pane lifecycle hardening, terminal search

## Overview

Phase 3 turns RustTerm from a terminal multiplexer into a workspace tool:

- **Git sidebar section** — branch + changed files always visible under the project list; a focusable panel for diffs and branch switching
- **File finder** — fuzzy picker over the project's files; opens in `nvim` (user's LSP config — no editor work needed)
- **AI auto-commit** — OpenAI-generated commit messages, review-before-commit
- **Pane lifecycle** — `Running`/`Exited(code)`/`Failed` states; exit codes captured, zombies reaped
- **Terminal search** — vim-style `/` search through pane scrollback

## Module layout

| File | Responsibility |
|---|---|
| `git.rs` | Shell-out git queries/actions: porcelain status parse, branch list/switch, add-all, staged diff, commit. Pure string in/out — no process state. |
| `finder.rs` | `ignore`-crate file walk + fuzzy filtering (reuses `palette::fuzzy_score`). |
| `ai_commit.rs` | OpenAI chat-completions call via `ureq`; prompt construction pure. |
| `search.rs` | Match engine over vt100 screen text incl. scrollback; match list + index math. |
| modified `app.rs` | `InputMode::{Sidebar, Finder, Search}` + `LinePurpose::{CommitMsg, Search}`, git status cache, sidebar selection state, finder state, second event channel |
| modified `pane.rs` | `PaneStatus` replaces `exited`, `reap()` |
| modified `input.rs` | leader `g`/`G`/`f`/`/`, sidebar/finder/search mode keys, commit-message submit |
| modified `ui.rs` | sidebar git section + focus highlight, finder overlay, match highlight, status title `[exited N]` |
| modified `main.rs` | `AppEvent` channel, 1s git-status poll thread spawn, dispatch |

New dependencies: `ignore = "0.4"`, `ureq = "2"`, `serde_json = "1"`.

## Events

`PaneEvent` stays as-is. A second channel carries app-level async results:

```rust
pub enum AppEvent {
    GitStatus { root: PathBuf, status: Option<git::GitStatus> },
    AiMessage(Result<String, String>),
}
```

`App::new(pane_tx, app_tx)` — signature gains the sender (all `App::new(tx)` call sites updated; tests pass a second throwaway channel).

- **Git poll**: main loop every 1s spawns a worker thread → `git::status(active_root)` → posts `AppEvent::GitStatus`. A `git_poll_in_flight: bool` on App prevents thread pile-up; the result applies only when the active project's root still matches the polled `root`.
- **AI**: sidebar `c` / palette → `git::add_all` + `git::staged_diff` (sync, fast) → empty diff → flash "nothing staged" → else spawn worker → `ai_commit::generate_message` → `AppEvent::AiMessage` → on `Ok` open `LineInput(CommitMsg)` prefilled with the message; on `Err` flash it.

## Git sidebar section

Sidebar splits vertically: project list (existing, top, capped ~10 rows) + git section (bottom, gets the remaining height, titled `⎇ <branch>` or `git` when no repo). Rows: `<status-letter> <path>` — porcelain XY collapsed to one display letter (`M`, `A`, `D`, `R`, `?`, `U`).

```rust
pub struct GitStatus { pub branch: String, pub files: Vec<ChangedFile> }
pub struct ChangedFile { pub status: char, pub path: String }
pub fn status(root: &Path) -> Option<GitStatus>        // None = not a repo / git missing
pub fn branches(root: &Path) -> Vec<String>            // local, current first
pub fn switch(root: &Path, branch: &str) -> Result<(), String>
pub fn add_all(root: &Path) -> Result<(), String>
pub fn staged_diff(root: &Path) -> Result<String, String>
pub fn commit(root: &Path, msg: &str) -> Result<String, String> // Ok(short-hash)
```

Branch via `symbolic-ref --short HEAD`, detached → `rev-parse --short HEAD`.

### Focus: `InputMode::Sidebar`

`leader g` enters (`g` is unassigned today, as are `G`, `f`, `/`); the section border goes cyan (same language as focused panes) and the selected row gets REVERSED (same as palette). Keys:

- `j`/`k`/`↑`/`↓` — move selection
- `b` — toggle file list ↔ branch list
- `Enter` — file → `spawn_pane(Some("git --no-pager diff --color=always -- <quoted-file>"))` (single-quote shell quoting, same rule as the finder); branch → `git::switch` → flash result + status refresh
- `c` — AI commit flow (below)
- `g`/`Esc`/`h` — back to Normal; `Ctrl+A` still enters Leader (works from every non-text-entry mode)

App state: `sidebar_sel: usize`, `sidebar_branches: bool`, `git_status: Option<GitStatus>` (cached from poll).

## Lazygit

`leader G` → `spawn_pane(Some("lazygit"))` in the active project root. If lazygit isn't installed the shell reports it in-pane (same as palette `Run` entries — no special-casing).

## File finder

`leader f` → `InputMode::Finder` + `app.finder = Some(FinderState::open(root))`:

```rust
pub struct FileIndex { root: PathBuf, files: Vec<PathBuf> } // relative paths, capped at 10_000
pub fn build(root: &Path) -> FileIndex    // ignore::WalkBuilder — respects .gitignore, skips .git/hidden
pub fn filter<'a>(index: &'a FileIndex, query: &str) -> Vec<&'a Path>  // palette::fuzzy_score (promoted to pub), top-50
```

Overlay reuses the palette's centered-rect math + rendering shape (query line + filtered list + REVERSED selection). Keys identical to palette (type/backspace/↑↓/Ctrl-n-p/Enter/Esc).

`Enter` → spawn pane in the file's project: `spawn_pane(Some("<editor> <quoted-path>"))`. Editor chain: `$VISUAL` → `$EDITOR` → `nvim` → `vi` (first set/in-PATH wins). Paths shell-quoted single-quote style (`'` → `'\''`).

## AI auto-commit

```rust
pub fn generate_message(diff: &str, api_key: &str) -> Result<String, String>
// POST https://api.openai.com/v1/chat/completions, model gpt-4o-mini
// prompt: conventional-commit subject line ≤72 chars, diff truncated at ~8k chars
```

Key from `OPENAI_API_KEY` env var — missing → flash "OPENAI_API_KEY not set". Flow (sidebar `c` or palette `Git: AI commit`):

1. `git::add_all` → `git::staged_diff`; empty → flash "nothing to commit"
2. Worker thread → `AppEvent::AiMessage`
3. `Ok(msg)` → `LineInput(CommitMsg)` prefilled with `msg` — user edits or accepts
4. Enter → `git::commit` in the **active project root** → flash `committed <short-hash>` / error text; Esc cancels

## Pane lifecycle

```rust
pub enum PaneStatus { Running, Exited(i32), Failed(String) }
```

- `pane.exited: Option<String>` → `pane.status: PaneStatus` (all `exited.is_some()` call sites updated: ui title, watcher poll skip, notify)
- On `PaneEvent::Exited(id)` → `pane.reap()`: `child.wait()` (the PTY EOF'd — returns promptly) → `Exited(status.code().unwrap_or(-1))`; reaps the zombie
- Titles: `[exited 1]`, `[exited]` on -1; `Failed` shows `[failed]` (spawn-time failures already flash; `Failed` covers future runtime marks)
- `is_running`/`running` badge unaffected; watcher poll skips `status != Running`

## Terminal search

`leader /` → `LineInput(Search)` → submit → `InputMode::Search` + `pane.search = Some(state)`:

```rust
pub struct SearchMatch { pub row: usize, pub col: usize, pub len: usize }
pub fn find_matches(screen: &vt100::Screen, query: &str) -> Vec<SearchMatch>
// rows indexed over scrollback+visible; case-insensitive substring
```

- On commit: jump to the **last** match (most recent output); `n` → next (wraps), `N`/`Shift-n` → prev — `set_scrollback` so the match row is visible
- `InputMode::Search`: `n`/`N`/`Esc`/`Enter` handled; Esc exits (match highlight cleared); **any other key exits to Normal without being forwarded** (no surprise bytes to the PTY)
- Highlight: ui.rs post-render pass — matched cells inside the pane's inner rect get `Modifier::REVERSED` (translate match row → view row via `screen().scrollback()`)
- Search is per-pane, stored on the pane; switching panes keeps each pane's state

## Error handling

Same status-flash channel as Phase 2: git command stderr → flash, API errors → flash, `git switch` failure → flash. Git poll failures → `GitStatus(None)` → section shows "no repo". Finder on a non-repo/huge dir → capped at 10k, note in overlay title. All worker failures post through `AppEvent` — never panic the loop.

## Testing

- `git.rs`: fixture repo in `temp_dir` (`git init` + commits + dirty files) — status parse, branch list, switch round-trip, staged diff, commit hash
- `finder.rs`: fixture tree incl. `.gitignore` + hidden + `.git` dir — filter order/exclusions
- `ai_commit.rs`: prompt construction + diff truncation pure tests (no network)
- `search.rs`: synthetic `vt100::Parser` feeds → match rows/cols, scrollback mapping
- `input.rs`: sidebar nav/enter/b-toggle routing, finder Enter → spawn, search mode transitions, commit-msg submit → commit
- `ui.rs`: git section render, focus border, `[exited N]` title, match highlight cells, finder overlay
- Lifecycle: `reap()` on a spawned `sh -c 'exit 3'` → `Exited(3)`

## Deferred

Editor/LSP (nvim owns it), git staging/hunk-level actions, AI commit via agent CLIs, session persistence, workspaces, themes, `Ctrl+A` passthrough, row-split keys.
