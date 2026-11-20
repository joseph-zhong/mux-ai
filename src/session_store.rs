use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    pub repo_root: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
    /// Preset the session was started from, or `custom` for an explicit command.
    /// Defaulted on read so session stores written before presets existed still load.
    #[serde(default = "legacy_agent_name")]
    pub agent: String,
    pub command: String,
    pub created_at: String,
}

fn legacy_agent_name() -> String {
    "claude".to_string()
}

#[derive(Debug, Default)]
pub struct SessionStore {
    sessions: Vec<Session>,
    repositories: Vec<PathBuf>,
    path: PathBuf,
    repositories_path: PathBuf,
}

impl SessionStore {
    pub fn store_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        Ok(home.join(".local/state/muxai/sessions.json"))
    }

    fn repositories_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        Ok(home.join(".local/state/muxai/repositories.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::store_path()?;
        let sessions: Vec<Session> = if path.exists() {
            let data = fs::read_to_string(&path)
                .with_context(|| format!("reading session store at {}", path.display()))?;
            serde_json::from_str(&data)
                .with_context(|| format!("parsing session store at {}", path.display()))?
        } else {
            Vec::new()
        };

        let repositories_path = Self::repositories_path()?;
        let mut repositories: Vec<PathBuf> = if repositories_path.exists() {
            let data = fs::read_to_string(&repositories_path).with_context(|| {
                format!(
                    "reading repository store at {}",
                    repositories_path.display()
                )
            })?;
            serde_json::from_str(&data).with_context(|| {
                format!(
                    "parsing repository store at {}",
                    repositories_path.display()
                )
            })?
        } else {
            Vec::new()
        };
        for session in &sessions {
            if !repositories.contains(&session.repo_root) {
                repositories.push(session.repo_root.clone());
            }
        }

        Ok(Self {
            sessions,
            repositories,
            path,
            repositories_path,
        })
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(&self.sessions)?;
        fs::write(&self.path, data)
            .with_context(|| format!("writing session store at {}", self.path.display()))?;
        let data = serde_json::to_string_pretty(&self.repositories)?;
        fs::write(&self.repositories_path, data).with_context(|| {
            format!(
                "writing repository store at {}",
                self.repositories_path.display()
            )
        })?;
        Ok(())
    }

    pub fn list(&self) -> &[Session] {
        &self.sessions
    }

    pub fn get(&self, name: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.name == name)
    }

    pub fn get_by_worktree(&self, path: &Path) -> Option<&Session> {
        self.sessions.iter().find(|s| s.worktree_path == path)
    }

    pub fn repositories(&self) -> &[PathBuf] {
        &self.repositories
    }

    pub fn remember_repo(&mut self, repo_root: &Path) -> Result<()> {
        *self = Self::load()?;
        if !self.repositories.iter().any(|repo| repo == repo_root) {
            self.repositories.push(repo_root.to_path_buf());
            self.save()?;
        }
        Ok(())
    }

    pub fn reload(&mut self) -> Result<()> {
        *self = Self::load()?;
        Ok(())
    }

    /// Several muxai processes share one store file, and `save` rewrites the whole
    /// file — so every mutation re-reads first, or a stale in-memory copy silently
    /// deletes sessions another process created.
    pub fn add(&mut self, session: Session) -> Result<()> {
        *self = Self::load()?;
        self.sessions.retain(|s| s.name != session.name);
        if !self.repositories.contains(&session.repo_root) {
            self.repositories.push(session.repo_root.clone());
        }
        self.sessions.push(session);
        self.save()
    }

    pub fn remove(&mut self, name: &str) -> Result<Option<Session>> {
        *self = Self::load()?;
        let removed = self
            .sessions
            .iter()
            .position(|s| s.name == name)
            .map(|idx| self.sessions.remove(idx));
        self.save()?;
        Ok(removed)
    }

    pub fn remove_by_worktree(&mut self, path: &Path) -> Result<Option<Session>> {
        *self = Self::load()?;
        let removed = self
            .sessions
            .iter()
            .position(|s| s.worktree_path == path)
            .map(|idx| self.sessions.remove(idx));
        self.save()?;
        Ok(removed)
    }
}

pub fn worktree_root(repo_root: &Path) -> PathBuf {
    repo_root.join(".muxai/worktrees")
}

#[cfg(test)]
mod tests {
    use super::Session;

    /// Session stores written before agent presets existed have no `agent` field.
    #[test]
    fn a_record_without_an_agent_field_still_loads() {
        let json = r#"{
            "name": "old",
            "repo_root": "/repo",
            "worktree_path": "/repo/.muxai/worktrees/old",
            "branch": "old",
            "command": "claude",
            "created_at": "2026-08-01T00:00:00Z"
        }"#;
        let s: Session = serde_json::from_str(json).unwrap();
        assert_eq!(s.agent, "claude");
    }
}
