use anyhow::{bail, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// mux-ai runs its own tmux server on a dedicated socket, isolated from the user's
/// normal tmux config and sessions. This is what lets us rebind a single, unprefixed
/// detach key (C-\) server-wide without touching ~/.tmux.conf.
const SOCKET: &str = "muxai";
const DETACH_KEY: &str = "C-\\";

fn tmux() -> Command {
    let mut cmd = Command::new("tmux");
    cmd.args(["-L", SOCKET]);
    cmd
}

fn run_ok(cmd: &mut Command) -> Result<String> {
    let out = cmd.output()?;
    if !out.status.success() {
        bail!(
            "{:?} failed: {}",
            cmd,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Best-effort: starts the dedicated server and applies our keybind. tmux's
/// `exit-empty` default means a server with zero sessions can exit right back out
/// before the next command reaches it, so failures here are not fatal — `new_session`
/// re-applies the bind right after creating a session, which does keep the server up.
pub fn ensure_server() -> Result<()> {
    let _ = tmux().args(["start-server"]).output();
    let _ = bind_detach_key();
    let _ = configure_status_bar();
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
    run_ok(tmux().args(["set-option", "-g", "status-left", " C-\\ dashboard  [#S] "]))?;
    Ok(())
}

pub fn new_session(name: &str, cwd: &Path, command: &str) -> Result<()> {
    run_ok(tmux().args([
        "new-session",
        "-d",
        "-s",
        name,
        "-c",
        &cwd.to_string_lossy(),
        command,
    ]))?;
    // The server is now guaranteed to have a live session, so these are guaranteed
    // to apply (see ensure_server's note on exit-empty).
    bind_detach_key()?;
    configure_status_bar()?;
    Ok(())
}

pub fn list_sessions() -> Result<Vec<String>> {
    let out = tmux()
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()?;
    if !out.status.success() {
        // "no server running" / "no sessions" both land here — treat as empty.
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.to_string())
        .collect())
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
    let status = tmux()
        .args(["attach-session", "-t", name])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        bail!("tmux attach-session -t {name} exited with {status}");
    }
    Ok(())
}

pub fn kill_session(name: &str) -> Result<()> {
    run_ok(tmux().args(["kill-session", "-t", name]))?;
    Ok(())
}
