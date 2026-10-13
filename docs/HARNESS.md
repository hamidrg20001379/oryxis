# Headless E2E harness

Oryxis ships a headless end-to-end harness behind the `harness` cargo
feature. It runs the **real application**, real vault, real
subscriptions, real SSH side effects, inside `iced_test`'s `Emulator`
(from the same wilsonglasser/iced fork the app renders with), with no
window and no display server, and renders PNG screenshots on demand.
Think "Playwright for the iced UI".

The feature is dev-only: release and CI artifact builds never enable
it, so it adds zero weight to shipped binaries.

## Isolation

Both modes redirect `$HOME` (and `%USERPROFILE%` on Windows) to a
sandbox directory **before anything reads the vault**, and point
`ORYXIS_HOME` at it too: the app resolves its whole `.oryxis` tree
through that override (`oryxis_core::paths`), which is what makes the
sandbox hold on Windows as well, where the OS home comes from a WinAPI
call that ignores env vars. A harness run can never touch your real
`~/.oryxis`.

- Default sandbox: `<system tmp>/oryxis-harness`. It persists across
  runs on purpose: a master password set in one session is still
  there in the next, which keeps iterative QA cheap.
- Override with `--home <dir>`. Batch runs in CI should always pass a
  fresh directory so first-boot flows (onboarding) are reproducible.

## Batch mode (CI): `--harness-run <dir>`

```bash
cargo run -p oryxis-app --features harness -- \
    --harness-run crates/oryxis-app/tests/e2e --home "$(mktemp -d)"
```

Runs every `.ice` file in `<dir>` in file-name order (`fs::read_dir`
order is not deterministic, so the runner sorts), each one on a
freshly wiped sandbox: the `.oryxis` directory is removed before
every test, so every test starts from the first-run (onboarding)
state and never depends on what ran before it. A failing instruction
dumps a PNG screenshot plus a truncated reproduction `.ice` into
`<dir>/errors/` and exits non-zero.

The `.ice` format:

```text
viewport: 1200x750
mode: Zen
-----
# comments and blank lines are skipped
expect "Welcome to Oryxis"
click "Skip"
expect "Protect your vault"
click "Continue without password"
expect "Create host"
```

`mode` is required: `Zen` waits for all tasks an instruction spawns
(including indirect ones), `Patient` only for direct ones,
`Immediate` never waits. See `crates/oryxis-app/tests/e2e/` for the
committed suite.

The batch runner executes instructions with the same per-instruction
timeout as the interactive modes (`--timeout-ms`, default 20 s), so a
live PTY cannot deadlock a test: a timed-out instruction still
executed and the test moves on. It also understands the harness
pacing lines, which makes terminal-session tests batchable:

```text
timeout 500        # per-instruction timeout (use once a PTY is open)
settle 800         # pump until the event stream stays quiet
wait 250           # pump for a fixed duration
screenshot name    # PNG into the shots dir, printed as `== shot ...`
```

Screenshots taken by a test land in the shots directory (`--shots`,
default `<home>/shots`) and are the way to validate canvas content
(the terminal grid is invisible to `expect`); collect them as CI
artifacts for visual review. `save_ice` in the interactive modes
records these pacing lines too, so a recorded terminal flow replays
with the same rhythm.

## Interactive mode (agent/manual QA): `--harness-repl`

```bash
cargo run -p oryxis-app --features harness -- --harness-repl
```

A line protocol on stdin/stdout. Every response line is prefixed with
`== ` so it can be told apart from tracing output on the same stream.
A convenient way to drive it from another process is a `tail -f`'d
command file:

```bash
: > /tmp/cmds.txt
tail -f -n +1 /tmp/cmds.txt | oryxis --harness-repl > /tmp/out.log 2> /tmp/err.log &
echo 'screenshot boot' >> /tmp/cmds.txt
grep '^== ' /tmp/out.log
```

### Commands

Any `.ice` instruction works as a command:

| Command | Meaning |
|---------|---------|
| `click "Text"` / `click #id` / `click (x, y)` | click a target (`click right ...` for right-click) |
| `click double <target>` | both clicks of a double click on one drawn frame, with no update between them (a harness command, recorded and replayed as one line). Two `click` lines are NOT a double click you can rely on: each draws a frame and waits for the app first, so the gap between the presses is whatever that took, inside the system interval on a fast machine and outside it on a loaded runner |
| `press` / `release` / `move <target>` | lower-level mouse steps |
| `scroll (dx, dy) [<target>]` | mouse wheel in lines (negative y = down); `scroll pixels (dx, dy)` for pixel deltas; the optional target moves the cursor first |
| `pinch <delta> [<target>]` | one whole touchpad pinch magnifying by `delta` (positive spreads the fingers), replayed with its start and end phases; the optional target moves the cursor first |
| `type "some text"` | typewrite into the focused widget |
| `type enter` / `escape` / `tab` / `backspace` | named keys (`press enter` / `release tab` for the halves) |
| `type ctrl+k` / `type ctrl+shift+f` / `type alt+enter` | modifier chords; reach the app's global hotkeys |
| `expect "Text"` | fail unless a widget currently shows `Text` |

