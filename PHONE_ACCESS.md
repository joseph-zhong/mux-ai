# mux-ai — Phone access (Tier 0: Tailscale + SSH)

Reach your mux-ai sessions from a phone, from anywhere, without exposing anything to
the public internet and without writing any code.

This is **Tier 0** of three possible tiers:

| Tier | Shape | Public internet exposure | Setup |
|---|---|---|---|
| **0 (this doc)** | Tailscale + SSH, native terminal app on the phone | None | ~15 min, no code |
| 1 | Tailscale + `ttyd`, browser terminal over HTTPS | None | +5 min, no code |
| 2 | Cloudflare Tunnel + Access on a real domain | Yes — a shell behind an SSO gate | +30 min, DNS migration |

Tier 0 is the recommended stopping point. Tiers 1 and 2 are not documented here yet.

## Why this shape

mux-ai already has a client/server split — **tmux is the server**. Session state lives in
the `tmux -L muxai` server and the JSON store (`~/.local/state/muxai/sessions.json`), not
in the `muxai` process, which is a fresh short-lived client on every invocation (see
[`DESIGN.md`](DESIGN.md)). So "access from phone" does not require building an HTTP API,
a daemon, or a web UI. It requires getting a terminal onto the phone.

The consequence worth stating plainly: **mux-ai has no listening port, no server process,
and no authentication of its own.** There is nothing in this project to expose, so there
is nothing in this project to secure. Remote access is your operating system's SSH, and
the security review is the one every Unix admin already knows how to do.

That is also why mux-ai should *not* copy the OpenCode / OpenChamber architecture. Those
tools need a client/server protocol because their agent state lives inside their own
process. Once you have a server, you need an auth story for it, and theirs is weaker than
an identity-based WireGuard mesh — HTTP basic auth via `OPENCODE_SERVER_PASSWORD`, or a UI
password plus a revocable tunnel link. Note that OpenCode's own docs recommend putting
Tailscale in front rather than opening a port. Tailscale is not the differentiator; anyone
can put Tailscale in front of anything. The differentiator is having no server that would
tempt you to skip it.

Note that mux-ai contains **zero Tailscale code** and has no dependency on it. Tailscale
here is a deployment recommendation, not an integration. Any other way of getting SSH to
your machine — a VPN you already run, a jump host, a LAN you trust — works identically.

## What this precisely enables

After setup, from a phone on cellular data or any foreign Wi-Fi:

- SSH into the desktop at its stable tailnet name, from anywhere, with no port forwarding,
  no dynamic DNS, and no public hostname.
