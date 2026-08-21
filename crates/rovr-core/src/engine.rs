use rovr_config::Config;
use rovr_types::{Direction, PlatformSnapshot, Rect, SpaceId, WindowId};
use std::path::Path;
use thiserror::Error;

use crate::persistence::PersistedState;
use anyhow::{Context, Result};

use crate::layout_state::{Axis, Layouts, ScratchpadState};
use crate::workspace::WorkspaceRegistry;
use crate::{
    layout::apply_layout, reconcile::reconcile, Action, DesiredState, Event, FlightRecorder,
    ObservedState,
};
use rovr_layout_plugin::Registry as PluginRegistry;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("window {0:?} does not exist")]
    WindowNotFound(WindowId),
    #[error("space {0:?} does not exist")]
    SpaceNotFound(SpaceId),
    #[error("cannot move a space after itself")]
    SameSpace,
    #[error("no focused space in observed state")]
    NoFocusedSpace,
    #[error("no focusable window in direction {direction:?} from {from:?}")]
    NoWindowInDirection {
        from: WindowId,
        direction: Direction,
    },
    #[error("window {0:?} is not on an observed display")]
    WindowNotOnDisplay(WindowId),
    #[error("workspace {0} not found")]
    WorkspaceNotFound(String),
    #[error("workspace {0} has no backing space")]
    WorkspaceNoBacking(String),
}
#[derive(Default)]
pub struct Engine {
    pub config: Config,
    pub observed: ObservedState,
    pub desired: DesiredState,
    pub flight_recorder: FlightRecorder,
    pub layouts: Layouts,
    pub scratchpads: ScratchpadState,
    pub workspaces: WorkspaceRegistry,
    pub plugins: PluginRegistry,
}

fn load_plugins_from_disk(registry: &mut PluginRegistry) {
    let dir = match std::env::var("HOME") {
        Ok(h) => std::path::PathBuf::from(h).join(".config/rovr/plugins"),
        Err(_) => return,
    };
    if !dir.exists() {
        return;
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("wasm") {
            let _ = registry.load_wasm_file(&path);
        }
    }
}

