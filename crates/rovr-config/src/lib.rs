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

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
