use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};
use std::collections::HashMap;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::session_store::{worktree_root, SessionStore};
use crate::{agent, create_session, tmux, worktree};

const POLL_INTERVAL: Duration = Duration::from_millis(300);
/// Re-scanning git and tmux costs two subprocesses, so it runs slower than the pane
/// poll — fast enough that a session created in another muxai window shows up on its own.
const DISCOVER_INTERVAL: Duration = Duration::from_secs(2);
const PANE_LINES: u16 = 200;

/// Legibility floor for a tile. Below this a cell shows nothing useful, so we'd rather
/// use fewer columns (or, past that, overflow) than keep slicing.
const MIN_CELL_W: u16 = 44;
const MIN_CELL_H: u16 = 12;
/// A terminal character is about twice as tall as it is wide, so a cell's *visual*
/// aspect is `cols * CHAR_ASPECT / rows`. Without this the grid thinks a 44x100 tower
/// is wide.
const CHAR_ASPECT: f32 = 0.5;
/// Shape we're aiming each tile at: an 80x24 terminal, i.e. 80 * 0.5 / 24.
const TARGET_ASPECT: f32 = 1.6;
/// Cost added per empty cell, so a grid with holes only wins if it's clearly better
/// shaped than a tight one.
const HOLE_PENALTY: f32 = 0.15;

enum Mode {
    Normal,
    /// Which agent to run comes before the name: it decides what the session *is*,
    /// and skipping it is what made every new session a claude session.
    NewAgent {
        agent: usize,
        repo_root: PathBuf,
    },
    NewInput {
        name: String,
        agent: usize,
        repo_root: PathBuf,
    },
    ConfirmKill,
}

/// One dashboard cell. Derived from durable state, not from the session store.
struct Tile {
    name: String,
    repo_root: PathBuf,
    worktree_path: PathBuf,
    running: bool,
    tmux_session: Option<String>,
    /// Agent preset from the session store, absent when that record was lost.
    agent: Option<String>,
}

struct CleanupJob {
    name: String,
    repo_root: PathBuf,
    path: PathBuf,
}

struct CleanupResult {
    name: String,
    result: Result<()>,
}

struct AppState {
    selected: usize,
    cols: usize,
    tiles: Vec<Tile>,
    panes: HashMap<String, String>,
    mode: Mode,
    message: Option<String>,
    light_bg: bool,
    repo_roots: Vec<PathBuf>,
    /// Installed agent presets, resolved once at startup — PATH doesn't move under us.
    agents: Vec<&'static agent::Agent>,
    cleanup_tx: Sender<CleanupJob>,
    cleanup_rx: Receiver<CleanupResult>,
}

impl AppState {
    fn highlight(&self) -> Color {
        if self.light_bg {
            Color::Blue
        } else {
            Color::Yellow
        }
    }
}

