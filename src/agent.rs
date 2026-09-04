use anyhow::{bail, Result};
use std::os::unix::fs::PermissionsExt;

/// A named agent preset. Only the command differs today; presets that need
/// environment injection (see `plans/investigation/local-agent-fallback.md` §9.1)
/// are not built yet.
#[derive(Debug)]
pub struct Agent {
    pub name: &'static str,
    pub command: &'static str,
}

pub const PRESETS: &[Agent] = &[
    Agent {
        name: "claude",
        command: "claude",
    },
    Agent {
        name: "codex",
        command: "codex",
    },
];

pub const DEFAULT: &str = "claude";

/// Tag for a session started with an explicit `-- <command>` rather than a preset.
pub const CUSTOM: &str = "custom";

pub fn default_name() -> String {
    DEFAULT.to_string()
}

pub fn default_preset() -> &'static Agent {
    &PRESETS[0]
}

pub fn all_names() -> Vec<&'static str> {
    PRESETS.iter().map(|a| a.name).collect()
}

/// Presets whose command is actually installed. The dashboard only offers these, so a
/// machine with one agent never asks and a machine with two always does.
pub fn available() -> Vec<&'static Agent> {
    PRESETS.iter().filter(|a| on_path(a.command)).collect()
}

pub fn on_path(command: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        std::fs::metadata(dir.join(command))
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

pub fn resolve(name: &str) -> Result<&'static Agent> {
    match PRESETS.iter().find(|a| a.name == name) {
        Some(a) => Ok(a),
        None => {
            bail!("unknown agent '{name}' (known: {})", all_names().join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_resolves_and_leads_the_picker() {
        assert_eq!(resolve(DEFAULT).unwrap().command, "claude");
        assert_eq!(default_preset().name, DEFAULT);
    }

    #[test]
    fn codex_is_a_preset() {
        assert_eq!(resolve("codex").unwrap().command, "codex");
    }

    #[test]
    fn on_path_finds_a_ubiquitous_binary_and_not_a_made_up_one() {
        assert!(on_path("sh"));
        assert!(!on_path("muxai-no-such-agent"));
    }

    #[test]
    fn available_is_a_subset_of_the_presets() {
        assert!(available().iter().all(|a| resolve(a.name).is_ok()));
        assert!(available().len() <= PRESETS.len());
    }

    #[test]
    fn unknown_agent_lists_the_known_ones() {
        let err = resolve("gpt").unwrap_err().to_string();
        assert!(err.contains("claude") && err.contains("codex"), "{err}");
    }
}
