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

That is also why mux-ai should *not* copy the OpenCode / OpenChamber architecture. Those
tools need a client/server protocol because their agent state lives inside their own
process. Ours lives in tmux. Their remote-access auth story (HTTP basic auth via
`OPENCODE_SERVER_PASSWORD`; a UI password plus a revocable tunnel link) is also weaker
than an identity-based WireGuard mesh — and OpenCode's own docs recommend Tailscale over
opening a port anyway.

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
- No public URL on `example.com` (that is Tier 2).
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

## Current state of this machine (verified 2026-08-19)

| Thing | State |
|---|---|
| Tailnet name | `your-mac.tailnet-name.ts.net` (`100.100.0.1`) |
| MagicDNS | Enabled |
| Devices in tailnet | 4 — desktop, `your-phone`, `other-host`, `your-mac-2` |
| Phone enrolled | **Yes** — `your-phone` (`100.100.0.2`) |
| macOS Remote Login (sshd) | **Off** — nothing is listening on port 22 |
| `~/.ssh/authorized_keys` | Does not exist |
| Tailscale install | **Two of them.** See below |

So Step 1 is already done. What is missing: resolve the dual install, then turn on sshd
with key-only auth (Steps 2–5).

### The dual install — read this before anything else

This machine has **two** Tailscale installs, and only one of them is doing anything:

| Install | Version | State |
|---|---|---|
| macOS app, `/Applications/Tailscale.app` (`macsys` variant) | 1.102.2 | **Live.** Owns `/var/run/tailscaled.socket`, is the node the tailnet sees |
| Homebrew `tailscale` formula | 1.98.10 | **Inert.** `tailscaled` is running as root (started 2026-08-10) but has no state directory, so it never registered as a node |

Evidence: the Homebrew daemon is running —

```sh
ps aux | grep 'tailscale[d]'
# root  35671  ...  /opt/homebrew/opt/tailscale/bin/tailscaled
```

— but it has no state anywhere (`/opt/homebrew/var/lib/tailscale/` and
`/var/lib/tailscale/` both do not exist), and whatever answers on the shared socket
reports the *app's* version, not Homebrew's:

```sh
/opt/homebrew/bin/tailscale status | head -1
# Warning: client version "1.98.10-..." != tailscaled server version "1.102.2-..."
```

