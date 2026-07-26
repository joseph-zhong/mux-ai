use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::collections::HashMap;
use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use crate::session_store::SessionStore;
use crate::{create_session, tmux};

const POLL_INTERVAL: Duration = Duration::from_millis(300);
const PANE_LINES: u16 = 200;
const CELL_WIDTH: u16 = 44;

enum Mode {
    Normal,
    NewInput(String),
    ConfirmKill,
}

struct AppState {
    selected: usize,
    cols: usize,
    panes: HashMap<String, String>,
    mode: Mode,
    message: Option<String>,
}

/// Drop session-store entries whose tmux session no longer exists (e.g. killed
/// outside mux-ai, or the process inside it exited).
fn reconcile(store: &mut SessionStore) -> Result<()> {
    let running = tmux::list_sessions()?;
    let dropped = store.retain_running(&running);
    if !dropped.is_empty() {
        store.save()?;
    }
    Ok(())
}

pub fn run() -> Result<()> {
    tmux::ensure_server()?;
    let mut store = SessionStore::load()?;
    reconcile(&mut store)?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = AppState {
        selected: 0,
        cols: 1,
        panes: HashMap::new(),
        mode: Mode::Normal,
        message: None,
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

    loop {
        if last_poll.elapsed() >= POLL_INTERVAL {
            for s in store.list() {
                if let Ok(text) = tmux::capture_pane(&s.name, PANE_LINES) {
                    state.panes.insert(s.name.clone(), text);
                }
            }
            last_poll = Instant::now();
        }

        terminal.draw(|f| draw(f, store, state))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match &mut state.mode {
                    Mode::Normal => {
                        let len = store.list().len();
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
                                if let Some(session) = store.list().get(state.selected) {
                                    let name = session.name.clone();
                                    disable_raw_mode()?;
                                    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                                    let attach_result = tmux::attach(&name);
                                    enable_raw_mode()?;
                                    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                                    terminal.clear()?;
                                    if let Err(e) = attach_result {
                                        state.message = Some(format!("attach error: {e}"));
                                    }
                                }
                            }
                            KeyCode::Char('n') => {
                                state.mode = Mode::NewInput(String::new());
                            }
                            KeyCode::Char('k') => {
                                if len > 0 {
                                    state.mode = Mode::ConfirmKill;
                                }
                            }
                            _ => {}
                        }
                    }
                    Mode::NewInput(buf) => match key.code {
                        KeyCode::Enter => {
                            let name = buf.trim().to_string();
                            state.mode = Mode::Normal;
                            if !name.is_empty() {
                                match new_from_cwd(store, &name) {
                                    Ok(()) => state.message = Some(format!("created '{name}'")),
                                    Err(e) => state.message = Some(format!("error: {e}")),
                                }
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
                            if let Some(session) = store.list().get(state.selected).cloned() {
                                let _ = tmux::kill_session(&session.name);
                                store.remove(&session.name);
                                store.save()?;
                                let len = store.list().len();
                                if len > 0 && state.selected >= len {
                                    state.selected = len - 1;
                                }
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

fn new_from_cwd(store: &mut SessionStore, name: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo_root = crate::worktree::find_repo_root(&cwd)?;
    create_session(store, &repo_root, name, None, "claude")?;
    Ok(())
}

fn draw(f: &mut Frame, store: &SessionStore, state: &mut AppState) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    draw_grid(f, chunks[0], store, state);
    draw_status_line(f, chunks[1], store, state);
}

fn draw_grid(f: &mut Frame, area: Rect, store: &SessionStore, state: &mut AppState) {
    let sessions = store.list();
    if sessions.is_empty() {
        let msg = Paragraph::new("No sessions yet. Press 'n' to create one, 'q' to quit.")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(msg, area);
        return;
    }

    let cols = ((area.width / CELL_WIDTH).max(1) as usize).min(sessions.len());
    state.cols = cols;
    let rows = sessions.len().div_ceil(cols);

    let row_constraints: Vec<Constraint> = (0..rows)
        .map(|_| Constraint::Ratio(1, rows as u32))
        .collect();
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    for (row_idx, row_area) in row_areas.iter().enumerate() {
        let col_constraints: Vec<Constraint> =
            (0..cols).map(|_| Constraint::Ratio(1, cols as u32)).collect();
        let col_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints)
            .split(*row_area);

        for col_idx in 0..cols {
            let idx = row_idx * cols + col_idx;
            let Some(session) = sessions.get(idx) else {
                continue;
            };
            let selected = idx == state.selected;
            let border_style = if selected {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let title = format!(" {} ", session.name);
            let block = Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style);

            let text = state
                .panes
                .get(&session.name)
                .cloned()
                .unwrap_or_default();
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

            let para = Paragraph::new(tail)
                .block(block)
                .wrap(Wrap { trim: false });
            f.render_widget(para, col_areas[col_idx]);
        }
    }
}

fn draw_status_line(f: &mut Frame, area: Rect, store: &SessionStore, state: &AppState) {
    let line = match &state.mode {
        Mode::NewInput(buf) => format!("New session name: {buf}_"),
        Mode::ConfirmKill => {
            let name = store
                .list()
                .get(state.selected)
                .map(|s| s.name.as_str())
                .unwrap_or("?");
            format!("Kill '{name}'? y/n")
        }
        Mode::Normal => state.message.clone().unwrap_or_else(|| {
            "\u{2191}\u{2193}\u{2190}\u{2192} select   Enter attach (C-\\ in session returns here)   n new   k kill   q quit"
                .to_string()
        }),
    };
    f.render_widget(Paragraph::new(Line::from(line)), area);
}
