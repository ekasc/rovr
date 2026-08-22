use std::collections::HashMap;

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
    #[error("no focused window in observed state")]
    NoFocusedWindow,
    #[error("resize would shrink the window below its minimum size")]
    ResizeTooSmall,
}
#[derive(Default)]
pub struct Engine {
    pub config: Config,
    /// Rules with regex matchers compiled once per config load/reload
    /// (blocker 10). Order preserved from config for deterministic evaluation.
    pub rules: Vec<rovr_config::CompiledRule>,
    /// Platform capability bits, set by the daemon at startup. The engine
    /// gates lifecycle actions (e.g. persistent-space creation) on them so a
    /// missing payload does not produce failing actions every cycle.
    pub capabilities: rovr_types::Capabilities,
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
        let rules = config.compile_rules().unwrap_or_default();
        let mut plugins = PluginRegistry::new();
        load_plugins_from_disk(&mut plugins);
        Self {
            plugins,
            config,
            rules,
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
        // Blocker 5: logical workspaces OWN their layout state (BSP tree);
        // raw SpaceId keys only describe the current runtime backing Space.
        // Workspace-owned trees are persisted under the stable workspace name
        // so they survive SpaceId churn; leftover SpaceId-keyed entries belong
        // to unmanaged/non-logical Spaces.
        let mut layouts = self.layouts.clone();
        let mut workspace_layouts = HashMap::new();
        for (name, ws) in &self.workspaces.0 {
            if let Some(sid) = ws.backing_space {
                if let Some(state) = layouts.remove(&sid) {
                    workspace_layouts.insert(name.clone(), state);
                }
            }
        }
        let persisted = PersistedState {
            layouts: layouts
                .iter()
                .map(|(id, state)| (id.0.to_string(), state.clone()))
                .collect(),
            workspace_layouts,
            scratchpads: self.scratchpads.0.clone(),
            workspaces: self.workspaces.0.clone(),
        };
        let json = serde_json::to_string_pretty(&persisted).context("serialize state")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("create state dir")?;
        }
        // Durable replacement: a crash or power loss mid-save must never
        // truncate the live state file. Write to a sibling temp file, fsync
        // it, rename over the target (atomic on the same fs), then fsync the
        // parent directory so the rename's directory-entry update itself
        // survives power loss (macOS honors fsync on directory fds).
        let tmp = path.with_extension("json.tmp");
        {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&tmp).context("create state temp file")?;
            f.write_all(json.as_bytes())
                .context("write state temp file")?;
            f.sync_all().context("sync state temp file")?;
        }
        std::fs::rename(&tmp, path).context("replace state file")?;
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)
                .context("open state dir")?
                .sync_all()
                .context("sync state dir")?;
        }
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
        // Blocker 5: restore workspace-owned layout state onto each
        // workspace's current backing Space.
        for (name, state) in persisted.workspace_layouts {
            if let Some(sid) = self.workspaces.backing_for(&name) {
                self.layouts.insert(sid, state);
            }
        }
        // After load, ensure workspaces still reflect current config (preserve backing where name matches)
        self.workspaces.ensure_from_config(&self.config.workspaces);
        Ok(())
    }

    /// Carry SpaceId-keyed layout state to each workspace's new backing Space
    /// after a remap, so BSP topology/ratios/order stay attached to the LOGICAL
    /// workspace when macOS reassigns SpaceIds (blocker 5).
    fn apply_remap_moves(&mut self, moves: &[crate::workspace::RemapMove]) {
        for mv in moves {
            if let Some(from) = mv.from {
                if from == mv.to {
                    continue;
                }
                if let Some(state) = self.layouts.remove(&from) {
                    self.layouts.insert(mv.to, state);
                    self.flight_recorder.record(
                        "workspace.remap_layout",
                        format!("{}: {:?} -> {:?}", mv.name, from, mv.to),
                    );
                }
            }
        }
    }

    /// Anchor for recreating the lowest-ordinal missing PERSISTENT workspace
    /// (blocker 4): one CreateSpace per snapshot cycle, deterministically.
    /// Returns None when nothing is missing or creation is unsupported.
    fn persistent_creation_anchor(&self) -> Option<SpaceId> {
        if !self.capabilities.create_space {
            return None;
        }
        let first_missing = self.workspaces.ensure_persistent().into_iter().next()?;
        // Prefer an anchor on the workspace's desired display, else focused.
        let desired_display = self
            .workspaces
            .0
            .get(&first_missing)
            .and_then(|w| w.desired_display.clone());
        let anchor = self
            .observed
            .spaces
            .values()
            .find(|s| match &desired_display {
                Some(name) => self
                    .observed
                    .displays
                    .get(&s.display_id)
                    .map(|d| {
                        name == "main" && d.focused
                            || name.parse::<u32>().map(|n| n == d.id.0).unwrap_or(false)
                    })
                    .unwrap_or(false),
                None => false,
            })
            .or_else(|| self.observed.spaces.values().find(|s| s.focused))
            .or_else(|| self.observed.spaces.values().next())
            .map(|s| s.id)?;
        Some(anchor)
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
        self.rules = config.compile_rules().unwrap_or_default();
        self.config = config;
        self.workspaces.ensure_from_config(&self.config.workspaces);
        let moves = self
            .workspaces
            .remap_after_snapshot(&self.observed.spaces, &self.observed.displays);
        self.apply_remap_moves(&moves);
        // Reload WASM plugins from disk (isolated, fuel-limited, version-checked)
        self.plugins = PluginRegistry::new();
        load_plugins_from_disk(&mut self.plugins);
    }

    pub fn apply_event(&mut self, event: Event) -> Vec<Action> {
        self.flight_recorder.record("event", format!("{event:?}"));

        let mut lifecycle_action: Option<Action> = None;
        match event {
            Event::Snapshot(snapshot) => {
                self.apply_snapshot(snapshot);
                let moves = self
                    .workspaces
                    .remap_after_snapshot(&self.observed.spaces, &self.observed.displays);
                // Blocker 5: carry BSP/layout state to each workspace's new
                // backing Space so topology survives SpaceId churn.
                self.apply_remap_moves(&moves);
                // Blocker 4: recreate missing persistent workspaces. One
                // CreateSpace per cycle, lowest ordinal first; the new Space's
                // real id is only learned by OBSERVING the next snapshot, and
                // deterministic remap then binds it to the logical workspace.
                if let Some(anchor) = self.persistent_creation_anchor() {
                    self.flight_recorder.record(
                        "workspace.create_persistent",
                        "missing persistent workspace — requesting CreateSpace",
                    );
                    lifecycle_action = Some(Action::CreateSpace { anchor });
                }
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
            &self.rules,
        );

        let mut actions = reconcile(&self.observed, &self.desired);
        if let Some(lifecycle) = lifecycle_action {
            actions.push(lifecycle);
        }
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
        window: Option<WindowId>,
        layer: i32,
    ) -> Result<Vec<Action>, EngineError> {
        let window = self.resolve_window(window)?;
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
        window: Option<WindowId>,
        name: &str,
    ) -> Result<Vec<Action>, EngineError> {
        let window = self.resolve_window(window)?;
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

    /// The currently focused window, if observation saw one.
    pub fn focused_window(&self) -> Option<WindowId> {
        self.observed
            .windows
            .values()
            .find(|w| w.focused)
            .map(|w| w.id)
    }

    /// Resolve an optional window id: an explicit id must exist; None means
    /// "the focused window". Commands that default to focus use this so
    /// hotkey binds never need a `query focused` subshell.
    pub fn resolve_window(&self, window: Option<WindowId>) -> Result<WindowId, EngineError> {
        match window {
            Some(id) => {
                self.require_window(id)?;
                Ok(id)
            }
            None => self.focused_window().ok_or(EngineError::NoFocusedWindow),
        }
    }

    /// Close a window via its AX close button (None = the focused one).
    pub fn close_window(&mut self, window: Option<WindowId>) -> Result<Vec<Action>, EngineError> {
        let window = self.resolve_window(window)?;
        Ok(vec![Action::CloseWindow { window }])
    }

    /// Toggle the native fullscreen state of a window.
    pub fn toggle_fullscreen(
        &mut self,
        window: Option<WindowId>,
    ) -> Result<Vec<Action>, EngineError> {
        let window = self.resolve_window(window)?;
        Ok(vec![Action::ToggleNativeFullscreen { window }])
    }

    /// Pull a managed window out of the tiling layout, or tile it again. The
    /// flag lives in desired state so it survives restarts and flows through
    /// reconciliation like every other desired property.
    pub fn toggle_float(&mut self, window: Option<WindowId>) -> Result<(), EngineError> {
        let window = self.resolve_window(window)?;
        if !self.observed.windows.contains_key(&window) {
            return Err(EngineError::WindowNotFound(window));
        }
        let target = self.desired.windows.entry(window).or_default();
        target.floating = !target.floating;
        if target.floating {
            // Release from the layout immediately; reconciliation keeps the
            // current frame for floating windows.
            target.frame = None;
        }
        Ok(())
    }

    /// Swap with the nearest neighbor in `direction` (None = focused).
    pub fn swap_windows_direction(
        &mut self,
        direction: Direction,
        window: Option<WindowId>,
    ) -> Result<(), EngineError> {
        let from = self.resolve_window(window)?;
        let target = self.closest_window_in_direction(from, direction)?;
        self.swap_windows(from, target)
    }

    /// Insert at the nearest neighbor's tree position (None = focused).
    pub fn warp_window_direction(
        &mut self,
        direction: Direction,
        window: Option<WindowId>,
    ) -> Result<(), EngineError> {
        let from = self.resolve_window(window)?;
        let target = self.closest_window_in_direction(from, direction)?;
        self.warp_window(from, target)
    }

    /// Move one window edge outward by `delta` points (None = focused).
    /// Absolute-frame composition over the observed frame; BSP ratios are
    /// re-derived from observed geometry on the next layout pass.
    pub fn resize_window_edge(
        &mut self,
        window: Option<WindowId>,
        edge: Direction,
        delta: i32,
    ) -> Result<Vec<Action>, EngineError> {
        let window = self.resolve_window(window)?;
        let snapshot = self
            .observed
            .windows
            .get(&window)
            .ok_or(EngineError::WindowNotFound(window))?;
        let mut frame = snapshot.frame;
        let d = delta as f64;
        match edge {
            Direction::North => {
                frame.y -= d;
                frame.height += d;
            }
            Direction::South => frame.height += d,
            Direction::East => frame.width += d,
            Direction::West => {
                frame.x -= d;
                frame.width += d;
            }
        }
        // Reject degenerate results only: a resize that would leave less
        // than a title-bar-sized sliver is refused, everything else passes.
        const GESTURE_MIN_SIZE: f64 = 40.0;
        if frame.width < GESTURE_MIN_SIZE || frame.height < GESTURE_MIN_SIZE {
            return Err(EngineError::ResizeTooSmall);
        }
        Ok(vec![Action::SetWindowFrame { window, frame }])
    }

    pub fn focus_direction(
        &mut self,
        from: Option<WindowId>,
        direction: Direction,
    ) -> Result<Vec<Action>, EngineError> {
        let from = self.resolve_window(from)?;
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
                    && candidate.minimized == rovr_types::ObservedBool::No
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
            minimized: rovr_types::ObservedBool::No,
            fullscreen: rovr_types::ObservedBool::No,
            managed: rovr_types::ObservedBool::Yes,
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
            .focus_direction(Some(WindowId(1)), Direction::East)
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
            minimized: rovr_types::ObservedBool::No,
            fullscreen: rovr_types::ObservedBool::No,
            managed: rovr_types::ObservedBool::Yes,
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
            minimized: rovr_types::ObservedBool::No,
            fullscreen: rovr_types::ObservedBool::No,
            managed: rovr_types::ObservedBool::Yes,
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
            &[],
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
            &[],
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

    // ---- Blockers 4 + 5: persistent workspace lifecycle and BSP ownership ----

    fn space_snap(id: u64, display: u32, pos: u32, focused: bool) -> SpaceSnapshot {
        SpaceSnapshot {
            id: SpaceId(id),
            display_id: DisplayId(display),
            label: None,
            focused,
            generation: 0,
            position: pos,
        }
    }

    fn display_snap(id: u32) -> DisplaySnapshot {
        DisplaySnapshot {
            id: DisplayId(id),
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 800.0,
            },
            label: None,
            focused: id == 1,
            generation: 0,
        }
    }

    /// Blocker 5 acceptance: a non-trivial BSP tree built for logical
    /// workspace "code" (backed by SpaceId 11) must survive SpaceId churn —
    /// after 11 disappears and 101 takes over, the exact topology, ratios and
    /// window ordering remain attached to "code".
    #[test]
    fn blocker5_bsp_tree_survives_space_id_remap() {
        let mut engine = Engine::new(Config::default());
        engine.capabilities.create_space = false; // no lifecycle noise in this test

        // Observed: space 11 with windows 1..4.
        let snap = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        engine.apply_event(Event::Snapshot(snap));
        // Bind "code" to space 11 (no workspaces configured by default).
        let mut ws = crate::workspace::WorkspaceState::new("code".into(), None, true);
        ws.ordinal = 0;
        ws.backing_space = Some(SpaceId(11));
        engine.workspaces.0.insert("code".into(), ws);

        // Build a non-trivial tree: 4 leaves, custom root ratio, rotated.
        for id in [1u32, 2, 3, 4] {
            engine
                .layouts
                .entry(SpaceId(11))
                .or_default()
                .bsp
                .insert(WindowId(id));
        }
        engine.set_split_ratio(SpaceId(11), 0.62).unwrap();
        engine.rotate_layout(SpaceId(11));

        let area = Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let before = engine.layouts[&SpaceId(11)].bsp.placements(area, 8.0);
        assert_eq!(before.len(), 4, "non-trivial tree has 4 placements");

        // Dock restart: 11 disappears, 101 appears at the same position.
        let snap2 = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(101, 1, 0, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        engine.apply_event(Event::Snapshot(snap2));

        // The tree moved WITH the logical workspace to the new backing.
        assert_eq!(
            engine.workspaces.backing_for("code"),
            Some(SpaceId(101)),
            "code must be remapped onto the new backing space"
        );
        let after = engine.layouts[&SpaceId(101)].bsp.placements(area, 8.0);
        assert_eq!(
            before, after,
            "exact BSP topology, ratios and window order must survive the remap"
        );
        assert!(!engine.layouts.contains_key(&SpaceId(11)));
    }

    /// Blocker 5: persistence round-trip — the BSP tree is stored under the
    /// workspace NAME, so it reloads onto whatever Space now backs "code",
    /// even when the SpaceId changed across daemon restarts.
    #[test]
    fn blocker5_persistence_keys_tree_by_workspace_not_space_id() {
        let config = Config {
            workspaces: vec![rovr_config::WorkspaceConfig {
                name: "code".into(),
                layout: rovr_types::LayoutKind::Bsp,
                display: None,
                persistent: true,
                plugin: None,
            }],
            ..Default::default()
        };
        let mut engine = Engine::new(config.clone());
        engine.capabilities.create_space = false;
        let snap = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        engine.apply_event(Event::Snapshot(snap));
        engine.workspaces.0.get_mut("code").unwrap().backing_space = Some(SpaceId(11));
        for id in [1u32, 2, 3] {
            engine
                .layouts
                .entry(SpaceId(11))
                .or_default()
                .bsp
                .insert(WindowId(id));
        }

        let path = std::env::temp_dir().join(format!("rovr-blocker5-{}.json", std::process::id()));
        engine.save_state(&path).unwrap();

        // Restart: fresh engine (same config) loads state. The persisted
        // backing SpaceId 11 is now STALE (macOS rebuilt Spaces as 101); the
        // next observation must detect that and carry the tree across.
        let mut restored = Engine::new(engine.config.clone());
        restored.load_state(&path).unwrap();
        assert_eq!(
            restored.workspaces.backing_for("code"),
            Some(SpaceId(11)),
            "persisted backing survives the restart"
        );
        assert!(
            restored.layouts.contains_key(&SpaceId(11)),
            "tree restores onto persisted backing first"
        );
        let area = Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let expected = restored.layouts[&SpaceId(11)].bsp.placements(area, 8.0);

        let snap2 = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(101, 1, 0, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        restored.apply_event(Event::Snapshot(snap2));
        let got = restored.layouts[&SpaceId(101)].bsp.placements(area, 8.0);
        assert_eq!(expected, got, "tree survives restart AND remap intact");
        let _ = std::fs::remove_file(&path);
    }

    /// Blocker 4: when a persistent workspace has no backing AND no unclaimed
    /// space exists to claim, the engine emits exactly ONE CreateSpace per
    /// snapshot cycle. Once macOS materializes the new Space (id learned only
    /// by observing), deterministic remap binds it and creation stops.
    #[test]
    fn blocker4_missing_persistent_workspace_is_recreated() {
        let config = Config {
            workspaces: vec![
                rovr_config::WorkspaceConfig {
                    name: "code".into(),
                    layout: rovr_types::LayoutKind::Bsp,
                    display: None,
                    persistent: true,
                    plugin: None,
                },
                rovr_config::WorkspaceConfig {
                    name: "chat".into(),
                    layout: rovr_types::LayoutKind::Bsp,
                    display: None,
                    persistent: true,
                    plugin: None,
                },
            ],
            ..Default::default()
        };
        let mut engine = Engine::new(config);
        engine.capabilities.create_space = true;

        // Cycle 1: only space 11 exists. Ordinal 0 ("code") claims it; "chat"
        // has no backing and there is no unclaimed space left -> CreateSpace.
        let snap1 = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let actions = engine.apply_event(Event::Snapshot(snap1));
        assert_eq!(
            actions,
            vec![Action::CreateSpace { anchor: SpaceId(11) }],
            "missing persistent workspace with nothing claimable must request exactly one CreateSpace"
        );
        assert_eq!(engine.workspaces.backing_for("code"), Some(SpaceId(11)));
        assert_eq!(engine.workspaces.backing_for("chat"), None);

        // Cycle 2: macOS created Space 20 (id only known by OBSERVING).
        let snap2 = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, false), space_snap(20, 1, 1, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let actions2 = engine.apply_event(Event::Snapshot(snap2));
        assert!(
            !actions2
                .iter()
                .any(|a| matches!(a, Action::CreateSpace { .. })),
            "creation must stop once every persistent workspace is backed"
        );
        assert_eq!(
            engine.workspaces.backing_for("chat"),
            Some(SpaceId(20)),
            "the newly created space must be bound to the logical workspace"
        );
    }

    /// Blocker 4: WHICH workspace claims the existing space and which waits
    /// for creation is decided by stable config ordinal order — deterministic.
    #[test]
    fn blocker4_claim_order_follows_ordinal() {
        let config = Config {
            workspaces: vec![
                rovr_config::WorkspaceConfig {
                    name: "chat".into(),
                    layout: rovr_types::LayoutKind::Bsp,
                    display: None,
                    persistent: true,
                    plugin: None,
                },
                rovr_config::WorkspaceConfig {
                    name: "code".into(),
                    layout: rovr_types::LayoutKind::Bsp,
                    display: None,
                    persistent: true,
                    plugin: None,
                },
            ],
            ..Default::default()
        };
        let mut engine = Engine::new(config);
        engine.capabilities.create_space = true;

        let snap1 = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let _ = engine.apply_event(Event::Snapshot(snap1));
        // "chat" has ordinal 0 -> it claims the single existing space.
        assert_eq!(engine.workspaces.backing_for("chat"), Some(SpaceId(11)));
        assert_eq!(engine.workspaces.backing_for("code"), None);

        let snap2 = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, false), space_snap(20, 1, 1, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let _ = engine.apply_event(Event::Snapshot(snap2));
        assert_eq!(
            engine.workspaces.backing_for("code"),
            Some(SpaceId(20)),
            "the created space goes to the lowest-ordinal still-missing workspace"
        );
    }

    /// Full observation context: one display, one focused Space, two tiled
    /// windows side by side (window 1 focused).
    fn snapshot_with_state(engine: &Engine) -> PlatformSnapshot {
        let mut snap = snapshot(engine.observed.windows.values().cloned().collect());
        for w in &mut snap.windows {
            w.generation = engine.observed.generation;
        }
        snap.spaces = engine.observed.spaces.values().cloned().collect();
        snap.displays = engine.observed.displays.values().cloned().collect();
        snap
    }

    fn two_window_engine() -> Engine {
        let mut engine = Engine::default();
        let mut snap = snapshot(vec![window(1, 0.0, 0.0), window(2, 100.0, 0.0)]);
        for w in &mut snap.windows {
            w.space_id = Some(SpaceId(11));
        }
        snap.spaces = vec![SpaceSnapshot {
            id: SpaceId(11),
            display_id: DisplayId(1),
            label: None,
            focused: true,
            generation: 0,
            position: 0,
        }];
        snap.displays = vec![DisplaySnapshot {
            id: DisplayId(1),
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 1440.0,
                height: 900.0,
            },
            label: None,
            focused: true,
            generation: 0,
        }];
        engine.apply_event(Event::Snapshot(snap));
        engine
    }

    /// Directional swap resolves the nearest neighbor via observed geometry
    /// and swaps the BSP positions of the focused (default) window.
    #[test]
    fn directional_swap_swaps_focused_with_neighbor() {
        let mut engine = two_window_engine();
        assert_eq!(engine.focused_window(), Some(WindowId(1)));
        // Seed the BSP by applying a layout pass first (as steady state does).
        let _ = engine.apply_event(Event::Snapshot(snapshot_with_state(&engine)));
        engine
            .swap_windows_direction(Direction::East, None)
            .unwrap();
        // After the swap the tree positions exchanged: window 2 now sits in
        // window 1's original node. Verify via a fresh layout pass.
        let mut desired = crate::state::DesiredState::default();
        apply_layout(
            &engine.config,
            &engine.observed,
            &mut desired,
            &mut engine.layouts,
            &engine.workspaces,
            &engine.plugins,
            &crate::layout_state::ScratchpadState::new(),
            &[],
        );
        let f1 = desired.windows[&WindowId(1)].frame.unwrap();
        assert!(f1.x > 50.0, "window 1 must now be on the right: {f1:?}");
    }

    /// Toggling float removes the window from tiling; toggling again
    /// re-tiles it. The flag lives in desired state (persists).
    #[test]
    fn toggle_float_excludes_window_from_tiling() {
        let mut engine = two_window_engine();
        assert!(engine.desired.windows[&WindowId(2)].clone().frame.is_some());

        engine.toggle_float(Some(WindowId(2))).unwrap();
        // Drive a fresh snapshot through the FULL pipeline so apply_layout
        // runs against the engine's own desired state (where the flag lives).
        let snap = snapshot_with_state(&engine);
        engine.apply_event(Event::Snapshot(snap));
        let target = engine.desired.windows[&WindowId(2)].clone();
        assert!(target.floating);
        assert!(
            target.frame.is_none(),
            "floated window must not receive a tile frame"
        );
        assert!(engine.desired.windows[&WindowId(1)].clone().frame.is_some());

        // Toggle back: re-enters the layout with a tile frame.
        engine.toggle_float(Some(WindowId(2))).unwrap();
        let snap = snapshot_with_state(&engine);
        engine.apply_event(Event::Snapshot(snap));
        assert!(
            engine.desired.windows[&WindowId(2)].clone().frame.is_some(),
            "un-floated window must be tiled again"
        );
    }

    /// Edge resize composes over the observed frame; shrinking below the
    /// minimum is rejected rather than producing a broken frame.
    #[test]
    fn edge_resize_adjusts_frame_and_rejects_too_small() {
        let mut engine = Engine::default();
        engine.apply_event(Event::Snapshot(snapshot(vec![window(3, 10.0, 10.0)])));

        let actions = engine
            .resize_window_edge(Some(WindowId(3)), Direction::East, 50)
            .unwrap();
        match actions.as_slice() {
            [Action::SetWindowFrame {
                window: WindowId(3),
                frame,
            }] => {
                assert!((frame.width - 150.0).abs() < f64::EPSILON);
                assert!((frame.x - 10.0).abs() < f64::EPSILON);
            }
            other => panic!("unexpected actions {other:?}"),
        }

        // West composes over the OBSERVED frame again (the SetWindowFrame
        // above is an action, not an observation): x moves outward 20.
        let actions = engine
            .resize_window_edge(Some(WindowId(3)), Direction::West, 20)
            .unwrap();
        match actions.as_slice() {
            [Action::SetWindowFrame { frame, .. }] => {
                assert!((frame.x - (-10.0)).abs() < f64::EPSILON);
                assert!((frame.width - 120.0).abs() < f64::EPSILON);
            }
            other => panic!("unexpected actions {other:?}"),
        }

        // Shrinking below minimum errors without emitting actions.
        let result = engine.resize_window_edge(Some(WindowId(3)), Direction::North, -10_000);
        assert!(matches!(result, Err(EngineError::ResizeTooSmall)));
    }

    /// resolve_window falls back to the focused window and validates ids.
    #[test]
    fn resolve_window_uses_focus_when_unspecified() {
        let mut engine = Engine::default();
        engine.apply_event(Event::Snapshot(snapshot(vec![
            window(1, 0.0, 0.0),
            window(2, 100.0, 0.0),
        ])));
        assert!(matches!(engine.resolve_window(None), Ok(WindowId(1))));
        assert!(matches!(
            engine.resolve_window(Some(WindowId(2))),
            Ok(WindowId(2))
        ));
        assert!(matches!(
            engine.resolve_window(Some(WindowId(99))),
            Err(EngineError::WindowNotFound(WindowId(99)))
        ));

        // No focused window in state: None must error, not guess.
        let mut empty = Engine::default();
        empty.apply_event(Event::Snapshot(snapshot(vec![])));
        assert!(matches!(
            empty.resolve_window(None),
            Err(EngineError::NoFocusedWindow)
        ));
    }
}