Plus harness meta-commands:

| Command | Meaning |
|---------|---------|
| `screenshot [name]` | render the UI to `<shots>/<name>.png`, print the path |
| `texts` | dump every visible text widget with bounds (reading order) |
| `find "Text"` | like `texts`, filtered to matches |
| `clipboard` / `clipboard "text"` | read / seed the emulated clipboard; `\n` / `\t` / `\"` / `\\` escapes decode, so multi-line content (PEM blocks) fits the line protocol |
| `drop hover` / `drop leave` / `drop "/local/path"` | synthesized OS file drag-and-drop: the emulator has no OS to drag from, so these inject the exact messages the window subscription maps `FileHovered` / `FilesHoveredLeft` / `FileDropped` to; repeat `drop "path"` for a multi-file gesture (the app's own debounce coalesces them) |
| `windows` / `window <n>` / `window <n> at (x, y)` | the emulated desktop: list the open windows, give the focus to the n-th (1-based, in opening order), or move it on the emulated screen. The emulator draws and instructs the FOCUSED window; a window the app opens takes the focus, and closing it hands the focus back to the previous one. A held mouse button stays with the window it was pressed in: its `move` and `release` are delivered there, in that window's coordinates, whichever window is focused |
| `wait <ms>` | pump emulator events for a fixed duration |
| `settle [idle_ms]` | pump until the event stream stays quiet (default 250 ms, 30 s cap) |
| `timeout <ms>` | set the per-instruction completion timeout (default 20 s) |
| `help` / `quit` | self-explanatory |

Responses: `== ok`, `== fail <instruction>`, `== timeout ...`,
`== shot <path>`, `== error <reason>`, plus `== text ...` entry lines
for `texts`/`find`. Lines starting with `#` and blank lines are
ignored, so a command file can be annotated.

### Flags (both modes)

| Flag | Default | Meaning |
|------|---------|---------|
| `--home <dir>` | `<tmp>/oryxis-harness` | sandbox `$HOME` |
| `--shots <dir>` | `<home>/shots` | where screenshots land |
| `--viewport <WxH>` | `1200x750` | logical window size |
| `--scale <f>` | `1` | screenshot scale factor (0.25..=4) |
| `--mode zen\|patient\|immediate` | `zen` | task-waiting strategy |
| `--timeout-ms <ms>` | `20000` | REPL per-instruction timeout |

## Daemon + CLI client (AI agents): `--harness-serve` / `--harness-ctl`

The agent-facing surface. The emulated app is stateful (unlocked
vault, navigated screens, live sessions), so CLI ergonomics come from
a long-lived daemon holding the emulator plus a one-shot client that
delivers commands to it over TCP (127.0.0.1 only, default port 6799,
`--port` on both sides to override):

```bash
oryxis --harness-serve &                  # daemon; prints "harness listening ..."
oryxis --harness-ctl status               # one command
oryxis --harness-ctl 'click "Keychain"'   # quote the whole command
oryxis --harness-ctl <<'EOF'              # or batch via stdin
reset wipe
click "Skip"
screenshot onboarding
EOF
oryxis --harness-ctl quit                 # stops the daemon
```

The wire protocol is the REPL line protocol verbatim (same command
grammar, `== `-prefixed responses; `harness/commands.rs` is shared by
both front-ends, so they cannot drift). The client exits 0 when every
line succeeded, 1 on any `== error` / `== fail`, 2 when the daemon is
unreachable, so shell `&&`-chaining works. `screenshot` prints the
PNG path for the caller to open. The agent workflow (rebuild cycle,
patterns, gotchas) is documented as the project skill
`.claude/skills/harness/SKILL.md`.

## MCP mode (other clients): `--harness-mcp`

```bash
oryxis --harness-mcp            # MCP server over stdio
```

Exposes the same driving surface as MCP tools for MCP-capable clients.
Note the process is spawned once by the client and holds its binary
for the whole session: after a rebuild the tools keep running OLD code
until the client reconnects, which is why agent sessions prefer the
daemon + ctl pair above (the agent controls the lifecycle itself).

| Tool | Meaning |
|------|---------|
| `run { script }` | execute `.ice` instructions, one per line; stops at the first failure; executed lines are recorded |
| `screenshot { name? }` | render the UI and return the PNG **inline** as MCP image content (also saved under shots) |
| `texts { filter? }` | visible text widgets + bounds (the inspector) |
| `settle { idle_ms? }` / `wait { ms }` | let async work land |
| `set_timeout { ms }` | per-instruction timeout (500 once a terminal is open) |
| `clipboard_get` / `clipboard_set { text }` | emulated clipboard |
| `history { clear? }` | instructions recorded so far |
| `save_ice { path }` | write the recorded session as a replayable `.ice` test |
| `reset { wipe? }` | reboot the app in place; `wipe` clears the sandbox for a first-run state |

Typical loops:

- **Visual validation**: `reset {wipe:true}` → `run` a flow →
  `screenshot` and inspect the returned image.
- **Producing a test**: same, then `save_ice {path}` and commit the
  file into `crates/oryxis-app/tests/e2e/`; replay with
  `--harness-run`.

## Driving a terminal session

Typing into the terminal works end to end (the canvas receives the
simulated keyboard through the same event path as the windowed app,
and the PTY is real). Verified recipe, local shell:

```text
timeout 500
type ctrl+k
click "Local Shell"
settle 800
screenshot shell-open
click (600, 400)
type "echo HARNESS-OK"
type enter
settle 800
screenshot shell-output
expect "● bash (default), connected"
```

(`timeout 500` goes first: the connect click itself already leaves
the never-ending PTY task pending, so it would otherwise burn the
full default timeout before responding.)

Two things to know:

- **Set `timeout 500` (or use `--mode patient`) once a session is
  open.** A live PTY keeps a never-ending reader task around, so the
  default `zen` mode ("wait for every task") never quiesces and each
  instruction burns the full instruction timeout before handing
  control back. Everything still executes; it is only wasted
  wall-clock. `settle` remains the right way to let output arrive.
- **Assert terminal output visually.** The grid is a canvas, so
  `expect` cannot match text inside it; take a `screenshot` instead.
  The status bar is a regular text widget, so
  `expect "● bash (default), connected"` works for connection state
  (the status dot lives inside the same text widget, and the selector
  matches exact text).

## How it works / limitations

The harness relies on harness-grade emulator work that lives in the
wilsonglasser/iced fork's `oryxis` branch (landed 2026-07-10 via the
`oryxis-harness` feature branch): a fixed `Emulator::screenshot` (the
upstream one loses the widget cache and poisons the next
instruction), a public `Emulator::operate` (backs `texts`/`find`), an
in-memory emulated clipboard (runtime tasks + widget-level paste,
fulfilled per event so a `ctrl+v` chord's key release can't cancel
the pending read), interaction-event broadcast to subscriptions
(this is what makes global hotkeys like `Ctrl+K` work), and the
`scroll`/chord grammar.