impl Engine {
    pub fn new(config: Config) -> Self {
        let workspaces = WorkspaceRegistry::from_config(&config.workspaces);
        let mut plugins = PluginRegistry::new();
        load_plugins_from_disk(&mut plugins);
        Self {
            plugins,
            config,
            workspaces,
            ..Default::default()
        }
    }
    pub fn toggle_scratchpad(&mut self, name: &str) -> Vec<Action> {
        self.scratchpads.toggle(name);
        let is_open = self.scratchpads.is_open(name);
        // Find pad config
        let pad = match self.config.scratchpads.iter().find(|p| p.name == name) {
            Some(p) => p.clone(),
            None => return vec![],
        };
        // Locate first matching window (including minimized, since we now enumerate All)
        let matching = self.observed.windows.values().find(|w| {
            let app_ok = pad
                .app
                .as_ref()
                .map(|a| w.bundle_id.as_deref() == Some(a.as_str()))
                .unwrap_or(true);
            let title_ok = pad
                .title
                .as_ref()
                .map(|t| w.title.contains(t.as_str()))
                .unwrap_or(true);
            app_ok && title_ok
        });
        let Some(win) = matching else {
            // No window yet — toggle open state is still persisted, layout will float when it appears.
            // Spawn support can be added where pad specifies a command.
            return vec![];
        };
        let win_id = win.id;
        if is_open {
            // Show: unminimize, move to focused space, center 800x600, focus
            let focused_space = self
                .observed
                .spaces
                .values()
                .find(|s| s.focused)
                .map(|s| s.id);
            let display_frame = focused_space
                .and_then(|sid| self.observed.spaces.get(&sid))
                .and_then(|s| self.observed.displays.get(&s.display_id))
                .map(|d| d.frame)
                .or_else(|| self.observed.displays.values().next().map(|d| d.frame));
            let mut actions = vec![Action::SetWindowMinimized {
                window: win_id,
                minimized: false,
            }];
            if let Some(space) = focused_space {
                let current = self.observed.windows.get(&win_id).and_then(|w| w.space_id);
                if current != Some(space) {
                    actions.push(Action::MoveWindowToSpace {
                        window: win_id,
                        space,
                    });
                }
            }
            if let Some(frame) = display_frame {
                let w = 800.0;
                let h = 600.0;
                let x = frame.x + (frame.width - w) / 2.0;
                let y = frame.y + (frame.height - h) / 2.0;
                actions.push(Action::SetWindowFrame {
                    window: win_id,
                    frame: Rect {
                        x,
                        y,
                        width: w,
                        height: h,
                    },
                });
            }
            actions.push(Action::FocusWindow { window: win_id });
            actions
        } else {
            // Hide: minimize
            vec![Action::SetWindowMinimized {
                window: win_id,
                minimized: true,
            }]
        }
    }
    /// Returns the BSP orientation of a space, if tracked, as `(horizontal,
    /// reversed)`. Used by the daemon to describe layout changes without
    /// reaching into `layout_state` internals.
    pub fn layout_orientation(&self, space: SpaceId) -> Option<(bool, bool)> {
        self.layouts.get(&space).map(|state| {
            (
                state.orientation.axis == Axis::Horizontal,
                state.orientation.reversed,
            )
        })
    }
    pub fn save_state(&self, path: &Path) -> Result<()> {
        let persisted = PersistedState {
            layouts: self
                .layouts
                .iter()
                .map(|(id, state)| (id.0.to_string(), state.clone()))
                .collect(),
            scratchpads: self.scratchpads.0.clone(),
            workspaces: self.workspaces.0.clone(),
        };
        let json = serde_json::to_string_pretty(&persisted).context("serialize state")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("create state dir")?;
        }
        std::fs::write(path, json).context("write state file")?;
        Ok(())
    }

    pub fn load_state(&mut self, path: &Path) -> Result<()> {
        let data = std::fs::read_to_string(path).context("read state file")?;
        let persisted: PersistedState = serde_json::from_str(&data).context("parse state file")?;
        self.layouts = persisted
            .layouts
            .into_iter()
            .filter_map(|(k, v)| k.parse::<u64>().ok().map(|n| (SpaceId(n), v)))
            .collect();
        self.scratchpads = ScratchpadState(persisted.scratchpads);
        self.workspaces = crate::workspace::WorkspaceRegistry(persisted.workspaces);
        // After load, ensure workspaces still reflect current config (preserve backing where name matches)
        self.workspaces.ensure_from_config(&self.config.workspaces);
        Ok(())
    }
}

impl Engine {
    pub fn rotate_layout(&mut self, space: SpaceId) {
        let state = self.layouts.entry(space).or_default();
        state.orientation = state.orientation.rotate();
        state.bsp.rotate();
    }

    pub fn mirror_layout(&mut self, space: SpaceId) {
        let state = self.layouts.entry(space).or_default();
        state.orientation = state.orientation.mirror();
        state.bsp.mirror();
    }

    pub fn balance_layout(&mut self, space: SpaceId) {
        if let Some(state) = self.layouts.get_mut(&space) {
            state.bsp.balance();
        }
    }

    pub fn swap_windows(&mut self, a: WindowId, b: WindowId) -> Result<(), EngineError> {
        // Find space containing both? For now swap in any space that contains both.
        for state in self.layouts.values_mut() {
            if state.bsp.contains(a) && state.bsp.contains(b) && state.bsp.swap(a, b) {
                return Ok(());
            }
        }
        Err(EngineError::WindowNotFound(a))
    }

    pub fn warp_window(&mut self, window: WindowId, target: WindowId) -> Result<(), EngineError> {
        self.require_window(window)?;
        self.require_window(target)?;
        for state in self.layouts.values_mut() {
            if state.bsp.contains(target) && state.bsp.warp(window, target, false) {
                return Ok(());
            }
        }
        Err(EngineError::WindowNotFound(target))
    }

