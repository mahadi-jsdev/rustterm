# RustTerm

A terminal workspace for people who live in the terminal — and for the AI
agents that live there with you. RustTerm is a native-Rust TUI multiplexer:
multiple projects, real PTY panes, floating tool popups, git tooling, and
first-class awareness of CLI agents (claude, codex, devin, gemini, …) —
all in one binary, no Electron, no daemon required until you want one.

```
┌pane-1 ────────────────────────────┐┌pane-2 ────────────────────────────┐
│ ~/RustTerm                        ││ ~/RustTerm                        │
│                                   ││                                   │
│                                   ││                                   │
│                                   ││                                   │
│                                   ││                                   │
│                                   ││                                   │
│                                   ││                                   │
└───────────────────────────────────┘└───────────────────────────────────┘
Ctrl+A for commands   [2/2] zoom   +1 hidden   [1 popup]        RustTerm
```

## Features

- **Multi-project workspaces** — each project gets its own pane grid, cwd,
  git state, and scrollback; switch instantly with `C-a [` / `C-a ]` or
  the command palette (`C-a p`). The active project name sits in the
  bottom-right of the status bar.
- **Real PTY panes** — every pane is a genuine pseudoterminal (via
  `portable-pty` + `vt100`), rendered with `ratatui`. Neighboring panes
  share a single border line — zero dead cells. No sidebar: panes always
  own the full width.
- **Floating popup panes** — the file finder, file manager, git panel,
  lazygit, `git diff`, and `git log` open as centered overlays that never
  reflow your grid.
- **AI agent awareness** — panes running known agents get colored borders
  (claude amber, codex blue, devin coral, gemini teal…), plus `●` waiting /
  `!` attention badges and desktop notifications when an agent needs you.
  `C-a .` jumps straight to the next flagged pane.
- **File manager + git popups** — `C-a e` opens a lazy project file tree;
  Enter opens files in an editor popup. `C-a g` opens the **Git panel** —
  live status (staged vs. worktree), branch switching, `space` to
  stage/unstage, Enter for a diff popup, `c` for an AI-written commit
  message (OpenAI, via `OPENAI_API_KEY`).
- **Copy that just works** — `C-a v` enters a vim-style copy mode over
  the full scrollback, or just **drag with the mouse** — release copies
  to your clipboard via OSC52 (works over SSH, no X11 dependency).
- **Safe multiline paste** — bracketed paste end-to-end: pasting into a
  shell inserts the text as one buffer instead of executing each line.
  Paste also lands in the palette, finder, and prompt fields.
- **Full mouse support** — click to focus panes, click panel rows,
  drag-select to copy, wheel to scroll scrollback (events forward to apps
  that capture the mouse; `Shift`-drag always selects).
- **Detach / reattach** — `C-a d` forks a keeper daemon holding your live
  session; `rustterm -a` reattaches from any terminal, `rustterm -k`
  kills it. Scrollback and running processes survive.
- **Session persistence** — quitting saves projects, panes, titles, and
  colors; the next bare `rustterm` restores everything.
- **Configurable** — `~/.config/rustterm/config.toml` for leader key,
  editor, colors, scrollback, popup size.

## Install

### Install script (Linux x86_64)

```sh
curl -fsSL https://raw.githubusercontent.com/mahadi-jsdev/rustterm/main/install.sh | bash
```

Downloads the latest release binary into `~/.local/bin` (override with
`PREFIX=/some/dir`).

### From source (any platform Rust supports)

```sh
cargo install --git https://github.com/mahadi-jsdev/rustterm
```

### Dependencies

Runtime needs only a POSIX-y system. Optional integrations: `lazygit`
(`C-a G`), `nvim` or `$EDITOR` (file popups), `git` (git panel, diffs,
commits), `OPENAI_API_KEY` (AI commit messages).

## Usage

```sh
rustterm              # restore your saved session, or start in cwd
rustterm ~/proj1 ~/proj2   # open specific project roots
rustterm -a           # reattach to a detached session
rustterm -k           # kill the detached keeper
```

Everything hangs off the **leader key**, `Ctrl+A` by default.

### Leader commands

