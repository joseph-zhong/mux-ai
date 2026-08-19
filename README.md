# mux-ai

Grid-dashboard TUI for running several coding agents in parallel, each in its own git
worktree, on top of tmux.

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — what it does: state ownership, process model,
  data flow, known structural gaps.
- [`DESIGN.md`](DESIGN.md) — why it is shaped this way.
- [`ALTERNATIVES.md`](ALTERNATIVES.md) — why this exists instead of aoe / Claude Squad / etc.
- [`FUTURE.md`](FUTURE.md) — backlog.
- [`POSTMORTEM.md`](POSTMORTEM.md) — incidents and what they taught.
- [`plans/`](plans/) — stock-takes and strategy notes.

## Install & demo

Prerequisites: Rust (stable), `tmux`, `git` — see [Dependencies](#dependencies) below.

```sh
cargo build --release
# put target/release/muxai on your PATH, or:
cargo install --path .
```

Demo, from inside any git repo:

```sh
muxai new demo -- 'echo hello from demo; sleep 300'   # worktree + tmux session
muxai new demo2 -- claude                              # a second one, running claude

muxai                # opens the grid dashboard
# arrow keys: move selection    Enter: attach into the session
# C-q (inside a session): detach back to the dashboard
# n: new session   k: kill selected   q: quit (only place quit exists)

muxai status         # disk (shared cache vs. reclaimable) + memory, across all sessions
muxai reset          # dry run — shows what it would reclaim
muxai reset --yes    # actually reclaims it
```

## Dev / local run

```sh
cargo run -- status          # iterate without installing
cargo run                    # dashboard
cargo build                  # debug build, faster compiles
```

Module layout: `tmux.rs` (dedicated-socket tmux wrapper), `worktree.rs` (git worktree
lifecycle), `session_store.rs` (JSON state file), `stats.rs` (disk/memory accounting),
`ui/dashboard.rs` (the ratatui grid), `main.rs` (CLI dispatch + `create_session`, shared
by `muxai new` and the dashboard's `n` key). No daemon — every invocation is a fresh
process that shells out to `tmux`/`git` and reads/writes the store.

## Dependencies

- **Rust** stable, via [rustup](https://rustup.rs) — no MSRV pinned yet.
- **tmux** — developed against 3.7b; anything reasonably recent should work.
- **git** ≥ 2.5 (worktree support).
- Crate dependencies (ratatui, crossterm, clap, sysinfo, serde, chrono, dirs) are
  pulled by Cargo — nothing else to install manually.
- Developed and tested on **macOS**. Should work on Linux (nothing macOS-specific in
  the code path) but that's untested.

## Known gotchas & trade-offs

- **Own tmux socket.** Every mux-ai session runs on `tmux -L muxai`, isolated from your
  normal tmux server — they won't show up in a plain `tmux ls` and your `~/.tmux.conf`
  is untouched. This is deliberate: it's what lets us rebind a single unprefixed `C-q`
  to detach without clobbering your own keybindings. To nuke everything mux-ai owns:
  `tmux -L muxai kill-server` (kills the sessions) and remove
  `~/.local/state/muxai/sessions.json` (drops the bookkeeping).
- **Grid is polled, not streamed.** Each cell redraws from `tmux capture-pane` every
  ~300ms rather than a true live PTY feed, so there's a few hundred ms of lag. Fine for
  "is this agent stuck or working," not frame-accurate. Real fix tracked in
  [`FUTURE.md`](FUTURE.md) (item 1).
- **Needs a real terminal.** The dashboard requires an actual TTY; run it from a
  non-interactive shell/pipe and it fails fast with a clean error instead of hanging or
  corrupting terminal state (raw mode is only entered after that check succeeds).
- **Memory budget is hardcoded** (32GB default in `stats::DEFAULT_MEMORY_BUDGET_BYTES`)
  — not yet config-driven. See [`FUTURE.md`](FUTURE.md) (item 6).
- **`reset` is conservative by design.** It only ever deletes known per-worktree build
  dirs (`target/`, `node_modules/`, `.venv/`, `dist/`, `.next/`, `__pycache__/`) and
  never touches shared global caches (uv/cargo/pnpm) — see `DESIGN.md`'s
  "global-buck-clone" table for why that split matters. It won't reclaim anything
  outside that list, even if it's large.
- **No execution sandboxing.** Worktrees isolate source state, not what a command run
  inside one can do — there's no seatbelt/container boundary yet. Don't point an agent
  in full-auto mode at a worktree doing anything you wouldn't run unsandboxed today.
  Docker is intentionally not the answer here (no Metal/GPU passthrough on macOS, and
  it competes with unified memory reserved for local inference) — see `DESIGN.md`.

## Future work

See [`FUTURE.md`](FUTURE.md) — live control-mode streaming, a sandboxed/remote runner,
merged-branch-aware `reset`, cache-env auto-injection on session creation, a config
file, multi-repo dashboard filtering, and open-model agent presets. Not yet filed as
GitHub issues (this repo has no remote / `gh` isn't authenticated on this machine yet).
