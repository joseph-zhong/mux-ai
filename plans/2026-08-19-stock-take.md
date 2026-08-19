# Stock-take — 2026-08-19

Where mux-ai actually is 25 days after the first commit, what is load-bearing, what is
weak, and which of the three stated future directions are worth the next month.

Read `ARCHITECTURE.md` first for the state model, and `POSTMORTEM.md` for the incident
that prompted this.

## What exists

1,396 lines of Rust across 8 files, 19 commits, 162 crates in the lock file, 10 tests.
One binary, no daemon, no server, no config file.

| Capability | State | Notes |
|---|---|---|
| Grid dashboard, live tail per session | works | `capture-pane` polled at 300ms |
| Aspect-aware 2D grid layout | works | `pick_cols`, 9 tests, the best-tested code here |
| Tile-sized tmux windows | works | agents wrap at tile width instead of being re-wrapped |
| Attach / detach on one unprefixed key | works | `C-\` server-wide on a private socket |
| Worktree per session, branch per worktree | works | `<repo>/.muxai/worktrees/<name>` |
| Repo-scoped session discovery | works, just fixed | derived from git + tmux, PR #11 |
| Stopped-session tiles + restart | works, shallow | restarts the command, not the conversation |
| Disk accounting (`status`) | works | `du -sk`, shared-cache split |
| Memory accounting (`status`) | works | RSS over pane process trees, 32GB hardcoded |
| Reclaim (`reset`) | works | build dirs only, dry-run by default |
| Model / agent choice | absent | `muxai new x -- <anything>` works, nothing above it |
| Remote or phone access | absent | branch `phone-access` has a design doc only |
| Sandboxing | absent | worktrees isolate source, not execution |

The honest summary: the **local, single-machine, single-user terminal UX is done and
good**. Everything else is a design doc.

## Weaknesses, ranked by how much they will hurt

**1. The tmux server is a single point of failure for every agent's conversation.**
All sessions live on one socket. If that server dies, every conversation dies with it.
Worktrees survive — the post-mortem's fix means they now show up as `(stopped)` tiles
rather than vanishing — but `Enter` starts a *fresh* agent in that worktree. The commits
are safe; the reasoning that produced them is not. This is the single largest gap
between "my work is safe" and what actually happens.

*Fix:* restart should resume, not restart — `claude --continue` in the worktree, and per
agent-type equivalents. Requires knowing the agent type, which requires the next item.

**2. There is no notion of an "agent type", only a command string.** The store holds
`command: "claude"` as opaque text. Nothing can ask "is this a Claude session?", so
nothing can resume it, label it, cost it, or route it. Every one of the three future
directions below needs this, and it is perhaps 50 lines.

**3. Session names are global on the tmux socket; worktree names are per-repo.** Two
repos both wanting a session called `fix` collide, and `create_session` guards against
the *global* store, so the second is refused with a confusing "already exists". The
dashboard is now repo-scoped, which makes the collision more likely to be encountered
and harder to understand. *Fix:* namespace tmux sessions as `<repo-slug>/<name>`,
display the bare name.

**4. Nothing tests anything that touches tmux.** 9 of 10 tests cover pure grid-layout
arithmetic. `discover()`, the resize/hook dance, attach/detach, and the store's
concurrent-write behaviour are all verified by hand. The post-mortem bug lived in
exactly this untested band. *Fix:* integration tests against a scratch tmux socket —
cheap, and they would have caught it.

**5. `capture-pane` polling, not streaming.** Known, documented in `FUTURE.md` #1.
Costs a few hundred ms of latency and one subprocess per session per 300ms. Not urgent
at 7 sessions; it is the wall at 30.

**6. No execution sandbox.** An agent in full-auto reaches everything the user does.
`FUTURE.md` #2 has the design (`sandbox-exec`). This is the item that blocks running
many agents unattended, which is the entire premise of a grid dashboard.

**7. Branches accumulate.** `k` removes the worktree, never the branch — deliberate,
since the commits are the point, but this repo already has 13 branches for 6 worktrees.
`FUTURE.md` #4 (merged-branch-aware reset) covers it.

**8. 32GB memory budget is hardcoded.** `FUTURE.md` #6.

## The three future directions

### a) Model-agnostic support

**Verdict: do it, and do it first — but as plumbing, not as a feature.**

The tmux session already runs any command, so "model-agnostic" is not about execution.
It is about the four things listed in weakness #2: resume correctly, label the tile,
attribute cost, and eventually route. Concretely:

- an `agent` field on `Session` (`claude` | `codex` | `opencode` | `custom`),
- a small per-agent table: how to start, how to resume, how to detect "idle vs working"
  from the pane, where its session files live,
- tile shows the agent, `muxai new --agent`, resume-on-restart.

This is the enabling work for both of the other two directions, it is small, and it pays
for itself immediately by fixing weakness #1.

### b) Tailscale access — tmux via phone

**Verdict: worth it, and it is mostly not mux-ai's problem.**

The mechanism is already there: sessions are real tmux sessions on a known socket, so
`ssh` over a Tailscale tailnet and `tmux -L muxai attach -t <name>` works today with
zero code. The `phone-access` branch documents this. What mux-ai would add:

- a `muxai attach <name>` CLI so the socket is not a thing you have to know,
- readable output on a phone-shaped viewport, which the aspect-aware grid already half
  solves — a phone is one tall narrow tile, and `pick_cols` already stacks for that,
- the honest constraint: typing prose to an agent on a phone keyboard is miserable, so
  the realistic phone use is **triage, not authoring** — see what is running, read the
  last output, approve or kill.

Scoping it to triage makes it a weekend, not a project. Scoping it to "code from your
phone" makes it a product nobody finishes.

### c) Bring-your-own-GPU for choose-your-own-model

**Verdict: build the seam, do not build the substance. The economics do not currently
work for the individual user.** Full argument in
`plans/2026-08-19-economics.md`.

Short version: a rented H100 at ~$2–3/hr is ~$1,500–2,200/month at full-time
utilisation, and a single developer cannot keep it busy — the duty cycle of one person's
agent work is maybe 10–20%, and idle GPU time is a pure loss that an API provider
absorbs across thousands of tenants. Local inference on unified-memory Apple hardware
avoids the idle cost entirely but currently trades away the capability that makes
agentic coding work at all. The seam worth building is (a), the agent abstraction: it
makes "point this session at an OpenAI-compatible endpoint on my own box" a
configuration line rather than a project.

## Suggested order

1. **Agent abstraction** (a) — unblocks resume, labels, cost, routing. Small.
2. **Resume-on-restart** — closes the largest real gap in the product's promise.
3. **tmux integration tests** — the untested band is where the last bug lived.
4. **Session namespacing** — before a second repo makes it a support question.
5. **Tailscale triage path** (b) — mostly documentation plus `muxai attach`.
6. **Sandbox** (`FUTURE.md` #2) — the real unlock for unattended parallelism.
7. Everything else.

BYO-GPU (c) is deliberately not on this list as a build item. Step 1 makes it a
config line if and when the economics change.
