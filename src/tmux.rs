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
    let _ = set_window_size_manual();
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
    run_ok(tmux().args(["set-option", "-g", "status-left", " ctrl-\\ to return to dashboard  [#S] "]))?;
    Ok(())
}

/// Dashboard tiles are much narrower than a real terminal. Re-wrapping a session's
/// 80-column output into a 44-column tile is what shreds the text, so instead we size
/// each window to its tile and let the agent inside wrap its own output correctly.
/// tmux only honours `resize-window` while `window-size` is `manual`; under the default
/// (`latest`) it snaps the window back to the last client's size.
fn set_window_size_manual() -> Result<()> {
    run_ok(tmux().args(["set-option", "-g", "window-size", "manual"]))?;
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
    set_window_size_manual()?;
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
    // While attached the session owns the whole terminal, so drop the tile-sized
    // window we imposed for the grid and let tmux size to the real client. The
    // dashboard re-imposes tile sizes once it has the screen back.
    let _ = tmux()
        .args(["set-option", "-g", "window-size", "latest"])
        .output();
    let child = tmux()
        .args(["attach-session", "-t", name])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = child.wait_with_output()?;
    let _ = set_window_size_manual();
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