/// Git's worktree registry is the source of truth, so linked worktrees remain visible
/// regardless of which tool created them or where they live. The session store only
/// supplies repository scope and optional agent metadata.
fn discover(repo_roots: &[PathBuf]) -> Result<Vec<Tile>> {
    let running = tmux::list_sessions()?;
    let store = SessionStore::load().unwrap_or_default();
    let mut tiles = Vec::new();

    for repo_root in repo_roots {
        let worktrees = match worktree::list_all(repo_root) {
            Ok(worktrees) => worktrees,
            Err(e) if repo_roots.len() == 1 => return Err(e),
            Err(_) => continue,
        };
        tiles.extend(worktrees.into_iter().map(|worktree| {
            let running_session = running.iter().find(|session| {
                session.session_path == worktree.path && session.pane_path.exists()
            });
            Tile {
                agent: store
                    .get_by_worktree(&worktree.path)
                    .map(|session| session.agent.clone()),
                name: worktree.name,
                repo_root: repo_root.clone(),
                worktree_path: worktree.path,
                running: running_session.is_some(),
                tmux_session: running_session.map(|session| session.name.clone()),
            }
        }));
    }

    // A live session whose worktree was deleted underneath it still has a running
    // agent in it, so it needs a tile too.
    for session in running {
        if !session.pane_path.exists()
            || tiles
                .iter()
                .any(|tile| tile.worktree_path == session.session_path)
        {
            continue;
        }
        let repo_root = store
            .get(&session.name)
            .filter(|saved| saved.worktree_path == session.session_path)
            .map(|saved| saved.repo_root.clone())
            .or_else(|| {
                repo_roots
                    .iter()
                    .find(|repo| {
                        session.session_path.parent() == Some(worktree_root(repo).as_path())
                    })
                    .cloned()
            });
        if let Some(repo_root) = repo_root.filter(|repo| repo_roots.contains(repo)) {
            tiles.push(Tile {
                agent: store.get(&session.name).map(|saved| saved.agent.clone()),
                name: session.name.clone(),
                repo_root,
                worktree_path: session.session_path,
                running: true,
                tmux_session: Some(session.name),
            });
        }
    }

    tiles.sort_by(|a, b| {
        a.repo_root
            .cmp(&b.repo_root)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(tiles)
}

fn refresh(state: &mut AppState) {
    if let Ok(tiles) = discover(&state.repo_roots) {
        replace_tiles(state, tiles);
    }
}

fn replace_tiles(state: &mut AppState, tiles: Vec<Tile>) {
    let selected_path = state
        .tiles
        .get(state.selected)
        .map(|tile| tile.worktree_path.clone());
    state.tiles = tiles;
    if let Some(selected) = selected_path.as_deref().and_then(|path| {
        state
            .tiles
            .iter()
            .position(|tile| tile.worktree_path == path)
    }) {
        state.selected = selected;
        return;
    }
    if state.selected >= state.tiles.len() {
        state.selected = state.tiles.len().saturating_sub(1);
    }
}

fn repository_scope(current_repo: Option<PathBuf>, known_repos: &[PathBuf]) -> Vec<PathBuf> {
    current_repo
        .map(|repo| vec![repo])
        .unwrap_or_else(|| known_repos.to_vec())
}

/// Query the terminal's background color (via OSC 11) so we can pick a
/// selection highlight that stays visible on light-background terminals,
/// instead of always assuming a dark background.
fn detect_light_bg() -> bool {
    terminal_light::luma()
        .map(|luma| luma > 0.6)
        .unwrap_or(false)
}

pub fn run() -> Result<()> {
    tmux::ensure_server()?;
    let mut store = SessionStore::load()?;

    let current_repo = crate::worktree::find_repo_root_if_any(&std::env::current_dir()?)?;
    if let Some(repo_root) = &current_repo {
        store.remember_repo(repo_root)?;
    }
    let repo_roots = repository_scope(current_repo, store.repositories());
    let light_bg = detect_light_bg();
    let mut pending = Vec::new();
    for repo_root in &repo_roots {
        let items = match worktree::pending_removals(repo_root) {
            Ok(items) => items,
            Err(error) if repo_roots.len() == 1 => return Err(error),
            Err(_) => continue,
        };
        pending.extend(items.into_iter().map(|item| CleanupJob {
            name: item.name,
            repo_root: repo_root.clone(),
            path: item.path,
        }));
    }
    let (cleanup_tx, cleanup_rx) = cleanup_worker();
    for item in &pending {
        let _ = cleanup_tx.send(CleanupJob {
            name: item.name.clone(),
            repo_root: item.repo_root.clone(),
            path: item.path.clone(),
        });
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = AppState {
        selected: 0,
        cols: 1,
        tiles: Vec::new(),
        panes: HashMap::new(),
        mode: Mode::Normal,
        message: (!pending.is_empty())
            .then(|| format!("resuming {} pending cleanup(s)", pending.len())),
        light_bg,
        repo_roots,
        agents: agent::available(),
        cleanup_tx,
        cleanup_rx,
    };
    let result = event_loop(&mut terminal, &mut store, &mut state);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

/// A single worker keeps recursive deletions off the UI thread and avoids making
/// concurrent Git mutations. Staged worktrees remain registered if the process exits,
/// so the next dashboard launch can enqueue them again.
fn cleanup_worker() -> (Sender<CleanupJob>, Receiver<CleanupResult>) {
    let (job_tx, job_rx) = mpsc::channel::<CleanupJob>();
    let (result_tx, result_rx) = mpsc::channel::<CleanupResult>();
    std::thread::spawn(move || {
        for job in job_rx {
            let result = worktree::remove_registered(&job.repo_root, &job.path);
            let _ = result_tx.send(CleanupResult {
                name: job.name,
                result,
            });
        }
    });
    (job_tx, result_rx)
}

/// Stop a tmux session even when its pane has already exited. A disappearing session
/// between the list and kill calls is equivalent to a successful stop.
fn stop_session(name: &str) -> Result<()> {
    if !tmux::list_sessions()?
        .iter()
        .any(|session| session.name == name)
    {
        return Ok(());
    }
    if let Err(kill_error) = tmux::kill_session(name) {
        if tmux::list_sessions()?
            .iter()
            .any(|session| session.name == name)
        {
            return Err(kill_error);
        }
    }
    Ok(())
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    store: &mut SessionStore,
    state: &mut AppState,
) -> Result<()> {
    let mut last_poll = Instant::now() - POLL_INTERVAL;
    let mut last_discover = Instant::now() - DISCOVER_INTERVAL;

    loop {
        while let Ok(cleanup) = state.cleanup_rx.try_recv() {
            state.message = Some(match cleanup.result {
                Ok(()) => format!("removed '{}'", cleanup.name),
                Err(e) => format!(
                    "cleanup pending for '{}': {e} (will retry next launch)",
                    cleanup.name
                ),
            });
        }
        if last_discover.elapsed() >= DISCOVER_INTERVAL {
            refresh(state);
            last_discover = Instant::now();
        }
        if last_poll.elapsed() >= POLL_INTERVAL {
            if let Ok(size) = terminal.size() {
                sync_window_sizes(state, grid_area(size));
            }
            let live: Vec<String> = state
                .tiles
                .iter()
                .filter_map(|tile| tile.tmux_session.clone())
                .collect();
            for name in live {
                if let Ok(text) = tmux::capture_pane(&name, PANE_LINES) {
                    state.panes.insert(name, text);
                }
            }
            last_poll = Instant::now();
        }

        terminal.draw(|f| draw(f, state))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let mut attach_name = None;
                match &mut state.mode {
                    Mode::Normal => {
                        let len = state.tiles.len();
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Right => {
                                if len > 0 && state.selected + 1 < len {
                                    state.selected += 1;
                                }
                            }
                            KeyCode::Left => {
                                if state.selected > 0 {
                                    state.selected -= 1;
                                }
                            }
                            KeyCode::Down => {
                                if len > 0 && state.selected + state.cols < len {
                                    state.selected += state.cols;
                                }
                            }
                            KeyCode::Up => {
                                if state.selected >= state.cols {
                                    state.selected -= state.cols;
                                }
                            }
                            KeyCode::Enter => {
                                let Some((path, tmux_session)) =
                                    state.tiles.get(state.selected).map(|tile| {
                                        (tile.worktree_path.clone(), tile.tmux_session.clone())
                                    })
                                else {
                                    continue;
                                };
                                let name = if let Some(name) = tmux_session {
                                    name
                                } else {
                                    match restart(store, &path) {
                                        Ok(name) => {
                                            state.message = Some(format!("restarted '{name}'"));
                                            name
                                        }
                                        Err(e) => {
                                            state.message = Some(format!("restart error: {e}"));
                                            continue;
                                        }
                                    }
                                };
                                attach_name = Some(name);
                            }
                            KeyCode::Char('n') => {
                                let repo_root = state
                                    .tiles
                                    .get(state.selected)
                                    .map(|tile| tile.repo_root.clone())
                                    .or_else(|| {
                                        (state.repo_roots.len() == 1)
                                            .then(|| state.repo_roots[0].clone())
                                    });
                                state.mode = match (state.agents.len(), repo_root) {
                                    (_, None) => {
                                        state.message = Some(
                                            "no target repository; use `muxai new --repo <path>`"
                                                .to_string(),
                                        );
                                        Mode::Normal
                                    }
                                    (0, Some(_)) => {
                                        state.message = Some(format!(
                                            "no agent installed — looked for {} on PATH",
                                            agent::all_names().join(", ")
                                        ));
                                        Mode::Normal
                                    }
                                    // Nothing to choose between: go straight to the name.
                                    (1, Some(repo_root)) => Mode::NewInput {
                                        name: String::new(),
                                        agent: 0,
                                        repo_root,
                                    },
                                    (_, Some(repo_root)) => Mode::NewAgent {
                                        agent: 0,
                                        repo_root,
                                    },
                                };
                            }
                            KeyCode::Char('k') if len > 0 => {
                                state.mode = Mode::ConfirmKill;
                            }
                            _ => {}
                        }
                    }
                    Mode::NewAgent { agent, repo_root } => {
                        let len = state.agents.len();
                        match key.code {
                            KeyCode::Right | KeyCode::Tab => *agent = (*agent + 1) % len,
                            KeyCode::Left => *agent = (*agent + len - 1) % len,
                            KeyCode::Char(c) if c.is_ascii_digit() => {
                                if let Some(i) = c.to_digit(10).and_then(|d| d.checked_sub(1)) {
                                    if (i as usize) < len {
                                        *agent = i as usize;
                                    }
                                }
                            }
                            KeyCode::Enter => {
                                let agent = *agent;
                                state.mode = Mode::NewInput {
                                    name: String::new(),
                                    agent,
                                    repo_root: repo_root.clone(),
                                };
                            }
                            KeyCode::Esc => state.mode = Mode::Normal,
                            _ => {}
                        }
                    }
                    Mode::NewInput {
                        name: buf,
                        agent,
                        repo_root,
                    } => match key.code {
                        KeyCode::Enter => {
                            let name = buf.trim().to_string();
                            let preset = state.agents[*agent];
                            let repo_root = repo_root.clone();
                            state.mode = Mode::Normal;
                            if !name.is_empty() {
                                match create_session(
                                    store,
                                    &repo_root,
                                    &name,
                                    None,
                                    preset.name,
                                    preset.command,
                                ) {
                                    Ok(launch) => {
                                        let verb = if launch.recovered {
                                            "recovered"
                                        } else {
                                            "created"
                                        };
                                        state.message = Some(format!(
                                            "{verb} '{}' ({})",
                                            launch.session.name, launch.session.agent
                                        ));
                                        attach_name = Some(launch.session.name);
                                    }
                                    Err(e) => state.message = Some(format!("error: {e}")),
                                }
                                refresh(state);
                            }
                        }
                        KeyCode::Tab => *agent = (*agent + 1) % state.agents.len(),
                        KeyCode::Esc => state.mode = Mode::Normal,
                        KeyCode::Backspace => {
                            buf.pop();
                        }
                        KeyCode::Char(c) => buf.push(c),
                        _ => {}
                    },
                    Mode::ConfirmKill => match key.code {
                        KeyCode::Char('y') => {
                            if let Some((name, repo_root, path, tmux_session)) =
                                state.tiles.get(state.selected).map(|tile| {
                                    (
                                        tile.name.clone(),
                                        tile.repo_root.clone(),
                                        tile.worktree_path.clone(),
                                        tile.tmux_session.clone(),
                                    )
                                })
                            {
                                let stop_result =
                                    tmux_session.as_deref().map(stop_session).transpose();
                                if let Err(e) = stop_result {
                                    state.message = Some(format!(
                                        "could not stop '{name}': {e}; worktree was left in place"
                                    ));
                                } else {
                                    match worktree::stage_remove(&repo_root, &path) {
                                        Ok(staged) => {
                                            state.tiles.remove(state.selected);
                                            if let Some(tmux_session) = &tmux_session {
                                                state.panes.remove(tmux_session);
                                            }
                                            if state.selected >= state.tiles.len() {
                                                state.selected =
                                                    state.tiles.len().saturating_sub(1);
                                            }
                                            let queued = state
                                                .cleanup_tx
                                                .send(CleanupJob {
                                                    name: name.clone(),
                                                    repo_root,
                                                    path: staged,
                                                })
                                                .is_ok();
                                            let metadata_removed =
                                                store.remove_by_worktree(&path).is_ok();
                                            state.message = Some(match (queued, metadata_removed) {
                                                (true, true) => {
                                                    format!("removing '{name}' in background")
                                                }
                                                (false, true) => format!(
                                                    "'{name}' moved to trash; cleanup will retry next launch"
                                                ),
                                                (true, false) => format!(
                                                    "removing '{name}'; could not update session metadata"
                                                ),
                                                (false, false) => format!(
                                                    "'{name}' moved to trash; cleanup and metadata update are pending"
                                                ),
                                            });
                                        }
                                        Err(e) => {
                                            state.message = Some(format!(
                                                "could not remove '{name}': {e}; worktree was left in place"
                                            ));
                                            refresh(state);
                                        }
                                    }
                                }
                            }
                            state.mode = Mode::Normal;
                        }
                        _ => state.mode = Mode::Normal,
                    },
                }
                if let Some(name) = attach_name {
                    attach_session(terminal, state, &name)?;
                }
            }
        }
    }
    Ok(())
}

fn attach_session(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    state: &mut AppState,
    name: &str,
) -> Result<()> {
    // The pushed tile size outlives the attach.
    let _ = tmux::follow_client(name);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    let attach_result = tmux::attach(name);
    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;
    refresh(state);
    if let Some(selected) = state
        .tiles
        .iter()
        .position(|tile| tile.tmux_session.as_deref() == Some(name))
    {
        state.selected = selected;
    }
    match attach_result {
        Ok(tmux::AttachOutcome::Detached) => {}
        Ok(tmux::AttachOutcome::KillRequested) => state.mode = Mode::ConfirmKill,
        Err(e) => state.message = Some(format!("attach error: {e}")),
    }
    Ok(())
}

/// Bring a stopped session back up in its existing worktree, reusing the command the
/// store remembers for it (falling back to the default agent if that record was lost).
fn restart(store: &SessionStore, worktree_path: &Path) -> Result<String> {
    if !worktree_path.exists() {
        anyhow::bail!("worktree {} no longer exists", worktree_path.display());
    }
    let remembered = store.get_by_worktree(worktree_path);
    let name = remembered
        .map(|session| session.name.clone())
        .or_else(|| {
            worktree_path
                .file_name()
                .map(|name| tmux::sanitize_name(&name.to_string_lossy()))
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "muxai".to_string());
    let command = remembered
        .map(|session| session.command.clone())
        .unwrap_or_else(|| agent::default_preset().command.to_string());
    if let Some(running) = tmux::list_sessions()?
        .into_iter()
        .find(|running| running.name == name)
    {
        if running.session_path != worktree_path {
            anyhow::bail!(
                "tmux session '{name}' belongs to {}, not {}",
                running.session_path.display(),
                worktree_path.display()
            );
        }
        if running.pane_path.exists() {
            return Ok(name);
        }
        let _ = tmux::kill_session(&name);
    }
    tmux::new_session(&name, worktree_path, &command)?;
    Ok(name)
}

fn draw(f: &mut Frame, state: &mut AppState) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
    draw_grid(f, chunks[0], state);
    draw_commands_line(f, chunks[1]);
    draw_status_line(f, chunks[2], state);
}

fn draw_commands_line(f: &mut Frame, area: Rect) {
    let line = "\u{2191}\u{2193}\u{2190}\u{2192} select   Enter attach/restart   n new   k kill   q quit   in session: C-\\ dashboard, C-k kill";
    f.render_widget(
        Paragraph::new(Line::from(line)).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

/// The part of the screen `draw` hands to the grid: everything above the command bar
/// and the status line.
fn grid_area(size: Size) -> Rect {
    Rect::new(0, 0, size.width, size.height.saturating_sub(2))
}

/// Pick the column count whose resulting tiles are closest to a readable terminal
/// shape, instead of cramming in as many columns as physically fit. On a tall, narrow
/// screen three sessions become a 1x3 stack (three wide, short tiles) rather than a
/// 3x1 row of unreadable towers; on a wide screen the same three go side by side.
fn pick_cols(n: usize, area: Rect) -> usize {
    let mut best: Option<(f32, usize)> = None;
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let (w, h) = (area.width / cols as u16, area.height / rows as u16);
        if w < MIN_CELL_W || h < MIN_CELL_H {
            continue;
        }
        let aspect = (w as f32 * CHAR_ASPECT) / h as f32;
        // ln() so being 2x too wide and 2x too narrow cost the same.
        let cost = (aspect / TARGET_ASPECT).ln().abs() + (cols * rows - n) as f32 * HOLE_PENALTY;
        if best.is_none_or(|(b, _)| cost < b) {
            best = Some((cost, cols));
        }
    }
    // Nothing clears the legibility floor (tiny terminal, or many sessions): fall back
    // to the widest tiles the screen can hold.
    best.map(|(_, c)| c)
        .unwrap_or_else(|| ((area.width / MIN_CELL_W).max(1) as usize).min(n))
}

/// Size each tmux window to the tile it renders into, so the agent inside wraps its own
/// output at the tile width. Without this we re-wrap 80-column text into a 44-column
/// cell and every line comes out shredded.
fn sync_window_sizes(state: &AppState, area: Rect) {
    if state.tiles.is_empty() {
        return;
    }
    let cols = pick_cols(state.tiles.len(), area);
    let rows = state.tiles.len().div_ceil(cols);
    let inner_w = (area.width / cols as u16).saturating_sub(2);
    let inner_h = (area.height / rows as u16).saturating_sub(2);
    if inner_w == 0 || inner_h == 0 {
        return;
    }
    let Ok(live) = tmux::window_sizes() else {
        return;
    };
    for name in stale_windows(&state.tiles, &live, (inner_w, inner_h)) {
        // Best-effort: a session that died between poll and resize just retries later.
        let _ = tmux::resize_window(&name, inner_w, inner_h);
    }
}

/// Running sessions whose tmux window is not the size of the tile it renders into.
/// The size has to be measured, not remembered: `client-attached`, `client-resized`
/// and a second dashboard on a differently-sized terminal all resize a window behind
/// this one's back, and a dashboard that trusts what it last pushed never puts such a
/// window back — its tile then shows the left slice of a much wider render forever.
/// A session someone is attached to right now is left alone: resizing that one shrinks
/// it under them, leaving tmux's dotted background round the edge of their terminal.
fn stale_windows(
    tiles: &[Tile],
    live: &HashMap<String, (u16, u16, bool)>,
    target: (u16, u16),
) -> Vec<String> {
    tiles
        .iter()
        .filter_map(|tile| tile.tmux_session.as_ref().map(|session| (tile, session)))
        .filter(|(_, session)| match live.get(*session) {
            Some(&(w, h, attached)) => !attached && (w, h) != target,
            None => false,
        })
        .map(|(_, session)| session.clone())
        .collect()
}

fn draw_grid(f: &mut Frame, area: Rect, state: &mut AppState) {
    if state.tiles.is_empty() {
        let msg = Paragraph::new("No worktrees yet. Press 'n' to create one, 'q' to quit.")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(msg, area);
        return;
    }

    let cols = pick_cols(state.tiles.len(), area);
    state.cols = cols;
    let rows = state.tiles.len().div_ceil(cols);

    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Ratio(1, rows as u32))
        .collect();
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    for (row_idx, row_area) in row_areas.iter().enumerate() {
        let col_constraints: Vec<Constraint> = (0..cols)
            .map(|_| Constraint::Ratio(1, cols as u32))
            .collect();
        let col_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints)
            .split(*row_area);

        for col_idx in 0..cols {
            let idx = row_idx * cols + col_idx;
            let Some(tile) = state.tiles.get(idx) else {
                continue;
            };
            let selected = idx == state.selected;
            let (border_style, text_style) = if selected {
                let highlight = if state.light_bg {
                    Color::Blue
                } else {
                    Color::Yellow
                };
                (
                    Style::default().fg(highlight).add_modifier(Modifier::BOLD),
                    Style::default(),
                )
            } else {
                (
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::DarkGray),
                )
            };
            let title = tile_title(tile, state.repo_roots.len() > 1);
            let block = Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style);

            let text = if let Some(session) = &tile.tmux_session {
                state.panes.get(session).cloned().unwrap_or_default()
            } else {
                "no tmux session — press Enter to restart the agent here".to_string()
            };
            let inner_height = col_areas[col_idx].height.saturating_sub(2) as usize;
            let tail: String = text
                .lines()
                .rev()
                .take(inner_height.max(1))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");

            // No .wrap(): tmux already wrapped this text at the tile's width (see
            // sync_window_sizes), so re-wrapping only doubles up lines and throws off
            // the tail count above. Anything still too long gets clipped, which is what
            // a terminal would do anyway.
            let para = Paragraph::new(tail).style(text_style).block(block);
            f.render_widget(para, col_areas[col_idx]);
        }
    }
}

