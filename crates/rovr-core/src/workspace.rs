use std::collections::HashMap;

use rovr_types::{DisplayId, SpaceId, WindowId};
use serde::{Deserialize, Serialize};

/// A logical workspace's backing Space moved from `from` (possibly gone) to
/// `to`. The engine uses this to carry SpaceId-keyed layout state (BSP tree,
/// orientation) to the new backing Space so topology survives volatility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemapMove {
    /// Registry key (`<display>:<name>`). Opaque outside the registry.
    pub name: String,
    pub from: Option<SpaceId>,
    pub to: SpaceId,
}

/// Registry key for one display's workspace. Each display owns its own strip
/// of names, so `alt-2` on the external display and `alt-2` on the built-in
/// display are different workspaces that happen to share a name. Keys are
/// runtime handles; the user-facing identity is `(display, name)`.
pub fn workspace_key(display: DisplayId, name: &str) -> String {
    format!("{}:{}", display.0, name)
}

fn unassigned_display() -> DisplayId {
    DisplayId(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceState {
    /// User-facing name ("1", "code"), the same string on every display.
    pub name: String,
    /// Display that owns this workspace.
    #[serde(default = "unassigned_display")]
    pub display: DisplayId,
    /// Whether workspace is persistent (should be recreated if missing)
    pub persistent: bool,
    /// Session-only observed backing. Native IDs are never persisted as identity.
    #[serde(skip)]
    pub backing: Option<WorkspaceBacking>,
    /// Stable order within its display — the deterministic tiebreaker for
    /// remapping. Persisted so identity survives daemon restarts.
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

    pub fn new(name: String, display: DisplayId, persistent: bool) -> Self {
        Self {
            name,
            display,
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
    /// Config entries are templates, not workspaces: an entry without
    /// `display` exists on EVERY observed display, `main` only on the main
    /// display, and a numeric entry only on that display id. Running this per
    /// complete observation is what makes a hot-plugged display arrive with
    /// its own 1..9 instead of inheriting the other display's workspaces.
    ///
    /// Entries owned by a display that is no longer observed are dropped here;
    /// their Spaces died with the display, and a reconnect re-instantiates the
    /// configured ones.
    /// `prune`: when true (config load/reload) entries config no longer
    /// declares are removed. When false (display hot-plug, first observation)
    /// the sync is additive only, so persisted and runtime-created workspaces
    /// are never evicted just because a display appeared.
    pub fn sync_config(
        &mut self,
        workspaces: &[rovr_config::WorkspaceConfig],
        displays: &HashMap<DisplayId, rovr_types::DisplaySnapshot>,
        prune: bool,
    ) {
        let main = displays.values().find(|d| d.is_main).map(|d| d.id);
        let mut next_ordinal = self
            .0
            .values()
            .map(|w| w.ordinal.saturating_add(1))
            .max()
            .unwrap_or(0);
        let mut configured: std::collections::HashSet<String> = std::collections::HashSet::new();

        for cfg in workspaces {
            let targets: Vec<DisplayId> = match cfg.display.as_deref() {
                Some("main") => main.into_iter().collect(),
                Some(selector) => match selector.parse::<u32>() {
                    Ok(id) => {
                        let id = DisplayId(id);
                        displays
                            .contains_key(&id)
                            .then_some(id)
                            .into_iter()
                            .collect()
                    }
                    Err(_) => Vec::new(),
                },
                // No display: this name exists on every display.
                None => displays.keys().copied().collect(),
            };
            for display in targets {
                let key = workspace_key(display, &cfg.name);
                configured.insert(key.clone());
                match self.0.get_mut(&key) {
                    Some(state) => {
                        state.persistent = cfg.persistent;
                        state.dynamic = false;
                    }
                    None => {
                        let mut state =
                            WorkspaceState::new(cfg.name.clone(), display, cfg.persistent);
                        state.ordinal = next_ordinal;
                        next_ordinal = next_ordinal.saturating_add(1);
                        self.0.insert(key, state);
                    }
                }
            }
        }

        // Entries on an unobserved display lost their Spaces with it, so they
        // always go. Removing config-declared entries is only authoritative on
        // a real config load/reload; an additive sync must not evict them.
        self.0.retain(|key, state| {
            displays.contains_key(&state.display)
                && (!prune || state.dynamic || configured.contains(key))
        });
        self.1 = false;
    }

    pub fn key_for(&self, display: DisplayId, name: &str) -> String {
        workspace_key(display, name)
    }

    pub fn get(&self, display: DisplayId, name: &str) -> Option<&WorkspaceState> {
        self.0.get(&workspace_key(display, name))
    }

    pub fn backing_for(&self, display: DisplayId, name: &str) -> Option<SpaceId> {
        self.get(display, name)
            .and_then(WorkspaceState::active_space)
    }

    /// Backing Space for an already-resolved registry key (internal queues
    /// carry keys, never names).
    pub fn backing_for_key(&self, key: &str) -> Option<SpaceId> {
        self.0.get(key).and_then(WorkspaceState::active_space)
    }

    pub fn name_for_space(&self, space: SpaceId) -> Option<&str> {
        self.0
            .values()
            .find(|w| w.active_space() == Some(space))
            .map(|w| w.name.as_str())
    }

    /// One display's workspaces in assignment order: numeric names sort
    /// numerically (`1`..`9`), named workspaces fall back to config order.
    pub fn keys_for_display(&self, display: DisplayId) -> Vec<String> {
        let mut keys: Vec<String> = self
            .0
            .iter()
            .filter(|(_, w)| w.display == display)
            .map(|(key, _)| key.clone())
            .collect();
        keys.sort_by_key(|key| {
            let state = &self.0[key];
            match state.name.parse::<u32>() {
                Ok(number) => (0u8, number as usize, state.name.clone()),
                Err(_) => (1u8, state.ordinal, state.name.clone()),
            }
        });
        keys
    }

    /// Keys on one display missing a backing space, in deterministic order.
    pub fn missing_backing_in_order(&self, display: DisplayId) -> Vec<String> {
        self.keys_for_display(display)
            .into_iter()
            .filter(|key| self.0[key].active_space().is_none())
            .collect()
    }

    /// Keys of PERSISTENT workspaces on this display without a backing Space —
    /// these need a CreateSpace lifecycle operation (blocker 4).
    pub fn ensure_persistent(&self, display: DisplayId) -> Vec<String> {
        self.missing_backing_in_order(display)
            .into_iter()
            .filter(|key| self.0[key].persistent)
            .collect()
    }

    /// Recompute observed handles from native slots, never from remembered IDs.
    /// Each display is remapped independently: its workspaces take its own
    /// ordinary Spaces in name order, so one display's topology can never move
    /// another display's workspaces.
    pub fn remap_after_snapshot(
        &mut self,
        spaces: &HashMap<SpaceId, rovr_types::SpaceSnapshot>,
        displays: &HashMap<DisplayId, rovr_types::DisplaySnapshot>,
    ) -> Vec<RemapMove> {
        let parked: std::collections::HashSet<SpaceId> = self
            .0
            .values()
            .filter_map(|w| w.backing.and_then(WorkspaceBacking::restore_space))
            .collect();

        let mut slots: HashMap<DisplayId, Vec<&rovr_types::SpaceSnapshot>> = HashMap::new();
        for space in spaces.values() {
            if space.is_fullscreen || space.is_system || parked.contains(&space.id) {
                continue;
            }
            slots.entry(space.display_id).or_default().push(space);
        }
        for list in slots.values_mut() {
            list.sort_by_key(|space| (space.position, space.id));
        }

        // Displays in physical arrangement order (left→right, top→bottom):
        // stable across daemon restarts and independent of display-id churn.
        let mut order: Vec<DisplayId> = displays.keys().copied().collect();
        order.sort_by_key(|id| {
            displays
                .get(id)
                .map(|d| (d.frame.x.round() as i64, d.frame.y.round() as i64))
                .unwrap_or((i64::MAX, i64::from(id.0)))
        });

        // A workspace whose display disappeared has no Space to show. Clearing
        // the handle (not the workspace) lets a reconnect rebind it.
        let observed: std::collections::HashSet<DisplayId> = displays.keys().copied().collect();
        for state in self.0.values_mut() {
            if !observed.contains(&state.display) {
                state.backing = None;
                state.last_position = None;
            }
        }

        let mut moves = Vec::new();
        for display in order {
            let mut available = slots.remove(&display).unwrap_or_default();
            for key in self.keys_for_display(display) {
                let Some(state) = self.0.get_mut(&key) else {
                    continue;
                };
                if matches!(
                    state.backing,
                    Some(WorkspaceBacking::FullscreenReplacement { .. })
                ) {
                    continue;
                }
                let old = state.active_space();
                let new = if available.is_empty() {
                    None
                } else {
                    Some(available.remove(0))
                };
                state.backing = new.map(|space| WorkspaceBacking::Normal { space: space.id });
                state.last_position = new.map(|space| space.position);
                if old != state.active_space() {
                    if let Some(to) = state.active_space().or(old) {
                        moves.push(RemapMove {
                            name: key,
                            from: old,
                            to,
                        });
                    }
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

    fn display_at(id: u32, x: f64, is_main: bool) -> rovr_types::DisplaySnapshot {
        rovr_types::DisplaySnapshot {
            id: DisplayId(id),
            frame: rovr_types::Rect {
                x,
                y: 0.0,
                width: 1000.0,
                height: 800.0,
            },
            label: None,
            focused: false,
            is_main,
            generation: 0,
        }
    }

    fn displays(
        items: &[rovr_types::DisplaySnapshot],
    ) -> HashMap<DisplayId, rovr_types::DisplaySnapshot> {
        items.iter().map(|d| (d.id, d.clone())).collect()
    }

    fn two_displays() -> HashMap<DisplayId, rovr_types::DisplaySnapshot> {
        displays(&[display_at(1, -1470.0, false), display_at(2, 0.0, true)])
    }

    fn fill(reg: &mut WorkspaceRegistry, display: DisplayId, names: &[&str], persistent: bool) {
        for (i, name) in names.iter().enumerate() {
            let mut state = WorkspaceState::new(name.to_string(), display, persistent);
            state.ordinal = i;
            reg.0.insert(workspace_key(display, name), state);
        }
    }

    /// The behaviour the user asked for: `alt-1` is local to a display. Each
    /// display keeps its own 1..9, and remapping one display's Spaces never
    /// moves the other display's workspace.
    #[test]
    fn workspace_names_are_independent_per_display() {
        let mut reg = WorkspaceRegistry::default();
        fill(&mut reg, DisplayId(1), &["1", "2"], false);
        fill(&mut reg, DisplayId(2), &["1", "2"], false);

        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(11), space(11, 1, 0));
        spaces.insert(SpaceId(12), space(12, 1, 1));
        spaces.insert(SpaceId(21), space(21, 2, 2));
        spaces.insert(SpaceId(22), space(22, 2, 3));

        reg.remap_after_snapshot(&spaces, &two_displays());

        assert_eq!(reg.backing_for(DisplayId(1), "1"), Some(SpaceId(11)));
        assert_eq!(reg.backing_for(DisplayId(1), "2"), Some(SpaceId(12)));
        assert_eq!(reg.backing_for(DisplayId(2), "1"), Some(SpaceId(21)));
        assert_eq!(reg.backing_for(DisplayId(2), "2"), Some(SpaceId(22)));
    }

    /// A display with fewer Spaces than workspaces leaves the surplus unbound
    /// instead of stealing a slot from the other display.
    #[test]
    fn a_display_never_claims_another_displays_space() {
        let mut reg = WorkspaceRegistry::default();
        fill(&mut reg, DisplayId(1), &["1", "2", "3"], false);
        fill(&mut reg, DisplayId(2), &["1"], false);

        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(11), space(11, 1, 0));
        spaces.insert(SpaceId(21), space(21, 2, 1));
        spaces.insert(SpaceId(22), space(22, 2, 2));

        reg.remap_after_snapshot(&spaces, &two_displays());

        assert_eq!(reg.backing_for(DisplayId(1), "1"), Some(SpaceId(11)));
        assert_eq!(
            reg.backing_for(DisplayId(1), "2"),
            None,
            "display 1 must not claim display 2's second Space"
        );
        assert_eq!(reg.backing_for(DisplayId(2), "1"), Some(SpaceId(21)));
        assert_eq!(
            reg.backing_for(DisplayId(2), "2"),
            None,
            "the second display keeps its surplus Space unclaimed by name"
        );
    }

    /// A config entry without `display` exists on every display, and only on
    /// displays that are actually observed.
    #[test]
    fn config_without_display_instantiates_on_every_display() {
        let workspace = |name: &str, display: Option<&str>| rovr_config::WorkspaceConfig {
            name: name.into(),
            layout: rovr_types::LayoutKind::Bsp,
            display: display.map(str::to_string),
            persistent: true,
            plugin: None,
        };
        let mut reg = WorkspaceRegistry::default();
        reg.sync_config(&[workspace("1", None)], &two_displays(), true);

        assert!(reg.get(DisplayId(1), "1").is_some());
        assert!(reg.get(DisplayId(2), "1").is_some());
        assert!(
            reg.get(DisplayId(3), "1").is_none(),
            "not instantiated on an unobserved display"
        );
    }

    #[test]
    fn config_pinned_to_main_skips_other_displays() {
        let mut reg = WorkspaceRegistry::default();
        reg.sync_config(
            &[rovr_config::WorkspaceConfig {
                name: "code".into(),
                layout: rovr_types::LayoutKind::Bsp,
                display: Some("main".into()),
                persistent: true,
                plugin: None,
            }],
            &two_displays(),
            true,
        );

        assert!(reg.get(DisplayId(2), "code").is_some(), "main is display 2");
        assert!(reg.get(DisplayId(1), "code").is_none());
    }

    /// A display that disappears must not leave its workspaces pointing at
    /// Spaces that no longer exist.
    #[test]
    fn unobserved_display_clears_backing_and_drops_entries() {
        let mut reg = WorkspaceRegistry::default();
        fill(&mut reg, DisplayId(1), &["1"], true);
        fill(&mut reg, DisplayId(2), &["1"], true);
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(11), space(11, 1, 0));
        spaces.insert(SpaceId(21), space(21, 2, 1));
        reg.remap_after_snapshot(&spaces, &two_displays());
        assert_eq!(reg.backing_for(DisplayId(2), "1"), Some(SpaceId(21)));

        // Display 2 unplugged: only its own entries go, display 1 survives.
        let only_one = displays(&[display_at(1, -1470.0, false)]);
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(11), space(11, 1, 0));
        reg.remap_after_snapshot(&spaces, &only_one);

        assert_eq!(reg.backing_for(DisplayId(1), "1"), Some(SpaceId(11)));
        assert_eq!(
            reg.backing_for(DisplayId(2), "1"),
            None,
            "the vanished display's binding is cleared, never left dangling"
        );
        // A config load/reload is what drops the entry itself.
        reg.sync_config(&[], &only_one, true);
        assert!(reg.get(DisplayId(2), "1").is_none());
    }

    /// Numeric names sort numerically within a display, so `alt-2` is the
    /// second Space regardless of creation order; named workspaces follow
    /// config order after them.
    #[test]
    fn numeric_names_order_ahead_of_named_ones() {
        let mut reg = WorkspaceRegistry::default();
        fill(&mut reg, DisplayId(1), &["10", "2"], false);
        let mut named = WorkspaceState::new("chat".into(), DisplayId(1), false);
        named.ordinal = 0;
        reg.0.insert(workspace_key(DisplayId(1), "chat"), named);

        let keys = reg.keys_for_display(DisplayId(1));
        assert_eq!(keys.len(), 3);
        assert_eq!(reg.0[&keys[0]].name, "2");
        assert_eq!(reg.0[&keys[1]].name, "10");
        assert_eq!(reg.0[&keys[2]].name, "chat");
    }

    /// `name_for_space` is the public face of a backing Space: it must report
    /// the user-facing name, never the composite key.
    #[test]
    fn name_for_space_strips_the_display_qualifier() {
        let mut reg = WorkspaceRegistry::default();
        fill(&mut reg, DisplayId(2), &["3"], false);
        let mut spaces = HashMap::new();
        spaces.insert(SpaceId(21), space(21, 2, 0));
        reg.remap_after_snapshot(&spaces, &two_displays());

        assert_eq!(reg.name_for_space(SpaceId(21)), Some("3"));
    }
}
