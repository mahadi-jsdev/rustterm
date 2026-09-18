# RustTerm Phase 2: Multi-Project, Agent Awareness, Command Palette — Design

## Context

Phase 1 delivered the terminal-multiplexer kernel: real PTYs rendered through
`vt100` + `tui-term`, a `Projects → Panes` model, leader-key navigation, and a
synchronous per-frame layout/resize pass. Phase 2 ports the parts of FLAME
(`ai-terminal-agent`, the Tauri+React app this project reimplements) that make
it an *agent* terminal rather than a plain multiplexer:

- **Multi-project support.** The `[`/`]` project switching and sidebar exist
  but are dead code — `main.rs` hardcodes exactly one project. Phase 2 makes
  the Projects layer real.
- **Agent awareness.** A port of FLAME's `taskWatcher.ts`: detect when a pane
  is running a known agent CLI, when a long task finishes, and when a pane is
  blocked on a yes/no or permission prompt — surfaced as in-app badges and
  desktop notifications.
- **Command palette.** A fuzzy-searchable command overlay (the discoverable
  front door to every action), plus closed-pane history ("reopen last closed
  pane"), pane rename, and one-key agent launching.

The flat `Projects → Panes` model stays — no workspace layer is introduced.
Persistence remains out of scope: everything is still ephemeral per process.

## Non-goals for Phase 2

- Git panel, file finder, code editor, LSP (Phase 3+)
- Session persistence, workspace templates, settings UI
- The Phase 1 `Pane` `Running`/`Failed` refactor (exit codes, zombie reaping)
  — stays on the deferred list; spawn failures now surface via a status flash
  instead of silently doing nothing, which covers the visible part of the gap
- Mouse support, scrollback UI, themes

## Architecture

New dependencies: `regex = "1"` (prompt-pattern and agent detection — ported
verbatim from FLAME), `notify-rust = "4"` (D-Bus desktop notifications;
failures are best-effort/ignored). No other crates — fuzzy matching and `~`
expansion are small hand-rolled functions.

```
watcher.rs     — NEW. Per-pane pure state machine (port of taskWatcher.ts).
                 Fed screen-text snapshots + input bytes; emits
                 Done{command} / Waiting(bool) / Command(String) events.
agents.rs      — NEW. Agent registry: known CLIs (claude, codex, devin,
                 gemini, aider, cursor-agent, opencode, copilot) with colors
                 ported from AGENT_COLORS. detect(cmd) for typed commands,
                 installed() for a which-style PATH scan.
notify.rs      — NEW. Routes watcher events → pane badges + notify-rust
                 desktop notifications, with a port of shouldNotify plus a
                 per-pane cooldown.
palette.rs     — NEW. Command registry (rebuilt on open), fuzzy_score()
                 (port of fuzzy.ts), palette UI state.
text_input.rs  — NEW. Shared single-line edit buffer (chars, backspace,
                 Ctrl+U) backing the add-project and rename prompts.
app.rs         — InputMode gains Palette and LineInput(LinePurpose);
                 closed-pane history; add_project/dedupe; status flash.
pane.rs        — Gains startup_command, cwd, color, agent_tagged, running,
                 waiting, attention fields + a Watcher.
project.rs     — Unchanged.
input.rs       — New leader keys (`:` palette, `c` add project) + the two
                 new input modes.
ui.rs          — Badge rendering (borders, sidebar glyphs, status bar),
                 palette overlay, line-input prompt, status flash.
main.rs        — CLI args → initial projects; the 400ms watcher poll tick.
pty.rs, keys.rs, layout.rs — unchanged.
```

## Multi-project

- `rustterm [dir…]`: each arg is canonicalized; non-directories are skipped
  with a warning to stderr before the TUI starts. Zero args = current-dir
  behavior (unchanged). Only the first project spawns a pane at startup.
- **Empty-project auto-spawn:** whenever a project becomes active and has no
  panes, one pane is spawned in its root. A single `App::ensure_active_pane`
  helper (it needs `alloc_pane_id` + `events_tx`, so it lives on `App`) is
  called after every project activation: `[`/`]` switching, palette
  `SwitchProject`, and `add_project`.
- `App::add_project(root: PathBuf)` canonicalizes, dedupes (an already-open
  root just switches to it), names the project after the directory basename
  (fallback `"project"`), pushes it, makes it active, and spawns a pane.
- Leader `c` opens `InputMode::LineInput(AddProject)`: a prompt line with
  `~` expansion, Enter validates the directory exists (invalid → inline
  error text in the prompt, stays open), Esc cancels.
- Closing a project (palette only): kills its panes, removes it, activates a
  neighbor. Closing the *last* project is refused via status flash — a
  project-less RustTerm has no useful state.

## Watcher (screen-poll design)

`Pane` owns a `Watcher`. Two feeds:

- **Input tap** — in `handle_key`'s Normal branch, the same bytes written to
  the PTY are passed to `watcher.on_input(&bytes) -> Option<String>`:
  printable chars accumulate into `input_buf`, `\r` commits and returns the
  line as the new `last_command` (this returned line *is* the `Command`
  event), `\x7f` pops, `\x03` clears. Any input also clears `waiting`,
  emitting `Waiting(false)` if it was set — FLAME clears on both new output
  and keystrokes. Leader-mode keys never reach this path (they don't reach
  the PTY either).
- **Screen poll** — the main loop already ticks at 50ms; every 400ms
  (`POLL_MS`) it iterates all panes, locks each parser (`Err` → skip, no
  unwrap — the mutex-poison gap stays a known limitation but the poll adds
  no new panic site), takes `screen().contents()`, and calls
  `watcher.update(now, &text) -> Vec<WatchEvent>`. Exited panes are skipped.

State machine (thresholds ported from FLAME: `BUSY_MIN = 10s`,
`QUIET = 8s`, `WAITING_QUIET = 1.5s`):

- Screen text changed (hash compare): `last_change = now`,
  `busy_since = busy_since.or(now)`, `waiting` clears (emits
  `Waiting(false)` if it was set), `output_tail` updated (last ~2000 chars),
  `running = true`.
- Quiet ≥ `WAITING_QUIET` and `looks_like_prompt(output_tail)` → `waiting`,
  emits `Waiting(true)`.
- Quiet ≥ `QUIET` and busy duration ≥ `BUSY_MIN` → emits
  `Done(last_command)`, `busy_since` reset.
- `running` is recomputed each poll: `now - last_change < 1.5s`.

`looks_like_prompt` ports FLAME's `PROMPT_PATTERNS` verbatim (y/n
phrasings, "enter to select", "esc to cancel", aider's (Y)es/(N)o, gemini's
"allow execution", etc.) as `regex::Regex` in a `OnceLock<Vec<Regex>>`.
Matching runs against parsed screen text, so no ANSI-stripping pass is
needed (FLAME needs one; the vt100 parser already did the work here).

