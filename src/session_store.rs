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
    pub command: String,
    pub created_at: String,
}

#[derive(Debug, Default)]
pub struct SessionStore {
    sessions: Vec<Session>,
    path: PathBuf,
}

impl SessionStore {
    pub fn store_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("could not determine home directory")?;
        Ok(home.join(".local/state/muxai/sessions.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::store_path()?;
        if !path.exists() {
            return Ok(Self {
                sessions: Vec::new(),
                path,
            });
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("reading session store at {}", path.display()))?;
        let sessions: Vec<Session> = serde_json::from_str(&data)
            .with_context(|| format!("parsing session store at {}", path.display()))?;
        Ok(Self { sessions, path })
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(&self.sessions)?;
        fs::write(&self.path, data)
            .with_context(|| format!("writing session store at {}", self.path.display()))?;
        Ok(())
    }

    pub fn list(&self) -> &[Session] {
        &self.sessions
    }

    pub fn get(&self, name: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.name == name)
    }

    /// Several muxai processes share one store file, and `save` rewrites the whole
    /// file — so every mutation re-reads first, or a stale in-memory copy silently
    /// deletes sessions another process created.
    pub fn add(&mut self, session: Session) -> Result<()> {
        *self = Self::load()?;
        self.sessions.retain(|s| s.name != session.name);
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
}

pub fn worktree_root(repo_root: &Path) -> PathBuf {
    repo_root.join(".muxai/worktrees")
}
