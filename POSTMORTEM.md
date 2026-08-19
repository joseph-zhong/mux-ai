# mux-ai — Post-mortems

## 2026-08-19 — the dashboard showed nothing while six agents were running

**Impact.** Launched from inside one of its own worktrees — the normal way to use it —
`muxai` rendered `No sessions yet. Press 'n' to create one, 'q' to quit.` At that moment
the repo had six worktrees on disk and three live tmux sessions with running Claude
processes in them (`auto-2d-grid`, `phone-access`, `enable-other-models`). Over the
preceding weeks the same underlying fault had also been quietly deleting session records
for work that was still very much alive.

No work was lost. Every worktree and every branch survived, because git held them.
That is the entire lesson.

**Detection.** By a human noticing that the dashboard was missing worktrees they knew
existed, and asking whether the sessions had died. They had not.

### What happened

Two independent faults, one shared root: `~/.local/state/muxai/sessions.json` was
documented and implemented as *the source of truth* for what mux-ai manages, while git
and tmux held the same state durably and correctly.

**Fault 1 — the dashboard looked in the wrong repo.** `find_repo_root` used
`git rev-parse --show-toplevel`, which returns the *linked worktree* when you are inside
one, not the main repo. `for_repo()` then filtered the store by that path and matched
nothing at all. The failure was total and silent: an empty dashboard is
indistinguishable from having no sessions.

**Fault 2 — the store lost rows, three different ways.**

| mechanism | effect |
|---|---|
| `reconcile()` → `retain_running()` at startup | any entry whose tmux session was not running was deleted permanently. An agent exiting erased the only record of its worktree. |
| `list_sessions()` returned `Ok(vec![])` for *"no server running"* as well as *"no sessions"* | `retain_running(&[])` then wiped **every** record, including sessions that were alive. A transient tmux hiccup was a total wipe. |
| `save()` rewrote the whole file from an in-memory snapshot taken at process start | concurrent muxai processes silently deleted each other's sessions. |

The third was caught red-handed during the investigation: `enable-other-models` was
created at `14:11:54`, `sessions.json`'s mtime was `14:11:54`, and the file did not
contain it. Later in the same session the store shrank from 6 rows to 3 on its own,
while pre-fix binaries were still running in other terminals.

### Why it took weeks to notice

Every failure mode was silent and looked like a legitimate state. A missing tile is
indistinguishable from a session that was never created. A shrinking store file is
invisible unless you read it. The dashboard had no way to say *"I know something is
there and I cannot show it"*, because after `retain_running` ran, it genuinely did not
know.

`DESIGN.md` said the store was the source of truth and that "tmux/git are re-queried
live and reconciled against it." That sentence is exactly backwards, and it was written
before any of the code that made it dangerous. The design doc licensed the bug.

### The fix

The dashboard derives its tiles from durable state on every refresh: git's worktree list
for the repo, unioned with tmux's live session list. The store is demoted to a metadata
cache holding one non-derivable field — the command to re-run — and mutations re-read
the file before writing. Nothing prunes on tmux liveness. A worktree with no session is
a `(stopped)` tile that `Enter` restarts, not a row to delete.

See PR #11 and `ARCHITECTURE.md`.

### Learnings

**1. A cache that is allowed to delete is not a cache.** The store was described as a
cache and behaved as an authority: `retain_running` gave it the power to make a worktree
un-discoverable. Caches may be stale, may be empty, may be wrong — they may never be the
reason a real thing becomes invisible.

**2. "Absent" and "unknown" are different, and conflating them is what makes a bug
silent.** `list_sessions()` mapped every failure onto the empty list, so *"tmux is not
answering"* became *"there are no sessions"* became *"delete everything."* Any function
that swallows an error into a neutral-looking value should be read as a landmine,
especially when its return value drives a deletion.

**3. Derive, do not mirror.** Every piece of state mux-ai kept about worktrees and
sessions was a second copy of something git or tmux already knew better. Two copies of a
fact are a bug with a schedule. The cost of re-deriving here was two subprocesses every
two seconds — nothing — and it was paid for a whole class of bugs.

**4. Test the tool from inside the environment it creates.** mux-ai puts you inside a
worktree. Nothing tested it *from* a worktree, which is why a `--show-toplevel` call
that is correct in a plain checkout and wrong in a linked one lived for weeks. The
regression test now asserts `find_repo_root` from inside a linked worktree.

**5. When state disappears, ask what still holds it before rebuilding.** The first
instinct on seeing missing sessions was that the agents or tmux sessions had died. They
had not — `tmux list-sessions` and `git worktree list` both had the answer immediately.
Checking the durable layers first turned a suspected data-loss incident into a display
bug in about ten minutes.

**6. Correct the design doc in the same change as the code.** `DESIGN.md` asserted the
inverted model. Left alone, it would have re-authorised the same mistake in the next
feature. It is corrected in this PR, and `ARCHITECTURE.md` now carries the state-ownership
table as the thing to check against.
