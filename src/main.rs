mod cli;
mod session_store;
mod stats;
mod tmux;
mod ui;
mod worktree;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use cli::{Cli, Command};
use session_store::{Session, SessionStore};
use std::path::{Path, PathBuf};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Dashboard) {
        Command::Dashboard => ui::dashboard::run(),
        Command::New {
            name,
            branch,
            repo,
            command,
        } => {
            let repo_root = resolve_repo_root(repo)?;
            let mut store = SessionStore::load()?;
            let cmd = if command.is_empty() {
                "claude".to_string()
            } else {
                command.join(" ")
            };
            let session = create_session(&mut store, &repo_root, &name, branch.as_deref(), &cmd)?;
            println!(
                "created session '{}' in {} (branch '{}')\n  attach: muxai attach {}   (or `muxai`, then select it and press Enter)",
                session.name,
                session.worktree_path.display(),
                session.branch,
                session.name
            );
            Ok(())
        }
        Command::Attach { name } => {
            // Live tmux sessions, not the store — same source of truth the dashboard
            // uses, so `attach` can't disagree with what the grid shows.
            let live = tmux::list_sessions_with_paths()?;
            if !live.iter().any(|(n, _)| *n == name) {
                let names: Vec<&str> = live.iter().map(|(n, _)| n.as_str()).collect();
                anyhow::bail!(
                    "no running session '{name}'{}",
                    if names.is_empty() {
                        " (none are running)".to_string()
                    } else {
                        format!(" — running: {}", names.join(", "))
                    }
                );
            }
            tmux::attach(&name)
        }
        Command::Kill { name, remove_worktree } => {
            let mut store = SessionStore::load()?;
            let session = store
                .get(&name)
                .cloned()
                .with_context(|| format!("no such session '{name}'"))?;
            let _ = tmux::kill_session(&name); // already-gone is fine
            if remove_worktree {
                worktree::remove(&session.repo_root, &session.worktree_path)?;
            }
            store.remove(&name)?;
            println!("killed '{name}'{}", if remove_worktree { " and removed its worktree" } else { "" });
            Ok(())
        }
        Command::Status => {
            let store = SessionStore::load()?;
            print_status(&store);
            Ok(())
        }
        Command::Reset { yes } => {
            let mut store = SessionStore::load()?;
            let log = stats::reset(&mut store, yes)?;
            for line in log {
                println!("{line}");
            }
            if !yes {
                println!("\n(dry run — pass --yes to actually delete/prune)");
            }
            Ok(())
        }
    }
}

fn resolve_repo_root(repo: Option<PathBuf>) -> Result<PathBuf> {
    let start = repo.unwrap_or(std::env::current_dir()?);
    worktree::find_repo_root(&start)
}

/// Shared by `muxai new` and the dashboard's 'n' key.
pub fn create_session(
    store: &mut SessionStore,
    repo_root: &Path,
    name: &str,
    branch: Option<&str>,
    command: &str,
) -> Result<Session> {
    if store.get(name).is_some() {
        anyhow::bail!("session '{name}' already exists");
    }
    let branch = branch.unwrap_or(name).to_string();
    let worktree_path = worktree::create(repo_root, name, &branch)?;

    tmux::ensure_server()?;
    if let Err(e) = tmux::new_session(name, &worktree_path, command) {
        // Don't leave an orphaned worktree if the tmux session failed to start.
        let _ = worktree::remove(repo_root, &worktree_path);
        return Err(e);
    }

    let session = Session {
        name: name.to_string(),
        repo_root: repo_root.to_path_buf(),
        worktree_path,
        branch,
        command: command.to_string(),
        created_at: Utc::now().to_rfc3339(),
    };
    store.add(session.clone())?;
    Ok(session)
}

fn print_status(store: &SessionStore) {
    let report = stats::build_report(store);

    println!("Sessions: {}", store.list().len());
    println!();
    println!("{:<20} {:>10} {:>14}", "WORKTREE", "TOTAL", "RECLAIMABLE");
    for w in &report.worktrees {
        println!(
            "{:<20} {:>10} {:>14}",
            w.name,
            stats::format_kb(w.total_kb),
            stats::format_kb(w.reclaimable_kb)
        );
    }

    if !report.shared_caches.is_empty() {
        println!("\nShared caches (not reclaimable by `muxai reset`):");
        for (label, kb) in &report.shared_caches {
            println!("  {:<28} {:>10}", label, stats::format_kb(*kb));
        }
    }

    let budget = report.memory_budget_bytes;
    let used = report.memory_bytes;
    let ratio = if budget > 0 { used as f64 / budget as f64 } else { 0.0 };
    let flag = if ratio < 0.5 {
        "green"
    } else if ratio < 0.85 {
        "yellow"
    } else {
        "red"
    };
    println!(
        "\nMemory (agent process trees): {} / {} budget [{flag}]",
        stats::format_bytes(used),
        stats::format_bytes(budget)
    );
}
