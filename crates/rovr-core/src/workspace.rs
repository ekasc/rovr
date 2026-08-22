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
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceRegistry(pub HashMap<String, WorkspaceState>);

impl WorkspaceRegistry {
    pub fn from_config(workspaces: &[rovr_config::WorkspaceConfig]) -> Self {
        let mut m = HashMap::new();
        for (ordinal, w) in workspaces.iter().enumerate() {
            let mut state = WorkspaceState::new(w.name.clone(), w.display.clone(), w.persistent);
            state.ordinal = ordinal;
            m.insert(w.name.clone(), state);
        }
        Self(m)
    }

    pub fn ensure_from_config(&mut self, workspaces: &[rovr_config::WorkspaceConfig]) {
        // Add new, remove deleted, preserve backing_space for existing.
        // Ordinals always come from CURRENT config order (stable input).
        let mut existing = std::mem::take(&mut self.0);
        let mut new_map = HashMap::new();
        for (ordinal, cfg) in workspaces.iter().enumerate() {
            if let Some(mut state) = existing.remove(&cfg.name) {
                state.desired_display = cfg.display.clone();
                state.persistent = cfg.persistent;
                state.ordinal = ordinal;
                new_map.insert(cfg.name.clone(), state);
            } else {
                let mut state =
                    WorkspaceState::new(cfg.name.clone(), cfg.display.clone(), cfg.persistent);
                state.ordinal = ordinal;
                new_map.insert(cfg.name.clone(), state);
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
        // 1. Drop stale backings (volatile macOS SpaceIds), remembering the
        //    old id so the engine can carry layout state to the new backing.
        let mut stale_from: HashMap<String, SpaceId> = HashMap::new();
        for state in self.0.values_mut() {
            if let Some(sid) = state.backing_space {
                if !observed_spaces.contains_key(&sid) {
                    state.backing_space = None;
                    stale_from.insert(state.name.clone(), sid);
                }
            }
        }

        let claimed: std::collections::HashSet<SpaceId> =
            self.0.values().filter_map(|w| w.backing_space).collect();

        // Unclaimed spaces sorted deterministically by position then id.
        let mut unclaimed: Vec<(SpaceId, u32, DisplayId)> = observed_spaces
            .iter()
            .filter(|(sid, _)| !claimed.contains(sid))
            .map(|(sid, s)| (*sid, s.position, s.display_id))
            .collect();
        unclaimed.sort_by_key(|&(sid, pos, _)| (pos, sid));

        let focused_display = observed_displays
            .iter()
            .find(|(_, d)| d.focused)
            .map(|(id, _)| *id);

        // Does this space's display satisfy the workspace's desired display?
        let display_ok = |desired: &Option<String>, display_id: DisplayId| -> bool {
            match desired.as_deref() {
                None => true,
                Some("main") => focused_display == Some(display_id),
                Some(numeric) => numeric
                    .parse::<u32>()
                    .map(|id| id == display_id.0)
                    .unwrap_or(false),
            }
        };

        // Workspaces needing a backing, lowest ordinal first.
        let missing = self.missing_backing_in_order();

        let mut moves: Vec<RemapMove> = Vec::new();

        // Pass 1: resume previous positions (deterministic slot recovery).
        for name in &missing {
            let Some(state) = self.0.get(name) else {
                continue;
            };
            let Some(last_pos) = state.last_position else {
                continue;
            };
            let desired = state.desired_display.clone();
            if let Some(idx) = unclaimed
                .iter()
                .position(|&(_, pos, disp)| pos == last_pos && display_ok(&desired, disp))
            {
                let (sid, _, _) = unclaimed.remove(idx);
                let ws = self.0.get_mut(name).expect("name from own map");
                moves.push(RemapMove {
                    name: name.clone(),
                    from: ws.backing_space.or_else(|| stale_from.get(name).copied()),
                    to: sid,
                });
                ws.backing_space = Some(sid);
                ws.last_position = Some(pos_of(observed_spaces, sid));
            }
        }

        // Pass 2: remaining workspaces in ordinal order; prefer desired
        // display, else first unclaimed in position order.
        for name in &missing {
            let Some(ws) = self.0.get(name) else { continue };
            if ws.backing_space.is_some() {
                continue;
            }
            let desired = ws.desired_display.clone();
            let pick = unclaimed
                .iter()
                .position(|&(_, _, disp)| display_ok(&desired, disp))
                // Desired display absent (e.g. disconnected): fall back to the
                // first unclaimed space rather than staying unmapped forever.
                .or(if unclaimed.is_empty() { None } else { Some(0) });
            let Some(idx) = pick else { continue };
            if idx >= unclaimed.len() {
                continue;
            }
            let (sid, pos, _) = unclaimed.remove(idx);
            let ws = self.0.get_mut(name).expect("name from own map");
            moves.push(RemapMove {
                name: name.clone(),
                from: ws.backing_space.or_else(|| stale_from.get(name).copied()),
                to: sid,
            });
            ws.backing_space = Some(sid);
            ws.last_position = Some(pos);
        }

        moves
    }
}

fn pos_of(observed_spaces: &HashMap<SpaceId, rovr_types::SpaceSnapshot>, sid: SpaceId) -> u32 {
    observed_spaces
        .get(&sid)
        .map(|s| s.position)
        .unwrap_or(u32::MAX)
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
}
