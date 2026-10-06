use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// mux-ai runs its own tmux server on a dedicated socket, isolated from the user's
/// normal tmux config and sessions. This is what lets us rebind a single, unprefixed
/// detach key (C-\) server-wide without touching ~/.tmux.conf.
const SOCKET: &str = "muxai";
const DETACH_KEY: &str = "C-\\";

pub struct LiveSession {
    pub name: String,
    pub session_path: PathBuf,
    pub pane_path: PathBuf,
}

fn tmux() -> Command {
    let mut cmd = Command::new("tmux");
    cmd.args(["-L", SOCKET]);
    cmd
}

fn run_ok(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().with_context(|| format!("running {cmd:?}"))?;
    if !out.status.success() {
        bail!(
            "{:?} failed: {}",
            cmd,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Every command shells out to tmux, and the failures are deliberately swallowed
/// throughout (a dead server is normal). Without this check a machine with no tmux
/// installed gets an empty dashboard, or a bare `No such file or directory (os error 2)`
/// from `muxai new`, neither of which names tmux.
pub fn ensure_available() -> Result<()> {
    match Command::new("tmux").arg("-V").output() {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "tmux is not installed or not on PATH — muxai runs every agent session inside it.\n  \
             macOS:  brew install tmux\n  \
             Debian: sudo apt install tmux"
        ),
        Err(e) => Err(e).context("running tmux -V"),
    }
}

/// Best-effort: starts the dedicated server and applies our keybind. tmux's
/// `exit-empty` default means a server with zero sessions can exit right back out
/// before the next command reaches it, so failures here are not fatal — `new_session`
/// re-applies the bind right after creating a session, which does keep the server up.
pub fn ensure_server() -> Result<()> {
    let _ = tmux().args(["start-server"]).output();
    let _ = bind_detach_key();
    let _ = configure_status_bar();
    let _ = configure_window_sizing();
    Ok(())
}

/// -n binds with no prefix key, so C-\ detaches directly from inside any session.
fn bind_detach_key() -> Result<()> {
    run_ok(tmux().args(["bind-key", "-n", DETACH_KEY, "detach-client"]))?;
    Ok(())
}

/// Once attached, a session owns the whole screen and our dashboard's own
/// command bar can't render there — so the "how do I get back" hint has to
/// live in tmux's own status line instead, which stays on screen no matter
/// which pane is attached.
fn configure_status_bar() -> Result<()> {
    // status-left-length defaults to 10, which truncates our hint before the
    // session name even starts rendering.
    run_ok(tmux().args(["set-option", "-g", "status-left-length", "40"]))?;
    run_ok(tmux().args([
        "set-option",
        "-g",
        "status-left",
        " ctrl-\\ to return to dashboard  [#S] ",
    ]))?;
    Ok(())
}

/// Dashboard tiles are much narrower than a real terminal. Re-wrapping a session's
/// 80-column output into a 44-column tile is what shreds the text, so instead we size
/// each window to its tile and let the agent inside wrap its own output correctly.
/// tmux only honours `resize-window` while `window-size` is `manual`; under the default
/// (`latest`) it snaps the window back to the last client's size.
///
/// `resize-window` also marks the window *permanently* manually-sized — flipping the
/// `window-size` option back is not enough to undo it, which is why attaching used to
/// leave the session stuck inside a tile-sized box in the corner of the terminal. The
/// two hooks undo it for attaches made outside the dashboard: on attach, and on every
/// later terminal resize, `-A` snaps the window to the attached client. They only fire
/// when a client exists, i.e. only while someone is attached — the dashboard itself is
/// not a tmux client, so tile sizing is untouched.
fn configure_window_sizing() -> Result<()> {
    run_ok(tmux().args(["set-option", "-g", "window-size", "manual"]))?;
    run_ok(tmux().args(["set-hook", "-g", "client-attached", "resize-window -A"]))?;
    run_ok(tmux().args(["set-hook", "-g", "client-resized", "resize-window -A"]))?;
    Ok(())
}

/// tmux target specs split on `:` (window) and `.` (pane), so a session whose name
/// contains either can never be addressed by name — not even with the `=` exact-match
/// prefix, which is applied after the split. tmux accepts such a name at creation and
/// only fails later, on every attach and kill, so names are normalised up front.
pub fn sanitize_name(name: &str) -> String {
    name.replace(['.', ':'], "-")
}

/// Hand a window's sizing back to whoever attaches to it. `resize_window` leaves
/// `window-size` at `manual`, which outlives the attach.
pub fn follow_client(name: &str) -> Result<()> {
    run_ok(tmux().args(["set-option", "-w", "-t", name, "window-size", "latest"]))?;
    Ok(())
}

pub fn resize_window(name: &str, width: u16, height: u16) -> Result<()> {
    run_ok(tmux().args([
        "resize-window",
        "-t",
        name,
        "-x",
        &width.to_string(),
        "-y",
        &height.to_string(),
    ]))?;
    Ok(())
}

pub fn new_session(name: &str, cwd: &Path, command: &str) -> Result<()> {
    let pane_cwd = format!("MUXAI_CWD={}", cwd.to_string_lossy());
    run_ok(
        tmux()
            .args([
                "new-session",
                "-d",
                "-s",
                name,
                "-c",
                &cwd.to_string_lossy(),
                "-e",
                &pane_cwd,
            ])
            // tmux's server keeps the PWD from the process that first started it. If
            // that directory is later deleted, tmux 3.7 can ignore `-c` for the pane
            // even though `session_path` reports the requested directory. An explicit
            // shell `cd` repairs the process cwd before the interactive shell starts.
            .arg(r#"cd -- "$MUXAI_CWD" && exec "$SHELL""#),
    )?;
    let started = (|| {
        wait_for_pane_path(name, cwd)?;

        // The server is now guaranteed to have a live session, so these are guaranteed
        // to target it. UI configuration is best-effort: a failed keybind or hook must
        // not destroy a usable shell or strand it in a deleted worktree.
        let _ = bind_detach_key();
        let _ = configure_status_bar();
        let _ = configure_window_sizing();

        // Run the agent inside the pane's interactive shell instead of replacing the
        // shell with it. If the agent exits during startup, the pane stays usable and
        // preserves the error at a prompt rather than disappearing without evidence.
        run_ok(tmux().args(["send-keys", "-t", name, "-l", command]))?;
        run_ok(tmux().args(["send-keys", "-t", name, "Enter"]))?;
        Ok(())
    })();
    if let Err(e) = started {
        let _ = kill_session(name);
        return Err(e);
    }
    Ok(())
}

fn wait_for_pane_path(name: &str, cwd: &Path) -> Result<()> {
    let mut actual = None;
    for _ in 0..100 {
        if let Some(session) = list_sessions()?
            .into_iter()
            .find(|session| session.name == name)
        {
            if session.pane_path == cwd && session.pane_path.exists() {
                return Ok(());
            }
            actual = Some(session.pane_path);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    bail!(
        "tmux pane '{name}' started in {}, not {}",
        actual.as_deref().map_or_else(
            || "an unknown directory".to_string(),
            |path| path.display().to_string()
        ),
        cwd.display()
    )
}

/// Live sessions plus both tmux's configured session directory and the active pane's
/// actual directory. They can differ when a shell changes directory, and the latter
/// can become invalid if its directory is deleted underneath it.
pub fn list_sessions() -> Result<Vec<LiveSession>> {
    let out = tmux()
        .args([
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_path}\t#{pane_current_path}",
        ])
        .output()?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(parse_sessions(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_sessions(out: &str) -> Vec<LiveSession> {
    out.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            Some(LiveSession {
                name: fields.next()?.to_string(),
                session_path: PathBuf::from(fields.next()?),
                pane_path: PathBuf::from(fields.next()?),
            })
        })
        .collect()
}

/// Every session's actual window size, plus whether a client is attached to it. The
/// dashboard has to ask rather than remember what it last pushed: `client-attached`
/// and `client-resized` resize a window to whatever terminal attaches to it, and a
/// second dashboard on a differently-sized terminal sizes it to *its* tiles, so a
/// window drifts behind our back and its tile ends up showing the left slice of a
/// much wider render.
pub fn window_sizes() -> Result<HashMap<String, (u16, u16, bool)>> {
    let out = tmux()
        .args([
            "list-sessions",
            "-F",
            "#{session_name}\t#{window_width}\t#{window_height}\t#{session_attached}",
        ])
        .output()?;
    if !out.status.success() {
        return Ok(HashMap::new());
    }
    Ok(parse_window_sizes(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_window_sizes(out: &str) -> HashMap<String, (u16, u16, bool)> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let name = f.next()?.to_string();
            let w = f.next()?.parse().ok()?;
            let h = f.next()?.parse().ok()?;
            Some((name, (w, h, f.next()? != "0")))
        })
        .collect()
}

/// Live tail of a session's pane, most recent `lines` rows.
pub fn capture_pane(name: &str, lines: u16) -> Result<String> {
    let start = format!("-{lines}");
    let out = tmux()
        .args(["capture-pane", "-p", "-t", name, "-S", &start])
        .output()?;
    if !out.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// PID of the process tmux runs for this session's active pane (root of its process
/// tree — used for the memory rollup in stats.rs).
pub fn pane_pid(name: &str) -> Result<Option<u32>> {
    let out = tmux()
        .args(["list-panes", "-t", name, "-F", "#{pane_pid}"])
        .output()?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|s| s.trim().parse().ok()))
}

/// Hands the real terminal to tmux for an interactive attach. Blocks until the user
/// detaches (C-\, bound above) or the session ends. Caller is responsible for
/// suspending/resuming its own raw-mode TUI around this call.
pub fn attach(name: &str) -> Result<()> {
    // The client-attached hook resizes the window to the real terminal as soon as
    // this client lands, undoing the tile size we imposed for the grid. The dashboard
    // re-imposes tile sizes once it has the screen back.
    let child = tmux()
        .args(["attach-session", "-t", name])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let reason = stderr.trim();
        if reason.is_empty() {
            bail!("tmux attach-session -t {name} exited with {}", out.status);
        }
        bail!("tmux attach-session -t {name}: {reason}");
    }
    Ok(())
}

pub fn kill_session(name: &str) -> Result<()> {
    run_ok(tmux().args(["kill-session", "-t", name]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_sessions, parse_window_sizes, sanitize_name};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn window_sizes_carry_the_measured_size_and_attach_state() {
        let out = "idle\t142\t59\t0\none-client\t288\t186\t1\ntwo-clients\t80\t24\t2\n";
        assert_eq!(
            parse_window_sizes(out),
            HashMap::from([
                ("idle".to_string(), (142, 59, false)),
                ("one-client".to_string(), (288, 186, true)),
                ("two-clients".to_string(), (80, 24, true)),
            ])
        );
        assert!(parse_window_sizes("").is_empty());
    }

    #[test]
    fn sessions_carry_configured_and_actual_paths() {
        let sessions = parse_sessions("headlamp\t/repo/headlamp\t/deleted/old-cwd\n");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "headlamp");
        assert_eq!(sessions[0].session_path, PathBuf::from("/repo/headlamp"));
        assert_eq!(sessions[0].pane_path, PathBuf::from("/deleted/old-cwd"));
    }

    #[test]
    fn target_separators_become_dashes() {
        assert_eq!(sanitize_name("fix-josephzho.ng"), "fix-josephzho-ng");
        assert_eq!(sanitize_name("a:b.c"), "a-b-c");
        assert_eq!(sanitize_name("already-fine"), "already-fine");
    }
}
