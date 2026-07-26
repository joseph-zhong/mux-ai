use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "muxai", version, about = "Grid-dashboard TUI for parallel coding agents across git worktrees")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create a worktree + tmux session running an agent command in it.
    New {
        /// Session name (also used as the worktree directory name).
        name: String,
        /// Branch to create for the worktree (defaults to the session name).
        #[arg(long)]
        branch: Option<String>,
        /// Repo to create the worktree in (defaults to the current git repo).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Command to run in the session (defaults to `claude`).
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Grid dashboard of all sessions (default when run with no subcommand).
    Dashboard,
    /// Kill a session.
    Kill {
        name: String,
        /// Also remove its git worktree.
        #[arg(long)]
        remove_worktree: bool,
    },
    /// Disk (shared cache vs. reclaimable per-worktree build state) and memory usage.
    Status,
    /// Reclaim per-worktree build state, prune dead worktrees, drop stale sessions.
    Reset {
        /// Actually delete/prune; without this, reset prints what it would do.
        #[arg(long)]
        yes: bool,
    },
}
