use std::collections::HashMap;

use rovr_types::{DisplayId, SpaceId, WindowId};
use serde::{Deserialize, Serialize};

/// A logical workspace's backing Space moved from `from` (possibly gone) to
/// `to`. The engine uses this to carry SpaceId-keyed layout state (BSP tree,
/// orientation) to the new backing Space so topology survives volatility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemapMove {
    pub name: String,
    pub from: Option<SpaceId>,
    pub to: SpaceId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceState {
    /// Logical name, matches config [[workspace]] name
    pub name: String,
    /// Desired display string from config ("main" or numeric display id)
    pub desired_display: Option<String>,
    /// Whether workspace is persistent (should be recreated if missing)
    pub persistent: bool,
    /// Session-only observed backing. Native IDs are never persisted as identity.
    #[serde(skip)]
    pub backing: Option<WorkspaceBacking>,
    /// Stable config order — the deterministic tiebreaker for remapping
    /// (blocker 3). Persisted so identity survives daemon restarts.
    #[serde(default)]
    pub ordinal: usize,
    /// Diagnostic observation only; never used to reclaim identity.
    #[serde(skip)]
    pub last_position: Option<u32>,
    /// Runtime-created workspace; retained across configuration reload.
    #[serde(default)]
    pub dynamic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceBacking {
    Normal {
        space: SpaceId,
    },
    FullscreenReplacement {
        fullscreen_space: SpaceId,
        restore_space: SpaceId,
        window: WindowId,
    },
}

impl WorkspaceBacking {
    pub fn active_space(self) -> SpaceId {
        match self {
            Self::Normal { space } => space,
            Self::FullscreenReplacement {
                fullscreen_space, ..
            } => fullscreen_space,
        }
    }
    pub fn restore_space(self) -> Option<SpaceId> {
        match self {
            Self::Normal { .. } => None,
            Self::FullscreenReplacement { restore_space, .. } => Some(restore_space),
        }
    }
}

impl WorkspaceState {
    pub fn active_space(&self) -> Option<SpaceId> {
        self.backing.map(WorkspaceBacking::active_space)
    }

    pub fn new(name: String, desired_display: Option<String>, persistent: bool) -> Self {
        Self {
            name,
            desired_display,
            persistent,
            backing: None,
            ordinal: usize::MAX,
            last_position: None,
            dynamic: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceRegistry(
    pub HashMap<String, WorkspaceState>,
    #[serde(skip, default)] pub bool,
);

impl WorkspaceRegistry {
    pub fn from_config(workspaces: &[rovr_config::WorkspaceConfig]) -> Self {
        let mut m = HashMap::new();
        for (ordinal, w) in workspaces.iter().enumerate() {
            let mut state = WorkspaceState::new(w.name.clone(), w.display.clone(), w.persistent);
            state.ordinal = ordinal;
            m.insert(w.name.clone(), state);
        }
        Self(m, false)
    }

    pub fn ensure_from_config(&mut self, workspaces: &[rovr_config::WorkspaceConfig]) {
        // Add new, remove deleted configured entries, and preserve backing
        // SpaceIds for existing logical names. Runtime-created dynamic
        // workspaces survive reload so reloading while on workspace 2 cannot
        // forget it and create a duplicate on the next alt-2.
        let mut existing = std::mem::take(&mut self.0);
        let mut new_map = HashMap::new();
        for (ordinal, cfg) in workspaces.iter().enumerate() {
            if let Some(mut state) = existing.remove(&cfg.name) {
                state.desired_display = cfg.display.clone();
                state.persistent = cfg.persistent;
                state.ordinal = ordinal;
                state.dynamic = false;
                new_map.insert(cfg.name.clone(), state);
            } else {
                let mut state =
                    WorkspaceState::new(cfg.name.clone(), cfg.display.clone(), cfg.persistent);
                state.ordinal = ordinal;
                new_map.insert(cfg.name.clone(), state);
            }
        }
        for (name, state) in existing {
            if state.dynamic {
                new_map.insert(name, state);
            }
        }
        self.0 = new_map;
        self.1 = false;
    }

    pub fn backing_for(&self, name: &str) -> Option<SpaceId> {
        self.0.get(name).and_then(|w| w.active_space())
    }

    pub fn name_for_space(&self, space: SpaceId) -> Option<&str> {
        self.0
            .iter()
            .find(|(_, w)| w.active_space() == Some(space))
            .map(|(k, _)| k.as_str())
    }

    /// Names of workspaces missing a backing space, in deterministic ordinal
    /// order (lowest config order first).
    pub fn missing_backing_in_order(&self) -> Vec<String> {
        let mut names: Vec<(usize, String)> = self
            .0
            .iter()
            .filter(|(_, w)| w.active_space().is_none())
            .map(|(k, w)| (w.ordinal, k.clone()))
            .collect();
        names.sort();
        names.into_iter().map(|(_, n)| n).collect()
    }

    /// Names of PERSISTENT workspaces without a backing Space, in ordinal
    /// order — these need a CreateSpace lifecycle operation (blocker 4).
    pub fn ensure_persistent(&self) -> Vec<String> {
        self.missing_backing_in_order()
            .into_iter()
            .filter(|name| self.0.get(name).is_some_and(|w| w.persistent))
            .collect()
    }

    /// Numeric names sort numerically; named workspaces retain configured order.
    pub fn ordered_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.0.keys().cloned().collect();
        names.sort_by_key(|name| match name.parse::<u32>() {
            Ok(number) => (0, number as usize, name.clone()),
            Err(_) => (1, self.0[name].ordinal, name.clone()),
        });
        names
    }

    /// Recompute observed handles from native slots, never from remembered IDs.
    /// Creation and restoration temporarily reserve their bindings in the engine.
    pub fn remap_after_snapshot(
        &mut self,
        spaces: &HashMap<SpaceId, rovr_types::SpaceSnapshot>,
        displays: &HashMap<DisplayId, rovr_types::DisplaySnapshot>,
    ) -> Vec<RemapMove> {
        let parked: std::collections::HashSet<_> = self
            .0
            .values()
            .filter_map(|w| w.backing.and_then(WorkspaceBacking::restore_space))
            .collect();
        let mut slots: Vec<_> = spaces
            .values()
            .filter(|s| !s.is_fullscreen && !s.is_system && !parked.contains(&s.id))
            .collect();
        slots.sort_by_key(|s| (s.display_id, s.position, s.id));
        let main = displays.values().find(|d| d.is_main).map(|d| d.id);
        let mut moves = Vec::new();
        for name in self.ordered_names() {
            let ws = self.0.get_mut(&name).unwrap();
            if matches!(
                ws.backing,
                Some(WorkspaceBacking::FullscreenReplacement { .. })
            ) {
                continue;
            }
            let old = ws.active_space();
            let preferred = match ws.desired_display.as_deref() {
                Some("main") => main,
                Some(id) => id.parse::<u32>().ok().map(DisplayId),
                None => None,
            };
            let index = slots
                .iter()
                .position(|s| preferred.map_or(true, |d| s.display_id == d));
            let new = index.map(|i| slots.remove(i));
            ws.backing = new.map(|s| WorkspaceBacking::Normal { space: s.id });
            ws.last_position = new.map(|s| s.position);
            if old != ws.active_space() {
                if let Some(to) = ws.active_space().or(old) {
                    moves.push(RemapMove {
                        name,
                        from: old,
                        to,
                    });
                }
            }
        }
        moves
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
            is_fullscreen: false,
            is_system: false,
        }
    }

    fn registry_with(entries: &[(&str, bool, Option<u64>, usize)]) -> WorkspaceRegistry {
        let mut reg = WorkspaceRegistry::default();
        for (name, persistent, backing, ordinal) in entries {
            reg.0.insert(
                (*name).into(),
                WorkspaceState {
                    name: (*name).into(),
                    persistent: *persistent,
                    backing: backing.map(|id| WorkspaceBacking::Normal { space: SpaceId(id) }),
                    desired_display: None,
                    ordinal: *ordinal,
                    last_position: None,
                    dynamic: false,
                },
            );
        }
        reg
    }

    /// Blocker 3: the acceptance scenario repeated many times must ALWAYS map
    /// code->101 and chat->102 (never swapped), regardless of hash order.
    #[test]
    fn blocker3_remap_is_deterministic_across_repetitions() {
        for iteration in 0..200 {
            let mut reg =
                registry_with(&[("code", true, Some(11), 0), ("chat", true, Some(12), 1)]);
            // Record previous positions like a live session would.
            reg.0.get_mut("code").unwrap().last_position = Some(0);
            reg.0.get_mut("chat").unwrap().last_position = Some(1);

            // Dock restart: old 11/12 gone, new 101/102 appear at positions 0/1.
            let mut spaces = HashMap::new();
            spaces.insert(SpaceId(101), space(101, 1, 0));
            spaces.insert(SpaceId(102), space(102, 1, 1));
            let displays = HashMap::new();

            let moves = reg.remap_after_snapshot(&spaces, &displays);

            assert_eq!(
                reg.backing_for("code"),
                Some(SpaceId(101)),
                "iteration {iteration}: code must deterministically claim position-0 space"
            );
            assert_eq!(
                reg.backing_for("chat"),
                Some(SpaceId(102)),
                "iteration {iteration}: chat must deterministically claim position-1 space"
            );
            assert_eq!(moves.len(), 2);
        }
    }

    /// Blocker 3: even WITHOUT last_position history, ordinal order decides —
    /// the lowest-ordinal workspace gets the lowest-position unclaimed space.
    #[test]
    fn blocker3_ordinal_order_decides_without_history() {
        let mut reg = registry_with(&[("chat", false, None, 1), ("code", false, None, 0)]);
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(50), space(50, 1, 1));
        spaces.insert(SpaceId(40), space(40, 1, 0));
        let displays = HashMap::new();
        reg.remap_after_snapshot(&spaces, &displays);
        assert_eq!(
            reg.backing_for("code"),
            Some(SpaceId(40)),
            "ordinal 0 claims position 0"
        );
        assert_eq!(reg.backing_for("chat"), Some(SpaceId(50)));
    }

    /// Blocker 3: "secondary" is not defined semantics — it must be rejected
    /// during config validation (see rovr-config tests). Here we verify the
    /// runtime treats only "main" and numeric ids as meaningful.
    #[test]
    fn blocker3_numeric_desired_display_is_honored() {
        let mut reg = registry_with(&[("mail", false, None, 0)]);
        reg.0.get_mut("mail").unwrap().desired_display = Some("2".into());
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(10), space(10, 1, 0));
        spaces.insert(SpaceId(20), space(20, 2, 1));
        let mut displays = HashMap::new();
        displays.insert(
            DisplayId(1),
            rovr_types::DisplaySnapshot {
                id: DisplayId(1),
                frame: rovr_types::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                },
                label: None,
                focused: true,
                is_main: false,
                generation: 0,
            },
        );
        displays.insert(
            DisplayId(2),
            rovr_types::DisplaySnapshot {
                id: DisplayId(2),
                frame: rovr_types::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                },
                label: None,
                focused: false,
                is_main: false,
                generation: 0,
            },
        );
        reg.remap_after_snapshot(&spaces, &displays);
        assert_eq!(
            reg.backing_for("mail"),
            Some(SpaceId(20)),
            "numeric display preference must select that display's space"
        );
    }

    #[test]
    fn remap_survives_restart_with_new_ids() {
        let mut reg = registry_with(&[("code", true, Some(11), 0), ("chat", true, Some(12), 1)]);
        reg.0.get_mut("code").unwrap().last_position = Some(0);
        reg.0.get_mut("chat").unwrap().last_position = Some(1);
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(101), space(101, 1, 0));
        spaces.insert(SpaceId(102), space(102, 1, 1));
        let displays = HashMap::new();
        reg.remap_after_snapshot(&spaces, &displays);
        assert!(reg.backing_for("code").is_some());
        assert!(reg.backing_for("chat").is_some());
        assert_ne!(reg.backing_for("code"), Some(SpaceId(11)));
    }

    /// Deleting one Space must never renumber surviving logical workspaces.
    /// IDs that are still observed are authoritative; only the deleted
    /// workspace loses its backing. This is the i3 invariant behind alt-N:
    /// workspace identity is its logical name, not Mission Control position.
    #[test]
    fn observed_slots_determine_contents_without_identity_healing() {
        let mut reg = registry_with(&[
            ("1", true, Some(11), 0),
            ("2", false, Some(12), 1),
            ("3", false, Some(13), 2),
        ]);
        reg.0.get_mut("1").unwrap().last_position = Some(0);
        reg.0.get_mut("2").unwrap().last_position = Some(1);
        reg.0.get_mut("3").unwrap().last_position = Some(2);

        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(11), space(11, 1, 0));
        // Space 12 was deleted; macOS compacts 13 from position 2 to 1.
        spaces.insert(SpaceId(13), space(13, 1, 1));

        reg.remap_after_snapshot(&spaces, &HashMap::new());

        assert_eq!(reg.backing_for("1"), Some(SpaceId(11)));
        assert_eq!(reg.backing_for("2"), Some(SpaceId(13)));
        assert_eq!(
            reg.backing_for("3"),
            None,
            "only two ordinary native slots are available"
        );
    }

    #[test]
    fn persistent_missing_detection() {
        let reg = registry_with(&[("code", true, None, 0), ("temp", false, None, 1)]);
        let missing = reg.ensure_persistent();
        assert_eq!(
            missing,
            vec!["code"],
            "only persistent workspaces are reported"
        );
        assert_eq!(
            reg.missing_backing_in_order(),
            vec!["code".to_string(), "temp".to_string()],
            "missing list is ordinal-sorted"
        );
    }

    #[test]
    fn config_reload_preserves_runtime_dynamic_workspace() {
        let mut reg = registry_with(&[("1", true, Some(11), 0), ("2", false, Some(12), 1)]);
        reg.0.get_mut("2").unwrap().dynamic = true;
        reg.ensure_from_config(&[rovr_config::WorkspaceConfig {
            name: "1".into(),
            layout: rovr_types::LayoutKind::Bsp,
            display: None,
            persistent: true,
            plugin: None,
        }]);

        assert_eq!(reg.backing_for("1"), Some(SpaceId(11)));
        assert_eq!(reg.backing_for("2"), Some(SpaceId(12)));
        assert!(reg.0["2"].dynamic);
    }

    /// Stale dynamic bindings (persisted backing Space no longer observed)
    /// must be cleared by the next remap so alt-N re-spawns instead of
    /// erroring on a missing Space.
    #[test]
    fn dynamic_workspace_rebinds_by_slot_after_id_churn() {
        let mut reg = WorkspaceRegistry::default();
        reg.0.insert(
            "2".into(),
            WorkspaceState {
                name: "2".into(),
                desired_display: None,
                persistent: false,
                backing: Some(WorkspaceBacking::Normal {
                    space: SpaceId(1778),
                }),
                ordinal: 1,
                last_position: Some(3),
                dynamic: true,
            },
        );
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(11), space(11, 1, 0));
        let moves = reg.remap_after_snapshot(&spaces, &HashMap::new());
        assert_eq!(
            reg.backing_for("2"),
            Some(SpaceId(11)),
            "dynamic workspaces use the same positional mapping as configured ones"
        );
        assert_eq!(
            moves.len(),
            1,
            "stale-clear must be reported as a remap move so the engine drops the layout"
        );
        assert_eq!(moves[0].from, Some(SpaceId(1778)));
        assert_eq!(moves[0].to, SpaceId(11));
    }

    #[test]
    fn reordered_config_changes_logical_slot_order() {
        let workspace = |name: &str| rovr_config::WorkspaceConfig {
            name: name.into(),
            layout: rovr_types::LayoutKind::Bsp,
            display: None,
            persistent: false,
            plugin: None,
        };
        let mut reg = WorkspaceRegistry::from_config(&[workspace("code"), workspace("chat")]);
        reg.0.get_mut("code").unwrap().backing =
            Some(WorkspaceBacking::Normal { space: SpaceId(11) });
        reg.0.get_mut("chat").unwrap().backing =
            Some(WorkspaceBacking::Normal { space: SpaceId(12) });

        reg.ensure_from_config(&[workspace("chat"), workspace("code")]);
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(11), space(11, 1, 0));
        spaces.insert(SpaceId(12), space(12, 1, 1));
        let moves = reg.remap_after_snapshot(&spaces, &HashMap::new());

        assert_eq!(reg.backing_for("chat"), Some(SpaceId(11)));
        assert_eq!(reg.backing_for("code"), Some(SpaceId(12)));
        assert_eq!(moves.len(), 2);
    }

    /// Manual drags in Mission Control must make alt-N follow the *visual*
    /// position, not the stale SpaceId. If the user drags Space 12 from
    /// position 1 to 0 and Space 11 from 0 to 1, workspace "code"
    /// (ordinal 0, alt-1) must now be bound to the Space now at position 0
    /// (12), and "chat" (ordinal 1, alt-2) to the Space now at position 1
    /// (11). Layout state follows via RemapMove.
    #[test]
    fn reload_preserves_ids_after_manual_drag() {
        let mut reg = registry_with(&[("code", true, Some(11), 0), ("chat", true, Some(12), 1)]);
        // Before drag: code at pos0 (11), chat at pos1 (12). User drags chat
        // ahead of code: Space 12 now at pos0, Space 11 at pos1. last_position
        // still holds the old positions (0 and 1), so remap must detect the
        // mismatch and reassign ordinal→position.
        reg.0.get_mut("code").unwrap().last_position = Some(0);
        reg.0.get_mut("chat").unwrap().last_position = Some(1);
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(11), space(11, 1, 1));
        spaces.insert(SpaceId(12), space(12, 1, 0));
        let displays = HashMap::new();

        // Drag: ordinal→position reassignment, alt-1 now follows position 0.
        let moves = reg.remap_after_snapshot(&spaces, &displays);
        assert_eq!(
            (reg.backing_for("code"), reg.backing_for("chat")),
            (Some(SpaceId(12)), Some(SpaceId(11))),
            "alt-N must follow visual position after a manual drag"
        );
        assert_eq!(moves.len(), 2);

        let moves = reg.remap_after_snapshot(&spaces, &displays);
        assert_eq!(
            (reg.backing_for("code"), reg.backing_for("chat")),
            (Some(SpaceId(12)), Some(SpaceId(11))),
            "drag mapping must be stable across snapshots"
        );
        assert!(moves.is_empty());
    }
}
