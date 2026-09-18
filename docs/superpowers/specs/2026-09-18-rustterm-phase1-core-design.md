# RustTerm Phase 1: Core Terminal Multiplexer — Design

## Context

RustTerm is a from-scratch reimplementation of FLAME's core value as a pure
TUI, run inside an existing terminal, with no GUI/webview layer. This spec
covers **Phase 1 only**: a working multi-pane terminal multiplexer with
project scoping. Later phases (not designed here) add file finder, a code
editor with LSP, a git panel, and a command palette — the parts of FLAME
that are "IDE-lite" rather than "terminal multiplexer."

FLAME's `Workspace` concept (a workspace containing multiple projects) is
explicitly dropped. RustTerm's model is flat: **Projects → Panes**, matching
FLAME's own sidebar "PROJECTS" list, just promoted to be the only
top-level container.

Persistence: PTYs live for the lifetime of the RustTerm process, same as
FLAME today. No daemon, no detach/reattach (unlike tmux). This keeps Phase 1
a single monolithic binary with no client/server split.

## Non-goals for Phase 1

- File finder, code editor, LSP integration, git panel, command palette
- Session persistence across RustTerm restarts
- Agent-CLI detection / desktop notifications (FLAME's `taskWatcher`)
- Mouse-driven drag-to-resize (keyboard-driven resize only — see Input)

## Architecture

Single Rust binary crate. Crates: `ratatui` 0.30 (crossterm 0.29 backend),
`portable-pty`, `vt100`, `tui-term`.

```
main.rs        — event loop: poll crossterm input + PTY output, redraw
app.rs         — AppState: Vec<Project>, active_project, per-project
                 active_pane, mode (Normal | Leader)
project.rs     — Project { name, root: PathBuf, panes: Vec<Pane>,
                 active_pane, split_ratios }
pane.rs        — Pane { pty: PtyHandle, parser: Arc<Mutex<vt100::Parser>>,
                 title, exited: Option<i32> }
pty.rs         — spawn/write/resize/kill via portable-pty; spawns a reader
                 thread per pane that feeds bytes into the pane's
                 vt100::Parser and wakes the render loop
layout.rs      — compute ratatui Rects for N panes in a project (mirrors
                 FLAME's hand-placed 2/3/4-pane layouts, N>=5 falls back to
                 a wrapping grid), pure functions, unit-testable
ui.rs          — draw sidebar (project list) + pane grid + status/leader
                 hint bar
input.rs       — crossterm KeyEvent routing: Leader-mode command dispatch
                 vs. passthrough to the focused pane's PTY stdin
```

## Data flow

Each `Pane` owns a `PtyPair` (master half) and an `Arc<Mutex<vt100::Parser>>`.
A dedicated reader thread blocks on `master.try_clone_reader()`, and on each
read calls `parser.lock().unwrap().process(&buf[..n])` directly, then sends
a `PaneEvent::Output(pane_id)` on an `mpsc::Sender` cloned into every pane's
reader thread. The main loop's single `mpsc::Receiver<PaneEvent>` and
crossterm's input stream are both polled with a short timeout each
iteration (crossterm's `event::poll(Duration)` alongside a non-blocking
`try_recv` on the pane-event channel), so one thread drives both input and
redraws — a new frame is drawn whenever either produces something, and at a
minimum on a periodic timeout so cursor blink etc. still animates. The
render loop never blocks on PTY I/O — it only reads the current parser
state each frame.

Keyboard input for the focused pane goes straight to `master.take_writer()`
via a `write()` call, exactly mirroring FLAME's `term.onData` → `writePty`.

**Resize** is the one place FLAME struggled all session, and it's
structurally simpler here: layout.rs recomputes every pane's `Rect` once per
frame, synchronously, on the same thread that owns the ratatui `Frame`.
Whenever a pane's computed `(rows, cols)` changes from its last known size,
call `parser.set_size(rows, cols)` and `master.resize(PtySize { rows, cols,
.. })` immediately, in the same synchronous pass — no async IPC round-trip,
no browser layout engine to race against, no window where a child process
can observe a stale size. This eliminates the entire bug class (initial
spawn size, debounced resize, ResizeObserver timing) that we spent this
session chasing in FLAME.

## Input / keyboard routing

A **leader key** (`Ctrl+B`, tmux's convention) enters Leader mode for one
keypress: `n` new pane, `x` close pane, arrow keys switch focused pane,
`[`/`]` cycle projects, `+`/`-` adjust split ratio, `q` quit RustTerm. Any
key that isn't `Ctrl+B` passes straight through to the focused pane's PTY,
same as tmux/zellij. This is a deliberate departure from FLAME's direct
Ctrl-chord interception (Ctrl+P/B/G etc.) — a GUI keydown handler can freely
claim any chord before the shell sees it; a real terminal app can't do that
safely for arbitrary chords, so a single reserved leader key is the standard
answer.

## Layout

Mirrors FLAME's `panePlacement`/`getGridTemplate`: 1 pane fills the screen;
2 panes split by a vertical divider at a configurable ratio; 3 panes are two
on top plus one spanning the bottom; 4 panes are a 2x2 grid; 5+ falls back
to a wrapping grid. Split ratios adjust via Leader `+`/`-` rather than a
mouse-dragged gutter (no mouse-drag in Phase 1).

## Error handling

A pane whose child process exits is not removed automatically — it shows
`[exited, code N]` in place, mirroring FLAME's `"[session ended]"`, so
scrollback stays visible. PTY spawn failure renders the error text into the
pane instead of the shell prompt. Reader-thread panics are caught and
converted into the same in-pane error display rather than crashing the
whole app.

## Testing

- `layout.rs`: pure functions, straightforward unit tests for pane count →
  Rect assignments (2/3/4/5+ cases).
- `ui.rs` rendering: `ratatui::backend::TestBackend` renders to an in-memory
  cell buffer that can be snapshot-asserted, avoiding the need to drive a
  real terminal in CI.
- `pty.rs`/resize logic: integration-style tests spawning a real shell
  (e.g. `cat`) and asserting `vt100::Parser` screen contents after known
  input, rather than mocking the PTY layer.
- No attempt to test actual terminal rendering pixel-for-pixel against a
  real terminal emulator — out of scope.