- Run `muxai` and get the full grid dashboard: live tail of every agent session at once.
- Arrow keys to select, `Enter` to attach into a session, type to the agent, answer its
  permission prompts, `C-\` to detach back to the dashboard.
- Run every other subcommand: `muxai new`, `muxai kill`, `muxai status`, `muxai reset`.
- **Sessions survive the phone.** Closing the terminal app, locking the phone, losing
  signal, or switching from Wi-Fi to cellular does not kill anything — the agents keep
  running in tmux on the desktop, and reconnecting drops you back where you were. This is
  the property that makes phone access actually useful rather than a novelty.

What it explicitly does **not** enable:

- No browser access (that is Tier 1 — `ttyd`).
- No public URL on a domain you own (that is Tier 2).
- No push notifications when an agent finishes or blocks on a prompt.
- No access for anyone but you — a tailnet is single-account by default.
- No file editing UI. You get a terminal; use the agent, or `vim`.

### What using it actually feels like

**No browser involved.** Tier 0 is a native terminal app on the phone, not a web page.
Concretely: open Blink Shell, tap a saved host, and you are at your Mac's shell — same
prompt, same `$PATH`, same `muxai` binary. Type `muxai`, the grid renders on the phone
screen, arrow keys select, `Enter` attaches, you type at the agent directly.

There is no daemon, no web server, and no port beyond SSH involved. What crosses the
network is the rendered terminal frame, exactly like SSH from a laptop.

Browser-based access is **Tier 1** (`ttyd`) and is optional — its only advantage is
skipping the app install, e.g. on a borrowed device. For your own phone, the native app is
better: real key handling, saved hosts and keys, working scrollback, and reconnect on
network change.

## Before you start

You need, on the desktop running the agents:

- A [Tailscale](https://tailscale.com) account with this machine enrolled, and MagicDNS
  enabled (Tailscale admin console → DNS → MagicDNS). Check with `tailscale status`.
- macOS with Remote Login available. Linux works too; the sshd hardening in Step 3 is
  portable, only the Remote Login toggle in Step 4 is macOS-specific.

Take note of your tailnet name — `tailscale status --json | grep -i magicdnssuffix`, or
the machine list in the admin console. It looks like `tailXXXXXX.ts.net`, and your
desktop's full name is `<machine>.<tailnet>.ts.net`. Everywhere below that appears as
`<user>@<machine>.<tailnet>.ts.net`, substitute your own.

### Check which Tailscale you have installed — it changes your options

macOS has two Tailscale distributions and they are not equivalent:

| Install | What it is | Tailscale SSH server |
|---|---|---|
| `/Applications/Tailscale.app` (App Store or `macsys` variant) | Menu bar app, auto-updates, no sudo | **Not supported** |
| Homebrew `tailscale` formula | The open source `tailscale`/`tailscaled` build | **Supported** |

This matters because `tailscale set --ssh` — which is a genuinely better auth story, since
it needs no `authorized_keys`, no key distribution, and listens only on the tailnet
interface rather than on every interface — only works on Linux and on the open source
macOS build. **This doc uses macOS's built-in OpenSSH instead, so it works with either
install.** Connecting *from* the phone works from any Tailscale client on any platform.

If you want Tailscale SSH on a Mac, the path is to run the Homebrew daemon as the real
node (`sudo brew services start tailscale`, `tailscale up --ssh`) and add an SSH rule to
your tailnet ACL — at the cost of re-registering the node and losing the menu bar app.

**If you have both installed**, only one is actually serving your tailnet, and the other
is dead weight — a root process doing nothing, confusing `brew services` / `launchctl`
output, version-skew warnings, and often a duplicate node in your tailnet. Diagnose it:

```sh
ps aux | grep 'tailscale[d]'          # which daemons are running
which -a tailscale                    # which CLI wins on PATH
tailscale status | head -1            # a version-skew warning means the CLI is not the daemon
```

A Homebrew `tailscaled` with no state directory (neither `/opt/homebrew/var/lib/tailscale/`
nor `/var/lib/tailscale/` exists) never registered as a node and is doing nothing. Drop it
with `sudo brew services stop tailscale`, then delete any stale duplicate node in the
[admin console](https://login.tailscale.com/admin/machines) so MagicDNS cannot route your
machine's name to a dead registration.

---

## Setup

### Step 1 — Enroll the phone in the tailnet

Install Tailscale from the
[iOS App Store](https://apps.apple.com/us/app/tailscale/id1470499037) or
[Google Play](https://play.google.com/store/apps/details?id=com.tailscale.ipn), sign in
with the **same account** as the desktop, toggle the VPN on, and accept the OS
VPN-configuration prompt.

Confirm from the desktop that the phone shows up:

```sh
tailscale status
```

### Step 2 — Generate an SSH key on the phone, and install it on the desktop

**MANUAL VERIFICATION REQUIRED** (phone-side).

Use a terminal app that supports SSH keys and a hardware-ish keyboard row:

- **iOS**: [Blink Shell](https://blink.sh) (paid, best-in-class TUI support) — `Settings →
  Keys → +` to generate an ed25519 key, then `Actions → Copy Public Key`.
- **Android**: [Termux](https://termux.dev) (free) — `pkg install openssh`, then
  `ssh-keygen -t ed25519`, then `cat ~/.ssh/id_ed25519.pub`.

Get that **public** key text onto the desktop (AirDrop, a note, a paste buffer — it is
public, it is not sensitive), then run on the desktop, substituting the real key:

```sh
mkdir -p ~/.ssh && chmod 700 ~/.ssh
printf '%s\n' 'ssh-ed25519 AAAA...REPLACE_WITH_PHONE_PUBLIC_KEY... phone' >> ~/.ssh/authorized_keys
chmod 600 ~/.ssh/authorized_keys
```

Verify:

```sh
wc -l ~/.ssh/authorized_keys && ssh-keygen -l -f ~/.ssh/authorized_keys
```

### Step 3 — Harden sshd before turning it on

Do this **before** Step 4, so sshd is never briefly listening with password auth enabled.

```sh
sudo tee /etc/ssh/sshd_config.d/200-muxai-phone.conf >/dev/null <<'EOF'
# Key-only SSH for phone access to mux-ai. See PHONE_ACCESS.md.
PasswordAuthentication no
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
PermitRootLogin no
AllowUsers YOUR_USERNAME
EOF
sudo chmod 644 /etc/ssh/sshd_config.d/200-muxai-phone.conf
sudo sshd -t && echo "sshd config OK"
```

Replace `YOUR_USERNAME` with the output of `whoami`. macOS's `/etc/ssh/sshd_config`
contains `Include /etc/ssh/sshd_config.d/*`, so this drop-in is picked up automatically —
confirm with `grep -n '^Include' /etc/ssh/sshd_config` if you want to be sure. `sshd -t`
must print `sshd config OK` with no other output before you continue.

### Step 4 — Enable Remote Login

```sh
sudo systemsetup -setremotelogin on
sudo systemsetup -getremotelogin
```

Then restrict it to your user in the GUI as a second layer:

**MANUAL VERIFICATION REQUIRED**: System Settings → General → Sharing → Remote Login →
`(i)` → set **"Allow access for: Only these users"** → your user.

Verify it is listening:

```sh
nc -z -G 2 localhost 22 && echo "sshd LISTENING" || echo "sshd NOT listening"
```

### Step 5 — Connect from the phone

**MANUAL VERIFICATION REQUIRED** (phone-side):

```sh
ssh <user>@<machine>.<tailnet>.ts.net
muxai
```

The short MagicDNS name `<machine>` also works while the phone's Tailscale VPN is on. Use
the full name if you hit resolver weirdness on cellular.

---

## Security model — read this before running Step 4

Being precise about what each piece does and does not protect, because it is easy to
believe this setup is more isolated than it is:

- **Tailscale provides reachability, not sshd isolation.** It is what lets the phone reach
  the desktop from cellular data with no port forward, no public DNS record, and no
  inbound firewall hole. It does not restrict who can talk to sshd on your LAN.
- **macOS sshd will listen on all interfaces**, including your home LAN — not only the
  tailnet. This is not fixable via `ListenAddress`: macOS runs sshd through launchd socket
  activation (`SockServiceName ssh` in `/System/Library/LaunchDaemons/ssh.plist`), and
  launchd owns the socket, so `sshd_config`'s `ListenAddress` has no effect on it.
- **Therefore the real access control is Step 3**: public-key-only authentication, no
  passwords, no root, one permitted user. Do not skip it or reorder it after Step 4.
- **Nothing here is reachable from the public internet** as long as your router does not
  forward port 22. It does not by default. If you have ever set up port forwarding on this
  network, verify it does not include 22 before enabling Remote Login.
- **Blast radius is your whole machine.** SSH access is not scoped to mux-ai — it is a
  full interactive shell with your API keys, SSH keys, and every repo. Guard the phone
  itself with a device passcode and biometric lock on the terminal app; a stolen unlocked
  phone is a compromised workstation.
- **Phone-sized mistakes are real.** You are reaching coding agents, and worktrees isolate
  source state, not execution (see [`DESIGN.md`](DESIGN.md)). Approving a destructive
  action by fat-fingering "yes" on a 6-inch screen is the most likely way this setup hurts
  you. Keep agent permission prompts enabled for anything you plan to reach from the
  phone, and treat the phone as a monitor-and-approve surface rather than a place to
  launch unattended full-auto runs.

### Turning it off

```sh
sudo systemsetup -setremotelogin off
sudo rm /etc/ssh/sshd_config.d/200-muxai-phone.conf
```

Tailscale can stay on — by itself it exposes no services.

---

## Known rough edges on a phone screen

Tier 0 works today with zero code changes, but mux-ai was not designed for a 40-column
screen. These are the real friction points, with their locations:

| Edge | Effect on phone | Where |
|---|---|---|
| No `muxai attach <name>` subcommand | Must open the dashboard and arrow-key to a session; no direct jump | [`src/cli.rs`](src/cli.rs) has no `Attach` variant |
| Detach is bound to `C-\` only | iOS/Android soft keyboards have no Ctrl key. Blink and Termux both provide one on their accessory row, so this is usable — but there is no non-Ctrl fallback | [`src/tmux.rs:42`](src/tmux.rs) — `bind-key -n C-\ detach-client` |
| Grid assumes a wide terminal | `CELL_WIDTH` is 44 columns, so a phone collapses to a single column; with 5 sessions that is 5 stacked cells of roughly 4 lines each | [`src/ui/dashboard.rs:20`](src/ui/dashboard.rs) |
| `capture-pane` polling every ~300ms | Not a problem — polling happens on the desktop; only the rendered frame crosses the network | [`DESIGN.md`](DESIGN.md) |

None of these block Tier 0. Fixing the first three is roughly 60 lines of Rust and is
tracked in [`FUTURE.md`](FUTURE.md).
