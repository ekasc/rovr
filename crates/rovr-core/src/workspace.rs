use std::collections::HashMap;

use rovr_types::{DisplayId, SpaceId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceState {
    /// Logical name, matches config [[workspace]] name
    pub name: String,
    /// Desired display string from config (e.g. "main"), if any
    pub desired_display: Option<String>,
    /// Layout kind name for serialization (we store string to avoid LayoutKind version skew)
    /// But we will store directly via config lookup, not here.
    /// Whether workspace is persistent (should be recreated if missing)
    pub persistent: bool,
    /// Volatile backing macOS SpaceId, if currently mapped
    pub backing_space: Option<SpaceId>,
}

impl WorkspaceState {
    pub fn new(name: String, desired_display: Option<String>, persistent: bool) -> Self {
        Self {
            name,
            desired_display,
            persistent,
            backing_space: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceRegistry(pub HashMap<String, WorkspaceState>);

impl WorkspaceRegistry {
    pub fn from_config(workspaces: &[rovr_config::WorkspaceConfig]) -> Self {
        let mut m = HashMap::new();
        for w in workspaces {
            m.insert(
                w.name.clone(),
                WorkspaceState::new(w.name.clone(), w.display.clone(), w.persistent),
            );
        }
        Self(m)
    }

    pub fn ensure_from_config(&mut self, workspaces: &[rovr_config::WorkspaceConfig]) {
        // Add new, remove deleted, preserve backing_space for existing
        let mut existing = std::mem::take(&mut self.0);
        let mut new_map = HashMap::new();
        for cfg in workspaces {
            if let Some(mut state) = existing.remove(&cfg.name) {
                state.desired_display = cfg.display.clone();
                state.persistent = cfg.persistent;
                new_map.insert(cfg.name.clone(), state);
            } else {
                new_map.insert(
                    cfg.name.clone(),
                    WorkspaceState::new(cfg.name.clone(), cfg.display.clone(), cfg.persistent),
                );
            }
        }
        self.0 = new_map;
    }

    pub fn backing_for(&self, name: &str) -> Option<SpaceId> {
        self.0.get(name).and_then(|w| w.backing_space)
    }

    pub fn name_for_space(&self, space: SpaceId) -> Option<&str> {
        self.0
            .iter()
            .find(|(_, w)| w.backing_space == Some(space))
            .map(|(k, _)| k.as_str())
    }

    pub fn remap_after_snapshot(
        &mut self,
        observed_spaces: &HashMap<SpaceId, rovr_types::SpaceSnapshot>,
        observed_displays: &HashMap<DisplayId, rovr_types::DisplaySnapshot>,
    ) {
        // If backing_space no longer exists, clear it (volatile)
        for state in self.0.values_mut() {
            if let Some(sid) = state.backing_space {
                if !observed_spaces.contains_key(&sid) {
                    state.backing_space = None;
                }
            }
        }
        // For workspaces without backing, try to assign an existing unclaimed space
        // that matches desired_display if possible, otherwise any unclaimed.
        // Unclaimed = space not already backing a workspace
        let claimed: std::collections::HashSet<SpaceId> =
            self.0.values().filter_map(|w| w.backing_space).collect();
        let mut unclaimed: Vec<SpaceId> = observed_spaces
            .keys()
            .filter(|sid| !claimed.contains(sid))
            .copied()
            .collect();
        unclaimed.sort_by_key(|sid| observed_spaces.get(sid).map(|s| s.position).unwrap_or(0));

        for state in self.0.values_mut() {
            if state.backing_space.is_some() {
                continue;
            }
            // Prefer space on desired display
            if let Some(desired) = &state.desired_display {
                // display string "main" means focused display? Simplify: try display id 1 as main, else first
                // We match by DisplayId? But config display is string like "main" or numeric.
                // For now, try to find display matching label or main.
                let mut candidate: Option<SpaceId> = None;
                for sid in &unclaimed {
                    if let Some(space) = observed_spaces.get(sid) {
                        let disp = space.display_id;
                        // Check if this display matches desired
                        // Very simplistic: if desired is "main", pick focused display
                        let is_main = observed_displays
                            .get(&disp)
                            .map(|d| d.focused)
                            .unwrap_or(false);
                        if desired == "main" && is_main {
                            candidate = Some(*sid);
                            break;
                        }
                        if desired.parse::<u32>().ok() == Some(disp.0) {
                            candidate = Some(*sid);
                            break;
                        }
                    }
                }
                if let Some(sid) = candidate {
                    state.backing_space = Some(sid);
                    unclaimed.retain(|x| *x != sid);
                    continue;
                }
            }
            // Fallback: first unclaimed
            if let Some(sid) = unclaimed.first().copied() {
                state.backing_space = Some(sid);
                unclaimed.retain(|x| *x != sid);
            }
        }
    }

    pub fn ensure_persistent(&self) -> Vec<String> {
        // Return names of persistent workspaces that lack backing (need creation)
        self.0
            .iter()
            .filter(|(_, w)| w.persistent && w.backing_space.is_none())
            .map(|(k, _)| k.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rovr_types::{DisplayId, SpaceId, SpaceSnapshot};
    use std::collections::HashMap;

    fn space(id: u64, display: u32, pos: u32) -> SpaceSnapshot {
        SpaceSnapshot {
            id: SpaceId(id),
            display_id: DisplayId(display),
            label: None,
            focused: false,
            generation: 0,
            position: pos,
        }
    }

    #[test]
    fn remap_survives_restart_with_new_ids() {
        let mut reg = WorkspaceRegistry::default();
        reg.0.insert(
            "code".into(),
            WorkspaceState {
                name: "code".into(),
                persistent: true,
                backing_space: Some(SpaceId(11)),
                desired_display: None,
            },
        );
        reg.0.insert(
            "chat".into(),
            WorkspaceState {
                name: "chat".into(),
                persistent: true,
                backing_space: Some(SpaceId(12)),
                desired_display: None,
            },
        );
        // Simulate Dock restart: old 11/12 gone, new 101/102 appear
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(101), space(101, 1, 0));
        spaces.insert(SpaceId(102), space(102, 1, 1));
        let displays = HashMap::new();
        reg.remap_after_snapshot(&spaces, &displays);
        // Both workspaces should be remapped to new ids, not cleared
        assert!(reg.backing_for("code").is_some());
        assert!(reg.backing_for("chat").is_some());
        assert_ne!(reg.backing_for("code"), Some(SpaceId(11)));
    }

    #[test]
    fn persistent_missing_detection() {
        let mut reg = WorkspaceRegistry::default();
        reg.0.insert(
            "code".into(),
            WorkspaceState {
                name: "code".into(),
                persistent: true,
                backing_space: None,
                desired_display: None,
            },
        );
        let missing = reg.ensure_persistent();
        assert_eq!(missing, vec!["code"]);
    }
}