| Key | Action |
|-----|--------|
| `n` | New pane |
| `x` | Close popup, else close active pane |
| `h` / `l` / arrows | Switch pane |
| `z` | Zoom pane to fill the grid (toggle) |
| `.` | Jump to next waiting/attention pane |
| `H` | Background (hide) pane — reopen via palette |
| `[` / `]` | Previous / next project |
| `+` / `-` | Adjust the split |
| `:` or `p` | Command palette |
| `c` | Add project… |
| `e` | File manager popup |
| `g` | Git panel popup |
| `o` | Scratch terminal popup (quick command; `exit`/`C-a x` closes) |
| `G` | lazygit popup |
| `L` | `git log --graph` popup |
| `f` | File finder → editor popup |
| `/` | Search terminal output |
| `v` | Copy mode |
| `d` | Detach session |
| `q` | Quit |

### Panel popups (`C-a e` / `C-a g`)

The file manager and git view are modal overlays — they float over the
grid, own input while open, and dismiss with `Esc`, `q`, or a click
outside. `e`/`g` inside the panel flips between Files and Git; clicking
the title row does too.

- **Files** (`C-a e`): lazy directory tree, dirs first — `j`/`k` moves
  (the list scrolls to follow), `l`/`→` expands, `h`/`←`/`Backspace`
  collapses or hops to the parent, `Enter` toggles a dir or opens a file
  in an editor popup. Dotfiles are hidden by default — `Ctrl+Shift+H`
  (kitty-protocol terminals) or `.` toggles them.
- **Git** (`C-a g`): `j`/`k` moves, `Enter` opens a diff popup (files) or
  `git switch` (branches — `b` toggles the list), `space` stages/unstages,
  `c` generates an AI commit message and prefills the commit prompt.

### Copy mode (`C-a v`)

| Key | Action |
|-----|--------|
| `h j k l` / arrows | Move cursor |
| `v` | Toggle selection anchor |
| `y` / `Enter` | Yank selection (or line) via OSC52 |
| `Esc` / `q` | Cancel |
| `PgUp` / `PgDn` | Page through scrollback |
| `g` / `G`, `0` / `$` | First/last row, first/last column |

Clicking inside the pane places the cursor; dragging selects.

### Mouse

- **Click a pane** — focus it
- **Drag in a pane** — select; release copies to clipboard (OSC52)
- **Drag past the top/bottom edge** — auto-scrolls scrollback
- **`Shift`+drag** — select even when the app captures the mouse
- **Wheel** — scroll the pane under the cursor (forwarded to apps like
  nvim/lazygit when they ask for it)
- **Panel overlay** — rows select (click the selected row to activate),
  the title row toggles Files ↔ Git, a click outside dismisses
- **Wheel over the panel** — scrolls the list; wheel outside is swallowed

## Configuration

`~/.config/rustterm/config.toml` (override path with `RUSTTERM_CONFIG`):

```toml
leader = "ctrl+a"        # leader key — any ctrl+<char>
editor = "nvim"          # finder/panel → open; beats $VISUAL/$EDITOR
scroll_lines = 3         # wheel scroll step
scrollback = 10000       # per-pane scrollback lines
float_pct = 90           # popup size, % of screen
accent = "cyan"          # focused pane border (name or "#rrggbb")
selection = "#264f78"    # select highlight background
```

A missing file is defaults; a parse error falls back and flashes the
reason at startup.

## AI commit messages

Set `OPENAI_API_KEY` in the environment, then in the Git panel (`C-a g`)
press `c`: RustTerm stages the working tree, sends the staged diff to
OpenAI (`gpt-4o-mini`), and prefills the commit prompt — `Enter` commits.

## How it works

Each pane is a real PTY process; a reader thread feeds a `vt100` parser
per pane, and `ratatui` renders the grid. Popups are transient panes in a
separate float stack — they own input while open and vanish when their
tool exits. Detach forks a keeper that keeps PTYs and watchers alive
while clients attach over a unix socket (fds pass via `SCM_RIGHTS`).
Sessions serialize to JSON; popups and transient state don't persist.

Development docs and design history live in `docs/superpowers/`.

## License

[MIT](LICENSE)
