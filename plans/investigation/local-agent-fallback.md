# mux-ai — Agent backends & the Anthropic-outage fallback

Status: **design proposal**, not implemented. Researched 2026-08-19.

Companion to [`DESIGN.md`](../../DESIGN.md) (architecture), [`ALTERNATIVES.md`](../../ALTERNATIVES.md)
(why mux-ai exists), [`FUTURE.md`](../../FUTURE.md) items 5 and 8 (cache-env injection,
open-model fleets — this doc supersedes and expands item 8).

---

## 1. Problem

`muxai new <name>` creates a worktree and starts `claude` in a tmux session. That
default is hardcoded in two places (`main.rs:28` and `ui/dashboard.rs:259`). When
Anthropic is unavailable, **every session in the grid is simultaneously dead** — the
worktrees survive, the tmux sessions survive, the agents inside them are useless. The
fleet has a single point of failure and no degraded mode.

This is not hypothetical. 2026 outage record:

| Date | Duration | Scope |
|---|---|---|
| 2026-01-22 | multi-hour | Claude.ai + API |
| 2026-06-02 | ~5h 30m | Claude.ai, API, Console, Claude Code |
| 2026-07-29→30 | network failures | multi-service |
| 2026-08-16 | ~36 min | auth failure — Claude.ai, Claude Code, Cowork |