- The emulator boots `Oryxis::boot` through the same
  `iced::application(...)` builder `main()` uses (fonts, theme,
  subscriptions), so behavior matches the windowed app. Tray,
  single-instance IPC and the window itself are skipped.
- Rendering picks wgpu-headless when a GPU adapter exists and falls
  back to tiny-skia (CPU) otherwise, no display needed either way.
- Screenshots come straight from the emulator's renderer and
  widget-state cache: scroll offsets, focus rings and carets all
  show, exactly like the windowed app.
- Text selectors see iced text widgets only. The terminal grid is a
  custom canvas, so `expect` cannot match terminal output; verify
  terminal content visually via `screenshot`. Typing into the PTY
  works normally (events flow through the widget). Text *inputs*
  expose their value visually, not to `expect` (it matches text
  widgets, not input values).
- The emulated clipboard covers everything that goes through iced
  (widget paste, `iced::clipboard` tasks). App code that talks to
  the system clipboard directly via `arboard` (e.g. the copy
  actions behind `Message::CopyToClipboard`) bypasses it and hits
  the real system clipboard of the machine running the harness.
- Real window/WM concerns (multi-monitor geometry, DPI, tray) stay
  manual QA.

## Recording tests from the real app

```bash
cargo run -p oryxis-app --features tester
```

Runs the real windowed app with iced's tester overlay: F12 opens a
record/play panel that captures your interactions as `.ice` files,
which this harness replays headless. Chorded shortcuts and wheel
scrolls record through the same grammar the harness executes
(`type ctrl+k`, `scroll (0, -3)`). Dev-only, like `harness`; never
enabled in release builds. Recording against your real `~/.oryxis`
is fine (it is a windowed run like any other), but replaying the
recorded `.ice` needs a sandbox `--home` prepared with the same
state the recording assumed.
