# mux-ai

Grid-dashboard TUI for running several coding agents in parallel, each in its own git
worktree, on top of tmux. See [`DESIGN.md`](DESIGN.md) for the architecture and
[`ALTERNATIVES.md`](ALTERNATIVES.md) for why this exists instead of just using aoe /
Claude Squad / etc.

**No daemon, no server, no port.** Session state lives in the tmux server, not in the
`muxai` process — every invocation is a short-lived client. So there is nothing here to
expose and nothing here to authenticate. Reaching your agents from a phone is your
operating system's SSH over a WireGuard mesh, not a web UI behind a shared password:
see [`PHONE_ACCESS.md`](PHONE_ACCESS.md).

## Install

Runtime prerequisites either way: `tmux` and `git`. See
[Dependencies](#dependencies) below.

**Prebuilt binary (macOS, no Rust needed).** One universal binary, Apple Silicon and
Intel:

```sh
curl -fsSL https://raw.githubusercontent.com/joseph-zhong/mux-ai/main/install.sh | sh
```

It drops `muxai` in `~/.local/bin` and prints a PATH hint if that directory isn't on
your `PATH`. Override with `MUXAI_INSTALL_DIR=/usr/local/bin` or pin a version with
`MUXAI_VERSION=v0.1.0`.

While this repo is private the plain download 404s, and the script falls back to the
GitHub CLI — so recipients need `brew install gh`, `gh auth login`, and read access to
the repo. Once the repo is public the one-liner works unauthenticated, with no script
change.

**From source (any platform).** Needs Rust stable via [rustup](https://rustup.rs):

```sh
cargo install --git https://github.com/joseph-zhong/mux-ai   # from anywhere
cargo install --path .                                       # from a clone
cargo build --release                                        # or just build it
```

**If macOS refuses to open the binary.** Downloading the tarball in a browser tags it
with a quarantine flag, and these binaries are ad-hoc signed rather than notarized, so
Gatekeeper blocks them. The installer above avoids this entirely (`curl` doesn't set
the flag). If you did download by hand:

```sh
xattr -d com.apple.quarantine ./muxai
```

## Demo

From inside any git repo:

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

- **tmux** — developed against 3.7b; anything reasonably recent should work.
- **git** ≥ 2.5 (worktree support).
- **Rust** stable, via [rustup](https://rustup.rs) — only to build from source; the
  prebuilt binary needs no toolchain. No MSRV pinned yet.
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
GitHub issues.

## Releasing

Releases are cut by tag. `.github/workflows/release.yml` builds both macOS
architectures, fuses them with `lipo` into one universal binary, and attaches the
tarball and its `.sha256` to a GitHub Release. The workflow refuses to run if the tag
and the `version` in `Cargo.toml` disagree.

```sh
# 1. bump the version, commit it on a PR, merge
sed -i '' 's/^version = .*/version = "0.2.0"/' Cargo.toml

# 2. tag the merged commit — after the PR is merged, not before, or the tag lands
#    on a commit the workflow does not exist on yet
git checkout main && git pull
git tag v0.2.0 && git push origin v0.2.0

# 3. watch it, then check the assets landed. `gh run watch` needs an explicit run id
#    when stdin is not a TTY, so resolve it first.
gh run watch "$(gh run list --repo joseph-zhong/mux-ai --limit 1 --json databaseId --jq '.[0].databaseId')" --exit-status
gh release view v0.2.0 --repo joseph-zhong/mux-ai
```

`workflow_dispatch` also takes a tag, for re-running a build without re-tagging.

## License

MIT — see [`LICENSE`](LICENSE).
