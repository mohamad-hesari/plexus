# Plexus

A Rust TUI that runs pnpm workspace scripts in dependency order. You give it a set of
packages and a set of script names, and it walks the dependency graph bottom up, running
each package's script only once every package it depends on has finished the same script
successfully. It ships as prebuilt binaries behind a thin npm wrapper.

## Build and run

```bash
cargo build                 # debug
cargo build --release
cargo test                  # unit tests live in src/tui_managerv2.rs
cargo clippy --all-targets
```

Formatting is `rustfmt.toml`: 2-space indent, 120 column width. Run `cargo fmt` before
committing.

Running it needs both a filter and a command, and this trips people up: with no `-f` the
task list loads empty and the TUI shows nothing.

```bash
plexus config --generate                          # writes plexus.json from `pnpm list`
plexus run -t -W -f web -f api -C dev -C build    # TUI, watch mode, two packages
plexus run -f web -C build                        # headless, exits when everything finishes
```

Useful flags on `run`: `-t` TUI, `-W` watch, `-S` run commands sequentially in the order
given to `-C`, `-l debug` for real log levels (without it you only see task stdout/stderr),
`--mouse` for terminal mouse capture, `-d` to change working directory.

## Read this before touching anything

Roughly half of `src/` is dead. Two generations of the same design sit side by side and
only the v2 files are compiled into the running program.

Live path, everything `main()` actually reaches:

```
main.rs (Commands::Run)
  ├── cli.rs              NewCli / RunArgs / ConfigArgs
  ├── config.rs           plexus.json shape, init_default_config()
  ├── task_managerv2.rs   task store, scheduler, process runner
  ├── tui_managerv2.rs    the whole TUI
  ├── watch_managerv2.rs  file watching
  ├── hmr_websocket.rs    localhost websocket for HMR clients
  ├── env.rs              .env / .env.local / .env.<command> loading
  └── pnpm_bin.rs         resolves the pnpm executable once
```

Dead, kept around but never executed: `app.rs`, `app_state.rs`, `task_manager.rs`,
`tui_manager.rs`, `watch_manager.rs`, `log_view.rs`, `pnpm.rs`, and all of `src/old/`
(which is not even in the module tree). They reference each other and are reachable only
from `_old_main()` in `main.rs`, which nothing calls. `log_view.rs` in particular is a stale
copy of the TUI's scroll state and will mislead you if you read it as current.

The one live crossing: `watch_managerv2.rs` imports `Debouncer` from `watch_manager.rs`.
That is the only thing keeping the v1 file compiled.

Field names carry a leading underscore (`_id`, `_commands`, `_status`) as a house style, not
because they are unused. `tokio_select!` comes from `better_tokio_select` and is imported
via `#[macro_use]` in `lib.rs`.

## Domain model

A `Task` is one workspace package. It holds a `HashMap<String, TaskCommand>` of the scripts
selected by `-C`, plus `_children_id` pointing at the tasks it depends on. `TaskStatus` runs
`Init → Starting → Running → Successed | Failed | Stopped`, with `Stopping` as the state a
kill request parks in until the process actually dies.

`TaskStore` in `task_managerv2.rs` owns everything behind `RwLock`s: tasks, a flat
`command_id → status` map, per-command log buffers, and last run times. `TaskManager::main_loop`
polls `get_runnable_commands` every 50ms and spawns a `TaskRunner` per runnable command.

The dependency gate is `TaskStore::can_run`: a command may start when every child task's
command *of the same name* has reached `Successed`. Dependencies are matched by command name,
so `web:build` waits on `ui:build`, not on `ui:dev`. This is why the CLI help warns that
prerequisites must be finite processes.

`TaskRunner::run_command` spawns `pnpm --filter <pkg> <script>` as a process group
(`command-group`, so child processes die with it), reads stdout and stderr line by line into
the store prefixed `[OUT]:` / `[ERR]:`, and watches for its status flipping to `Stopping` as
the signal to kill.

`_commands` is a `HashMap`, so iteration order changes between passes. Anything that
addresses a command by index must go through `sorted_commands()` in `task_managerv2.rs`.
Skipping it produces tabs that silently point at a different command each frame.

## TUI

`tui_managerv2.rs` draws at 15fps from one loop that owns the terminal, the event stream and
a single `RwLock<State>`. Build the `EventStream` once outside the loop; constructing one per
frame drops keystrokes.

Three ideas hold the file together.

