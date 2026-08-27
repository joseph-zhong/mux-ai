use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use sysinfo::{Pid, System};

use crate::session_store::{Session, SessionStore};
use crate::{tmux, worktree};

/// Directories that toolchains regenerate per-worktree instead of sharing through a
/// global cache. These are exactly what `reset` is allowed to delete — see
/// DESIGN.md's "global-buck-clone" table for which toolchains already share instead.
const RECLAIMABLE_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".venv",
    "dist",
    ".next",
    "__pycache__",
];

pub struct WorktreeUsage {
    pub name: String,
    pub total_kb: u64,
    pub reclaimable_kb: u64,
}

pub struct StatusReport {
    pub worktrees: Vec<WorktreeUsage>,
    pub shared_caches: Vec<(String, u64)>,
    pub memory_bytes: u64,
    pub memory_budget_bytes: u64,
}

fn du_kb(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    Command::new("du")
        .args(["-sk", &path.to_string_lossy()])
        .output()
        .ok()
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

fn worktree_breakdown(path: &Path) -> (u64, u64) {
    let total = du_kb(path);
    let reclaimable: u64 = RECLAIMABLE_DIRS.iter().map(|d| du_kb(&path.join(d))).sum();
    (total, reclaimable)
}

/// Global caches that *should* be shared across every worktree. Reported for context
/// only — reset never touches these.
fn shared_cache_sizes() -> Vec<(String, u64)> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let candidates = [
            ("uv cache", home.join(".cache/uv")),
            ("cargo registry", home.join(".cargo/registry")),
            (
                "pnpm store (linux-style)",
                home.join(".local/share/pnpm/store"),
            ),
            ("pnpm store (macOS)", home.join("Library/pnpm/store")),
        ];
        for (label, path) in candidates {
            if path.exists() {
                out.push((label.to_string(), du_kb(&path)));
            }
        }
    }
    out
}

/// Sums RSS across a process and all its descendants — a session's tmux pane process
/// plus whatever it forked (shell, agent CLI, any build/test child processes).
fn process_tree_bytes(sys: &System, root: u32) -> u64 {
    let mut children_of: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, proc_) in sys.processes() {
        if let Some(parent) = proc_.parent() {
            children_of.entry(parent).or_default().push(*pid);
        }
    }

    let root_pid = Pid::from_u32(root);
    let mut total = 0u64;
    let mut stack = vec![root_pid];
    while let Some(pid) = stack.pop() {
        if let Some(proc_) = sys.process(pid) {
            total += proc_.memory();
        }
        if let Some(children) = children_of.get(&pid) {
            stack.extend(children.iter().copied());
        }
    }
    total
}

pub fn worktree_usage(sessions: &[Session]) -> Vec<WorktreeUsage> {
    sessions
        .iter()
        .map(|s| {
            let (total_kb, reclaimable_kb) = worktree_breakdown(&s.worktree_path);
            WorktreeUsage {
                name: s.name.clone(),
                total_kb,
                reclaimable_kb,
            }
        })
        .collect()
}

pub fn memory_bytes(sessions: &[Session]) -> u64 {
    let mut sys = System::new_all();
    sys.refresh_all();
    sessions
        .iter()
        .filter_map(|s| tmux::pane_pid(&s.name).ok().flatten())
        .map(|pid| process_tree_bytes(&sys, pid))
        .sum()
}

pub const DEFAULT_MEMORY_BUDGET_BYTES: u64 = 32 * 1024 * 1024 * 1024; // 32GB — see DESIGN.md sizing notes

pub fn build_report(store: &SessionStore) -> StatusReport {
    let sessions = store.list();
    StatusReport {
        worktrees: worktree_usage(sessions),
        shared_caches: shared_cache_sizes(),
        memory_bytes: memory_bytes(sessions),
        memory_budget_bytes: DEFAULT_MEMORY_BUDGET_BYTES,
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1}GB", b / GB)
    } else {
        format!("{:.0}MB", b / MB)
    }
}

pub fn format_kb(kb: u64) -> String {
    format_bytes(kb * 1024)
}

/// Deletes only RECLAIMABLE_DIRS inside each worktree, prunes dead worktree
/// registrations, and drops sessions from the store whose worktree is gone.
/// Never touches shared caches.
pub fn reset(store: &mut SessionStore, yes: bool) -> Result<Vec<String>> {
    let mut log = Vec::new();

    for session in store.list() {
        for dir in RECLAIMABLE_DIRS {
            let target = session.worktree_path.join(dir);
            let size_kb = du_kb(&target);
            if size_kb == 0 {
                continue;
            }
            if !yes {
                log.push(format!(
                    "would remove {} ({}) [pass --yes to actually delete]",
                    target.display(),
                    format_kb(size_kb)
                ));
                continue;
            }
            std::fs::remove_dir_all(&target).ok();
            log.push(format!(
                "removed {} ({})",
                target.display(),
                format_kb(size_kb)
            ));
        }
    }

    if yes {
        let mut seen_repos = std::collections::HashSet::new();
        for session in store.list() {
            if seen_repos.insert(session.repo_root.clone()) {
                worktree::prune(&session.repo_root)?;
            }
        }
        // Keyed on the worktree, not on tmux: an agent exiting leaves the work on
        // disk, and dropping the record there would hide it from the dashboard.
        let dropped: Vec<String> = store
            .list()
            .iter()
            .filter(|s| !s.worktree_path.exists())
            .map(|s| s.name.clone())
            .collect();
        for name in dropped {
            store.remove(&name)?;
            log.push(format!("dropped stale session record: {name}"));
        }
    }

    Ok(log)
}