All methods take `now: Instant` as a parameter — unit tests inject times
and assert on emitted events; no sleeps, no threads.

## Agents

`agents.rs` holds a static `AGENTS` table — name + color, ported from
`AGENT_COLORS` (claude `#ffb238`, codex `#8bb4e8`, devin `#ff6b52`, gemini
`#7ec9c9`, aider `#ffcb6b`, cursor-agent `#c9a877`, opencode `#e0894a`,
copilot `#8c8172`).

- `detect(command: &str) -> Option<&'static AgentSpec>` — the same
  `\b(claude|codex|devin|gemini|aider|cursor-agent|opencode|copilot)\b`
  regex (case-insensitive) as FLAME.
- `installed() -> Vec<&'static AgentSpec>` — scans each `PATH` entry for an
  executable file named after the agent binary.
- **Auto-tag** (port of `maybeAutoTag`): on a `Command` event, if the pane
  still has its default `pane-N` title, no color, and `!agent_tagged`, set
  `title = agent.name` and `color = agent.color`; `agent_tagged` latches
  true so it happens at most once per pane.

## Notifications and badges

`notify::dispatch(app, pane_id, event)` handles each `WatchEvent`:

- `Waiting(true)` → `pane.waiting = true` + desktop
  `"{title} needs your input"`.
- `Done(cmd)` → `pane.attention = true` (held until the pane is focused) +
  desktop `"{agent} finished"` or `"Terminal task finished"`, body = the
  command.
- `Command(cmd)` → auto-tag (above). `Waiting(false)` clears the badge.

