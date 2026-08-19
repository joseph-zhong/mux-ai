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

/// The *main* worktree's root, even when called from inside a linked worktree.
/// `--show-toplevel` returns the linked worktree there, which would make muxai nest
/// a `.muxai/worktrees` tree inside a worktree and hide the repo's real sessions.
pub fn find_repo_root(start: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .context("running git rev-parse")?;
    if !out.status.success() {
        bail!(
            "{} is not inside a git repository",
            start.display()
        );
    }
    // <root>/.git -> <root>
    let common = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    Ok(common.parent().unwrap_or(&common).to_path_buf())
}

pub struct Worktree {
    pub name: String,
    pub path: PathBuf,
}

/// The worktrees git actually knows about under `<repo>/.muxai/worktrees`. This is the
/// durable record of a session's work — it survives the agent exiting, the tmux server
/// restarting, and the session store losing its row.
pub fn list(repo_root: &Path) -> Result<Vec<Worktree>> {
    let out = run_ok(git(repo_root).args(["worktree", "list", "--porcelain"]))?;
    let root = worktree_root(repo_root);
    let mut found = Vec::new();
    let mut path: Option<PathBuf> = None;
    // Porcelain emits one blank-line-terminated block per worktree.
    for line in out.lines().chain(std::iter::once("")) {
        if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(rest));
        } else if line.is_empty() {
            if let Some(p) = path.take() {
                if p.parent() == Some(root.as_path()) {
                    if let Some(name) = p.file_name() {
                        found.push(Worktree {
                            name: name.to_string_lossy().into_owned(),
                            path: p,
                        });
                    }
                }
            }
        }
    }
    Ok(found)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "muxai-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        // macOS temp_dir is a symlink; canonicalize so it matches git's absolute output.
        let dir = dir.canonicalize().unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
            vec!["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            assert!(git(&dir).args(&args).status().unwrap().success());
        }
        dir
    }

    #[test]
    fn lists_muxai_worktrees_and_resolves_main_root_from_inside_one() {
        let repo = tmp_repo();
        let alpha = create(&repo, "alpha", "alpha").unwrap();
        create(&repo, "beta", "beta").unwrap();
        // A worktree outside .muxai/worktrees belongs to the user, not to muxai.
        let outside = repo.join("elsewhere");
        run_ok(git(&repo).args(["worktree", "add", "-q", "-b", "other", &outside.to_string_lossy()]))
            .unwrap();

        let mut names: Vec<String> = list(&repo).unwrap().into_iter().map(|w| w.name).collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);

        // The bug this guards: from inside a linked worktree, --show-toplevel returns
        // the worktree itself, so the dashboard would look for sessions in the wrong place.
        assert_eq!(find_repo_root(&alpha).unwrap(), repo);

        fs::remove_dir_all(&repo).unwrap();
    }
}