Sources: [BleepingComputer](https://www.bleepingcomputer.com/news/artificial-intelligence/anthropic-confirms-claude-is-down-in-major-outage-affecting-multiple-services/),
[StatusGator outage history](https://statusgator.com/services/claude/outage-history),
[explainX Aug 16 writeup](https://www.explainx.ai/blog/claude-outage-authentication-august-16-2026).

Two distinct failure modes, and they want different answers:

1. **Provider-side outage** (5xx / 529 / auth failure). Minutes to hours. Fix: route
   elsewhere. Nothing about the worktree or the task changes.
2. **Quota exhaustion** (Max 5x/20x rolling-5h + weekly caps). Predictable, self-inflicted,
   often mid-task. Fix: same as above, but you want it *cheap*, because it happens
   routinely and you were already paying for the primary.

A third, weaker motivation: **cost arbitrage**. Not every session in a 6-tile grid needs
frontier intelligence. A "run the test suite until it's green" session and a "design this
subsystem" session are not the same workload, and pricing them the same is waste.

### The requirement that constrains everything

> "as simply as possible … as cost-effective as possible, but keeping high frontier level
> output and not compromising there."

These pull against each other. The honest resolution is that **frontier quality and local
inference are not simultaneously achievable on one M3 Ultra today** (§6 proves this with
numbers). So the design is a *ladder*, not a switch: each rung trades a little quality for
a lot of independence, and you descend only as far as the outage forces you.

---

## 2. Success criteria

Define these first so the plan is testable rather than aspirational.

| # | Criterion | Check |
|---|---|---|
| S1 | Anthropic 100% down → a new session still starts and does useful work | `unset` nothing; kill network to api.anthropic.com via `/etc/hosts`; `muxai new probe --agent glm` produces a passing commit |
| S2 | Zero-network → a session still starts and does useful work | Wi-Fi off; `muxai new probe --agent local` edits a file and runs tests |
| S3 | Switching a session's backend is one flag, not a config safari | `muxai new x --agent <preset>`; no manual env export, no per-repo config edit |
| S4 | The grid tells you what's driving each tile | dashboard tile shows agent tag; `muxai status` groups by agent |
| S5 | Backup tier costs < 15% of primary | primary Max 20x $200/mo; backup target ≤ $20/mo (§7) |
| S6 | Backup quality is measured on *our* repo, not on a vendor benchmark | §8 eval harness produces a per-model pass rate on real mux-ai tasks |

Non-goals: replacing Claude as primary; a router that silently switches models mid-task
(surprise model swaps make debugging agent behaviour impossible — switching is explicit);
multi-machine inference clusters; a web UI.

---

## 3. The fallback ladder

Four rungs, ordered by *how much of your stack changes*. Rung 1 is a config line; rung 4
is a different universe. Descend only as far as needed.

```
 R0  claude  →  Anthropic                      frontier, primary, $200/mo
 R1  claude  →  Anthropic-compatible provider  ~frontier, $18/mo, 1 env var
 R2  opencode → hosted open model (OpenRouter) ~frontier, per-token, different harness
 R3  opencode → local MLX on the M3 Ultra      good-not-frontier, $0, works offline
```

**Why the ladder rather than a router.** An automatic router (LiteLLM/claude-code-router
picking a backend per request) is more machinery and *less* useful here: mux-ai sessions
are long-horizon and stateful, so a mid-task backend swap changes tool-calling behaviour
and prompt-cache economics underneath a running agent. Explicit per-session backend
selection is simpler to build, simpler to reason about, and matches the existing
`(worktree, runner)` seam DESIGN.md already committed to.

### R0 — free hardening, do this regardless

Claude Code 2.1.166+ has a built-in `fallbackModel` chain (up to three models, tried on
overload / non-retryable errors) settable in `~/.claude/settings.json` or via
`--fallback-model`. Costs nothing, covers the 529-overload case, does **not** cover a
full Anthropic outage (auth failures and transport errors surface immediately rather than
falling back). Set it and stop thinking about it.
([digitalapplied](https://www.digitalapplied.com/blog/claude-code-safe-mode-fallback-models-production-resilience-guide),
[ofox](https://ofox.ai/blog/claude-code-fallbackmodel-3-tier-failover-2026/))

### R1 — same harness, different provider (**the primary recommendation**)

Several vendors expose an **Anthropic-compatible `/v1/messages` endpoint**, so the
`claude` binary itself — same muscle memory, same skills, same `CLAUDE.md`, same tmux
session shape — drives a different model with two environment variables:

```sh
ANTHROPIC_BASE_URL=https://api.z.ai/api/anthropic
ANTHROPIC_AUTH_TOKEN=<provider key>      # NOT ANTHROPIC_API_KEY
```

Setting `ANTHROPIC_AUTH_TOKEN` (not `ANTHROPIC_API_KEY`) matters: leaving `API_KEY`
populated lets Claude Code fall back to Anthropic auth and silently defeat the point.

| Provider | Endpoint | Notes |
|---|---|---|
| Z.ai (GLM) | `https://api.z.ai/api/anthropic` | Most mature drop-in; GLM-5.2 natively parses Anthropic `tools`/`tool_choice` schemas |
| Moonshot (Kimi) | `https://api.moonshot.ai/anthropic` | Documented for Claude Code; canonical reference docs still requested upstream |
| MiniMax | `https://api.minimax.io/anthropic/v1/messages` | Works; the CN endpoint is reportedly non-standard — pin the international one |

Sources: [Z.ai devpack](https://docs.z.ai/devpack/quick-start),
[Moonshot Kimi-K2 issue #129](https://github.com/MoonshotAI/Kimi-K2/issues/129),
[MiniMax Anthropic SDK docs](https://platform.minimax.io/docs/api-reference/text-anthropic-api),
[pi issue #2768 (MiniMax CN quirk)](https://github.com/earendil-works/pi/issues/2768).

**This rung is where "as simply as possible" lives.** No new binary to learn, no new
config format, no MCP re-plumbing. In mux-ai terms it is a two-entry env map on the
session — nothing else in the architecture moves.

### R2 — different harness, hosted open model

R1 still depends on the `claude` binary and on Anthropic-compat endpoints staying
compatible. R2 removes that: a second, genuinely independent harness.

**OpenCode** is the pick (§5). It runs any of 75+ providers including local servers,
has a headless mode (`opencode run -m provider/model "<prompt>"`) and a persistent
server (`opencode serve` + `opencode run --attach http://localhost:4096`) that avoids MCP
cold-start on every invocation — which matters when a grid of six tiles all spawn at once.

Paired with **OpenRouter** as the model plane, this rung also gives *provider-layer*
failover for free: OpenRouter deprioritises any provider that erred in the last 30s, and
an opt-in `models: [...]` array does model-layer fallback on downtime, rate limits,
context-length errors and moderation refusals — billing only the successful run.
([OpenRouter fallbacks](https://openrouter.ai/docs/guides/routing/model-fallbacks),
[reliability writeup](https://openrouter.ai/blog/insights/reliability-failover/))

### R3 — local, on the M3 Ultra

The only rung that survives *your* network being down, and the only one with zero
marginal cost. It is also the one where the "no compromise on frontier output" constraint
genuinely cannot be met (§6). Treat it as the **lifeboat**, not the daily driver.

---

## 4. Harness survey (the OpenCode / OpenChamber question)

| Harness | License | Shape | Local models | Headless | Verdict for mux-ai |
|---|---|---|---|---|---|
| **OpenCode** | MIT | TUI, provider-agnostic (75+) | Ollama, LM Studio, llama.cpp, vLLM, Exo, llama-swap, oMLX, MLX-VLM | `opencode run`, `opencode serve`, ACP over stdio | **Adopt.** Most active, most stars (~165k), MIT, explicit local-server support, real headless story |
| **OpenChamber** | open source | Desktop/web/VS Code **GUI over OpenCode** | inherits OpenCode | n/a (it's the UI layer) | **Skip.** It is a GUI competing with mux-ai's own dashboard; mux-ai is terminal-native by charter. Worth stealing *ideas* from (multi-run/fusion across up to 5 models, changes-walkthrough) |
| Crush | FSL→MIT after 2y | TUI, multi-model, LSP+MCP | yes | partial | Good; second choice. Licence is the only real knock |
| Aider | Apache-2.0 | diff-centric, not agentic | yes | yes | Different tool for a different job. No tagged release since Aug 2025 — velocity has visibly slowed |
| Goose (Block) | Apache-2.0 | general agent, not code-first | yes | yes | Overshoots; better for non-code automation |
| Qwen Code | Apache-2.0 | Gemini-CLI fork tuned for Qwen | yes | yes | Narrow. Useful as a *model-specific* preset, not a general backup |
| Kimi CLI / Kimi Code | vendor | first-party K-series | no | yes | Fine if you buy the Kimi plan; single-vendor lock is the thing we're escaping |

Sources: [Pinggy 2026 roundup](https://pinggy.io/blog/best_open_source_cli_coding_agents/),
[OpenCode providers docs](https://opencode.ai/docs/providers/),
[OpenCode CLI docs](https://opencode.ai/docs/cli/),
[OpenChamber](https://github.com/openchamber/openchamber).

**Important framing correction:** OpenChamber is *not* an alternative to Claude Code — it
is a GUI for OpenCode. The inspiration the user is reaching for ("OpenCode or
OpenChamber") is really: **OpenCode is the harness; OpenChamber demonstrates the
multi-model UX on top of it.** mux-ai already owns the UX layer, so it wants the harness
and should build the multi-model UX itself.

---

## 5. Hosted open-frontier models — where the quality actually is (Aug 2026)

The honest headline: **hosted open-weight models have closed most of the gap; local
inference has not.** These two facts get conflated constantly and they are not the same
claim.

| Model | Open weights | Terminal-Bench 2.1 | Other | API $/M in → out |
|---|---|---|---|---|
| GPT-5.6 Sol | no | 89.5% | — | — |
| Grok 4.6 | no | 88.4% | — | — |
| **Kimi K3** (2.8T MoE, 104B active, 1M ctx) | **yes**, 2026-07-27 | **88.3%** | leads Frontend Code Arena (1679) | $2.60 → $13 (OpenRouter) |
| **DeepSeek V4 Pro** | **yes** | 87.9% | 80.6% SWE-bench Verified | ~$0.66 → $1.98 off-peak; ~$1.32 → $3.96 peak |
| Claude Opus 5 | no | 84.64%* | — | $5 → $25 |
| **GLM-5.2** (753B) | **yes** | 81.0% | 62.1% SWE-bench Pro (top open) | bundled in coding plan |
| Claude Opus 4.8 | no | 74.6% | — | $5 → $25 |

\* Opus 5's 84.64% run used Opus 4.8 as a refusal fallback; counting those nine passes as
failures gives 81.27%.

Sources: [Terminal-Bench 2.1 leaderboard analysis](https://benchlm.ai/benchmarks/valsterminalbench21),
[CodingFleet TB leaderboard](https://codingfleet.com/blog/terminal-bench-leaderboard-2026/),
[Kimi K3 open weights](https://explainx.ai/blog/kimi-k3-open-weights-2-8-trillion-parameters-july-2026),
[DeepSeek pricing](https://deepseek.ai/pricing),
[OpenRouter Kimi K3](https://openrouter.ai/moonshotai/kimi-k3),
[Anthropic pricing](https://www.anthropic.com/pricing).

**Benchmark caveat, stated once and meant:** Terminal-Bench and SWE-bench measure
short-horizon, well-specified, English-language tasks in throwaway containers. They do not
measure the thing mux-ai sessions actually do — hours-long work in a real repo with a
`CLAUDE.md`, skills, MCP servers and accumulated context. A model 4 points higher on TB2.1
can still be materially worse at turn 200. §8 exists because of this.

Reported real-world shape of the gap: a good open 80B MoE needs *more turns* (order 150 vs
120) to reach a comparable success rate — you pay in latency and token volume, not
necessarily in outcomes.
([braindetox agentic-coding feasibility study](https://braindetox.kr/en/posts/local_llm_agentic_coding_2026.html))

---

## 6. Local inference on the M3 Ultra — the honest numbers

This is the section where "frontier, locally, on this box" has to be either proven or
abandoned. It's abandoned — but the consolation prize is genuinely good.

### What does not fit

| Model | Smallest usable form | Footprint | On a 256GB M3 Ultra? |
|---|---|---|---|
| Kimi K3 (2.8T) | REAP-80 expert-pruned, MLX | **350 GB** | **No** — sized for a 512GB machine, and 80% of experts pruned means it is not K3 any more |
| Kimi K3 | Unsloth 1-bit GGUF | 466–620 GB | No |
| Kimi K3 | Q4 GGUF | ~1.5 TB | No |
| Qwen3-Coder-480B-A35B | 4-bit | ~960 GB total | No |
| GLM-5.2 (753B) | 4-bit | ~400 GB (est.) | No |
| DeepSeek V4 Pro | 4-bit | > 256 GB | No |

Sources: [Kimi-K3-REAP80-MLX](https://huggingface.co/pipenetwork/Kimi-K3-REAP80-MLX-mxfp4-q8),
[modelfit K3 local math](https://modelfit.io/blog/can-you-run-kimi-k3-locally/),
[popularai K3 hardware](https://www.popularai.org/p/kimi-k3-local-ai-hardware-requirements).

**Conclusion: the frontier open models are not local models.** Every one of them is a
datacenter model that happens to have downloadable weights. Testing K3 or DeepSeek V4
locally is not the cost-effective path — it is the expensive path (§8 has the cheap one).

### What does fit, and fits well

| Model | Params | 4-bit MLX | Context | Quality signal |
|---|---|---|---|---|
| **Qwen3-Coder-Next** | 80B MoE, **3B active** | ~17.5 GB weights, **~46 GB** with working KV cache | 256K | **>70% SWE-bench Verified** (SWE-agent scaffold); 44.3% SWE-bench Pro — best open-weight at its compute class |
| MiniMax-M2.5-REAP-39 | pruned MoE | ~73 GB | — | pruned; treat as unvalidated |
| Qwen3.5-122B-A10B | 122B MoE | ~65–70 GB | — | strong generalist, ~35 tok/s reported |
| Qwen3-Coder-30B-A3B | 30B MoE | ~19 GB | 256K | ~84 tok/s; the "fast tier" |

Sources: [Qwen3-Coder-Next blog](https://qwen.ai/blog?id=qwen3-coder-next),
[Unsloth run-locally guide](https://unsloth.ai/docs/models/qwen3-coder-next),
[HF Qwen3-Coder-Next](https://huggingface.co/Qwen/Qwen3-Coder-Next).

**Pick: Qwen3-Coder-Next 80B-A3B, 4-bit MLX.** ~46 GB of 256 GB — leaves ~200 GB free,
which matters because that headroom is what the machine is *for* (DESIGN.md's hardware
sizing note). 3B active parameters means MoE decode speed far above what the parameter
count suggests. 256K context is enough for real agent sessions. Agentically trained
(executable task synthesis + environment interaction + RL), so tool-calling stability —
the actual failure mode of local coding agents — was a training objective, not an accident.

### The real local bottleneck: prefill, not decode

Everyone quotes decode tok/s. For agentic coding it is the wrong number.

- Prefill is **FLOPs-bound, not bandwidth-bound**, and Apple Silicon is bandwidth-rich /
  FLOPs-poor. Reported prefill on a large MLX model at long context: **~4.5 tok/s** —
  that is minutes of dead time before the first output token on a large agent prompt.
- At 128K context, **>70% of memory bandwidth goes to reading the FP16 KV cache**, which
  makes weight quantisation progressively irrelevant as the session grows.
- Default `PROMPT_PROCESSING_CHUNK_SIZE` of 512 wastes bandwidth on GPU/Python sync;
  raising it to 8192 has been measured at **up to 1.5× faster prefill**.

Sources: [mlx-lm #1480](https://github.com/ml-explore/mlx-lm/issues/1480),
[mlx-engine #245 "agent mode"](https://github.com/lmstudio-ai/mlx-engine/issues/245),
[mlx #3209 systematic M3 Ultra benchmarks](https://github.com/ml-explore/mlx/discussions/3209),
[lmstudio-js #507 chunk size](https://github.com/lmstudio-ai/lmstudio-js/issues/507).

**Design consequences, and these are the load-bearing ones:**

1. A local session must run with **aggressively smaller context** than a Claude session.
   Long-context habits that are free on Anthropic's infra are ruinous locally.
2. **KV-cache reuse across turns is mandatory**, not an optimisation. A server that
   re-prefills the whole conversation each turn makes local agentic coding unusable.
   This is a hard requirement on whichever local server we pick.
3. Local sessions are **latency-poor and throughput-fine** — they suit "grind on a
   well-specified task" (fix these tests, mechanical refactor, port this pattern), not
   "explore this unfamiliar subsystem."
4. **One local model, loaded once, shared by all local tiles.** N tiles × N model loads
   would blow the memory budget and thrash. The serving process is a machine-level
   singleton that sessions connect to — which is a new kind of resource for
   `muxai status` to account for (§9.4).

### Local serving choice

| Option | Port | For us |
|---|---|---|
| **LM Studio** (MLX engine) | 1234 | **Pick.** GUI model management, OpenAI-compatible server, multi-slot KV caching work in flight, tunable prefill chunk size. Lowest operational burden |
| `mlx_lm.server` | 8080 | Leanest, scriptable, no GUI. Good second |
| llama.cpp server | 8080 | Mature, GGUF; MLX generally faster on Apple Silicon |
| Ollama | 11434 | Easiest, but less MLX-native and weaker long-context control |

OpenCode supports all of these as providers out of the box, and requires ≥64K context —
Qwen3-Coder-Next's 256K clears it comfortably.

---

## 7. Cost

| Tier | Option | $/mo | Notes |
|---|---|---|---|
| R0 primary | Claude Max 20x | $200 | equivalent to ~$600–1500/mo of API tokens for heavy users; keep it |
| R1 backup | **GLM Coding Plan Lite** | **$18** ($12.60 annual) | Anthropic-compatible; drop-in for `claude` |
| R1 backup | Kimi Code plan | ~$19 | tiered; K3 unlocked at the ¥99 tier |
| R1 backup | MiniMax coding plan | $20 | works with Claude Code, Cursor, Cline, OpenCode |
| R2 eval | OpenRouter | pay-per-token | one key, every model, no subscription — the *evaluation* plane |
| R2 speed | Cerebras Code Pro | $50 | Qwen3-Coder at claimed ~2000 tok/s, 131K ctx; **the 2k TPS claim is contested in practice** |
| R3 local | Qwen3-Coder-Next on the Mac Studio | **$0** | hardware already owned; electricity only |

Sources: [codingplan.org comparison](https://codingplan.org/en),
[Cerebras Code](https://www.cerebras.ai/blog/introducing-cerebras-code),
[InfoWorld's dissent on Cerebras throughput](https://www.infoworld.com/article/4055909/down-and-out-with-cerebras-code),
[Claude Code pricing](https://www.anthropic.com/pricing).

**Recommended spend: $200 + $18 = $218/mo** for full continuity, plus a small
pay-as-you-go OpenRouter balance for evaluation. That is **S5 met at 9%**. The local rung
adds $0 and buys offline capability.

Prices in this table are the single most volatile thing in this document — DeepSeek raised
prices 50–1100% on 2026-08-16 and introduced peak/off-peak tiers. Re-verify before buying.

---

## 8. The cheap way to actually test K3 / DeepSeek V4 / GLM

The question "what is the most cost-effective way to test Kimi K3 or DeepSeek V4" has a
three-step answer, and the expensive mistake is starting at step 3.

**Step 1 — Triage on public leaderboards, spend $0.**
[LMArena](https://lmarena.ai) for vibes and for the *specialised* boards (Frontend Code
Arena, Coding Arena) — the aggregate text Elo is the wrong signal for coding. Terminal-Bench
2.1 and SWE-bench Pro for agentic capability. Use these only to pick 3–4 candidates. Never
to pick the winner.

**Step 2 — Evaluate on your own repo through OpenRouter, spend ~$5–20.**
One API key, every candidate model, no subscription, per-token billing. This is the
cost-effective answer to the user's question: *do not buy a coding plan to evaluate a
model, and do not download 350 GB of weights to evaluate a model.* Rent it by the token
first.

The eval must be **mux-ai's own tasks**, because that's what S6 demands:

```sh
# proposed: scripts/eval-agents.sh  (a plan artefact, not yet written)
#   for each MODEL in kimi-k3, deepseek-v4-pro, glm-5.3, qwen3-coder-next:
#     for each TASK in tasks/*.md:           # real, previously-completed mux-ai tasks
#       muxai new eval-$MODEL-$TASK --agent openrouter:$MODEL -- <headless opencode run>
#       record: cargo build && cargo test exit code, turn count, wall time, $ spent
```

Score on four axes, in this order of importance:

1. **Did it finish?** (task completed without human rescue)
2. **Tool-call stability** — malformed calls, loops, giving up. This is where open models
   most often fail, and it is invisible on benchmarks.
3. **Turns and wall time** — expect ~1.25× Claude's turn count as normal, not as failure.
4. **Dollars per completed task** — the only cost metric that means anything.

Reusing mux-ai's own git history is the trick that makes this cheap: tasks with known-good
outcomes and a `cargo test` oracle already exist in the repo. No benchmark authoring.

**Step 3 — Buy a subscription only for the winner**, and only if step 2 shows it clears
the bar. For the *local* rung there is no step 3: download Qwen3-Coder-Next (~46 GB) and
run the same harness against `localhost`.

---

## 9. Changes to mux-ai

Deliberately small. The existing architecture already anticipated this — DESIGN.md's
"keep a runner seam" note. What follows extends the *command* seam, not the runner seam,
because backend choice is about what runs in the pane, not where the pane runs.

### 9.1 An agent preset is `(command, env)`

Today `Session.command: String` is the whole story and `tmux::new_session` passes no
environment. tmux's `new-session -e KEY=VAL` (3.2+) injects env directly into the session,
which is exactly the seam needed — and it is the same mechanism FUTURE.md item 5 wants for
`CARGO_TARGET_DIR`. **Build it once, use it for both.**

Verified on this machine (tmux 3.7b):

```sh
$ tmux -L muxaitest new-session -d -s envtest -e FOO=bar 'sh -c "sleep 5"' \
    && tmux -L muxaitest show-environment -t envtest FOO
FOO=bar
```

```rust
// src/agent.rs  (new, ~60 lines)
pub struct Agent {
    pub name: String,             // "claude" | "glm" | "kimi" | "local" | ...
    pub command: String,          // "claude" | "opencode"
    pub env: Vec<(String, String)>,
}
```

Built-in presets:

| Preset | command | env |
|---|---|---|
| `claude` (default) | `claude` | — |
| `glm` | `claude` | `ANTHROPIC_BASE_URL=https://api.z.ai/api/anthropic`, `ANTHROPIC_AUTH_TOKEN=$ZAI_API_KEY`, `ANTHROPIC_API_KEY=` |
| `kimi` | `claude` | `ANTHROPIC_BASE_URL=https://api.moonshot.ai/anthropic`, `ANTHROPIC_AUTH_TOKEN=$MOONSHOT_API_KEY`, `ANTHROPIC_API_KEY=` |
| `openrouter` | `opencode` | `OPENROUTER_API_KEY=$OPENROUTER_API_KEY` + `-m openrouter/<model>` |
| `local` | `opencode` | points at `http://localhost:1234/v1` |

Note `ANTHROPIC_API_KEY=` (explicitly empty) in the compat presets — omitting it lets
Claude Code fall back to Anthropic auth and quietly defeat the failover.

Keys come from the environment, never from the session store — the store is
world-readable JSON at `~/.local/state/muxai/sessions.json`. **Persist the preset name;
resolve env at spawn time.**

### 9.2 Surface it

- `muxai new <name> --agent <preset> [-- <command...>]`. Explicit `-- <command>` still
  wins, so nothing existing breaks.
- Dashboard `n` gains an agent picker instead of the hardcoded `"claude"` at
  `ui/dashboard.rs:259`.
- `Session` gains `agent: String`. Serde-default it to `"claude"` so existing
  `sessions.json` files deserialise unchanged — that's the backwards-compat test.
- Each dashboard tile shows its agent tag in the border title. This is S4, and it is also
  what makes a mixed grid comprehensible at a glance.
- Config file (FUTURE.md item 6) gains a `[agents.<name>]` table for user-defined presets.

### 9.3 `muxai doctor` — know before you're blocked

A small preflight that answers "which rungs are available right now":

```
$ muxai doctor
claude        ok      api.anthropic.com reachable, auth ok
glm           ok      ZAI_API_KEY set, api.z.ai reachable
kimi          --      MOONSHOT_API_KEY unset
openrouter    ok      OPENROUTER_API_KEY set
local         degraded  LM Studio up on :1234, no model loaded
```

Cheap to build (an HTTP HEAD and an env check per preset), and it converts "Claude is
down and I'm now improvising" into "Claude is down, `--agent glm`".

### 9.4 Local inference as an accounted resource

The local rung breaks `stats.rs`'s current assumption that memory is the sum of RSS over
session process trees. The LM Studio / `mlx_lm` process is **shared** across every local
tile and can be 46 GB by itself — larger than every agent process combined, and belonging
to no single session.

`muxai status` should show it as its own line, matching the split DESIGN.md already
established for disk (shared vs. per-session):

```
Memory
  agent process trees      2.1 GB
  local inference server   46.3 GB   (qwen3-coder-next-4bit, shared by 3 sessions)
  ---
  total                    48.4 GB / 32.0 GB budget  [red]
```

Which immediately exposes that `DEFAULT_MEMORY_BUDGET_BYTES = 32GB` is wrong for this
machine — a real config file (FUTURE.md item 6) becomes a prerequisite for the local rung,
not an independent nicety.

### 9.5 Explicitly not building

- **No automatic mid-session failover.** Swapping backends under a running agent changes
  tool-calling behaviour and destroys prompt-cache economics mid-task. If Claude dies, you
  kill the session and restart it on another rung — same worktree, same branch, work
  preserved. That is the whole point of worktree-per-session.
- **No LiteLLM / claude-code-router in the default path.** Extra hop, extra failure mode,
  extra thing to keep patched — and note LiteLLM PyPI 1.82.7/1.82.8 shipped
  credential-stealing malware, so this dependency has a real, recent supply-chain history.
  Direct provider endpoints (R1) and OpenRouter's own routing (R2) already cover it.
- **No custom model router.** OpenRouter does provider- and model-layer failover better
  than we would, and it's someone else's on-call.

---

## 10. Phased plan

Each phase is independently shippable, with the check that closes it.

| Phase | Work | Success check |
|---|---|---|
| **0** | Set `fallbackModel` in `~/.claude/settings.json`. No code. | `claude --help \| grep fallback`; chain present in settings |
| **1** | `src/agent.rs` + presets, `Session.agent` (serde default), `--agent` flag, tmux `-e` env injection, tile tag | Old `sessions.json` loads unchanged; `muxai new x --agent glm` starts and completes a real task with Anthropic blocked in `/etc/hosts` → **S1, S3, S4** |
| **2** | Buy GLM Coding Plan Lite ($18). Run the §8 eval on 3 candidates via OpenRouter (~$20). | Per-model completion rate + $/task table committed to `plans/` → **S5, S6** |
| **3** | `muxai doctor` | Every rung correctly reported available/unavailable/degraded |
| **4** | Local rung: LM Studio + Qwen3-Coder-Next 4-bit MLX, `local` preset, prefill chunk size tuned to 8192, context capped | Wi-Fi off; `muxai new x --agent local` edits a file and gets `cargo test` green → **S2** |
| **5** | `stats.rs` accounts for the shared inference server; config file for the memory budget | `muxai status` shows the split; budget is configurable |

Phases 1–3 are the outage insurance and are worth doing on their own. Phase 4 is the
lifeboat. Phase 5 only matters once phase 4 is in daily use.

---

## 11. Open questions

1. **Does prompt caching survive R1?** Claude Code leans hard on prompt caching for cost;
   whether the Anthropic-compat endpoints implement `cache_control` faithfully is
   undocumented and directly determines R1's real cost. **Measure in phase 2.**
2. **Do skills/MCP/`CLAUDE.md` behave identically on R1?** The compat endpoints implement
   the *wire format*; they do not promise the same instruction-following. Same eval.
3. **Local KV-cache reuse across turns** — does LM Studio's MLX engine actually reuse the
   cache between agent turns, or re-prefill? At ~4.5 tok/s prefill this is the single
   pass/fail question for the local rung. Verify before committing to phase 4.
4. **Does `muxai` need per-session context-budget knobs** for the local rung, or is
   configuring OpenCode enough? Prefer the latter — keep it out of mux-ai.
5. **GLM-5.3** (2026-08-14, aimed at coding agents) postdates most of the benchmark data
   here. If it holds up, it likely displaces GLM-5.2 as the R1 model.
6. **REAP-pruned local variants** (MiniMax-M2.5-REAP-39 at 73 GB, K3-REAP-80 at 350 GB) —
   pruning 39–80% of experts is a large, poorly-characterised quality change. Assume
   unvalidated until measured on our own eval; do not let the parameter count seduce.

---

## 12. Sources

Harnesses — [OpenCode providers](https://opencode.ai/docs/providers/) ·
[OpenCode CLI](https://opencode.ai/docs/cli/) ·
[OpenChamber](https://github.com/openchamber/openchamber) ·
[Pinggy CLI-agent roundup 2026](https://pinggy.io/blog/best_open_source_cli_coding_agents/)

Anthropic-compatible endpoints — [Z.ai devpack](https://docs.z.ai/devpack/quick-start) ·
[Moonshot #129](https://github.com/MoonshotAI/Kimi-K2/issues/129) ·
[MiniMax Anthropic SDK](https://platform.minimax.io/docs/api-reference/text-anthropic-api) ·
[Morph: different LLM with Claude Code](https://www.morphllm.com/use-different-llm-claude-code)

Models & benchmarks — [Terminal-Bench 2.1 (BenchLM)](https://benchlm.ai/benchmarks/valsterminalbench21) ·
[CodingFleet TB leaderboard](https://codingfleet.com/blog/terminal-bench-leaderboard-2026/) ·
[Artificial Analysis TB v2.1](https://artificialanalysis.ai/evaluations/terminalbench-v2-1) ·
[Qwen3-Coder-Next](https://qwen.ai/blog?id=qwen3-coder-next) ·
[Kimi K3 open weights](https://explainx.ai/blog/kimi-k3-open-weights-2-8-trillion-parameters-july-2026) ·
[LMArena](https://lmarena.ai)

Local inference — [mlx #3209 M3 Ultra benchmarks](https://github.com/ml-explore/mlx/discussions/3209) ·
[mlx-engine #245 agent mode](https://github.com/lmstudio-ai/mlx-engine/issues/245) ·
[lmstudio-js #507 prefill chunk size](https://github.com/lmstudio-ai/lmstudio-js/issues/507) ·
[Unsloth Qwen3-Coder-Next](https://unsloth.ai/docs/models/qwen3-coder-next) ·
[Kimi-K3-REAP80-MLX](https://huggingface.co/pipenetwork/Kimi-K3-REAP80-MLX-mxfp4-q8) ·
[agentic coding with local LLMs, 2026](https://braindetox.kr/en/posts/local_llm_agentic_coding_2026.html)

Pricing & reliability — [codingplan.org](https://codingplan.org/en) ·
[DeepSeek pricing](https://deepseek.ai/pricing) ·
[OpenRouter model fallbacks](https://openrouter.ai/docs/guides/routing/model-fallbacks) ·
[Cerebras Code](https://www.cerebras.ai/blog/introducing-cerebras-code) ·
[InfoWorld on Cerebras throughput](https://www.infoworld.com/article/4055909/down-and-out-with-cerebras-code) ·
[Claude Code fallbackModel](https://www.digitalapplied.com/blog/claude-code-safe-mode-fallback-models-production-resilience-guide) ·
[Claude outage history](https://statusgator.com/services/claude/outage-history)