`should_notify(pane_is_focused)` ports FLAME's rule minus the
window-focus check (a TUI can't detect outer-terminal focus portably):
skip desktop only when the emitting pane is the active pane of the active
project — the one the user is presumably watching. Badges always update.
One cooldown: ≥30s between desktop notifications per pane
(`pane.last_notify_at`), any kind.

Badge rendering in `ui.rs`:

- Pane border color priority: waiting (yellow) → focused (cyan) → agent
  color → default. `pane.title` gains a `●` suffix while waiting, `▸` while
  `running`, `!` while `attention`.
- Sidebar project row: `●` if any of its panes is waiting, `!` if any has
  unseen `attention`.
- Status bar shows the focused pane's agent/state when relevant.
- Badges clear when the pane becomes the active pane of the active project.

## Command palette

Leader `:` opens `InputMode::Palette` — a centered overlay (~60% width):
query line, up to 10 visible matches, selected row highlighted.
Up/Down/Ctrl-n/Ctrl-p navigate, Enter executes, Esc closes. Query edits
re-filter through `fuzzy_score` (direct port of `fuzzy.ts`: in-order
subsequence match, +consecutive-run bonus, +word/path-boundary bonus,
length penalty; `None` = no match). Commands are built once at open and
resolved by id at execute time.

Command set:

| Command | Action |
|---|---|
| New pane / Close pane / Next pane / Previous pane | Same code paths as leader `n`/`x`/`h`/`l` |
| Jump to pane N | One entry per pane in the active project |
| Increase split / Decrease split | Same as `+`/`-` |
| Add project… | Enters `LineInput(AddProject)` |
| Switch to project: {name} | One per project; ensure-pane on arrival |
| Close project | Refused on last project (status flash) |
| Reopen last closed pane | Hidden when history empty |
| Run {agent} | One per `installed()` agent |
| Rename pane… | Enters `LineInput(RenamePane)` |
| Quit | Same as `q` |

**Reopen history:** `App.closed_panes: VecDeque<ClosedPane>` capped at 5
(FLAME's cap). `ClosedPane { title, cwd, startup_command, color,
project }` is captured on close; reopening respawns the pane in the
originating project (falling back to the active project if it's gone).

**Startup commands:** `Pane::spawn` gains `startup_command: Option<&str>`;
when set, the command + `\r` is written to the PTY immediately after spawn
(PTY input is buffered — the shell reads it when ready; FLAME does the
same). `Run {agent}` is just a pane spawned with `startup_command = binary`.

`InputMode` becomes `Normal | Leader | Palette | LineInput(LinePurpose)`
with `LinePurpose::{AddProject, RenamePane}` — `LineInput` renders a
bottom-line prompt sharing the `text_input.rs` buffer.

## Error handling

New `app.status_msg: Option<(String, Instant)>` — a 3-second status-bar
flash shown instead of the normal hint line. Covers: pane spawn failure
(fixing the silent `n` no-op), invalid project path, close-last-project
refusal, agent binary vanished between PATH scan and exec. `notify-rust`
failures are ignored. Watcher and palette code paths are pure and panic-free;
the reader thread is untouched.

## Testing

- `watcher.rs` — injected-`Instant` tests: busy→done timing, prompt→waiting,
  waiting cleared by input/output, input-line tracking, every ported prompt
  pattern.
- `agents.rs` — detection regex cases (including non-matches like
  `declared`), `installed()` against a temp PATH.
- `palette.rs` — `fuzzy_score` ordering and boundary cases; command-list
  construction against a canned `App`.
- `notify.rs` — `should_notify` truth table; cooldown logic (pure, Instant
  injected).
- `app.rs`/`input.rs` — `add_project` dedupe, switch-to-empty-project
  spawns, close-project refusal, reopen round-trip, leader `:`/`c` entering
  the new modes, LineInput editing + confirm/cancel.
- `pane.rs` — `startup_command` integration test (spawn with a command,
  assert it lands on screen — same real-PTY pattern as Phase 1).
- `ui.rs` stays untested, matching the Phase 1 convention.

## Deferred (unchanged from Phase 1 handoff)

`Pane` `Running`/`Failed` enum + real exit codes + zombie reaping; row-split
keybinding; `Ctrl+A Ctrl+A` passthrough; Alt/Home/End/Delete/PgUp/PgDn/F-key
translation; 5+-pane grid gutters; reader-thread panic catching (still a
documented known limitation).
