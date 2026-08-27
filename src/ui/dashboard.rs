use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};
use std::collections::HashMap;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::session_store::{worktree_root, SessionStore};
use crate::{create_session, tmux, worktree};

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
    NewInput(String),
    ConfirmKill,
}

/// One dashboard cell. Derived from durable state, not from the session store.
struct Tile {
    name: String,
    worktree_path: PathBuf,
    running: bool,
}

struct AppState {
    selected: usize,
    cols: usize,
    tiles: Vec<Tile>,
    panes: HashMap<String, String>,
    /// Tile size we last pushed to each tmux window, so we only resize on change
    /// instead of shelling out to tmux every poll.
    sizes: HashMap<String, (u16, u16)>,
    mode: Mode,
    message: Option<String>,
    light_bg: bool,
    repo_root: PathBuf,
}

/// What the dashboard shows is git's worktree list plus tmux's live session list —
/// both durable — never the session store, which is only a metadata cache: several
/// muxai processes overwrite it, and it used to be pruned on tmux liveness, which
/// deleted the only record of a worktree the moment its agent exited. A session with
/// no live tmux session stays visible as a stopped tile so its work is still reachable.
fn discover(repo_root: &Path) -> Result<Vec<Tile>> {
    let root = worktree_root(repo_root);
    let running = tmux::list_sessions_with_paths()?;

    let mut tiles: Vec<Tile> = worktree::list(repo_root)?
        .into_iter()
        .map(|w| Tile {
            running: running.iter().any(|(name, _)| *name == w.name),
            name: w.name,
            worktree_path: w.path,
        })
        .collect();

    // A live session whose worktree was deleted underneath it still has a running
    // agent in it, so it needs a tile too.
    for (name, path) in running {
        if path.parent() == Some(root.as_path()) && !tiles.iter().any(|t| t.name == name) {
            tiles.push(Tile {
                name,
                worktree_path: path,
                running: true,
            });
        }
    }

    tiles.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tiles)
}

fn refresh(state: &mut AppState) {
    if let Ok(tiles) = discover(&state.repo_root) {
        state.tiles = tiles;
    }
    if state.selected >= state.tiles.len() {
        state.selected = state.tiles.len().saturating_sub(1);
    }
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

    let repo_root = crate::worktree::find_repo_root(&std::env::current_dir()?)?;
    let light_bg = detect_light_bg();

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
        sizes: HashMap::new(),
        mode: Mode::Normal,
        message: None,
        light_bg,
        repo_root,
    };
    let result = event_loop(&mut terminal, &mut store, &mut state);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    store: &mut SessionStore,
    state: &mut AppState,
) -> Result<()> {
    let mut last_poll = Instant::now() - POLL_INTERVAL;
    let mut last_discover = Instant::now() - DISCOVER_INTERVAL;

    loop {
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
                .filter(|t| t.running)
                .map(|t| t.name.clone())
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
                                let Some((name, path, running)) = state
                                    .tiles
                                    .get(state.selected)
                                    .map(|t| (t.name.clone(), t.worktree_path.clone(), t.running))
                                else {
                                    continue;
                                };
                                if !running {
                                    match restart(store, &name, &path) {
                                        Ok(()) => {
                                            state.message = Some(format!("restarted '{name}'"))
                                        }
                                        Err(e) => {
                                            state.message = Some(format!("restart error: {e}"));
                                            continue;
                                        }
                                    }
                                }
                                disable_raw_mode()?;
                                execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                                let attach_result = tmux::attach(&name);
                                enable_raw_mode()?;
                                execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                                terminal.clear()?;
                                // tmux resized the window to the real terminal for
                                // the attach; forget our cached tile sizes so the
                                // next poll puts it back.
                                state.sizes.clear();
                                if let Err(e) = attach_result {
                                    state.message = Some(format!("attach error: {e}"));
                                }
                                refresh(state);
                            }
                            KeyCode::Char('n') => {
                                state.mode = Mode::NewInput(String::new());
                            }
                            KeyCode::Char('k') if len > 0 => {
                                state.mode = Mode::ConfirmKill;
                            }
                            _ => {}
                        }
                    }
                    Mode::NewInput(buf) => match key.code {
                        KeyCode::Enter => {
                            let name = buf.trim().to_string();
                            state.mode = Mode::Normal;
                            if !name.is_empty() {
                                match create_session(store, &state.repo_root, &name, None, "claude")
                                {
                                    Ok(s) => state.message = Some(format!("created '{}'", s.name)),
                                    Err(e) => state.message = Some(format!("error: {e}")),
                                }
                                refresh(state);
                            }
                        }
                        KeyCode::Esc => state.mode = Mode::Normal,
                        KeyCode::Backspace => {
                            buf.pop();
                        }
                        KeyCode::Char(c) => buf.push(c),
                        _ => {}
                    },
                    Mode::ConfirmKill => match key.code {
                        KeyCode::Char('y') => {
                            if let Some((name, path)) = state
                                .tiles
                                .get(state.selected)
                                .map(|t| (t.name.clone(), t.worktree_path.clone()))
                            {
                                let _ = tmux::kill_session(&name);
                                let _ = worktree::remove(&state.repo_root, &path);
                                let _ = store.remove(&name);
                                refresh(state);
                            }
                            state.mode = Mode::Normal;
                        }
                        _ => state.mode = Mode::Normal,
                    },
                }
            }
        }
    }
    Ok(())
}

/// Bring a stopped session back up in its existing worktree, reusing the command the
/// store remembers for it (falling back to `claude` if that record was lost).
fn restart(store: &SessionStore, name: &str, worktree_path: &Path) -> Result<()> {
    if !worktree_path.exists() {
        anyhow::bail!("worktree {} no longer exists", worktree_path.display());
    }
    let command = store
        .get(name)
        .map(|s| s.command.clone())
        .unwrap_or_else(|| "claude".to_string());
    tmux::new_session(name, worktree_path, &command)
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
    let line = "\u{2191}\u{2193}\u{2190}\u{2192} select   Enter attach/restart (C-\\ in session returns here)   n new   k kill   q quit";
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
fn sync_window_sizes(state: &mut AppState, area: Rect) {
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
    let stale: Vec<String> = state
        .tiles
        .iter()
        .filter(|t| t.running && state.sizes.get(&t.name) != Some(&(inner_w, inner_h)))
        .map(|t| t.name.clone())
        .collect();
    for name in stale {
        // Best-effort: a session that died between poll and resize just retries later.
        if tmux::resize_window(&name, inner_w, inner_h).is_ok() {
            state.sizes.insert(name, (inner_w, inner_h));
        }
    }
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
            let title = if tile.running {
                format!(" {} ", tile.name)
            } else {
                format!(" {} (stopped) ", tile.name)
            };
            let block = Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style);

            let text = if tile.running {
                state.panes.get(&tile.name).cloned().unwrap_or_default()
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

fn draw_status_line(f: &mut Frame, area: Rect, state: &AppState) {
    let line = match &state.mode {
        Mode::NewInput(buf) => format!("New session name: {buf}_"),
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

    #[test]
    fn grid_area_leaves_room_for_the_two_bottom_lines() {
        assert_eq!(grid_area(Size::new(132, 100)), area(132, 98));
        // Degenerate terminal: no underflow panic.
        assert_eq!(grid_area(Size::new(132, 1)).height, 0);
    }
}
