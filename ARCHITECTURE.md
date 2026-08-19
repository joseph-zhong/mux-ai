# mux-ai — Architecture

`DESIGN.md` records *why* mux-ai is shaped the way it is. This document records *what
it actually does* — the state model, the process model, and where each piece of state
really lives. If the two disagree, this one is the code's account and wins.

## The one rule

**State that git or tmux already holds durably is never mirrored into mux-ai's own
files.** git owns worktrees and branches. tmux owns sessions and the processes inside
them. mux-ai reads both, every time it needs them, and keeps nothing of its own that
could disagree with them.

This rule exists because it was broken once, expensively. See `POSTMORTEM.md`.

## Process model

No daemon. Every `muxai` invocation is a fresh, short-lived process that shells out to
`git` and `tmux` and exits. The dashboard is the only long-running one, and it is still
just a loop around those same two subprocesses.

There is exactly one long-lived thing: a dedicated tmux server on the socket `muxai`
(`tmux -L muxai`), isolated from the user's own tmux config and sessions. It outlives
every muxai process. Killing every muxai process loses nothing; killing that tmux
server kills every agent.

## State ownership

| State | Owner | How mux-ai reads it | Survives |
|---|---|---|---|
| Worktree exists, its path, its branch | git | `git worktree list --porcelain` | everything except `git worktree remove` |
| Session exists, its cwd, its pane process | tmux (socket `muxai`) | `tmux list-sessions -F …` | muxai exiting; **not** the tmux server dying |
| The command to re-run in a stopped worktree | `~/.local/state/muxai/sessions.json` | `SessionStore` | best-effort only |
| Everything on screen | derived per-frame | — | nothing; recomputed every 2s |

The JSON store is a **metadata cache, not a source of truth.** It holds one thing the
dashboard cannot re-derive: which command was originally run in a session, so a stopped
worktree can be restarted with the same agent. Lose the file and mux-ai degrades to
restarting everything as `claude`. Nothing else breaks, and no work is lost.

Because several muxai processes share that one file and `save()` rewrites it whole,
`add`/`remove` re-read it immediately before writing. That narrows the lost-update
window to a single syscall pair rather than a whole process lifetime. It is not a lock,
and it does not need to be — the file is no longer load-bearing.

## Dashboard data flow

```
  every 2s ─── discover(repo_root) ──────────────┐
                 │                               │
                 ├─ git worktree list --porcelain│   keep those under
                 │                               ├─  <repo>/.muxai/worktrees
                 └─ tmux list-sessions -F        │   union by name
                       (name + session_path)     │
                                                 ▼
                                          Vec<Tile>{name, path, running}
                                                 │
  every 300ms ─── tmux capture-pane -p ──────────┤   (running tiles only)
                                                 │
  every 300ms ─── tmux resize-window ────────────┤   (tile-sized windows)
                                                 │
                                                 ▼
                                            ratatui grid
```

The union has two directions, and both matter:

- A worktree with **no** live session is a real thing that happened — the agent exited,
  or the tmux server was restarted. It renders as a `(stopped)` tile. Enter restarts it
  in place. It is never silently dropped, because the worktree is the work.
- A live session whose worktree was **deleted underneath it** still has a running agent
  holding state. It gets a tile too, scoped by `session_path` being under this repo's
  `.muxai/worktrees`.

Repo scoping happens here, against git, not by filtering the store. `find_repo_root`
resolves `git rev-parse --git-common-dir` and takes its parent, so it returns the *main*
worktree's root even when muxai is run from inside a linked worktree — which is the
normal way to use it. `--show-toplevel` returns the linked worktree instead and is the
wrong call for this.

## Rendering

`capture-pane` polling at 300ms, not a real multiplexed PTY stream. Latency is a few
hundred ms. `FUTURE.md` item 1 is the upgrade to tmux control-mode.

Two things make the tiles legible:

- **`pick_cols`** chooses a column count by tile *visual* aspect ratio (characters are
  about twice as tall as wide), targeting an 80x24 shape, with a penalty for empty
  cells. Three sessions on a tall narrow terminal stack vertically; on a wide one they
  sit side by side.
- **`resize_window`** sizes each tmux window to the tile it renders into, so the agent
  inside wraps its own output at the tile width instead of mux-ai re-wrapping 80-column
  text into a 44-column cell. This requires `window-size manual`, which permanently
  marks the window manually-sized; the `client-attached` and `client-resized` hooks
  running `resize-window -A` are the undo, so attaching gives you the full terminal
  back.

## Navigation

One-directional, by design. Dashboard → `Enter` → attached session → `C-\` → dashboard.
`C-\` is bound with `-n` (no prefix) server-wide on the muxai socket, which is the whole
reason for the dedicated socket. `q` in the dashboard is the only quit.

## Resource accounting

`muxai status` — `du -sk` per worktree, split into total vs reclaimable (`target`,
`node_modules`, `.venv`, `dist`, `.next`, `__pycache__`), plus shared caches reported
for context and never touched. Memory is RSS summed over each session's pane process
tree via `sysinfo`, against a hardcoded 32GB budget.

`muxai reset` deletes only reclaimable dirs, prunes worktree registrations git already
knows are dead, and drops store records whose **worktree** is gone. It is keyed on the
worktree, never on tmux liveness — an agent exiting is not a reason to forget its work.

## Known structural gaps

These are properties of the current design, not bugs to be filed and forgotten. See
`plans/2026-08-19-stock-take.md` for the full accounting.

- **Session names are global on the tmux socket, but worktree names are per-repo.** Two
  repos both wanting a session called `fix` collide, and `create_session` guards on the
  global store, so the second one is refused with a confusing error.
- **Restart starts a fresh agent.** The `(stopped)` tile's Enter runs the stored command
  from scratch; it does not resume the agent's own conversation (`claude --continue`).
  The worktree survives, the conversation does not.
- **`k` removes the worktree but leaves the branch.** Deliberate — the commits are the
  point — but it accumulates branches.
- **Worktrees isolate source state, not execution.** There is no sandbox. An agent in
  full-auto mode can reach anything the user can.
- **The tmux server is a single point of failure.** Every agent lives on one socket. If
  that server dies, every session dies with it; the worktrees survive and now render as
  `(stopped)` tiles, which is the intended failure mode, but the conversations are gone.