    pub fn set_split_ratio(&mut self, space: SpaceId, ratio: f64) -> Result<(), EngineError> {
        let state = self
            .layouts
            .get_mut(&space)
            .ok_or(EngineError::SpaceNotFound(space))?;
        if !state.bsp.set_ratio(ratio) {
            // If tree is empty/single leaf, ratio has no effect but still success
            // Ensure at least orientation ratio concept? Return ok for empty.
            if state.bsp.is_empty() || state.bsp.len() == 1 {
                return Ok(());
            }
            return Err(EngineError::SpaceNotFound(space));
        }
        Ok(())
    }
    pub fn reload_config(&mut self, config: Config) {
        self.config = config;
        self.workspaces.ensure_from_config(&self.config.workspaces);
        self.workspaces
            .remap_after_snapshot(&self.observed.spaces, &self.observed.displays);
        // Reload WASM plugins from disk (isolated, fuel-limited, version-checked)
        self.plugins = PluginRegistry::new();
        load_plugins_from_disk(&mut self.plugins);
    }

    pub fn apply_event(&mut self, event: Event) -> Vec<Action> {
        self.flight_recorder.record("event", format!("{event:?}"));

        match event {
            Event::Snapshot(snapshot) => {
                self.apply_snapshot(snapshot);
                self.workspaces
                    .remap_after_snapshot(&self.observed.spaces, &self.observed.displays);
            }
            Event::WindowDestroyed { window } => {
                self.observed.windows.remove(&window);
                self.desired.windows.remove(&window);
            }
            Event::SpaceDestroyed { space } => {
                self.observed.spaces.remove(&space);
                for target in self.desired.windows.values_mut() {
                    if target.space == Some(space) {
                        target.space = None;
                    }
                }
            }
            Event::DisplayRemoved { display } => {
                self.observed.displays.remove(&display);
                self.observed.bump_generation();
            }
            Event::SystemWillSleep => {}
            Event::SystemWoke | Event::DockRestarted | Event::DisplayTopologyChanged => {
                self.observed.bump_generation();
            }
        }

        apply_layout(
            &self.config,
            &self.observed,
            &mut self.desired,
            &mut self.layouts,
            &self.workspaces,
            &self.plugins,
            &self.scratchpads,
        );

        let actions = reconcile(&self.observed, &self.desired);
        for action in &actions {
            self.flight_recorder
                .record("reconcile.action", format!("{action:?}"));
        }
        actions
    }

