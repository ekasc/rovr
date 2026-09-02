use std::collections::HashMap;

use rovr_types::{DisplayId, SpaceId};
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
    /// Volatile backing macOS SpaceId, if currently mapped
    pub backing_space: Option<SpaceId>,
    /// Stable config order — the deterministic tiebreaker for remapping
    /// (blocker 3). Persisted so identity survives daemon restarts.
    #[serde(default)]
    pub ordinal: usize,
    /// Last observed Mission-Control position of the backing space. Used to
    /// re-claim the same slot after a Dock restart when ids churn.
    #[serde(default)]
    pub last_position: Option<u32>,
    /// i3-style spawn-on-focus workspace: excluded from remap claiming (must
    /// never adopt a pre-existing Space) and destroyed when left empty.
    #[serde(default)]
    pub dynamic: bool,
}

impl WorkspaceState {
    pub fn new(name: String, desired_display: Option<String>, persistent: bool) -> Self {
        Self {
            name,
            desired_display,
            persistent,
            backing_space: None,
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
        self.0.get(name).and_then(|w| w.backing_space)
    }

    pub fn name_for_space(&self, space: SpaceId) -> Option<&str> {
        self.0
            .iter()
            .find(|(_, w)| w.backing_space == Some(space))
            .map(|(k, _)| k.as_str())
    }

    /// Names of workspaces missing a backing space, in deterministic ordinal
    /// order (lowest config order first).
    pub fn missing_backing_in_order(&self) -> Vec<String> {
        let mut names: Vec<(usize, String)> = self
            .0
            .iter()
            .filter(|(_, w)| w.backing_space.is_none())
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

    /// Deterministically re-assign backing Spaces after observation.
    ///
    /// Blocker 3: assignment NEVER depends on HashMap iteration order. All
    /// inputs are sorted:
    /// - unclaimed Spaces by Mission-Control position (ties by id),
    /// - workspaces by stable config ordinal.
    ///
    /// Strategy per pass:
    /// 1. Resume: a workspace whose persisted `last_position` matches an
    ///    unclaimed Space's position (and satisfies its display preference)
    ///    re-claims that exact slot — this keeps `code` on its old slot across
    ///    Dock restarts instead of shuffling identities.
    /// 2. Remaining workspaces in ordinal order claim the first unclaimed
    ///    Space on their desired display, else the first remaining unclaimed
    ///    Space in position order.
    ///
    /// Returns the moves so the engine can carry SpaceId-keyed layout state
    /// (BSP trees) to each workspace's new backing Space (blocker 5).
    pub fn remap_after_snapshot(
        &mut self,
        observed_spaces: &HashMap<SpaceId, rovr_types::SpaceSnapshot>,
        observed_displays: &HashMap<DisplayId, rovr_types::DisplaySnapshot>,
    ) -> Vec<RemapMove> {
        // Dynamic (i3-style) workspaces manage their own binding lifecycle:
        // they must never adopt a pre-existing unclaimed Space, and stale/gap
        // reassignment must not shuffle them. Exclude from all remapping.
        let mut dynamic: Vec<WorkspaceState> = Vec::new();
        let mut stale_dynamic: Vec<RemapMove> = Vec::new();
        self.0.retain(|_, ws| {
            if ws.dynamic {
                // A dynamic workspace whose persisted backing Space no longer
                // exists has effectively been destroyed out-of-band (manual
                // Space deletion, prior session that crashed before cleanup).
                // Clear the stale id so the next alt-N re-spawns cleanly
                // instead of erroring on a missing Space. Report it as a move
                // so the engine can drop any SpaceId-keyed layout state.
                if let Some(sid) = ws.backing_space {
                    if !observed_spaces.contains_key(&sid) {
                        stale_dynamic.push(RemapMove {
                            name: ws.name.clone(),
                            from: Some(sid),
                            to: sid,
                        });
                        dynamic.push(WorkspaceState {
                            backing_space: None,
                            last_position: None,
                            ..ws.clone()
                        });
                        return false;
                    }
                }
                dynamic.push(ws.clone());
                false
            } else {
                true
            }
        });
        let mut moves = self.remap_configured_after_snapshot(observed_spaces, observed_displays);
        for ws in dynamic {
            self.0.insert(ws.name.clone(), ws);
        }
        moves.extend(stale_dynamic);
        moves
    }

    fn remap_configured_after_snapshot(
        &mut self,
        observed_spaces: &HashMap<SpaceId, rovr_types::SpaceSnapshot>,
        observed_displays: &HashMap<DisplayId, rovr_types::DisplaySnapshot>,
    ) -> Vec<RemapMove> {
        // A backing SpaceId that still exists is authoritative. Mission
        // Control positions compact when another Space is deleted; using
        // those positions to reassign surviving workspaces is what made
        // alt-2 silently become alt-3/4. Only stale/missing workspaces are
        // candidates for assignment.
        let mut stale_from: HashMap<String, SpaceId> = HashMap::new();
        for state in self.0.values_mut() {
            match state.backing_space {
                Some(sid) if observed_spaces.contains_key(&sid) => {
                    state.last_position = observed_spaces.get(&sid).map(|s| s.position);
                }
                Some(sid) => {
                    state.backing_space = None;
                    stale_from.insert(state.name.clone(), sid);
                }
                None => {}
            }
        }
        self.1 = false;

        let claimed: std::collections::HashSet<SpaceId> =
            self.0.values().filter_map(|w| w.backing_space).collect();
        let mut unclaimed: Vec<(SpaceId, u32, DisplayId)> = observed_spaces
            .iter()
            .filter(|(sid, snap)| !claimed.contains(sid) && !snap.is_fullscreen && !snap.is_system)
            .map(|(sid, s)| (*sid, s.position, s.display_id))
            .collect();
        unclaimed.sort_by_key(|&(sid, pos, _)| (pos, sid));

        let main_display = observed_displays
            .iter()
            .find(|(_, d)| d.is_main)
            .map(|(id, _)| *id)
            .or_else(|| {
                observed_displays
                    .iter()
                    .find(|(_, d)| d.focused)
                    .map(|(id, _)| *id)
            });
        let display_ok = |desired: &Option<String>, display_id: DisplayId| -> bool {
            match desired.as_deref() {
                None => true,
                Some("main") => main_display == Some(display_id),
                Some(numeric) => numeric
                    .parse::<u32>()
                    .map(|id| id == display_id.0)
                    .unwrap_or(false),
            }
        };

        let missing = self.missing_backing_in_order();
        let mut moves = Vec::new();

        // Dock restarts replace every SpaceId at once. Persisted positions are
        // used only to recover missing identities; they never override a valid
        // surviving ID after an ordinary deletion.
        for name in &missing {
            let Some(state) = self.0.get(name) else {
                continue;
            };
            let Some(last_position) = state.last_position else {
                continue;
            };
            let desired = state.desired_display.clone();
            let Some(index) = unclaimed.iter().position(|&(_, pos, display)| {
                pos == last_position && display_ok(&desired, display)
            }) else {
                continue;
            };
            let (sid, pos, _) = unclaimed.remove(index);
            let ws = self.0.get_mut(name).expect("name from own registry");
            moves.push(RemapMove {
                name: name.clone(),
                from: stale_from.get(name).copied(),
                to: sid,
            });
            ws.backing_space = Some(sid);
            ws.last_position = Some(pos);
        }

        // Initial startup and newly configured workspaces have no position
        // history. Assign only those still missing, in stable config order.
        for name in &missing {
            let Some(ws) = self.0.get(name) else {
                continue;
            };
            if ws.backing_space.is_some() {
                continue;
            }
            let desired = ws.desired_display.clone();
            let pick = unclaimed
                .iter()
                .position(|&(_, _, display)| display_ok(&desired, display))
                .or_else(|| (!unclaimed.is_empty()).then_some(0));
            let Some(index) = pick else {
                continue;
            };
            let (sid, pos, _) = unclaimed.remove(index);
            let ws = self.0.get_mut(name).expect("name from own registry");
            moves.push(RemapMove {
                name: name.clone(),
                from: stale_from.get(name).copied(),
                to: sid,
            });
            ws.backing_space = Some(sid);
            ws.last_position = Some(pos);
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
                    backing_space: backing.map(SpaceId),
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
    fn deleting_middle_space_preserves_surviving_workspace_ids() {
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
        assert_eq!(reg.backing_for("2"), None);
        assert_eq!(
            reg.backing_for("3"),
            Some(SpaceId(13)),
            "surviving workspace must keep its SpaceId after position compaction"
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
    fn stale_dynamic_binding_is_cleared_on_remap() {
        let mut reg = WorkspaceRegistry::default();
        reg.0.insert(
            "2".into(),
            WorkspaceState {
                name: "2".into(),
                desired_display: None,
                persistent: false,
                backing_space: Some(SpaceId(1778)),
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
            None,
            "stale dynamic backing must be cleared"
        );
        assert_eq!(
            moves.len(),
            1,
            "stale-clear must be reported as a remap move so the engine drops the layout"
        );
        assert_eq!(moves[0].from, Some(SpaceId(1778)));
        assert_eq!(moves[0].to, SpaceId(1778));
    }

    #[test]
    fn reordered_config_preserves_live_space_ids() {
        let workspace = |name: &str| rovr_config::WorkspaceConfig {
            name: name.into(),
            layout: rovr_types::LayoutKind::Bsp,
            display: None,
            persistent: false,
            plugin: None,
        };
        let mut reg = WorkspaceRegistry::from_config(&[workspace("code"), workspace("chat")]);
        reg.0.get_mut("code").unwrap().backing_space = Some(SpaceId(11));
        reg.0.get_mut("chat").unwrap().backing_space = Some(SpaceId(12));

        reg.ensure_from_config(&[workspace("chat"), workspace("code")]);
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(11), space(11, 1, 0));
        spaces.insert(SpaceId(12), space(12, 1, 1));
        let moves = reg.remap_after_snapshot(&spaces, &HashMap::new());

        assert_eq!(reg.backing_for("chat"), Some(SpaceId(12)));
        assert_eq!(reg.backing_for("code"), Some(SpaceId(11)));
        assert!(moves.is_empty());
    }

    /// Reload/config reconciliation must preserve valid IDs even when Mission
    /// Control positions have been manually rearranged.
    #[test]
    fn reload_preserves_ids_after_manual_drag() {
        let mut reg = registry_with(&[("code", true, Some(11), 0), ("chat", true, Some(12), 1)]);
        // Dragged arrangement: chat was pulled ahead of code in Mission
        // Control; ids stay, positions swap, registry remembers the slots.
        reg.0.get_mut("code").unwrap().last_position = Some(1);
        reg.0.get_mut("chat").unwrap().last_position = Some(0);
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(11), space(11, 1, 1));
        spaces.insert(SpaceId(12), space(12, 1, 0));
        let displays = HashMap::new();

        // Not dirty: resume-by-position keeps each workspace on its dragged
        // slot — a legitimate drag must be stable across snapshots.
        let moves = reg.remap_after_snapshot(&spaces, &displays);
        assert_eq!(
            (reg.backing_for("code"), reg.backing_for("chat")),
            (Some(SpaceId(11)), Some(SpaceId(12))),
            "resume-by-position must preserve a legitimate drag when not dirty"
        );
        assert!(moves.is_empty());

        let moves = reg.remap_after_snapshot(&spaces, &displays);
        assert_eq!(
            (reg.backing_for("code"), reg.backing_for("chat")),
            (Some(SpaceId(11)), Some(SpaceId(12))),
            "reload must keep surviving logical names on their current IDs"
        );
        assert!(moves.is_empty());
    }
}
