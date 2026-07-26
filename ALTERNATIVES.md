# mux-ai — Alternatives Survey

Goal: a "hyperclaude"-style tool — tmux-like management of multiple coding agents in
per-project git worktrees of the same repo, with lifecycle commands (start / inspect &
continue / stop) plus resource accounting (`status` for disk + memory across clones,
`reset` to reclaim).

Surveyed 2026-07-18. **Verdict: the agent-lifecycle layer exists in open source (several
mature options); the resource-accounting layer does not — that's the thin piece worth
building here.**

## Currently trying: agent-of-empires (aoe)

- Rust TUI + web UI over tmux + git worktrees.
- Agent-agnostic: manages Claude Code, OpenCode, Codex CLI, Gemini CLI, Pi, Copilot
  CLI, Mistral Vibe, Factory Droid side by side — best fit for a mixed open-model fleet.
- Installed at `~/.local/bin/aoe` (v1.13.0).
- Install note (2026-07-18): crashed on first launch with
  `dyld: Library not loaded: /opt/homebrew/opt/xz/lib/liblzma.5.dylib` — the binary
  links Homebrew's liblzma but `xz` wasn't installed. Fixed with `brew install xz`.
  Not a PATH issue.

## Other lifecycle managers (a/ start, b/ inspect+continue, c/ stop)

| Tool | Shape | Notes |
|---|---|---|
| **Claude Squad** | Terminal TUI, tmux + worktrees | Closest to hyperclaude's shape; leanest terminal option; agent-agnostic slots (Claude Code / Codex / OpenCode / Aider). Fallback if aoe doesn't stick. |
| **amux** | Single Python file, tmux | Web dashboard, watchdog, kanban, agent-to-agent REST API, mobile PWA. |
| **herdr** | Agent-aware terminal multiplexer | Persistent workspaces/tabs/panes, agent status detection. |
| **Worktrunk (`wt`)** | Rust CLI | Thin worktree wrapper: create worktree, launch agent, `wt list`. |
| **agenttools/worktree** | CLI | Worktrees + GitHub issues + Claude Code + tmux sessions. |
| **Vibe Kanban** | Kanban web board | Agent-agnostic task cards. ⚠️ Bloop shut down Apr 2026; community-maintained, hosted cloud dying. Avoid for new setups. |
| **Crystal** | Desktop GUI | ⚠️ Deprecated Feb 2026 → paid closed-source Nimbalyst. Avoid. |
| **Conductor** | macOS desktop app | Polished but not open source. |

Also: **Claude Code has built-in worktree support** in the CLI now (each agent gets its
own worktree), and its Agent tool supports `isolation: "worktree"` for background
subagents — for the single-repo/multi-project case alone, less tooling may be needed
than expected.

## The gap = what mux-ai should be

None of the above do machine-resource accounting; they track agent state, not machine
state. Build a thin (~200-line) wrapper over the chosen orchestrator, not a fork:

- `status` → `git worktree list` + per-worktree `du -sh` (report as *delta* over the
  shared `.git` object store; real cost is working copies + per-clone `node_modules` /
  build dirs) + `ps` RSS rollup per agent process tree + red/yellow/green vs. a
  configured memory budget.
- `reset` → kill stale tmux sessions, `git worktree prune`, delete merged worktrees,
  optionally nuke per-worktree dependency/build caches.

### Hardware sizing notes

- Worktrees are nearly free on disk (shared object store).
- Each CLI agent process is a few hundred MB RSS; the spikes come from builds/tests
  *inside* worktrees. A 32–64 GB laptop handles 3–5 parallel agents fine — the
  "100 GB RAM floor" instinct is too conservative for agents alone.
- The 256 GB M3 Ultra budget matters for **local inference**: a 100B+ model via
  MLX/llama.cpp claims 60–150 GB+ of unified memory. `status` should show an
  "inference reservation" vs. "agent overhead" split.

## Open-model Claude-Code-equivalents (for mixed fleets)

Two flavors:

1. **Vendor CLIs**: Kimi CLI (Moonshot K2.5, 256K ctx), Qwen Code (Apache 2.0,
   Gemini-CLI fork, tuned for Qwen3-Coder). GLM / MiniMax lack flagship first-party
   CLIs of the same maturity.
2. **Model-agnostic harnesses**: OpenCode (MIT, ~161K stars — the de facto open-source
   Claude Code), Crush, Kilo CLI, Aider — all BYOK; point at Kimi/GLM/Qwen/MiniMax
   endpoints or local models.

Kimi, GLM, and MiniMax also expose **Anthropic-compatible APIs**, so Claude Code itself
can drive them via `ANTHROPIC_BASE_URL` — same muscle memory, different model.

Since aoe / Claude Squad spawn arbitrary agent commands per slot, the open-agent story
falls out for free: slot 1 `claude`, slot 2 `kimi`, slot 3 `opencode` against a local
Qwen served from the same box.

## Sources

- https://github.com/andyrewlee/awesome-agent-orchestrators
- https://github.com/bradAGI/awesome-cli-coding-agents
- https://vibecodinghub.org/tools/claude-squad
- https://github.com/BloopAI/vibe-kanban
- https://www.threads.com/@boris_cherny/post/DVAAnexgRUj/ (Claude Code built-in worktrees)
- https://pinggy.io/blog/top_cli_based_ai_coding_agents/
- https://kilo.ai/articles/best-cli-coding-agents
- https://devtoollab.com/blog/open-source-alternatives-claude-code
- https://nimbalyst.com/blog/best-agent-management-tools-2026/
