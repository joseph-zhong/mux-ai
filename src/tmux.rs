use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
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
/// two hooks are the undo: on attach, and on every later terminal resize, `-A` snaps
/// the window to the attached client. They only fire when a client exists, i.e. only
/// while someone is attached — the dashboard itself is not a tmux client, so tile
/// sizing is untouched.
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
    configure_window_sizing()?;
    Ok(())
}

/// Live sessions plus each one's working directory, so the dashboard can tell which
/// live sessions belong to the repo it was launched from.
pub fn list_sessions_with_paths() -> Result<Vec<(String, PathBuf)>> {
    let out = tmux()
        .args(["list-sessions", "-F", "#{session_name}\t#{session_path}"])
        .output()?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(name, path)| (name.to_string(), PathBuf::from(path)))
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
    use super::sanitize_name;

    #[test]
    fn target_separators_become_dashes() {
        assert_eq!(sanitize_name("fix-josephzho.ng"), "fix-josephzho-ng");
        assert_eq!(sanitize_name("a:b.c"), "a-b-c");
        assert_eq!(sanitize_name("already-fine"), "already-fine");
    }
}
