use std::{collections::HashSet, fs, path::Path};

use regex::Regex;
use rovr_types::LayoutKind;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub focus: FocusConfig,
    #[serde(default)]
    pub animations: AnimationConfig,
    #[serde(default, rename = "workspace")]
    pub workspaces: Vec<WorkspaceConfig>,
    #[serde(default, rename = "rule")]
    pub rules: Vec<RuleConfig>,
    #[serde(default, rename = "scratchpad")]
    pub scratchpads: Vec<ScratchpadConfig>,
    #[serde(default, rename = "bind")]
    pub binds: Vec<KeybindConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    pub layout: LayoutKind,
    pub gap: i32,
    pub padding: i32,
    pub reconcile_on_wake: bool,
    pub reconcile_interval_ms: u64,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            layout: LayoutKind::Bsp,
            gap: 8,
            padding: 8,
            reconcile_on_wake: true,
            reconcile_interval_ms: 1000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FocusConfig {
    pub follows_mouse: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimationConfig {
    pub enabled: bool,
    pub duration_ms: u64,
    pub curve: String,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            duration_ms: 160,
            curve: "ease_out_quint".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub name: String,
    #[serde(default = "default_layout")]
    pub layout: LayoutKind,
    pub display: Option<String>,
    #[serde(default)]
    pub persistent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuleConfig {
    pub app: Option<String>,
    pub title: Option<String>,
    pub workspace: Option<String>,
    #[serde(rename = "float")]
    pub floating: Option<bool>,
}
/// A scratchpad is a named, toggleable set of windows. Members are excluded
/// from tiling while the scratchpad is "open". Matched by `app` (exact bundle
/// id) and/or `title` (substring); `None` = wildcard.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ScratchpadConfig {
    pub name: String,
    pub app: Option<String>,
    pub title: Option<String>,
}
/// A keybind maps a macOS hotkey string to a rovr CLI command. The daemon does
/// not yet register global hotkeys itself; this table is the single source of
/// truth for skhd and for a future built-in listener. `key` uses skhd syntax
/// like "cmd - h" or "alt + shift - r". `command` is the rovr CLI invocation
/// without the leading "rovr", e.g. "window --focus 1" or "layout --rotate 1".
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KeybindConfig {
    pub key: String,
    pub command: String,
}

fn default_layout() -> LayoutKind {
    LayoutKind::Bsp
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("gap must be non-negative")]
    NegativeGap,
    #[error("padding must be non-negative")]
    NegativePadding,
    #[error("workspace name cannot be empty")]
    EmptyWorkspaceName,
    #[error("duplicate workspace name: {0}")]
    DuplicateWorkspace(String),
    #[error("rule references unknown workspace: {0}")]
    UnknownWorkspace(String),
    #[error("invalid regex in {field}: {source}")]
    InvalidRegex {
        field: &'static str,
        #[source]
        source: regex::Error,
    },
    #[error("keybind key cannot be empty")]
    EmptyBindKey,
    #[error("keybind command cannot be empty")]
    EmptyBindCommand,
    #[error("duplicate keybind key: {0}")]
    DuplicateBind(String),
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let content = fs::read_to_string(path)?;
        Self::parse(&content)
    }

    pub fn parse(content: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(content)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.general.gap < 0 {
            return Err(ConfigError::NegativeGap);
        }
        if self.general.padding < 0 {
            return Err(ConfigError::NegativePadding);
        }

        let mut names = HashSet::new();
        for workspace in &self.workspaces {
            if workspace.name.trim().is_empty() {
                return Err(ConfigError::EmptyWorkspaceName);
            }
            if !names.insert(workspace.name.as_str()) {
                return Err(ConfigError::DuplicateWorkspace(workspace.name.clone()));
            }
        }

        for rule in &self.rules {
            if let Some(app) = &rule.app {
                Regex::new(app).map_err(|source| ConfigError::InvalidRegex {
                    field: "app",
                    source,
                })?;
            }
            if let Some(title) = &rule.title {
                Regex::new(title).map_err(|source| ConfigError::InvalidRegex {
                    field: "title",
                    source,
                })?;
            }
            if let Some(workspace) = &rule.workspace {
                if !names.contains(workspace.as_str()) {
                    return Err(ConfigError::UnknownWorkspace(workspace.clone()));
                }
            }
        }

        let mut bind_keys = HashSet::new();
        for bind in &self.binds {
            if bind.key.trim().is_empty() {
                return Err(ConfigError::EmptyBindKey);
            }
            if bind.command.trim().is_empty() {
                return Err(ConfigError::EmptyBindCommand);
            }
            if !bind_keys.insert(bind.key.as_str()) {
                return Err(ConfigError::DuplicateBind(bind.key.clone()));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_binds() {
        let cfg = Config::parse(
            r#"
            [[bind]]
            key = "alt - h"
            command = "window --focus-direction --from 1 --direction west"
            [[bind]]
            key = "alt - l"
            command = "window --focus-direction --from 1 --direction east"
            "#,
        )
        .expect("valid binds");
        assert_eq!(cfg.binds.len(), 2);
    }

    #[test]
    fn rejects_empty_bind_key() {
        let err = Config::parse(
            r#"
            [[bind]]
            key = ""
            command = "query --windows"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::EmptyBindKey));
    }

    #[test]
    fn rejects_duplicate_bind_keys() {
        let err = Config::parse(
            r#"
            [[bind]]
            key = "alt - h"
            command = "query --windows"
            [[bind]]
            key = "alt - h"
            command = "query --spaces"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateBind(k) if k == "alt - h"));
    }

    #[test]
    fn rejects_duplicate_workspaces() {
        let err = Config::parse(
            r#"
            [[workspace]]
            name = "code"
            [[workspace]]
            name = "code"
            "#,
        )
        .unwrap_err();

        assert!(matches!(err, ConfigError::DuplicateWorkspace(name) if name == "code"));
    }

    #[test]
    fn rejects_rules_for_missing_workspace() {
        let err = Config::parse(
            r#"
            [[rule]]
            app = "Slack"
            workspace = "chat"
            "#,
        )
        .unwrap_err();

        assert!(matches!(err, ConfigError::UnknownWorkspace(name) if name == "chat"));
    }
}