    /// Request a one-shot frame mutation. Persistent layout policy belongs in
    /// `DesiredState`; CLI commands must not accidentally pin a floating window
    /// forever.
    pub fn set_window_frame(
        &self,
        window: WindowId,
        frame: Rect,
    ) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        Ok(vec![Action::SetWindowFrame { window, frame }])
    }

    /// Request a one-shot Space mutation. Named/persistent workspace assignment
    /// will be represented separately in desired policy.
    pub fn move_window_to_space(
        &self,
        window: WindowId,
        space: SpaceId,
    ) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        Ok(vec![Action::MoveWindowToSpace { window, space }])
    }

    /// Focus is inherently transient and must never be stored as desired state,
    /// otherwise periodic reconciliation would continuously steal focus back.
    pub fn focus_window(&self, window: WindowId) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        Ok(vec![Action::FocusWindow { window }])
    }

    /// Request a one-shot layer mutation. Persistent stacking policy belongs in
    /// `DesiredState` later.
    pub fn set_window_layer(
        &self,
        window: WindowId,
        layer: i32,
    ) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        Ok(vec![Action::SetWindowLayer { window, layer }])
    }

    /// Request a one-shot sticky mutation. Persistent pin policy belongs in
    /// `DesiredState` later.
    pub fn set_window_sticky(
        &self,
        window: WindowId,
        sticky: bool,
    ) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        Ok(vec![Action::SetWindowSticky { window, sticky }])
    }

    /// Request a one-shot shadow mutation. Persistent policy belongs in
    /// `DesiredState` later.
    pub fn set_window_shadow(
        &self,
        window: WindowId,
        shadow: bool,
    ) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        Ok(vec![Action::SetWindowShadow { window, shadow }])
    }

    /// Request a one-shot opacity mutation. Persistent policy belongs in
    /// `DesiredState` later.
    pub fn set_window_opacity(
        &self,
        window: WindowId,
        opacity: f64,
        duration_ms: u64,
    ) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        Ok(vec![Action::SetWindowOpacity {
            window,
            opacity,
            duration_ms,
        }])
    }
    /// Toggle Picture-in-Picture for a Window. Mirrors yabai's toggle_window_pip:
    /// the target rect is the window's observed display bounds; the SA decides
    /// scale-in vs scale-out by comparing the window transform to identity.
    pub fn toggle_window_pip(&self, window: WindowId) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        let display_id = self
            .observed
            .windows
            .get(&window)
            .and_then(|w| w.display_id)
            .ok_or(EngineError::WindowNotOnDisplay(window))?;
        let frame = self
            .observed
            .displays
            .get(&display_id)
            .map(|d| d.frame)
            .ok_or(EngineError::WindowNotOnDisplay(window))?;
        Ok(vec![Action::SetWindowScale {
            window,
            x: frame.x as f32,
            y: frame.y as f32,
            w: frame.width as f32,
            h: frame.height as f32,
        }])
    }

    /// Focus a Space. Like window focus this is transient: reconciliation must
    /// never re-steal focus, so the target is never stored in desired state.
    pub fn focus_space(&self, space: SpaceId) -> Result<Vec<Action>, EngineError> {
        self.require_space(space)?;
        Ok(vec![Action::FocusSpace { space }])
    }

    /// Create a new Space on the display of the anchor Space. The new Space's
    /// id is assigned by macOS; the anchor only selects the display. Without
    /// an explicit anchor the currently focused Space is used.
    pub fn create_space(&self, anchor: Option<SpaceId>) -> Result<Vec<Action>, EngineError> {
        let anchor = match anchor {
            Some(anchor) => {
                self.require_space(anchor)?;
                anchor
            }
            None => self
                .observed
                .spaces
                .values()
                .find(|space| space.focused)
                .map(|space| space.id)
                .ok_or(EngineError::NoFocusedSpace)?,
        };
        Ok(vec![Action::CreateSpace { anchor }])
    }

    pub fn destroy_space(&self, space: SpaceId) -> Result<Vec<Action>, EngineError> {
        self.require_space(space)?;
        Ok(vec![Action::DestroySpace { space }])
    }

    /// Move a Space to sit after another Space (SA-only reorder).
    pub fn move_space(&self, space: SpaceId, after: SpaceId) -> Result<Vec<Action>, EngineError> {
        self.require_space(space)?;
        self.require_space(after)?;
        if space == after {
            return Err(EngineError::SameSpace);
        }
        Ok(vec![Action::MoveSpace { space, after }])
    }

    pub fn focus_workspace(&self, name: &str) -> Result<Vec<Action>, EngineError> {
        let space = self
            .workspaces
            .backing_for(name)
            .ok_or_else(|| EngineError::WorkspaceNoBacking(name.to_string()))?;
        self.require_space(space)?;
        Ok(vec![Action::FocusSpace { space }])
    }

    pub fn move_window_to_workspace(
        &self,
        window: WindowId,
        name: &str,
    ) -> Result<Vec<Action>, EngineError> {
        self.require_window(window)?;
        let space = self
            .workspaces
            .backing_for(name)
            .ok_or_else(|| EngineError::WorkspaceNoBacking(name.to_string()))?;
        self.require_space(space)?;
        Ok(vec![Action::MoveWindowToSpace { window, space }])
    }

    pub fn workspace_for_space(&self, space: SpaceId) -> Option<&str> {
        self.workspaces.name_for_space(space)
    }

    pub fn focus_direction(
        &mut self,
        from: WindowId,
        direction: Direction,
    ) -> Result<Vec<Action>, EngineError> {
        let target = self.closest_window_in_direction(from, direction)?;
        self.focus_window(target)
    }

    fn require_window(&self, window: WindowId) -> Result<(), EngineError> {
        if self.observed.windows.contains_key(&window) {
            Ok(())
        } else {
            Err(EngineError::WindowNotFound(window))
        }
    }

    fn require_space(&self, space: SpaceId) -> Result<(), EngineError> {
        if self.observed.spaces.contains_key(&space) {
            Ok(())
        } else {
            Err(EngineError::SpaceNotFound(space))
        }
    }

    fn apply_snapshot(&mut self, mut snapshot: PlatformSnapshot) {
        let generation = self.observed.generation;
        for window in &mut snapshot.windows {
            window.generation = generation;
        }
        for space in &mut snapshot.spaces {
            space.generation = generation;
        }
        for display in &mut snapshot.displays {
            display.generation = generation;
        }

        self.observed.windows = snapshot
            .windows
            .into_iter()
            .map(|window| (window.id, window))
            .collect();
        self.observed.spaces = snapshot
            .spaces
            .into_iter()
            .map(|space| (space.id, space))
            .collect();
        self.observed.displays = snapshot
            .displays
            .into_iter()
            .map(|display| (display.id, display))
            .collect();
        if snapshot.complete {
            self.observed.refresh_required = false;
        }
    }

    fn closest_window_in_direction(
        &self,
        from: WindowId,
        direction: Direction,
    ) -> Result<WindowId, EngineError> {
        let source = self
            .observed
            .windows
            .get(&from)
            .ok_or(EngineError::WindowNotFound(from))?;
        let source_center = source.frame.center();

        self.observed
            .windows
            .values()
            .filter(|candidate| {
                candidate.id != from
                    && !candidate.minimized
                    && candidate.generation == self.observed.generation
            })
            .filter_map(|candidate| {
                let center = candidate.frame.center();
                let dx = center.x - source_center.x;
                let dy = center.y - source_center.y;
                let in_direction = match direction {
                    Direction::North => dy < 0.0,
                    Direction::South => dy > 0.0,
                    Direction::East => dx > 0.0,
                    Direction::West => dx < 0.0,
                };
                in_direction.then_some((candidate.id, dx * dx + dy * dy))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
            .ok_or(EngineError::NoWindowInDirection { from, direction })
    }
}

#[cfg(test)]
mod tests {
    use rovr_config::Config;
    use rovr_types::{
        DisplayId, DisplaySnapshot, PlatformSnapshot, ProcessId, Rect, SpaceId, SpaceSnapshot,
        WindowId, WindowSnapshot,
    };

    use super::*;
    use crate::layout::apply_layout;
    use crate::layout_state::{Axis, Orientation};
    use rovr_types::LayoutKind;

    fn snapshot(windows: Vec<WindowSnapshot>) -> PlatformSnapshot {
        PlatformSnapshot {
            windows,
            spaces: vec![],
            displays: vec![],
            complete: true,
        }
    }

    fn window(id: u32, x: f64, y: f64) -> WindowSnapshot {
        WindowSnapshot {
            id: WindowId(id),
            pid: ProcessId(id as i32),
            app: format!("app-{id}"),
            bundle_id: None,
            title: String::new(),
            frame: Rect {
                x,
                y,
                width: 100.0,
                height: 100.0,
            },
            space_id: None,
            display_id: Some(DisplayId(1)),
            focused: id == 1,
            minimized: false,
            fullscreen: false,
            managed: true,
            generation: 0,
        }
    }

    #[test]
    fn wake_invalidates_cached_windows() {
        let mut engine = Engine::default();
        engine.apply_event(Event::Snapshot(snapshot(vec![window(1, 0.0, 0.0)])));
        assert!(!engine.observed.refresh_required);

        let actions = engine.apply_event(Event::SystemWoke);
        assert!(engine.observed.refresh_required);
        assert_eq!(actions, vec![Action::RefreshAll]);
    }

    #[test]
    fn direction_focus_uses_nearest_candidate() {
        let mut engine = Engine::default();
        engine.apply_event(Event::Snapshot(snapshot(vec![
            window(1, 0.0, 0.0),
            window(2, 150.0, 0.0),
            window(3, 500.0, 0.0),
        ])));

        let actions = engine
            .focus_direction(WindowId(1), Direction::East)
            .unwrap();
        assert_eq!(
            actions,
            vec![Action::FocusWindow {
                window: WindowId(2)
            }]
        );
    }

    /// M3a-3: applying a multi-display snapshot through the engine must produce
    /// exactly one SetWindowFrame action per managed window (5 here).
    #[test]
    fn m3a3_engine_applies_tiling_actions() {
        let snap_window = |id: u32, space: SpaceId, display: DisplayId| WindowSnapshot {
            id: WindowId(id),
            pid: ProcessId(1),
            app: String::new(),
            bundle_id: None,
            title: String::new(),
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            space_id: Some(space),
            display_id: Some(display),
            focused: false,
            minimized: false,
            fullscreen: false,
            managed: true,
            generation: 0,
        };

        let snap = PlatformSnapshot {
            windows: vec![
                snap_window(1, SpaceId(11), DisplayId(1)),
                snap_window(2, SpaceId(11), DisplayId(1)),
                snap_window(3, SpaceId(11), DisplayId(1)),
                snap_window(4, SpaceId(22), DisplayId(2)),
                snap_window(5, SpaceId(22), DisplayId(2)),
            ],
            spaces: vec![
                SpaceSnapshot {
                    id: SpaceId(11),
                    display_id: DisplayId(1),
                    label: None,
                    focused: false,
                    generation: 0,
                    position: 0,
                },
                SpaceSnapshot {
                    id: SpaceId(22),
                    display_id: DisplayId(2),
                    label: None,
                    focused: false,
                    generation: 0,
                    position: 1,
                },
            ],
            displays: vec![
                DisplaySnapshot {
                    id: DisplayId(1),
                    frame: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 1440.0,
                        height: 900.0,
                    },
                    label: None,
                    focused: false,
                    generation: 0,
                },
                DisplaySnapshot {
                    id: DisplayId(2),
                    frame: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 1920.0,
                        height: 1080.0,
                    },
                    label: None,
                    focused: false,
                    generation: 0,
                },
            ],
            complete: true,
        };

        let mut engine = Engine::new(Config::default());
        let actions = engine.apply_event(Event::Snapshot(snap));
        let set_frame_count = actions
            .iter()
            .filter(|a| matches!(a, Action::SetWindowFrame { .. }))
            .count();
        assert_eq!(set_frame_count, 5, "expected 5 SetWindowFrame actions");
    }
    /// M3b: a BSP layout starts in the default vertical split; one rotate flips
    /// the axis to horizontal (top/bottom). Verified via the pure layout engine
    /// fed the engine's own per-Space orientation state.
    #[test]
    fn m3b_rotate_flips_axis() {
        let mut config = Config::default();
        config.general.layout = LayoutKind::Bsp;
        config.general.padding = 0;
        config.general.gap = 0;

        let mut observed = ObservedState::default();
        observed.displays.insert(
            DisplayId(1),
            DisplaySnapshot {
                id: DisplayId(1),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1000.0,
                    height: 1000.0,
                },
                label: None,
                focused: false,
                generation: 0,
            },
        );
        observed.spaces.insert(
            SpaceId(11),
            SpaceSnapshot {
                id: SpaceId(11),
                display_id: DisplayId(1),
                label: None,
                focused: false,
                generation: 0,
                position: 0,
            },
        );
        let mk = |id: u32| WindowSnapshot {
            id: WindowId(id),
            pid: ProcessId(1),
            app: String::new(),
            bundle_id: None,
            title: String::new(),
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            space_id: Some(SpaceId(11)),
            display_id: Some(DisplayId(1)),
            focused: false,
            minimized: false,
            fullscreen: false,
            managed: true,
            generation: 0,
        };
        observed.windows.insert(WindowId(1), mk(1));
        observed.windows.insert(WindowId(2), mk(2));

        let mut engine = Engine::new(config);

        // Default orientation => vertical split => side by side (different x, same y).
        let mut desired = DesiredState::default();
        apply_layout(
            &engine.config,
            &observed,
            &mut desired,
            &mut engine.layouts,
            &engine.workspaces,
            &engine.plugins,
            &ScratchpadState::new(),
        );
        let f1 = desired.windows[&WindowId(1)].frame.unwrap();
        let f2 = desired.windows[&WindowId(2)].frame.unwrap();
        assert!(
            (f1.y - f2.y).abs() < 1.0,
            "default must be side-by-side (same y)"
        );
        assert!(
            (f1.x - f2.x).abs() > 1.0,
            "default must be side-by-side (different x)"
        );

        // One rotate => horizontal axis => top/bottom (same x, different y).
        engine.rotate_layout(SpaceId(11));
        let mut desired2 = DesiredState::default();
        apply_layout(
            &engine.config,
            &observed,
            &mut desired2,
            &mut engine.layouts,
            &engine.workspaces,
            &engine.plugins,
            &ScratchpadState::new(),
        );
        let g1 = desired2.windows[&WindowId(1)].frame.unwrap();
        let g2 = desired2.windows[&WindowId(2)].frame.unwrap();
        assert!(
            (g1.x - g2.x).abs() < 1.0,
            "after rotate must be top/bottom (same x)"
        );
        assert!(
            (g1.y - g2.y).abs() > 1.0,
            "after rotate must be top/bottom (different y)"
        );
    }

    /// M3b: four rotations return to the starting orientation; the per-Space
    /// entry is created on first mutation.
    #[test]
    fn m3b_four_rotations_restore() {
        let mut engine = Engine::new(Config::default());
        assert!(
            !engine.layouts.contains_key(&SpaceId(11)),
            "no entry before rotation"
        );

        engine.rotate_layout(SpaceId(11));
        engine.rotate_layout(SpaceId(11));
        engine.rotate_layout(SpaceId(11));
        engine.rotate_layout(SpaceId(11));

        let state = engine
            .layouts
            .get(&SpaceId(11))
            .expect("entry exists after rotations");
        assert_eq!(
            state.orientation,
            Orientation::default(),
            "4 rotations restore start"
        );
        assert_eq!(state.orientation.axis, Axis::Vertical);
        assert!(!state.orientation.reversed);
    }

    /// M3b: rotating one Space must not affect another Space's orientation.
    #[test]
    fn m3b_per_space_independent() {
        let mut engine = Engine::new(Config::default());
        engine.rotate_layout(SpaceId(11));

        let a = engine.layouts.get(&SpaceId(11)).expect("space 11 mutated");
        let b = engine.layouts.get(&SpaceId(22));
        assert_eq!(
            a.orientation.axis,
            Axis::Horizontal,
            "space 11 rotated to horizontal"
        );
        assert!(b.is_none(), "space 22 untouched (no entry)");
    }
    /// M3e: toggling a scratchpad flips its open/closed state in the engine.
    #[test]
    fn m3e_toggle_scratchpad_flips_open() {
        let mut engine = Engine::new(Config::default());
        assert!(!engine.scratchpads.is_open("term"), "default closed");
        engine.toggle_scratchpad("term");
        assert!(engine.scratchpads.is_open("term"), "open after toggle");
        engine.toggle_scratchpad("term");
        assert!(
            !engine.scratchpads.is_open("term"),
            "closed after second toggle"
        );
    }
    /// M3f: rotated layout + open scratchpad survive a save/load round-trip.
    #[test]
    fn m3f_persist_restore() {
        let config = Config::default();
        let mut engine = Engine::new(config.clone());
        engine.rotate_layout(SpaceId(11)); // -> horizontal axis
        engine.toggle_scratchpad("term"); // -> open

        let path = std::env::temp_dir().join(format!("rovr-m3f-{}.json", std::process::id()));
        engine.save_state(&path).expect("save state");

        let mut restored = Engine::new(config);
        assert!(
            !restored.layouts.contains_key(&SpaceId(11)),
            "fresh engine has no orientation"
        );
        assert!(!restored.scratchpads.is_open("term"), "fresh engine closed");

        restored.load_state(&path).expect("load state");

        let st = restored
            .layouts
            .get(&SpaceId(11))
            .expect("restored orientation present");
        assert_eq!(
            st.orientation.axis,
            Axis::Horizontal,
            "rotated axis persisted"
        );
        assert!(
            restored.scratchpads.is_open("term"),
            "scratchpad open state persisted"
        );

        let _ = std::fs::remove_file(&path);
    }
}
