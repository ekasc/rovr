use std::{collections::HashSet, fs, path::Path};

use regex::Regex;
use rovr_types::LayoutKind;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CURRENT_CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_config_version", rename = "config-version")]
    pub config_version: u32,
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

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CURRENT_CONFIG_VERSION,
            general: GeneralConfig::default(),
            focus: FocusConfig::default(),
            animations: AnimationConfig::default(),
            workspaces: Vec::new(),
            rules: Vec::new(),
            scratchpads: Vec::new(),
            binds: Vec::new(),
        }
    }
}

fn default_config_version() -> u32 {
    CURRENT_CONFIG_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    pub layout: LayoutKind,
    pub gap: i32,
    pub padding: i32,
    pub reconcile_on_wake: bool,
    pub reconcile_interval_ms: u64,
    /// Optional WASM layout plugin name (e.g. "my_plugin"). If Some and plugin loads,
    /// it is used instead of built-in `layout` for tiling. Falls back to built-in on error.
    #[serde(default)]
    pub plugin: Option<String>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            layout: LayoutKind::Bsp,
            gap: 0,
            padding: 0,
            reconcile_on_wake: true,
            reconcile_interval_ms: 1000,
            plugin: None,
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
    /// Optional per-workspace WASM plugin override
    #[serde(default)]
    pub plugin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuleConfig {
    pub app: Option<String>,
    pub title: Option<String>,
    /// Match condition: window's current workspace name (via backing mapping)
    /// Not used as action; see `target_workspace` for action.
    pub workspace: Option<String>,
    #[serde(rename = "float")]
    pub floating: Option<bool>,
    /// Action: move matching window to named workspace (logical)
    #[serde(default, rename = "target_workspace")]
    pub target_workspace: Option<String>,
    /// Action: set opacity (0.0-1.0)
    pub opacity: Option<f64>,
    /// Action: set window layer
    pub layer: Option<i32>,
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
/// A keybind maps a macOS hotkey string to a typed Rovr command. This table is
/// the single source of truth for the built-in listener and `gen-skhd` migration
/// output. `key` uses skhd syntax
/// like "cmd - h" or "alt + shift - r". `command` is the rovr CLI invocation
/// without the leading "rovr", e.g. "window focus 1" or
/// "layout rotate --space 1".
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KeybindConfig {
    pub key: String,
    pub command: String,
}

/// One typed, compiled window-selection condition. Runtime rule evaluation is
/// exhaustive over this enum rather than coupling behavior to parallel
/// optional fields.
#[derive(Debug, Clone)]
pub enum Selector {
    App(Regex),
    Title(Regex),
    Workspace(String),
}

#[derive(Debug, Clone, Default)]
pub struct Predicate {
    /// Every selector must match. An empty predicate matches every window.
    pub all_of: Vec<Selector>,
}

/// A rule with its predicate compiled once at config load/reload time. The Vec
/// returned by [`Config::compile_rules`] preserves config order so evaluation
/// is deterministic.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub predicate: Predicate,
    #[allow(dead_code)]
    pub floating: Option<bool>,
    pub target_workspace: Option<String>,
    pub opacity: Option<f64>,
    pub layer: Option<i32>,
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
    #[error("invalid bind key {key:?}: {reason}")]
    InvalidBindKey { key: String, reason: String },
    #[error("invalid bind command for key {key:?}: {reason}")]
    InvalidBindCommand { key: String, reason: String },
    #[error("opacity must be between 0.0 and 1.0")]
    InvalidOpacity,
    #[error(
        "unsupported config-version {found}; this Rovr build supports config-version {supported}"
    )]
    UnsupportedConfigVersion { found: u32, supported: u32 },
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
        if self.config_version != CURRENT_CONFIG_VERSION {
            return Err(ConfigError::UnsupportedConfigVersion {
                found: self.config_version,
                supported: CURRENT_CONFIG_VERSION,
            });
        }
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
            if let Some(target) = &rule.target_workspace {
                if !names.contains(target.as_str()) {
                    return Err(ConfigError::UnknownWorkspace(target.clone()));
                }
            }
            if let Some(opacity) = rule.opacity {
                if !(0.0..=1.0).contains(&opacity) {
                    return Err(ConfigError::InvalidOpacity);
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
            let chord = rovr_protocol::hotkey::parse_hotkey(&bind.key).map_err(|parse_err| {
                ConfigError::InvalidBindKey {
                    key: bind.key.clone(),
                    reason: parse_err.to_string(),
                }
            })?;
            // Blocker 8: an invalid bind command fails config load/reload —
            // it can never silently become a different command at runtime.
            // Blocker 7: validation uses the ONE shared parser (same grammar
            // as the CLI and hotkey dispatch), so syntax cannot drift.
            if let Err(parse_err) = rovr_protocol::command_parser::parse_command(&bind.command) {
                return Err(ConfigError::InvalidBindCommand {
                    key: bind.key.clone(),
                    reason: parse_err.message,
                });
            }
            if !bind_keys.insert(chord) {
                return Err(ConfigError::DuplicateBind(bind.key.clone()));
            }
        }

        Ok(())
    }

    /// Compile all rule matchers into a deterministic, ready-to-evaluate
    /// representation. Call once per config load/reload — never per reconcile
    /// cycle. Regexes were already validated by [`Self::validate`], but a
    /// direct call on an unvalidated config surfaces the compile error.
    pub fn compile_rules(&self) -> Result<Vec<CompiledRule>, ConfigError> {
        self.rules
            .iter()
            .map(|rule| {
                let mut all_of = Vec::new();
                if let Some(pattern) = &rule.app {
                    all_of.push(Selector::App(Regex::new(pattern).map_err(|source| {
                        ConfigError::InvalidRegex {
                            field: "app",
                            source,
                        }
                    })?));
                }
                if let Some(pattern) = &rule.title {
                    all_of.push(Selector::Title(Regex::new(pattern).map_err(|source| {
                        ConfigError::InvalidRegex {
                            field: "title",
                            source,
                        }
                    })?));
                }
                if let Some(workspace) = &rule.workspace {
                    all_of.push(Selector::Workspace(workspace.clone()));
                }
                Ok(CompiledRule {
                    predicate: Predicate { all_of },
                    floating: rule.floating,
                    target_workspace: rule.target_workspace.clone(),
                    opacity: rule.opacity,
                    layer: rule.layer,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_config_version_means_version_one() {
        let config = Config::parse("").expect("empty config uses supported defaults");
        assert_eq!(config.config_version, CURRENT_CONFIG_VERSION);
        assert_eq!(Config::default().config_version, CURRENT_CONFIG_VERSION);
    }

    #[test]
    fn explicit_supported_config_version_is_accepted() {
        let config = Config::parse("config-version = 1").expect("version one is supported");
        assert_eq!(config.config_version, 1);
    }

    #[test]
    fn unsupported_config_version_is_rejected() {
        let error = Config::parse("config-version = 2").unwrap_err();
        assert!(matches!(
            error,
            ConfigError::UnsupportedConfigVersion {
                found: 2,
                supported: 1
            }
        ));
    }

    #[test]
    fn accepts_valid_binds() {
        let cfg = Config::parse(
            r#"
            [[bind]]
            key = "alt - h"
            command = "window focus-direction west 1"
            [[bind]]
            key = "alt - l"
            command = "window focus-direction east 1"
            [[bind]]
            key = "alt - tab"
            command = "query windows"
            [[bind]]
            key = "alt + shift - tab"
            command = "query spaces"
            "#,
        )
        .expect("valid binds");
        assert_eq!(cfg.binds.len(), 4);
    }

    #[test]
    fn rejects_unknown_bind_modifier_at_load() {
        let err = Config::parse(
            r#"
            [[bind]]
            key = "hyper - h"
            command = "query --windows"
            "#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidBindKey { ref key, ref reason }
                if key == "hyper - h" && reason.contains("unknown modifier")
        ));
    }

    #[test]
    fn rejects_unknown_bind_key_at_load() {
        let err = Config::parse(
            r#"
            [[bind]]
            key = "alt - banana"
            command = "query --windows"
            "#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidBindKey { ref key, ref reason }
                if key == "alt - banana" && reason.contains("unknown key")
        ));
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
            command = "query windows"
            [[bind]]
            key = "alt - h"
            command = "query spaces"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateBind(k) if k == "alt - h"));
    }

    #[test]
    fn rejects_modifier_alias_duplicate_bind_keys() {
        let err = Config::parse(
            r#"
            [[bind]]
            key = "alt - h"
            command = "query windows"
            [[bind]]
            key = "option - h"
            command = "query spaces"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateBind(k) if k == "option - h"));
    }

    #[test]
    fn rejects_case_duplicate_bind_keys() {
        let err = Config::parse(
            r#"
            [[bind]]
            key = "alt - h"
            command = "query windows"
            [[bind]]
            key = "ALT - H"
            command = "query spaces"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateBind(k) if k == "ALT - H"));
    }

    #[test]
    fn rejects_modifier_order_duplicate_bind_keys() {
        let err = Config::parse(
            r#"
            [[bind]]
            key = "alt + shift - h"
            command = "query windows"
            [[bind]]
            key = "shift + option - h"
            command = "query spaces"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateBind(k) if k == "shift + option - h"));
    }

    #[test]
    fn accepts_distinct_normalized_bind_keys() {
        Config::parse(
            r#"
            [[bind]]
            key = "alt - h"
            command = "query windows"
            [[bind]]
            key = "shift - h"
            command = "query spaces"
            "#,
        )
        .expect("distinct binds are valid");
    }

    #[test]
    fn blocker8_invalid_bind_command_fails_config_load() {
        // Flag-style syntax diverging from the real CLI must be rejected at
        // load time — never silently accepted and never substituted.
        let err = Config::parse(
            r#"
            [[bind]]
            key = "alt - h"
            command = "window --focus 1"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::InvalidBindCommand { ref key, .. } if key == "alt - h"));

        let err = Config::parse(
            r#"
            [[bind]]
            key = "alt - x"
            command = "definitely not a command"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::InvalidBindCommand { .. }));
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

    // ---- Blocker 10: regex validation and compiled-rule behavior ----

    #[test]
    fn blocker10_invalid_regex_rejected_at_load() {
        let err = Config::parse(
            r#"
            [[rule]]
            app = "^Finder$"
            title = "([unclosed"
            float = true
            "#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ConfigError::InvalidRegex { field: "title", .. }
        ));
    }

    #[test]
    fn blocker10_compile_rules_preserves_order_and_compiles_matchers() {
        let cfg = Config::parse(
            r#"
            [[rule]]
            app = "^Finder$"
            float = true
            [[rule]]
            title = "Preferences|Settings"
            target_workspace = "main"
            [[workspace]]
            name = "main"
            "#,
        )
        .unwrap();
        let rules = cfg.compile_rules().unwrap();
        assert_eq!(rules.len(), 2);
        // Deterministic order: first config rule first.
        let Selector::App(app) = &rules[0].predicate.all_of[0] else {
            panic!("first selector must be app");
        };
        assert!(app.is_match("Finder"));
        assert!(!app.is_match("Finder Helper"));
        let Selector::Title(title) = &rules[1].predicate.all_of[0] else {
            panic!("second rule selector must be title");
        };
        assert!(title.is_match("Settings"));
        assert_eq!(rules[1].target_workspace.as_deref(), Some("main"));
    }
}
