# mux-ai — Future / Backlog

Deferred out of the MVP on purpose (see DESIGN.md "MVP scope"). Candidates for
GitHub issues once this repo has a remote — `gh` isn't authenticated on this machine
yet (`gh auth status` reports not logged in), so these are tracked here for now.
Run `gh auth login`, then `gh repo create` + `gh issue create` per item below to
migrate them.

1. **Live tmux control-mode streaming, replacing capture-pane polling.** Current
   dashboard shells out to `tmux capture-pane -p` per session every ~300ms. Real
   fix: one persistent `tmux -C` (control-mode) connection that pushes pane output
   over a single socket, event-driven, near-zero idle cost, no polling latency.

2. **Execution sandboxing runner.** Worktrees isolate source state, not execution.
   Add a `runner` abstraction (`(worktree, runner)` per session) with a
   `sandbox-exec` (macOS seatbelt) implementation for agents run in full-auto/yolo
   mode. Docker explicitly out of scope on this machine (no Metal/GPU passthrough,
   competes with unified memory reserved for local inference) — revisit only for a
   remote Linux/CUDA runner if model-kernel work needs it.

3. **Remote runner** for the above — a session backed by a remote box instead of a
   local tmux pane, for when local execution isn't the point (CUDA, heavier
   sandboxing, etc).

4. **Merged-branch-aware `reset`.** Currently reset only reclaims known build dirs
   (`target/`, `node_modules/`, etc.) and prunes worktrees git already knows are
   gone. Extend it to detect worktrees whose branch is fully merged into the repo's
   main branch and offer to kill the session + remove the worktree entirely, not
   just its build artifacts.

5. **Cache-env injection on session creation.** `muxai new` should set
   `CARGO_TARGET_DIR` (shared, per-repo, outside any single worktree),
   `UV_CACHE_DIR`/`PNPM_HOME` equivalents, etc. in the spawned tmux session's
   environment, so every new worktree is born sharing caches instead of relying on
   the user (or the agent) having configured it. Needs a small per-toolchain
   detection step (look for `Cargo.toml`, `package.json`, `pyproject.toml` in the
   worktree) — first pass can be config-driven instead of auto-detected.

6. **Config file** (`~/.config/muxai/config.toml`): memory budget (currently
   hardcoded 32GB default in `stats::DEFAULT_MEMORY_BUDGET_BYTES`), default agent
   command, poll interval, extra reclaimable-dir patterns.

7. **Multi-repo dashboard.** MVP's session store is global but each `muxai new`
   targets one repo at a time via cwd/`--repo`; the dashboard already shows
   everything in the store regardless of repo, so this is mostly about tagging/
   filtering (`muxai dashboard --repo <path>`) once it matters in practice.

8. **Open-model fleets.** Nothing in the design prevents `muxai new kimi-session --
   kimi` or `-- opencode` today (the tmux session just runs whatever command you
   give it), but there's no convenience layer yet (e.g. a per-session "agent type"
   tag shown in the dashboard, or presets). Low priority — revisit once the MVP has
   mileage with plain `claude` sessions.

9. **Web/mobile view.** Explicitly out of scope — aoe already does this well if it's
   ever needed; mux-ai's whole reason to exist is the terminal-native grid. Note this
   rules out a *web UI*, not remote access: phone access already works over Tailscale +
   SSH with no code, documented in [`PHONE_ACCESS.md`](PHONE_ACCESS.md). Building an
   HTTP API / daemon the way OpenCode and OpenChamber do is unnecessary here because
   tmux is already the server.

10. **Small-screen ergonomics**, the friction found while documenting phone access
    (`PHONE_ACCESS.md`, "Known rough edges"). Roughly 60 lines total:
    - `muxai attach <name>` subcommand, so a phone can jump straight into a session
      instead of arrow-keying the grid (`src/cli.rs` has no `Attach` variant).
    - A non-Ctrl detach fallback alongside `C-\` (`src/tmux.rs:42`) — soft keyboards
      have no Ctrl key without the terminal app's accessory row.
    - Narrow-terminal layout: below ~50 columns, render a session list plus one large
      preview instead of the grid (`CELL_WIDTH` is 44 in `src/ui/dashboard.rs:20`).
