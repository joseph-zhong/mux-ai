use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::session_store::worktree_root;

fn git(repo_root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo_root);
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

pub fn find_repo_root(start: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("running git rev-parse")?;
    if !out.status.success() {
        bail!(
            "{} is not inside a git repository",
            start.display()
        );
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// Ensures .muxai/ (our worktrees + scratch space) is git-ignored in the host repo.
fn ensure_gitignored(repo_root: &Path) -> Result<()> {
    let gitignore = repo_root.join(".gitignore");
    let entry = ".muxai/";
    let existing = fs::read_to_string(&gitignore).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == entry) {
        return Ok(());
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(entry);
    updated.push('\n');
    fs::write(&gitignore, updated)
        .with_context(|| format!("writing {}", gitignore.display()))?;
    Ok(())
}

/// Creates `<repo_root>/.muxai/worktrees/<name>` on a new branch `branch`.
pub fn create(repo_root: &Path, name: &str, branch: &str) -> Result<PathBuf> {
    ensure_gitignored(repo_root)?;
    let path = worktree_root(repo_root).join(name);
    if path.exists() {
        bail!("worktree path {} already exists", path.display());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    run_ok(git(repo_root).args([
        "worktree",
        "add",
        "-b",
        branch,
        &path.to_string_lossy(),
    ]))?;
    Ok(path)
}

pub fn remove(repo_root: &Path, path: &Path) -> Result<()> {
    if path.exists() {
        run_ok(git(repo_root).args([
            "worktree",
            "remove",
            "--force",
            &path.to_string_lossy(),
        ]))?;
    }
    Ok(())
}

pub fn prune(repo_root: &Path) -> Result<()> {
    run_ok(git(repo_root).args(["worktree", "prune"]))?;
    Ok(())
}
