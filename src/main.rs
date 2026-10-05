mod agent;
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

pub struct SessionLaunch {
    pub session: Session,
    pub recovered: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    tmux::ensure_available()?;
    match cli.command.unwrap_or(Command::Dashboard) {
        Command::Dashboard => ui::dashboard::run(),
        Command::New {
            name,
            branch,
            repo,
            agent,
            command,
        } => {
            let repo_root = resolve_repo_root(repo)?;
            let mut store = SessionStore::load()?;
            // An explicit `-- <command>` still wins over the preset's command; the
            // preset name is kept either way, since it's what the dashboard tags.
            let agent_name = agent.unwrap_or_else(|| {
                if command.is_empty() {
                    agent::DEFAULT.to_string()
                } else {
                    agent::CUSTOM.to_string()
                }
            });
            let cmd = if command.is_empty() {
                let preset = agent::resolve(&agent_name)?;
                // Without this the tmux session starts, the command is not found, and
                // the pane dies — surfacing as an empty tile rather than a missing CLI.
                if !agent::on_path(preset.command) {
                    anyhow::bail!(
                        "agent '{}' needs '{}' on PATH, and it isn't installed",
                        preset.name,
                        preset.command
                    );
                }
                preset.command.to_string()
            } else {
                command.join(" ")
            };
            let launch = create_session(
                &mut store,
                &repo_root,
                &name,
                branch.as_deref(),
                &agent_name,
                &cmd,
            )?;
            let session = launch.session;
            println!(
                "{} session '{}' running '{}' in {} (branch '{}')\n  attach: muxai   (then select it and press Enter)",
                if launch.recovered { "recovered" } else { "created" },
                session.name,
                session.command,
                session.worktree_path.display(),
                session.branch
            );
            Ok(())
        }
        Command::Kill {
            name,
            remove_worktree,
        } => {
            let name = tmux::sanitize_name(&name);
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
            println!(
                "killed '{name}'{}",
                if remove_worktree {
                    " and removed its worktree"
                } else {
                    ""
                }
            );
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
    agent: &str,
    command: &str,
) -> Result<SessionLaunch> {
    let name = tmux::sanitize_name(name);
    store.reload()?;

    if let Some(existing) = worktree::list(repo_root)?
        .into_iter()
        .find(|candidate| candidate.name == name)
    {
        let remembered = store
            .get(&name)
            .filter(|session| {
                session.repo_root == repo_root && session.worktree_path == existing.path
            })
            .cloned();
        let session = remembered.unwrap_or_else(|| Session {
            name: name.clone(),
            repo_root: repo_root.to_path_buf(),
            worktree_path: existing.path.clone(),
            branch: branch.unwrap_or(&name).to_string(),
            agent: agent.to_string(),
            command: command.to_string(),
            created_at: Utc::now().to_rfc3339(),
        });

        let running_path = tmux::list_sessions_with_paths()?
            .into_iter()
            .find_map(|(running_name, path)| (running_name == name).then_some(path));
        match running_path {
            Some(path) if path != existing.path => anyhow::bail!(
                "tmux session '{name}' belongs to {}, not {}",
                path.display(),
                existing.path.display()
            ),
            Some(_) => {}
            None => {
                tmux::ensure_server()?;
                tmux::new_session(&name, &existing.path, &session.command)?;
            }
        }

        store.add(session.clone())?;
        return Ok(SessionLaunch {
            session,
            recovered: true,
        });
    }

    let branch = branch.unwrap_or(&name).to_string();
    let recovered = worktree::branch_exists(repo_root, &branch)?;
    let worktree_path = worktree::create(repo_root, &name, &branch)?;

    tmux::ensure_server()?;
    if let Err(e) = tmux::new_session(&name, &worktree_path, command) {
        // Don't leave an orphaned worktree if the tmux session failed to start.
        let _ = worktree::remove(repo_root, &worktree_path);
        return Err(e);
    }

    let session = Session {
        name,
        repo_root: repo_root.to_path_buf(),
        worktree_path,
        branch,
        agent: agent.to_string(),
        command: command.to_string(),
        created_at: Utc::now().to_rfc3339(),
    };
    store.add(session.clone())?;
    Ok(SessionLaunch { session, recovered })
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
    let ratio = if budget > 0 {
        used as f64 / budget as f64
    } else {
        0.0
    };
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