**Focus stack.** `State.focus: Vec<Focus>` with variants `Tasks`, `Logs`, `QuitConfirm`.
Opening a window pushes, closing pops, and the status bar renders `focus.last().keymap()`.
That is what makes the shortcut list follow whichever window is open and revert when it
closes, with no separate bookkeeping. The bar grows to as many rows as the shortcuts need
rather than truncating, so the layout height is computed by `status_height()` and passed to
both `main_widget` and `draw_logs_dialog`; if those two disagree the bar gets painted over.

**Rows are not lines.** `LogPane` stores wrapped display rows, and every scroll offset counts
rows. A log line wider than the pane occupies several rows, and mixing the two units is what
used to hide the tail of the log below the bottom border. `LogPane::sync` wraps only newly
arrived lines, rewraps everything on a width change, and is the only place that touches
`scroll`. Render the resulting slice with wrapping switched off, never `Paragraph::wrap`.

**One pane per tab.** `LogsWindow.panes` is keyed by command id, so each tab keeps its own
scroll position and follow flag, and `s` / `r` / `S` act on the visible tab's command only.
Every one of those actions goes through `spawn_action`, because `TaskManager::stop_command`
polls for up to five seconds and would otherwise freeze the render loop for that whole time.

Mouse handling is deliberately off by default. Capturing the mouse takes click-drag text
selection away from the terminal, so the TUI enables alternate scroll (`ESC[?1007h`) inside
the alternate screen instead, which makes the wheel emit cursor keys the log view already
handles. `--mouse` or the `m` key turns real capture on for terminals that lack it. On exit,
disable alternate scroll *before* leaving the alternate screen.

## Cross-platform notes

These are all real bugs that were fixed, and all easy to reintroduce.

Never write `Command::new("pnpm")`. Rust's Windows `resolve_exe` only appends `.exe` when
searching `PATH` and never reads `PATHEXT`, so `pnpm.cmd` from npm or corepack is invisible
to it. Go through `pnpm_bin::pnpm()` (returns a `Result` for per-task failure) or
`pnpm_bin::pnpm_or_exit()` (for startup paths). `PLEXUS_PNPM` overrides the lookup.

Paths that become glob patterns must go through `normalize_for_glob` in `watch_managerv2.rs`.
`canonicalize()` returns `\\?\C:\...` verbatim paths on Windows which never match the plain
paths in a notify event. Literal directory names also need `escape_glob_literal` so a package
under `packages/[locale]` still compiles.

`init_default_config` writes project paths with forward slashes on purpose. Those strings end
up in `plexus.json` and later become watch globs, so a backslash there breaks matching on
another machine.

## Distribution

`packages/main` is the published wrapper. `find_binary.js` resolves the platform package,
`index.js` execs the binary, `postinstall.js` generates the JSON schema. Nothing in
postinstall may exit non-zero: a failing postinstall aborts `npm install` for the entire
project, and npm records optional dependencies per platform, so a lockfile generated on Linux
routinely leaves the Windows binary package out. Warn and continue.

`packages/{linux-x64,linux-musl-x64,darwin-arm64,win32-x64}` each hold one binary under
`bin/`. `.github/workflows/release.yml` builds them on a tag push, rewrites versions and
scoped names with `jq` and `npm pkg set`, and publishes to both npm and GitHub Packages.
Adding a target means touching the matrix, a new `packages/` directory, the
`optionalDependencies` in `packages/main/package.json`, and the `SUPPORTED` list in
`find_binary.js`.

## Verifying TUI changes

Unit tests cover the wrap and scroll maths in `tui_managerv2.rs`. For anything visual, drive
the real binary in a pty and read back the rendered screen: fork with `pty.fork()`, set the
window size with `TIOCSWINSZ`, feed the output through `pyte` and print `screen.display`.
Send keys as raw bytes (`b"\r"`, `b"\x1b"` for Esc, `b"\x1b[6~"` for PageDown,
`b"\x1b[<65;40;10M"` for a wheel-down). That is how the scroll fix, the per-tab state, the
focus stack and the terminal mode sequences were all checked, and it catches layout problems
that no unit test will.

## Known issues

`plexus run` exits 0 when a command fails. In `TaskManager::main_loop`, `is_all_finished()`
counts `Failed` as finished and is checked before `is_any_failed()`, so the loop breaks with
`result = true`. Swapping the two checks fixes it. This matters for CI.

Watch mode registers each existing depth-1 child of a package directory individually rather
than the directory itself, so a brand new file created directly at the package root is not
picked up until the next run. Files created inside an already-watched subdirectory are fine.