Both CLIs on `PATH` therefore report the same node. `/usr/local/bin/tailscale` (a shim
that execs the app's binary) shadows `/opt/homebrew/bin/tailscale`, so `tailscale`
resolves to the app's CLI:

```sh
which -a tailscale
# /usr/local/bin/tailscale
# /opt/homebrew/bin/tailscale
```

Practical consequences: a root process doing nothing, confusing `brew services` /
`launchctl` output, version-skew warnings on every Homebrew-CLI invocation, and most
likely the duplicate `your-mac-2` node in the tailnet.

**Recommended: keep the app, drop the Homebrew daemon** (Step 0 below). The app is the
better fit for a desktop Mac — menu bar UI, auto-updates, no sudo, clean start at login —
and it is already the one working.

### Consequence: Tailscale SSH is not available as currently installed

`tailscale set --ssh` will not work here. Tailscale's SSH *server* component only runs on
Linux and on the macOS **open source** `tailscale`/`tailscaled` build. You *have* that
build installed via Homebrew, but it is not the daemon serving your tailnet — the app is,
and the app cannot be a Tailscale SSH server. So this doc uses macOS's built-in OpenSSH
(Remote Login). Connecting *from* the phone works from any Tailscale client regardless of
platform.

If you would rather have Tailscale SSH — which is genuinely a better auth story, since it
needs no `authorized_keys`, no key distribution, and listens only on the tailnet interface
rather than on every interface — the path is to go the other way: quit and uninstall the
app, then run the Homebrew daemon as the real node (`sudo brew services start tailscale`,
`tailscale up --ssh`), and add an SSH rule to the tailnet ACL. That means re-registering
this node and losing the menu bar app, so it is not the default recommendation here — and
it would disturb the separate use-case the phone is already set up for.

---

## Setup

### Step 0 — Drop the inert Homebrew daemon

```sh
sudo brew services stop tailscale
brew uninstall tailscale
```

Verify nothing is left running and the app is still the live node:

```sh
ps aux | grep 'tailscale[d]' || echo "no tailscaled — expected"
tailscale status
```

Then delete the stale `your-mac-2` node in the
[admin console](https://login.tailscale.com/admin/machines), so MagicDNS cannot route
`your-mac` to a dead registration.

If you want to keep the Homebrew binary for its CLI, `sudo brew services stop tailscale`
alone is enough — the daemon is the problem, not the binary. Expect the version-skew
warning to persist whenever you invoke `/opt/homebrew/bin/tailscale` directly.

### Step 1 — Enroll the phone in the tailnet ✅ done

Already complete — `your-phone` (`100.100.0.2`) is in the tailnet. Confirm any time
with:

```sh
tailscale status | grep iphone
```

For reference, if you ever add another device: install Tailscale from the
[iOS App Store](https://apps.apple.com/us/app/tailscale/id1470499037) or
[Google Play](https://play.google.com/store/apps/details?id=com.tailscale.ipn), sign in
with the **same account** as the desktop (`you@example.com`), toggle the VPN
on, and accept the OS VPN-configuration prompt.

### Step 2 — Generate an SSH key on the phone

**MANUAL VERIFICATION REQUIRED** (phone-side).

Use a terminal app that supports SSH keys and a hardware-ish keyboard row:

- **iOS**: [Blink Shell](https://blink.sh) (paid, best-in-class TUI support) — `Settings →
  Keys → +` to generate an ed25519 key, then `Actions → Copy Public Key`.
- **Android**: [Termux](https://termux.dev) (free) — `pkg install openssh`, then
  `ssh-keygen -t ed25519`, then `cat ~/.ssh/id_ed25519.pub`.

Get that **public** key text onto the desktop (AirDrop, a note, a paste buffer — it is
public, it is not sensitive).

### Step 3 — Install the phone's public key on the desktop

Run on the desktop, substituting the real key:

```sh
mkdir -p ~/.ssh && chmod 700 ~/.ssh
printf '%s\n' 'ssh-ed25519 AAAA...REPLACE_WITH_PHONE_PUBLIC_KEY... phone' >> ~/.ssh/authorized_keys
chmod 600 ~/.ssh/authorized_keys
```

Verify:

```sh
wc -l ~/.ssh/authorized_keys && ssh-keygen -l -f ~/.ssh/authorized_keys
```

### Step 4 — Harden sshd before turning it on

Do this **before** Step 5, so sshd is never briefly listening with password auth enabled.

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

`/etc/ssh/sshd_config` line 19 is `Include /etc/ssh/sshd_config.d/*`, so this drop-in is
picked up automatically. `sshd -t` must print `sshd config OK` with no other output before
you continue.

### Step 5 — Enable Remote Login

```sh
sudo systemsetup -setremotelogin on
sudo systemsetup -getremotelogin
```

Then restrict it to your user in the GUI as a second layer:

**MANUAL VERIFICATION REQUIRED**: System Settings → General → Sharing → Remote Login →
`(i)` → set **"Allow access for: Only these users"** → `YOUR_USERNAME`.

Verify it is listening:

```sh
nc -z -G 2 localhost 22 && echo "sshd LISTENING" || echo "sshd NOT listening"
```

### Step 6 — Connect from the phone

**MANUAL VERIFICATION REQUIRED** (phone-side):

```sh
ssh YOUR_USERNAME@your-mac.tailnet-name.ts.net
muxai
```

The short MagicDNS name `your-mac` also works while the phone's Tailscale VPN is
on. Use the full name if you hit resolver weirdness on cellular.

---

## Security model — read this before running Step 5

Being precise about what each piece does and does not protect, because it is easy to
believe this setup is more isolated than it is:

- **Tailscale provides reachability, not sshd isolation.** It is what lets the phone reach
  the desktop from cellular data with no port forward, no public DNS record, and no
  inbound firewall hole. It does not restrict who can talk to sshd on your LAN.
- **macOS sshd will listen on all interfaces**, including your home LAN — not only the
  tailnet. This is not fixable via `ListenAddress`: macOS runs sshd through launchd socket
  activation (`SockServiceName ssh` in `/System/Library/LaunchDaemons/ssh.plist`), and
  launchd owns the socket, so `sshd_config`'s `ListenAddress` has no effect on it.
- **Therefore the real access control is Step 4**: public-key-only authentication, no
  passwords, no root, one permitted user. Do not skip it or reorder it after Step 5.
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