/// `name [agent] (stopped)`. The agent tag is what makes a mixed grid readable at a
/// glance; it's absent for a worktree the session store has no record of.
fn tile_title(tile: &Tile, show_repo: bool) -> String {
    let repo = show_repo
        .then(|| {
            tile.repo_root
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_else(|| tile.repo_root.to_string_lossy())
        })
        .map(|repo| format!("{repo}/"))
        .unwrap_or_default();
    format!(
        " {repo}{}{}{} ",
        tile.name,
        tile.agent
            .as_deref()
            .map(|a| format!(" [{a}]"))
            .unwrap_or_default(),
        if tile.running { "" } else { " (stopped)" }
    )
}

/// `1 codex  2 claude`, the picked one highlighted. Numbered because on a phone
/// keyboard a digit is reachable and an arrow key is not.
fn agent_choices(state: &AppState, picked: usize) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw("Agent: ")];
    for (i, a) in state.agents.iter().enumerate() {
        let style = if i == picked {
            Style::default()
                .fg(state.highlight())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(format!("{} {}  ", i + 1, a.name), style));
    }
    spans
}

fn draw_status_line(f: &mut Frame, area: Rect, state: &AppState) {
    if let Mode::NewAgent { agent, .. } = &state.mode {
        let mut spans = agent_choices(state, *agent);
        spans.push(Span::styled(
            " \u{2190}\u{2192}/1-9 pick   Enter continue   Esc cancel",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    let line = match &state.mode {
        Mode::NewAgent { .. } => unreachable!("handled above"),
        Mode::NewInput {
            name,
            agent,
            repo_root,
        } => format!(
            "{} session in {}: {name}_   (Tab changes agent)",
            state.agents[*agent].name,
            repo_root.display()
        ),
        Mode::ConfirmKill => {
            let name = state
                .tiles
                .get(state.selected)
                .map(|t| t.name.as_str())
                .unwrap_or("?");
            format!("Kill '{name}' and remove its worktree? y/n")
        }
        Mode::Normal => state.message.clone().unwrap_or_default(),
    };
    f.render_widget(Paragraph::new(Line::from(line)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: u16, h: u16) -> Rect {
        Rect::new(0, 0, w, h)
    }

    #[test]
    fn a_repo_launch_stays_scoped_to_that_repo() {
        assert_eq!(
            repository_scope(
                Some(PathBuf::from("/code/current")),
                &[PathBuf::from("/code/one"), PathBuf::from("/code/two")],
            ),
            [PathBuf::from("/code/current")]
        );
    }

    #[test]
    fn a_non_repo_launch_uses_every_known_repo() {
        let known = [PathBuf::from("/code/one"), PathBuf::from("/code/two")];
        assert_eq!(repository_scope(None, &known), known);
    }

    fn tagged_tile(agent: Option<&str>, running: bool) -> Tile {
        Tile {
            name: "sess".to_string(),
            repo_root: PathBuf::from("/code/repo"),
            worktree_path: PathBuf::from("/tmp/sess"),
            running,
            tmux_session: running.then(|| "sess".to_string()),
            agent: agent.map(str::to_string),
        }
    }

    #[test]
    fn tile_title_tags_the_agent() {
        assert_eq!(
            tile_title(&tagged_tile(Some("codex"), true), false),
            " sess [codex] "
        );
        assert_eq!(
            tile_title(&tagged_tile(Some("codex"), false), false),
            " sess [codex] (stopped) "
        );
    }

    #[test]
    fn tile_title_omits_an_unknown_agent() {
        assert_eq!(tile_title(&tagged_tile(None, true), false), " sess ");
        assert_eq!(
            tile_title(&tagged_tile(None, false), false),
            " sess (stopped) "
        );
        assert_eq!(tile_title(&tagged_tile(None, true), true), " repo/sess ");
    }

    #[test]
    fn tall_narrow_screen_stacks_vertically() {
        // The case that motivated this: 3 sessions on a tall, narrow terminal used to
        // become three unreadable 44x100 towers.
        assert_eq!(pick_cols(3, area(132, 100)), 1);
    }

    #[test]
    fn wide_screen_goes_side_by_side() {
        assert_eq!(pick_cols(3, area(300, 50)), 3);
    }

    #[test]
    fn four_sessions_on_a_big_screen_form_a_square() {
        assert_eq!(pick_cols(4, area(200, 60)), 2);
    }

    #[test]
    fn single_session_uses_the_whole_screen() {
        assert_eq!(pick_cols(1, area(132, 100)), 1);
    }

    #[test]
    fn hole_penalty_breaks_a_near_tie_toward_the_tight_grid() {
        // 3 sessions, 2 cols leaves an empty cell. Shapes are close here (2 cols scores
        // 0.535 on aspect alone vs 1 col's 0.563), so the hole is what decides it.
        assert_eq!(pick_cols(3, area(180, 96)), 1);
    }

    #[test]
    fn a_hole_is_worth_it_when_it_clearly_improves_shape() {
        // Wide and short: 1 col gives 240x20 letterboxes, 3 cols gives 80x60 towers.
        // 2 cols (two on top, one below) is the readable answer despite the hole.
        assert_eq!(pick_cols(3, area(240, 60)), 2);
    }

    #[test]
    fn falls_back_to_widest_fitting_grid_when_nothing_clears_the_floor() {
        // Too short for any layout to clear MIN_CELL_H: don't panic, don't return 0.
        assert_eq!(pick_cols(3, area(132, 8)), 3);
        assert_eq!(pick_cols(3, area(50, 8)), 1);
    }

    #[test]
    fn never_returns_zero_columns() {
        for n in 1..12 {
            for w in [10u16, 44, 80, 132, 300] {
                for h in [4u16, 12, 50, 100] {
                    assert!(pick_cols(n, area(w, h)) >= 1, "n={n} w={w} h={h}");
                    assert!(pick_cols(n, area(w, h)) <= n, "n={n} w={w} h={h}");
                }
            }
        }
    }

    fn tile(name: &str, running: bool) -> Tile {
        Tile {
            name: name.to_string(),
            repo_root: PathBuf::from("/repo"),
            worktree_path: PathBuf::from("/tmp").join(name),
            running,
            tmux_session: running.then(|| name.to_string()),
            agent: None,
        }
    }

    #[test]
    fn a_window_resized_behind_our_back_is_stale() {
        // The bug this guards: attaching from another terminal fires tmux's
        // client-attached hook, which snaps the window to that terminal's size. The
        // old code cached what it had asked for and so never put the window back.
        let tiles = [tile("a", true), tile("b", true)];
        let live = HashMap::from([
            ("a".to_string(), (142, 59, false)),
            ("b".to_string(), (288, 186, false)),
        ]);
        assert_eq!(stale_windows(&tiles, &live, (142, 59)), ["b"]);
    }

    #[test]
    fn an_attached_session_is_left_at_its_terminals_size() {
        let tiles = [tile("a", true)];
        let live = HashMap::from([("a".to_string(), (288, 186, true))]);
        assert!(stale_windows(&tiles, &live, (142, 59)).is_empty());
    }

    #[test]
    fn stopped_and_unknown_sessions_are_not_resized() {
        let tiles = [tile("stopped", false), tile("gone", true)];
        let live = HashMap::from([("stopped".to_string(), (288, 186, false))]);
        assert!(stale_windows(&tiles, &live, (142, 59)).is_empty());
    }

    #[test]
    fn grid_area_leaves_room_for_the_two_bottom_lines() {
        assert_eq!(grid_area(Size::new(132, 100)), area(132, 98));
        // Degenerate terminal: no underflow panic.
        assert_eq!(grid_area(Size::new(132, 1)).height, 0);
    }

    #[test]
    fn refresh_keeps_the_same_session_selected_when_tiles_move() {
        let (cleanup_tx, _cleanup_jobs) = mpsc::channel();
        let (_cleanup_results, cleanup_rx) = mpsc::channel();
        let mut state = AppState {
            selected: 1,
            cols: 1,
            tiles: vec![tile("beta", true), tile("gamma", true)],
            panes: HashMap::new(),
            mode: Mode::Normal,
            message: None,
            light_bg: false,
            repo_roots: vec![PathBuf::from("/tmp")],
            agents: Vec::new(),
            cleanup_tx,
            cleanup_rx,
        };

        replace_tiles(
            &mut state,
            vec![
                tile("alpha", true),
                tile("beta", true),
                tile("gamma", false),
            ],
        );

        assert_eq!(state.selected, 2);
        assert_eq!(state.tiles[state.selected].name, "gamma");
    }
}
