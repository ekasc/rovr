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
        // Add new, remove deleted, preserve backing_space for existing.
        // Ordinals always come from CURRENT config order (stable input).
        let old_len = self.0.len();
        let old_ordinals: HashMap<String, usize> = self
            .0
            .iter()
            .map(|(name, state)| (name.clone(), state.ordinal))
            .collect();
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
        // If any workspace was added or removed, or ordinals changed,
        // the next remap needs a full ordinal→position compaction so that
        // alt-N stays N→desktop N (e.g. deleting workspace 3 should make
        // old 4→3, 5→4, …). Flag for the next snapshot.
        if new_map.len() != old_len || !existing.is_empty() {
            self.1 = true;
        } else {
            // Also flag if any ordinal changed (reordered config). A reorder
            // intentionally remaps by the new ordinal, not by remembered
            // Mission-Control positions.
            let reordered = new_map
                .iter()
                .any(|(name, ws)| old_ordinals.get(name) != Some(&ws.ordinal));
            if reordered {
                self.1 = true;
                for state in new_map.values_mut() {
                    state.last_position = None;
                }
            }
        }
        self.0 = new_map;
    }

    pub fn backing_for(&self, name: &str) -> Option<SpaceId> {
        self.0.get(name).and_then(|w| w.backing_space)
    }

    /// Force the next `remap_after_snapshot` to do a full ordinal→position
    /// reassignment (clears remembered positions). Used by explicit config
    /// reloads so the configured order is re-applied verbatim.
    pub fn mark_dirty(&mut self) {
        self.1 = true;
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
        let mut had_stale = false;
        for state in self.0.values_mut() {
            if let Some(sid) = state.backing_space {
                if !observed_spaces.contains_key(&sid) {
                    state.backing_space = None;
                    stale_from.insert(state.name.clone(), sid);
                    had_stale = true;
                }
            }
        }

        // Keep last_position in sync with manual Mission Control drags.
        // If a Space is dragged, its position changes but its ID stays;
        // without this, the next Dock restart would try to reclaim the old
        // position and misplace the workspace.
        for ws in self.0.values_mut() {
            if let Some(sid) = ws.backing_space {
                if let Some(space) = observed_spaces.get(&sid) {
                    ws.last_position = Some(space.position);
                }
            }
        }

        // If workspaces were added/removed/reordered via config reload,
        // the next snapshot needs a full compaction so that alt-N stays
        // N→desktop N (e.g. deleting workspace 3 should make old 4→3).
        if self.1 {
            had_stale = true;
            self.1 = false;
            for state in self.0.values_mut() {
                state.last_position = None;
            }
        }

        // If a workspace was deleted via config (or a Space was deleted
        // without going through the stale path because the Space still
        // exists but the workspace is gone), the remaining workspaces will
        // have a gap in their positions when sorted by ordinal (e.g.
        // ordinal 0→pos0, 1→pos1, 3→pos3, 4→pos4 with pos2 unclaimed).
        // Detect the gap and compact by doing a full reassignment. This
        // is distinct from a manual drag where positions are a permutation
        // of 0..n-1 but out of order – there we keep the dragged mapping.
        if !had_stale {
            let mut ord_pos: Vec<(usize, u32)> = Vec::new();
            for ws in self.0.values() {
                if let Some(sid) = ws.backing_space {
                    if let Some(space) = observed_spaces.get(&sid) {
                        ord_pos.push((ws.ordinal, space.position));
                    }
                }
            }
            ord_pos.sort_by_key(|(ord, _)| *ord);
            let positions: Vec<u32> = ord_pos.iter().map(|(_, pos)| *pos).collect();
            let n = positions.len() as u32;
            // Check if positions are exactly 0..n-1 as a set (no gap).
            // If not, we have a hole that needs compaction.
            let mut sorted_pos = positions.clone();
            sorted_pos.sort_unstable();
            let expected: Vec<u32> = (0..n).collect();
            if sorted_pos != expected {
                // Gap detected – need full reassignment to compact.
                // Reuse the had_stale path by setting had_stale = true and
                // falling through to the full reassignment logic below.
                // Instead of duplicating, just trigger the same full
                // reassignment as for stale.
                let all_names = {
                    let mut v: Vec<(usize, String)> =
                        self.0.iter().map(|(k, w)| (w.ordinal, k.clone())).collect();
                    v.sort();
                    v.into_iter().map(|(_, n)| n).collect::<Vec<_>>()
                };
                let mut old_backings: HashMap<String, Option<SpaceId>> = HashMap::new();
                for name in &all_names {
                    if let Some(ws) = self.0.get(name) {
                        old_backings.insert(name.clone(), ws.backing_space);
                    }
                }
                for ws in self.0.values_mut() {
                    ws.backing_space = None;
                }
                let mut unclaimed: Vec<(SpaceId, u32, DisplayId)> = observed_spaces
                    .iter()
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
                let mut moves: Vec<RemapMove> = Vec::new();
                for name in &all_names {
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
                        let ws = self.0.get_mut(name).unwrap();
                        moves.push(RemapMove {
                            name: name.clone(),
                            from: old_backings.get(name).copied().flatten(),
                            to: sid,
                        });
                        ws.backing_space = Some(sid);
                        ws.last_position = Some(pos_of(observed_spaces, sid));
                    }
                }
                for name in &all_names {
                    if self.0.get(name).and_then(|w| w.backing_space).is_some() {
                        continue;
                    }
                    let desired = self.0.get(name).unwrap().desired_display.clone();
                    let pick = unclaimed
                        .iter()
                        .position(|&(_, _, disp)| display_ok(&desired, disp))
                        .or(if unclaimed.is_empty() { None } else { Some(0) });
                    let Some(idx) = pick else { continue };
                    let (sid, pos, _) = unclaimed.remove(idx);
                    let ws = self.0.get_mut(name).unwrap();
                    moves.push(RemapMove {
                        name: name.clone(),
                        from: old_backings.get(name).copied().flatten(),
                        to: sid,
                    });
                    ws.backing_space = Some(sid);
                    ws.last_position = Some(pos);
                }
                return moves;
            }
        }

        // Multi-display fix: when a Space is deleted (stale), the global
        // position order shifts and the simple "keep existing backings, only
        // assign missing" leaves a hole (e.g. alt-5 goes to desktop 4).
        // If any stale was dropped, do a full ordinal→position reassignment
        // for all workspaces so that workspace 1→pos0, 2→pos1, … stays
        // invariant. This is deterministic and display-aware via
        // `display_ok`.
        if had_stale {
            let all_names = {
                let mut v: Vec<(usize, String)> =
                    self.0.iter().map(|(k, w)| (w.ordinal, k.clone())).collect();
                v.sort();
                v.into_iter().map(|(_, n)| n).collect::<Vec<_>>()
            };
            // Clear all current backings so the two passes below reassign
            // from scratch in ordinal order. Moves are recorded as
            // `from = old backing (or stale)` → `to = new`.
            let mut old_backings: HashMap<String, Option<SpaceId>> = HashMap::new();
            for name in &all_names {
                if let Some(ws) = self.0.get(name) {
                    old_backings.insert(
                        name.clone(),
                        ws.backing_space.or_else(|| stale_from.get(name).copied()),
                    );
                }
            }
            for ws in self.0.values_mut() {
                ws.backing_space = None;
            }
            // Rebuild unclaimed as all observed spaces (since we cleared)
            let mut unclaimed: Vec<(SpaceId, u32, DisplayId)> = observed_spaces
                .iter()
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
            let mut moves: Vec<RemapMove> = Vec::new();
            // Pass 1: resume by last_position
            for name in &all_names {
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
                    let ws = self.0.get_mut(name).unwrap();
                    moves.push(RemapMove {
                        name: name.clone(),
                        from: old_backings.get(name).copied().flatten(),
                        to: sid,
                    });
                    ws.backing_space = Some(sid);
                    ws.last_position = Some(pos_of(observed_spaces, sid));
                }
            }
            // Pass 2: ordinal order
            for name in &all_names {
                if self.0.get(name).and_then(|w| w.backing_space).is_some() {
                    continue;
                }
                let desired = self.0.get(name).unwrap().desired_display.clone();
                let pick = unclaimed
                    .iter()
                    .position(|&(_, _, disp)| display_ok(&desired, disp))
                    .or(if unclaimed.is_empty() { None } else { Some(0) });
                let Some(idx) = pick else { continue };
                let (sid, pos, _) = unclaimed.remove(idx);
                let ws = self.0.get_mut(name).unwrap();
                moves.push(RemapMove {
                    name: name.clone(),
                    from: old_backings.get(name).copied().flatten(),
                    to: sid,
                });
                ws.backing_space = Some(sid);
                ws.last_position = Some(pos);
            }
            return moves;
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

        // Does this space's display satisfy the workspace's desired display?
        // "main" is the actual main display (CGMainDisplayID), not the
        // focused/active menu-bar display. Falls back to focused only if
        // is_main is unavailable (e.g. mock platform).
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
    fn reordered_config_compacts_ordinal_to_position_on_next_snapshot() {
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

        assert_eq!(reg.backing_for("chat"), Some(SpaceId(11)));
        assert_eq!(reg.backing_for("code"), Some(SpaceId(12)));
        assert_eq!(moves.len(), 2);
    }

    /// c4a8c69 regression: mark_dirty must defeat resume-by-position so an
    /// explicit reload can re-apply the configured order over a manually
    /// dragged arrangement.
    #[test]
    fn mark_dirty_discards_last_positions_and_forces_ordinal_reassignment() {
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

        // Dirty (what a reload does): remembered positions are discarded and
        // the configured ordinal order wins.
        reg.mark_dirty();
        let moves = reg.remap_after_snapshot(&spaces, &displays);
        assert_eq!(
            (reg.backing_for("code"), reg.backing_for("chat")),
            (Some(SpaceId(12)), Some(SpaceId(11))),
            "dirty remap must reassign strictly by ordinal"
        );
        assert_eq!(moves.len(), 2);
    }
}
