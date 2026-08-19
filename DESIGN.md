# mux-ai — Design

## Problem

Manage several coding-agent sessions (Claude Code, OpenCode, Kimi CLI, etc.), each in
its own git worktree of the same repo, from one terminal. See `ALTERNATIVES.md` for the
survey that led here: existing tools (aoe, Claude Squad, Vibe Kanban, Crystal) cover
parts of this, but none combine a **grid dashboard** (see all sessions' live output at
once, arrow-key + Enter to jump in) with **worktree resource accounting**
(`status`/`reset`).

## Core UX decisions (from spec discussion)

1. **Dashboard is the home screen**, not a sidebar list. A grid of cells, one per
   session, each showing a live tail of that session's actual terminal output.
   Arrow keys move the selection; Enter attaches.
2. **Navigation is one-directional and unambiguous**: dashboard → attach (Enter) →
   session. From inside a session, one keystroke always means "back to dashboard" —
   never "quit," never a sidebar toggle. `q` in the dashboard is the only quit.
3. **Resource accounting is a first-class command**, not an afterthought: `status`
   (disk + memory across all worktrees) and `reset` (reclaim it), because worktrees
   are cheap in aoe/Claude Squad's world but not free once build tooling touches them
   (see below).

## Architecture

Rust, single static binary (`muxai`), built on:

- **ratatui + crossterm** — TUI rendering and input.
- **tmux**, on a **dedicated socket** (`tmux -L muxai`) — every mux-ai session is a
  real tmux session on this private server, isolated from the user's own tmux config
  and sessions. This lets us rebind keys server-wide (e.g. a single `C-\` →
  `detach-client`) without touching `~/.tmux.conf`, which directly fixes the "exit
  means control the sidebar" confusion from aoe: inside a session, `C-\` always
  detaches back to the dashboard, full stop.
- **git worktree** — one worktree per session, created under
  `<repo>/.muxai/worktrees/<name>` (git-ignored), branch name = session name unless
  overridden.
- **JSON session store** (`~/.local/state/muxai/sessions.json`) — maps session name →
  `{repo, worktree_path, branch, command, tmux_session, created_at}`. A **metadata
  cache, not a source of truth**: git owns worktrees, tmux owns sessions, and both are
  re-queried live on every dashboard refresh. The store exists only to remember the
  command a session was started with, so a stopped worktree can be restarted with the
  same agent. Losing the file costs that and nothing else.

  This originally read "source of truth for what mux-ai manages; tmux/git are
  re-queried live and reconciled against it", which is backwards and licensed a bug
  that hid six live worktrees. See `POSTMORTEM.md` (2026-08-19) and `ARCHITECTURE.md`.

No daemon. Every `muxai` invocation is a fresh process that shells out to `tmux`/`git`
and reads/writes the JSON store. Simplicity over a client/server split — revisit only
if polling overhead becomes a real problem (see Future).

### Dashboard rendering: capture-pane polling (MVP choice)

The dashboard grid renders each cell by shelling out to
`tmux capture-pane -p -t <session>` on a timer (~300ms) and drawing the captured text.
This is **not** a true multiplexed live PTY stream — there's a few hundred ms of
latency — but it satisfies "literally see the text going" with a fraction of the
complexity of parsing tmux control-mode (`tmux -C`) or embedding a terminal-emulator
crate. That upgrade is real and worth doing later (see FUTURE.md), but it's not needed
for a working MVP and its absence doesn't block any other feature.

### Attach / detach flow

1. Dashboard is a normal ratatui alt-screen app owning the terminal.
2. On Enter: leave raw mode / alt screen, `exec`-equivalent `tmux -L muxai attach -t
   <session>` as a foreground child, inheriting stdio.
3. The muxai tmux server has `C-\` bound (no prefix key) to `detach-client`. User
   presses it, tmux detaches, the child process exits.
4. mux-ai re-enters raw mode / alt screen and redraws the dashboard. No state is lost
   in the tmux session itself — it keeps running headless.

This is the same handoff pattern lazygit/gh-dash use for editors: the TUI cleanly
yields the terminal and reclaims it, rather than trying to render another program's
output inside our own widget.

## Worktrees vs sandboxing

Worktrees isolate **source state** (branch/index/working tree per agent). Sandboxing
isolates **execution** (blast radius of what a command can do). For a single-user
machine running agents under their own permission prompts, worktrees are the
correctly-scoped isolation for the MVP; execution sandboxing is a separate, later
concern (see FUTURE.md) — **not** Docker on this machine:

- Docker Desktop on macOS runs containers inside a Linux VM with no Metal/GPU
  passthrough, and it statically reserves unified memory. That's directly at odds with
  reserving this machine's 256GB for local model inference (MLX/Metal), which needs
  the opposite: as much unshared unified memory as possible.
- If/when we need real execution sandboxing, the right local primitive is macOS
  `sandbox-exec` (seatbelt) — what Claude Code's own sandbox mode uses — not
  containers. Containers remain the right call for a *remote* Linux/CUDA runner if
  model-kernel work eventually needs it, which is a different machine, not this design.
- Design implication: keep a **runner seam**. A session is conceptually
  `(worktree, runner)`; `runner` is a local-process today and could become
  `sandbox-exec` or a remote runner later without changing the worktree/dashboard
  model.

## Resource accounting: the "global-buck-clone" property

The Meta hyperclaude comparison (buck/bazel + a global content-addressed cache meant a
new worktree never paid for a cold build) is the right mental model, and it's not
automatic here — it depends on per-toolchain caching:

| Toolchain | Has the property by default? | Mechanism |
|---|---|---|
| Python (uv) | Yes | `~/.cache/uv` is content-addressed + concurrent-safe; per-worktree `.venv` materializes via APFS copy-on-write |
| JS (pnpm) | Yes | content-addressed global store |
| JS (npm/yarn) | No | duplicates `node_modules` per worktree |
| Rust (cargo) | No, by default | own `target/` per worktree unless `CARGO_TARGET_DIR` is shared, or sccache used |
| C/C++ | No, by default | needs ccache/sccache |

This directly shapes the resource commands:

- **`muxai status`** splits worktree disk usage into two buckets: *shared caches*
  (global uv/pnpm/cargo-registry caches — cheap, informational only) vs
  *per-worktree duplicated build state* (`target/`, `node_modules/`, `.venv/`,
  `dist/`, `.next/`, `__pycache__/` — the reclaimable part), plus a memory rollup
  (sum of RSS across each session's process tree) shown red/yellow/green against a
  configured budget.
- **`muxai reset`** only ever deletes from the second bucket (with confirmation
  unless `--yes`), plus `git worktree prune` and dropping stale entries from the
  session store. It never touches shared caches.
- Future: mux-ai can inject `CARGO_TARGET_DIR`/equivalent env vars when spawning a
  session so new worktrees are born sharing caches rather than relying on the user
  having configured it (tracked in FUTURE.md).

## Hardware sizing (informational, drives the `status` budget default)

- Worktrees themselves are near-free on disk (shared `.git` object store).
- Each CLI agent process is a few hundred MB RSS; real memory spikes come from
  builds/tests running inside a worktree, not the agent process itself.
- The machine this was designed against (M3 Ultra, 256GB unified memory) reserves the
  bulk of its headroom for local model inference, not agents — `status`'s default
  budget should leave that headroom untouched and warn well before agents encroach on
  it.

## MVP scope (this build)

In:
- `muxai new <name> [--branch <branch>] -- <command...>` — create worktree + tmux
  session running `<command>` (default: `claude`) in it.
- `muxai` (no args) / `muxai dashboard` — grid dashboard: live capture-pane tiles,
  arrow-key selection, Enter to attach, `n` new session (prompts), `k` kill selected
  (with confirmation), `q` quit.
- `muxai kill <name>` — kill tmux session; prompts to also remove the worktree.
- `muxai status` — disk (shared vs reclaimable split) + memory rollup, red/yellow/green.
- `muxai reset [--yes]` — reclaim per-worktree build dirs, prune worktrees, clean
  stale sessions.
- Single dedicated tmux socket, JSON session store, no daemon.

Out (see FUTURE.md): control-mode live streaming, sandboxed/remote runners, merged-
branch-aware reset, automatic cache-env injection, config file, web/mobile view.
