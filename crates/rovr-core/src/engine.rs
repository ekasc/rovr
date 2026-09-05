use std::collections::{HashMap, VecDeque};

use rovr_config::Config;
use rovr_types::{
    Direction, DisplayId, PlatformSnapshot, Rect, SpaceId, SpaceSnapshot, WindowId, WindowSnapshot,
};
use std::path::Path;
use thiserror::Error;

use crate::persistence::PersistedState;
use anyhow::{Context, Result};

use crate::layout_state::{Axis, Layouts, ScratchpadState};
use crate::workspace::{WorkspaceBacking, WorkspaceRegistry};
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
    #[error("display {0} not found")]
    DisplayNotFound(String),
    #[error("display has no other space to step to")]
    NoAdjacentSpace,
    #[error("space step command delta must be +1 or -1")]
    InvalidSpaceStep,
    #[error("resize would shrink the window below its minimum size")]
    ResizeTooSmall,
    #[error("exit native fullscreen before moving windows into or out of this workspace")]
    NativeFullscreenMove,
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
    space_cursors: HashMap<DisplayId, (SpaceId, bool)>,
    /// Session-scoped gap/padding collapse (space toggle-insets). Not
    /// persisted: a daemon restart restores configured insets.
    pub insets_off: bool,
    /// i3-style dynamic spawn: workspace awaiting its backing Space to
    /// materialize so the queued FocusSpace can fire (alt-N on an unknown
    /// name registers the workspace and creates its Space). Now a queue so
    /// rapid alt-2 -> alt-3 does not overwrite.
    pending_workspace_focus: VecDeque<(String, u32)>,
    /// Window waiting to be moved once `move-to-workspace`'s target Space
    /// materializes (same dynamic spawn flow as above). Queue for FIFO.
    pending_workspace_move: VecDeque<(WindowId, String)>,
    /// Dynamic spawns awaiting their created Space. `before` snapshots the
    /// SpaceIds observed when the CreateSpace was requested, so the binding
    /// step can identify the NEW Space and never adopt a pre-existing one.
    /// Serialized: only one in-flight per global at a time to avoid swap.
    awaited_creations: VecDeque<AwaitedCreation>,
    /// Queue for creations that arrived while another creation was in-flight.
    /// Serialized FIFO ensures alt-2 -> alt-3 does not swap identities.
    pending_creations: VecDeque<PendingCreationRequest>,
    /// Windows with a CloseWindow in flight. Excluded from placement so the
    /// remaining windows retile instantly, but kept in BSP until observation
    /// confirms destruction (prevents churn if close is cancelled).
    pending_close: std::collections::HashSet<WindowId>,
    /// Spaces with a DestroySpace in flight. Workspace/layout kept until a
    /// later complete snapshot proves the Space is gone.
    pending_destroy: HashMap<SpaceId, String>,
    /// Post-spawn protection: recently spawned/moved-into dynamic workspaces
    /// are immune to the empty-space sweep until this instant, because focus
    /// and window arrival land asynchronously (often one observe-tick later).
    dynamic_grace: HashMap<String, std::time::Instant>,
    external_spaces: HashMap<SpaceId, std::time::Instant>,
    topology_destroy: Option<SpaceId>,
    /// Only an explicit native mutation may reserve slot handles across observations.
    placement: Option<SpacePlacement>,
    topology_initialized: bool,
    restored_workspace_layouts: HashMap<String, crate::layout_state::LayoutState>,
    /// Last observed native Space per window (complete snapshots only).
    /// Bridges single-tick observation gaps during native fullscreen
    /// transitions: if window W vanishes for one tick between leaving its
    /// restore Space A and appearing on fullscreen Space F, entry detection
    /// and empty-space GC still know W belongs to A. Entries expire after
    /// `WINDOW_LAST_SPACE_TTL` so a genuinely closed window only delays GC
    /// by a bounded amount. Updated only from complete snapshots — partial
    /// snapshots must never rewrite lifecycle memory.
    window_last_space: HashMap<WindowId, (SpaceId, std::time::Instant)>,
}

struct SpacePlacement {
    order: Vec<SpaceId>,
    retry_at: std::time::Instant,
    deadline: std::time::Instant,
}

struct AwaitedCreation {
    name: String,
    display: Option<DisplayId>,
    before: Vec<SpaceId>,
    observation: CreationObservation,
}

enum CreationObservation {
    Awaiting {
        deadline: std::time::Instant,
    },
    /// The call may have succeeded. Keep the reservation, report uncertainty,
    /// and never retry a non-idempotent CreateSpace against an unchanged snapshot.
    Uncertain,
}

struct PendingCreationRequest {
    name: String,
}

/// How long a window's last observed Space stays authoritative after the
/// window stops being observed there. Covers a small number of missed
/// observation ticks (the periodic interval alone can be several seconds);
/// a genuinely closed window therefore delays empty-space GC by at most
/// this bound before its old Space becomes collectable again.
const WINDOW_LAST_SPACE_TTL: std::time::Duration = std::time::Duration::from_secs(10);

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
        let mut workspace_layouts = self.restored_workspace_layouts.clone();
        for (name, ws) in &self.workspaces.0 {
            if let Some(sid) = ws
                .backing
                .and_then(WorkspaceBacking::restore_space)
                .or(ws.active_space())
            {
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
        self.workspaces = crate::workspace::WorkspaceRegistry(persisted.workspaces, false);
        self.restored_workspace_layouts = persisted.workspace_layouts;
        // After load, ensure workspaces still reflect current config (preserve backing where name matches)
        self.workspaces.ensure_from_config(&self.config.workspaces);
        Ok(())
    }

    /// Carry SpaceId-keyed layout state to each workspace's new backing Space
    /// after a remap, so BSP topology/ratios/order stay attached to the LOGICAL
    /// workspace when macOS reassigns SpaceIds (blocker 5). When a move has
    /// `from == to`, the workspace lost its backing with no replacement slot:
    /// the native Space is gone (observed check above), so park the tree under
    /// the LOGICAL name for a future backing instead of dropping
    /// workspace-owned state. A Mission Control reorder (both handles
    /// observed) migrates nothing — live layouts stay with their native
    /// windows and the reorder is never reversed.
    fn apply_remap_moves(&mut self, moves: &[crate::workspace::RemapMove]) {
        for mv in moves {
            if let Some(from) = mv.from {
                if self.observed.spaces.contains_key(&from) {
                    continue;
                }
                if from == mv.to {
                    if let Some(state) = self.layouts.remove(&from) {
                        self.restored_workspace_layouts
                            .insert(mv.name.clone(), state);
                        self.flight_recorder.record(
                            "workspace.layout_parked",
                            format!(
                                "{}: backing {:?} lost with no slot; tree parked",
                                mv.name, from
                            ),
                        );
                    }
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

    /// The Space a new dynamic workspace's backing should be created on:
    /// the focused Space (its display), else any observed Space.
    fn creation_anchor(&self) -> Result<SpaceId, EngineError> {
        self.observed
            .spaces
            .values()
            .find(|s| s.focused)
            .or_else(|| self.observed.spaces.values().next())
            .map(|s| s.id)
            .ok_or(EngineError::NoFocusedSpace)
    }

    fn try_start_next_pending_creation(&mut self) -> Option<Action> {
        // Creation serializes against destruction and placement: every
        // destructive topology mutation must be observed before another
        // topology mutation is issued, and a pending new Space must be
        // inserted into its logical slot before the next one is requested.
        // (Pending destroys always resolve within a cycle or two — success
        // confirms via observation, failure releases via note_batch_failed —
        // so this gate cannot wedge.)
        if !self.awaited_creations.is_empty()
            || self.placement.is_some()
            || !self.pending_destroy.is_empty()
            || self.topology_destroy.is_some()
        {
            return None;
        }
        let req = self.pending_creations.front()?;
        let anchor = if self
            .workspaces
            .0
            .get(&req.name)
            .is_some_and(|w| w.persistent)
        {
            self.persistent_creation_anchor()
        } else {
            self.creation_anchor().ok()
        }?;
        let req = self.pending_creations.pop_front()?;
        if !self.workspaces.0.contains_key(&req.name)
            || self.workspaces.backing_for(&req.name).is_some()
        {
            return self.try_start_next_pending_creation();
        }
        self.awaited_creations.push_back(AwaitedCreation {
            name: req.name,
            display: self.observed.spaces.get(&anchor).map(|s| s.display_id),
            before: self.observed.spaces.keys().copied().collect(),
            observation: CreationObservation::Awaiting {
                deadline: std::time::Instant::now() + std::time::Duration::from_secs(10),
            },
        });
        Some(Action::CreateSpace { anchor })
    }

    fn request_workspace_creation(&mut self, name: &str) -> Result<Vec<Action>, EngineError> {
        self.creation_anchor()?;
        self.ensure_dynamic_workspace(name);
        if !self.awaited_creations.iter().any(|r| r.name == name)
            && !self.pending_creations.iter().any(|r| r.name == name)
        {
            self.pending_creations
                .push_back(PendingCreationRequest { name: name.into() });
        }
        Ok(self.try_start_next_pending_creation().into_iter().collect())
    }

    pub fn mark_pending_close(&mut self, window: WindowId) {
        self.pending_close.insert(window);
    }

    pub fn clear_pending_close(&mut self, window: WindowId) {
        self.pending_close.remove(&window);
    }

    pub fn clear_pending_destroy(&mut self, space: SpaceId) {
        self.pending_destroy.remove(&space);
    }

    pub fn clear_topology_destroy(&mut self) {
        self.topology_destroy = None;
    }

    /// Explicit platform evidence that the in-flight CreateSpace did not
    /// happen: the daemon's serial execution either saw the CreateSpace
    /// action itself fail, or saw an earlier action in the same batch fail
    /// before the (always last) CreateSpace could run. Both make retry safe —
    /// no duplicate Space can materialize — so the single-flight reservation
    /// is released. Pending focus/move intents are kept: the next Alt+N (or
    /// the next snapshot's persistent-recreation pass) re-queues the creation
    /// against fresh topology with a fresh `before` set.
    ///
    /// Never called on success or timeout, only on explicit failure, so a
    /// delayed successful creation keeps its reservation and can never be
    /// duplicated by this path. Never called for batches without a
    /// CreateSpace.
    pub fn note_create_space_failed(&mut self) {
        if let Some(awaited) = self.awaited_creations.pop_front() {
            self.flight_recorder.record(
                "workspace.creation_failed",
                format!(
                    "workspace {}: CreateSpace provably did not land; reservation released",
                    awaited.name
                ),
            );
        }
    }

    /// Explicit platform evidence that a batch of topology mutations failed
    /// partway. `unexecuted` is the failed action and everything after it —
    /// exactly the actions that never landed. Their reservations are released
    /// so lifecycle cannot wedge: an unexecuted-or-failed CreateSpace
    /// releases its single-flight slot (see `note_create_space_failed`), and
    /// unexecuted destroys release theirs so the next snapshot recomputes
    /// them from fresh observation instead of waiting forever on a
    /// confirmation that will never arrive. Actions before the failure did
    /// run and keep their reservations until observation confirms them —
    /// successful destroys stand until the Space is proven gone (prevents
    /// double destruction).
    pub fn note_batch_failed(&mut self, unexecuted: &[Action]) {
        if unexecuted
            .iter()
            .any(|a| matches!(a, Action::CreateSpace { .. }))
        {
            self.note_create_space_failed();
        }
        let unexecuted_destroys: Vec<SpaceId> = unexecuted
            .iter()
            .filter_map(|a| match a {
                Action::DestroySpace { space } => Some(*space),
                _ => None,
            })
            .collect();
        if !unexecuted_destroys.is_empty() {
            let mut released = false;
            for space in unexecuted_destroys {
                released |= self.pending_destroy.remove(&space).is_some();
            }
            if self.topology_destroy.is_some() {
                self.topology_destroy = None;
                released = true;
            }
            if released {
                self.flight_recorder.record(
                    "workspace.destroy_failed",
                    "destroy batch failed; unexecuted destroys released for recompute",
                );
            }
        }
    }

    /// Forget one logical workspace and every piece of state owned by it:
    /// registry entry, SpaceId-keyed BSP tree on its backing Space,
    /// name-keyed parked layout, post-spawn grace, and queued focus/move
    /// intents. Workspaces that truly die must not leak layout state into
    /// whatever native Space they last backed.
    fn remove_workspace(&mut self, name: &str) {
        if let Some(ws) = self.workspaces.0.remove(name) {
            if let Some(sid) = ws.active_space() {
                self.layouts.remove(&sid);
            }
        }
        self.restored_workspace_layouts.remove(name);
        self.dynamic_grace.remove(name);
        self.pending_workspace_focus.retain(|(n, _)| n != name);
        self.pending_workspace_move.retain(|(_, n)| n != name);
    }

    /// Clear a dead native binding, parking the workspace's BSP tree under its
    /// logical name so a later respawn/recreation reattaches it. Never leaves
    /// a SpaceId-keyed orphan for a Space that no longer exists.
    fn clear_stale_backing(&mut self, name: &str) {
        let old = self.workspaces.0.get(name).and_then(|ws| ws.active_space());
        if let Some(old) = old {
            if let Some(state) = self.layouts.remove(&old) {
                self.restored_workspace_layouts
                    .insert(name.to_string(), state);
                self.flight_recorder.record(
                    "workspace.layout_parked",
                    format!("{name}: stale backing {old:?} cleared; tree parked"),
                );
            }
        }
        if let Some(ws) = self.workspaces.0.get_mut(name) {
            ws.backing = None;
        }
    }

    /// Register `name` as an i3-style dynamic (non-persistent) workspace if it
    /// is unknown. Returns false when a PERSISTENT workspace of that name has
    /// no backing — its recreation belongs to the snapshot lifecycle, and a
    /// manual CreateSpace would fight it.
    fn ensure_dynamic_workspace(&mut self, name: &str) -> bool {
        match self.workspaces.0.get(name) {
            Some(_) => true,
            None => {
                let ordinal = self
                    .workspaces
                    .0
                    .values()
                    .map(|w| w.ordinal.saturating_add(1))
                    .max()
                    .unwrap_or(0);
                let mut state =
                    crate::workspace::WorkspaceState::new(name.to_string(), None, false);
                state.ordinal = ordinal;
                state.dynamic = true;
                self.workspaces.0.insert(name.to_string(), state);
                true
            }
        }
    }

    /// Bind each awaited dynamic spawn to the Space macOS created for it: an
    /// unclaimed Space on the anchor display that was NOT observed when the
    /// CreateSpace was requested. Pre-existing Spaces are never adopted.
    /// Awaited entries whose workspace disappeared (config reload) are dropped.
    fn bind_awaited_dynamic_spaces(&mut self) {
        if self.awaited_creations.is_empty() {
            return;
        }
        let mut still_awaited: VecDeque<AwaitedCreation> = VecDeque::new();
        // Deterministic: sort candidates by position, id so same-display creations don't swap.
        for mut awaited in std::mem::take(&mut self.awaited_creations) {
            if matches!(awaited.observation, CreationObservation::Awaiting { deadline } if std::time::Instant::now() >= deadline)
            {
                self.flight_recorder.record("workspace.creation_unconfirmed", format!("workspace {}: creation deadline elapsed; retaining single-flight reservation", awaited.name));
                awaited.observation = CreationObservation::Uncertain;
            }
            let AwaitedCreation {
                name,
                display: desired_display,
                before,
                ..
            } = &awaited;
            let Some(ws) = self.workspaces.0.get(name) else {
                continue; // workspace gone (config reload)
            };
            if ws.active_space().is_some() {
                continue; // already bound
            }
            let claimed: std::collections::HashSet<SpaceId> = self
                .workspaces
                .0
                .values()
                .filter_map(|w| w.active_space())
                .collect();
            let mut candidates: Vec<&SpaceSnapshot> = self
                .observed
                .spaces
                .values()
                .filter(|s| {
                    !s.is_fullscreen
                        && !s.is_system
                        && !claimed.contains(&s.id)
                        && !before.contains(&s.id)
                })
                .collect();
            candidates.sort_by_key(|s| (s.position, s.id));
            let pick = candidates
                .into_iter()
                .find(|s| match desired_display {
                    Some(d) => s.display_id == *d,
                    None => true,
                })
                .map(|s| s.id);
            match pick {
                Some(sid) => {
                    let ws = self.workspaces.0.get_mut(name).expect("checked above");
                    ws.backing = Some(crate::workspace::WorkspaceBacking::Normal { space: sid });
                    ws.last_position = self.observed.spaces.get(&sid).map(|s| s.position);
                    self.external_spaces.remove(&sid);
                    self.dynamic_grace.insert(
                        name.clone(),
                        std::time::Instant::now() + self.dynamic_grace_duration(),
                    );
                    self.reserve_logical_order();
                    self.flight_recorder.record(
                        "workspace.dynamic_bound",
                        format!("workspace {name} bound to created space {sid:?}"),
                    );
                }
                None => still_awaited.push_back(awaited),
            }
        }
        self.awaited_creations = still_awaited;
    }

    /// Grace window after a dynamic spawn before the empty-space sweep may
    /// judge it. Covers async focus dispatch and deferred window arrival.
    fn dynamic_grace_duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs(2)
    }

    fn reserve_logical_order(&mut self) {
        let order: Vec<_> = self
            .workspaces
            .ordered_names()
            .iter()
            .filter_map(|name| match self.workspaces.0[name].backing {
                Some(WorkspaceBacking::Normal { space }) => Some(space),
                _ => None,
            })
            .collect();
        let now = std::time::Instant::now();
        self.placement = Some(SpacePlacement {
            order,
            retry_at: now,
            deadline: now + std::time::Duration::from_secs(10),
        });
    }

    /// Move ordinary Spaces only. Compare observation before releasing the reservation.
    /// The API supports move-after, so insertion at the first slot moves its old
    /// occupant after the new first Space; subsequent passes settle the suffix.
    fn reconcile_placement(&mut self) -> Option<Action> {
        let placement = self.placement.as_ref()?;
        let mut desired_by_display: std::collections::BTreeMap<DisplayId, Vec<SpaceId>> =
            Default::default();
        for id in &placement.order {
            if let Some(space) = self.observed.spaces.get(id) {
                desired_by_display
                    .entry(space.display_id)
                    .or_default()
                    .push(*id);
            }
        }
        let mut action = None;
        for desired in desired_by_display.values() {
            let mut actual = desired.clone();
            actual.sort_by_key(|id| (self.observed.spaces[id].position, *id));
            if actual == *desired {
                continue;
            }
            let i = actual
                .iter()
                .zip(desired)
                .position(|(a, b)| a != b)
                .unwrap();
            action = Some(if i == 0 {
                Action::MoveSpace {
                    space: actual[0],
                    after: desired[0],
                }
            } else {
                Action::MoveSpace {
                    space: desired[i],
                    after: desired[i - 1],
                }
            });
            break;
        }
        if action.is_none() {
            self.placement = None;
            self.flight_recorder.record(
                "workspace.placement_settled",
                "native order matches the logical strip; reservation released",
            );
            return None;
        }
        let now = std::time::Instant::now();
        if !self.capabilities.reorder_space || now >= placement.deadline {
            self.flight_recorder.record(
                "workspace.placement_failed",
                "native slot insertion could not be verified; accepting observed order",
            );
            self.placement = None;
            return None;
        }
        if now < placement.retry_at {
            return None;
        }
        self.placement.as_mut().unwrap().retry_at = now + std::time::Duration::from_millis(500);
        action
    }

    fn reconcile_fullscreen(&mut self, previous: &HashMap<WindowId, WindowSnapshot>) {
        let mut restore_order = false;
        // Borrow the last-seen map immutably up front so the workspace loop
        // below can consult multi-tick memory while mutating bindings.
        let now = std::time::Instant::now();
        let restore_grace = self.dynamic_grace_duration();
        // Fullscreen Spaces already backing a logical workspace can never be
        // claimed as somebody else's substitution (prevents double-binding
        // when a window transitions between fullscreen Spaces).
        let owned_fullscreen: std::collections::HashSet<SpaceId> = self
            .workspaces
            .0
            .values()
            .filter_map(|w| w.active_space())
            .filter(|s| {
                self.observed
                    .spaces
                    .get(s)
                    .is_some_and(|space| space.is_fullscreen && !space.is_system)
            })
            .collect();
        for (name, ws) in self.workspaces.0.iter_mut() {
            match ws.backing {
                Some(WorkspaceBacking::FullscreenReplacement {
                    fullscreen_space,
                    restore_space,
                    window,
                }) => {
                    let alive = self.observed.windows.contains_key(&window);
                    let fullscreen_exists = self
                        .observed
                        .spaces
                        .get(&fullscreen_space)
                        .is_some_and(|s| s.is_fullscreen && !s.is_system);
                    if !alive || !fullscreen_exists {
                        ws.backing = self
                            .observed
                            .spaces
                            .get(&restore_space)
                            .filter(|s| !s.is_fullscreen && !s.is_system)
                            .map(|_| WorkspaceBacking::Normal {
                                space: restore_space,
                            });
                        // The restored window lands back on its Space
                        // asynchronously (often one observe-tick after the
                        // fullscreen Space vanishes). Without grace the
                        // just-restored, still-empty Space is immediately
                        // eligible for empty-workspace GC and can be
                        // destroyed before its window is observed on it.
                        self.dynamic_grace.insert(name.clone(), now + restore_grace);
                        restore_order = true;
                    }
                }
                Some(WorkspaceBacking::Normal {
                    space: restore_space,
                }) => {
                    let owner = self
                        .observed
                        .windows
                        .values()
                        .filter(|w| {
                            // The window must currently sit on a real
                            // (non-system) fullscreen Space nobody owns yet.
                            let on_unowned_fullscreen = w
                                .space_id
                                .and_then(|s| self.observed.spaces.get(&s))
                                .is_some_and(|s| {
                                    s.is_fullscreen
                                        && !s.is_system
                                        && !owned_fullscreen.contains(&s.id)
                                });
                            if !on_unowned_fullscreen {
                                return false;
                            }
                            // ...and it must have come from this workspace's
                            // restore Space. The previous tick is authoritative,
                            // but a window that vanishes for exactly the ticks
                            // between leaving A and appearing on F has no
                            // previous record — fall back to bounded last-seen
                            // memory so the race still enters substitution
                            // instead of leaving A to be garbage-collected.
                            match previous.get(&w.id).and_then(|old| old.space_id) {
                                Some(prev_space) => prev_space == restore_space,
                                None => self.window_last_space.get(&w.id).is_some_and(|(s, t)| {
                                    *s == restore_space
                                        && now.duration_since(*t) < WINDOW_LAST_SPACE_TTL
                                }),
                            }
                        })
                        .min_by_key(|w| w.id);
                    if let Some(window) = owner {
                        ws.backing = Some(WorkspaceBacking::FullscreenReplacement {
                            fullscreen_space: window.space_id.unwrap(),
                            restore_space,
                            window: window.id,
                        });
                    }
                }
                None => {}
            }
        }
        if restore_order {
            self.reserve_logical_order();
        }
    }

    /// Record the currently observed window->space associations for
    /// single-tick-gap bridging (see `window_last_space`). Complete
    /// snapshots only; entries for vanished windows expire by TTL instead of
    /// being dropped immediately so a one-tick disappearance still protects
    /// and re-binds correctly on the next tick.
    fn note_window_spaces(&mut self) {
        let now = std::time::Instant::now();
        for window in self.observed.windows.values() {
            if let Some(space) = window.space_id {
                self.window_last_space.insert(window.id, (space, now));
            }
        }
        self.window_last_space.retain(|id, (_, t)| {
            self.observed.windows.contains_key(id) || now.duration_since(*t) < WINDOW_LAST_SPACE_TTL
        });
    }

    fn remap_slots(&mut self) -> Vec<crate::workspace::RemapMove> {
        if self.placement.is_some() {
            return vec![];
        }
        // Unmaterialized workspaces do not consume a native slot.
        let pending: Vec<_> = self
            .pending_creations
            .iter()
            .map(|r| r.name.clone())
            .chain(self.awaited_creations.iter().map(|r| r.name.clone()))
            .collect();
        let held: Vec<_> = pending
            .iter()
            .filter_map(|name| self.workspaces.0.remove(name))
            .collect();
        let spaces = self
            .observed
            .spaces
            .iter()
            .filter(|(id, _)| !self.external_spaces.contains_key(id))
            .map(|(id, s)| (*id, s.clone()))
            .collect();
        let moves = self
            .workspaces
            .remap_after_snapshot(&spaces, &self.observed.displays);
        for ws in held {
            self.workspaces.0.insert(ws.name.clone(), ws);
        }
        moves
    }

    fn observe_external_spaces(&mut self) {
        let now = std::time::Instant::now();
        self.external_spaces.retain(|id, _| {
            self.observed.spaces.contains_key(id)
                && !self.workspaces.0.values().any(|w| {
                    w.active_space() == Some(*id)
                        || w.backing.and_then(WorkspaceBacking::restore_space) == Some(*id)
                })
        });
        if !self.topology_initialized {
            self.topology_initialized = true;
            return;
        }
        for space in self.observed.spaces.values() {
            if space.is_fullscreen || space.is_system {
                continue;
            }
            let known = self.workspaces.0.values().any(|w| {
                w.active_space() == Some(space.id)
                    || w.backing.and_then(WorkspaceBacking::restore_space) == Some(space.id)
            });
            if !known {
                self.external_spaces
                    .entry(space.id)
                    .or_insert(now + std::time::Duration::from_secs(3));
            }
        }
        // Focus or occupancy adopts an external Space as a new logical slot.
        let mut adopt: Vec<_> = self
            .external_spaces
            .keys()
            .copied()
            .filter(|id| {
                self.observed.spaces[id].focused
                    || self
                        .observed
                        .windows
                        .values()
                        .any(|w| w.space_id == Some(*id))
            })
            .collect();
        adopt.sort_by_key(|id| (self.observed.spaces[id].position, *id));
        if !self.awaited_creations.is_empty() {
            return;
        }
        for id in adopt {
            // Positional adoption: a manually created Space appended at the
            // end of Mission Control order extends the logical strip, so it
            // takes the next number AFTER the maximum (1,2,5 + D => 6), never
            // the first gap (3). Reusing a gap would shuffle the existing
            // binding (C would move from 5 to 3) on the next positional
            // remap; extending preserves it.
            let name = {
                let mut next = self
                    .workspaces
                    .0
                    .keys()
                    .filter_map(|n| n.parse::<u32>().ok())
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1)
                    .max(1);
                while self.workspaces.0.contains_key(&next.to_string()) {
                    next = next.saturating_add(1);
                    if next == u32::MAX {
                        break;
                    }
                }
                if self.workspaces.0.contains_key(&next.to_string()) {
                    continue; // numeric namespace exhausted; leave the Space in grace
                }
                next.to_string()
            };
            self.ensure_dynamic_workspace(&name);
            self.workspaces.0.get_mut(&name).unwrap().backing =
                Some(WorkspaceBacking::Normal { space: id });
            self.external_spaces.remove(&id);
        }
    }

    /// Destroy non-persistent workspaces whose backing Space holds no windows
    /// and is not focused (i3 semantics: empty workspaces die when you leave).
    /// Freshly (re)bound workspaces from this snapshot's remap, pending spawn
    /// targets, and workspaces inside their post-spawn grace window are
    /// exempt — their focus/window may not have landed yet.
    fn destroy_empty_dynamic_workspaces(
        &mut self,
        moves: &[crate::workspace::RemapMove],
    ) -> Vec<Action> {
        if !self.capabilities.destroy_space
            || self.placement.is_some()
            || !self.awaited_creations.is_empty()
            || !self.pending_destroy.is_empty()
            || self.topology_destroy.is_some()
        {
            return Vec::new();
        }
        let freshly_bound: std::collections::HashSet<&str> =
            moves.iter().map(|m| m.name.as_str()).collect();
        let pending_focus_names: std::collections::HashSet<&str> = self
            .pending_workspace_focus
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        let pending_move_names: std::collections::HashSet<&str> = self
            .pending_workspace_move
            .iter()
            .map(|(_, n)| n.as_str())
            .collect();
        let mut doomed: Vec<(String, SpaceId)> = Vec::new();
        let mut stale_dynamic: Vec<String> = Vec::new();
        for (name, ws) in &self.workspaces.0 {
            if ws.persistent
                || matches!(
                    ws.backing,
                    Some(WorkspaceBacking::FullscreenReplacement { .. })
                )
            {
                continue;
            }
            let Some(sid) = ws.active_space() else {
                continue;
            };
            if freshly_bound.contains(name.as_str())
                || pending_focus_names.contains(name.as_str())
                || pending_move_names.contains(name.as_str())
                || self
                    .dynamic_grace
                    .get(name)
                    .is_some_and(|until| *until > std::time::Instant::now())
            {
                continue;
            }
            let Some(space) = self.observed.spaces.get(&sid) else {
                // Dynamic workspaces manage their own lifecycle: if macOS
                // destroyed their backing outside our flow, forget them.
                // Configured volatile ones stay for remap to rebind.
                if ws.dynamic {
                    stale_dynamic.push(name.clone());
                }
                continue;
            };
            if space.is_fullscreen || space.is_system || space.focused {
                continue;
            }
            let occupied = self
                .observed
                .windows
                .values()
                .any(|w| w.space_id == Some(sid));
            // A window that vanishes for exactly the ticks between leaving
            // this Space and appearing on its fullscreen Space still owns it:
            // bounded last-seen memory keeps the Space occupied until the
            // substitution entry fires (or the memory expires, which bounds
            // the GC delay a genuinely closed window can cause).
            let recently_occupied = !occupied
                && self.window_last_space.iter().any(|(_, (s, t))| {
                    *s == sid
                        && std::time::Instant::now().duration_since(*t) < WINDOW_LAST_SPACE_TTL
                });
            if occupied || recently_occupied {
                continue;
            }
            // macOS requires at least one Space per display.
            let display_spaces = self
                .observed
                .spaces
                .values()
                .filter(|s| s.display_id == space.display_id && !s.is_fullscreen && !s.is_system)
                .count();
            if display_spaces <= 1 {
                // The native minimum is not a reservation for a logical name.
                stale_dynamic.push(name.clone());
                continue;
            }
            doomed.push((name.clone(), sid));
        }
        for name in stale_dynamic {
            self.remove_workspace(&name);
        }
        // Drop grace entries for workspaces that no longer exist.
        self.dynamic_grace
            .retain(|name, _| self.workspaces.0.contains_key(name));
        let mut actions = Vec::with_capacity(doomed.len());
        doomed.sort();
        for (name, sid) in doomed.into_iter().take(1) {
            if self.pending_destroy.contains_key(&sid) {
                continue;
            }
            self.pending_destroy.insert(sid, name.clone());
            self.flight_recorder.record(
                "workspace.destroy_empty",
                format!("workspace {name}: backing space empty and unfocused"),
            );
            actions.push(Action::DestroySpace { space: sid });
        }
        actions
    }

    /// Fire the queued FocusSpace / MoveWindowToSpace once a dynamic spawn's
    /// backing Space materializes (id learned only by observing). The focus
    /// is one-shot: an unobserved dispatch is the normal path on macOS, where
    /// a freshly created Space reports `focused = false` for one or more AX
    /// ticks while the window server is still settling. Re-dispatching on
    /// every reconcile hijacks the user's focus whenever they alt-tab away
    /// before the very first attempt lands. Pending entries whose workspace
    /// disappeared (config reload) are dropped.
    fn fulfill_pending_workspace_actions(&mut self, out: &mut Vec<Action>) {
        if self.placement.is_some() {
            return;
        }
        if let Some((name, attempts)) = self.pending_workspace_focus.front().cloned() {
            match self.workspaces.backing_for(&name) {
                Some(space) => {
                    if attempts == 0 {
                        self.pending_workspace_focus.pop_front();

                        self.dynamic_grace.insert(
                            name,
                            std::time::Instant::now() + self.dynamic_grace_duration(),
                        );

                        out.push(Action::FocusSpace { space });
                    } else if self.observed.spaces.get(&space).is_some_and(|s| s.focused) {
                        self.pending_workspace_focus.pop_front();
                        self.dynamic_grace.insert(
                            name,
                            std::time::Instant::now() + self.dynamic_grace_duration(),
                        );
                    } else {
                        self.pending_workspace_focus.pop_front();
                    }
                }
                None => {
                    if !self.workspaces.0.contains_key(&name) {
                        self.pending_workspace_focus.pop_front();
                    }
                }
            }
        }
        if let Some((window, name)) = self.pending_workspace_move.front().cloned() {
            match self.workspaces.backing_for(&name) {
                Some(space) => {
                    self.pending_workspace_move.pop_front();

                    self.dynamic_grace.insert(
                        name,
                        std::time::Instant::now() + self.dynamic_grace_duration(),
                    );

                    if self.observed.windows.contains_key(&window) {
                        out.push(Action::MoveWindowToSpace { window, space });
                    }
                }
                None => {
                    if !self.workspaces.0.contains_key(&name) {
                        self.pending_workspace_move.pop_front();
                    }
                }
            }
        }
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
        // Workspaces that truly die on reload must not leak layout state.
        // Snapshot their backings first; after the remap below, drop trees
        // for backings no surviving workspace reclaimed (reused backings
        // keep their tiles so the native Space's layout is continuous).
        let before: HashMap<String, Option<SpaceId>> = self
            .workspaces
            .0
            .iter()
            .map(|(n, w)| (n.clone(), w.active_space()))
            .collect();
        self.workspaces.ensure_from_config(&self.config.workspaces);
        // Native mutations outlive config reload. Preserve their reservations.
        self.remap_slots();
        let live: std::collections::HashSet<SpaceId> = self
            .workspaces
            .0
            .values()
            .filter_map(|w| w.active_space())
            .chain(
                self.workspaces
                    .0
                    .values()
                    .filter_map(|w| w.backing.and_then(WorkspaceBacking::restore_space)),
            )
            .collect();
        for (name, backing) in before {
            if !self.workspaces.0.contains_key(&name) {
                self.restored_workspace_layouts.remove(&name);
                self.dynamic_grace.remove(&name);
                self.pending_workspace_focus.retain(|(n, _)| n != &name);
                self.pending_workspace_move.retain(|(_, n)| n != &name);
                if let Some(sid) = backing {
                    if !live.contains(&sid) {
                        self.layouts.remove(&sid);
                    }
                }
            }
        }
        // Reload WASM plugins from disk (isolated, fuel-limited, version-checked)
        self.plugins = PluginRegistry::new();
        load_plugins_from_disk(&mut self.plugins);
    }

    /// Return one safe topology repair at a time.
    ///
    /// Rovr owns logical workspaces, not Mission Control positions. Empty
    /// macOS Spaces that are unclaimed by any workspace are orphaned topology
    /// (typically leftovers from an old config or interrupted create). Prune
    /// them from highest position downward while preserving focused/occupied
    /// Spaces and at least one Space per display. Returning only one action
    /// forces the daemon to re-observe after every deletion before deciding
    /// the next repair.
    pub fn next_topology_heal_action(&self) -> Option<Action> {
        if !self.capabilities.destroy_space
            || !self.awaited_creations.is_empty()
            || !self.pending_creations.is_empty()
            || self.placement.is_some()
            || self.topology_destroy.is_some()
            || !self.pending_destroy.is_empty()
        {
            return None;
        }

        let claimed: std::collections::HashSet<SpaceId> = self
            .workspaces
            .0
            .values()
            .flat_map(|workspace| {
                [
                    workspace.active_space(),
                    workspace.backing.and_then(WorkspaceBacking::restore_space),
                ]
                .into_iter()
                .flatten()
            })
            .collect();
        let occupied: std::collections::HashSet<SpaceId> = self
            .observed
            .windows
            .values()
            .filter_map(|window| window.space_id)
            .collect();
        let mut counts: HashMap<DisplayId, usize> = HashMap::new();
        for space in self.observed.spaces.values() {
            if space.is_fullscreen || space.is_system {
                continue;
            }
            *counts.entry(space.display_id).or_default() += 1;
        }

        self.observed
            .spaces
            .values()
            .filter(|space| {
                !space.is_fullscreen
                    && !space.is_system
                    && !space.focused
                    && !self
                        .external_spaces
                        .get(&space.id)
                        .is_some_and(|until| *until > std::time::Instant::now())
                    && !claimed.contains(&space.id)
                    && !occupied.contains(&space.id)
                    && counts.get(&space.display_id).copied().unwrap_or(0) > 1
            })
            .max_by_key(|space| (space.position, space.id))
            .map(|space| Action::DestroySpace { space: space.id })
    }

    pub fn apply_event(&mut self, event: Event) -> Vec<Action> {
        // Flight records stay small: a full PlatformSnapshot debug dump is
        // tens of KB (every window/space/display), and 2048 of those blow
        // past the 1MB IPC cap so `rovr debug events` fails exactly when it
        // is needed for live debugging. Snapshots log as one summary line.
        let summary = match &event {
            Event::Snapshot(s) => format!(
                "Snapshot(w={} s={} d={} complete={})",
                s.windows.len(),
                s.spaces.len(),
                s.displays.len(),
                s.complete
            ),
            other => format!("{other:?}"),
        };
        self.flight_recorder.record("event", summary);

        let is_snapshot = matches!(&event, Event::Snapshot(_));
        let mut lifecycle_action: Vec<Action> = Vec::new();
        match event {
            Event::Snapshot(snapshot) => {
                let complete = snapshot.complete;
                let previous_windows = self.observed.windows.clone();
                let old_normal: Vec<_> = self
                    .observed
                    .spaces
                    .values()
                    .filter(|s| !s.is_fullscreen && !s.is_system)
                    .map(|s| s.id)
                    .collect();
                let reset = complete
                    && !old_normal.is_empty()
                    && snapshot
                        .spaces
                        .iter()
                        .any(|s| !s.is_fullscreen && !s.is_system)
                    && old_normal
                        .iter()
                        .all(|id| !snapshot.spaces.iter().any(|s| s.id == *id));
                if reset {
                    self.topology_initialized = false;
                    self.external_spaces.clear();
                    // Topology discontinuity (Dock restart/rebuild): every
                    // SpaceId the in-flight creations measured their `before`
                    // sets against is gone, so those reservations are void.
                    // Re-queue them at the front (order preserved) so the
                    // remap below holds these workspaces out of positional
                    // assignment — otherwise a demoted workspace would steal
                    // a rebuilt survivor's slot and shuffle stable bindings.
                    // The same cycle re-issues ONE fresh CreateSpace per
                    // re-queued name with a fresh `before` set, which cannot
                    // duplicate: the old world provably no longer exists, and
                    // any survivor sits unclaimed under adoption grace.
                    if !self.awaited_creations.is_empty() {
                        let mut names = Vec::new();
                        let awaiting: Vec<AwaitedCreation> =
                            self.awaited_creations.drain(..).collect();
                        for req in awaiting.into_iter().rev() {
                            names.push(req.name.clone());
                            if !self.pending_creations.iter().any(|r| r.name == req.name) {
                                self.pending_creations
                                    .push_front(PendingCreationRequest { name: req.name });
                            }
                        }
                        names.reverse();
                        self.flight_recorder.record(
                            "workspace.creation_reset",
                            format!(
                                "topology reset re-queued in-flight creations for {}",
                                names.join(",")
                            ),
                        );
                    }
                }
                self.apply_snapshot(snapshot);
                if !complete {
                    return Vec::new();
                }
                self.reconcile_fullscreen(&previous_windows);
                // Multi-tick memory is refreshed only after fullscreen
                // reconciliation consumed the pre-snapshot view of it.
                self.note_window_spaces();
                if self
                    .topology_destroy
                    .is_some_and(|id| !self.observed.spaces.contains_key(&id))
                {
                    self.topology_destroy = None;
                }
                // Confirm pending destroys: Space gone => actually remove workspace/layout.
                let mut confirmed_destroys = Vec::new();
                for space in self.pending_destroy.keys() {
                    if !self.observed.spaces.contains_key(space) {
                        confirmed_destroys.push(*space);
                    }
                }
                for space in confirmed_destroys {
                    if let Some(name) = self.pending_destroy.remove(&space) {
                        self.remove_workspace(&name);
                    }
                }
                // Pending close: keep for this layout's instant retile, but if window already
                // gone, clear it now; otherwise it will be cleared after layout for cancel case.
                self.pending_close
                    .retain(|w| self.observed.windows.contains_key(w));
                self.reconcile_space_cursors();
                // External removal retires volatile logical slots before compaction.
                let retired: Vec<String> = self
                    .workspaces
                    .0
                    .iter()
                    .filter(|(_, ws)| {
                        !(reset
                            || ws.persistent
                            || ws.active_space().is_none()
                            || ws
                                .active_space()
                                .is_some_and(|id| self.observed.spaces.contains_key(&id)))
                    })
                    .map(|(name, _)| name.clone())
                    .collect();
                for name in retired {
                    self.remove_workspace(&name);
                }
                if !reset {
                    for name in self.workspaces.ordered_names() {
                        if self.workspaces.0[&name].persistent
                            && self.workspaces.0[&name]
                                .active_space()
                                .is_some_and(|id| !self.observed.spaces.contains_key(&id))
                        {
                            // The backing Space is gone: park the workspace's
                            // tree under its logical name so the recreation
                            // below reattaches it instead of stranding a
                            // SpaceId-keyed orphan.
                            self.clear_stale_backing(&name);
                            if !self.pending_creations.iter().any(|r| r.name == name) {
                                self.pending_creations
                                    .push_back(PendingCreationRequest { name });
                            }
                        }
                    }
                }
                self.bind_awaited_dynamic_spaces();
                self.observe_external_spaces();
                let mut lifecycle_actions: Vec<_> =
                    self.reconcile_placement().into_iter().collect();
                let moves = self.remap_slots();
                // A Mission Control reorder changes contents. Live layouts stay with
                // their native windows; only vanished handles require migration.
                self.apply_remap_moves(&moves);
                let restored = std::mem::take(&mut self.restored_workspace_layouts);
                for (name, layout) in restored {
                    if let Some(space) = self.workspaces.backing_for(&name) {
                        self.layouts.insert(space, layout);
                    } else {
                        self.restored_workspace_layouts.insert(name, layout);
                    }
                }
                // The first observation also gives surplus native desktops grace.
                for space in self
                    .observed
                    .spaces
                    .values()
                    .filter(|s| !s.is_fullscreen && !s.is_system)
                {
                    if !self.workspaces.0.values().any(|w| {
                        w.active_space() == Some(space.id)
                            || w.backing.and_then(WorkspaceBacking::restore_space) == Some(space.id)
                    }) {
                        self.external_spaces.entry(space.id).or_insert(
                            std::time::Instant::now() + std::time::Duration::from_secs(3),
                        );
                    }
                }
                lifecycle_actions.extend(self.destroy_empty_dynamic_workspaces(&moves));
                self.fulfill_pending_workspace_actions(&mut lifecycle_actions);
                for name in self.workspaces.ensure_persistent() {
                    if !self.awaited_creations.iter().any(|r| r.name == name)
                        && !self.pending_creations.iter().any(|r| r.name == name)
                    {
                        self.pending_creations
                            .push_back(PendingCreationRequest { name });
                    }
                }
                if let Some(next) = self.try_start_next_pending_creation() {
                    lifecycle_actions.push(next);
                }
                if lifecycle_actions.is_empty() {
                    if let Some(Action::DestroySpace { space }) = self.next_topology_heal_action() {
                        self.topology_destroy = Some(space);
                        lifecycle_actions.push(Action::DestroySpace { space });
                    }
                }
                lifecycle_action = lifecycle_actions;
            }
            Event::WindowDestroyed { window } => {
                self.observed.windows.remove(&window);
                self.desired.windows.remove(&window);
                self.pending_close.remove(&window);
            }
            Event::SpaceDestroyed { space } => {
                self.observed.spaces.remove(&space);
                for target in self.desired.windows.values_mut() {
                    if target.space == Some(space) {
                        target.space = None;
                    }
                }
                if let Some(name) = self.pending_destroy.remove(&space) {
                    self.remove_workspace(&name);
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
            self.insets_off,
            &self.pending_close,
        );
        if is_snapshot && !self.pending_close.is_empty() {
            // Pending close was used for this snapshot's instant retile. Clear it
            // so a cancelled close (window still observed) retires correctly next cycle.
            // Windows that were actually destroyed are already gone from observed and
            // pending_close was retained only for those still present, so clearing is safe.
            self.pending_close.clear();
        }

        let mut actions = reconcile(&self.observed, &self.desired);
        actions.extend(lifecycle_action);
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

    /// Step to the next (`delta = 1`) or previous (`delta = -1`) space on one
    /// display, wrapping at the ends. `display`: None = display of the
    /// currently focused space, "main" = CGMainDisplayID, numeric = that
    /// display id. Per-display focused Spaces (multi-display fix) mean every
    /// display always reports exactly one current Space, so this works even
    /// when the TARGET display is not the active one — e.g. alt+arrows
    /// driving the external display while typing on the built-in one.
    pub fn focus_space_step(
        &mut self,
        display: Option<&str>,
        delta: i32,
    ) -> Result<Vec<Action>, EngineError> {
        if !matches!(delta, -1 | 1) {
            return Err(EngineError::InvalidSpaceStep);
        }
        let target_display = match display {
            Some("main") => self
                .observed
                .displays
                .values()
                .find(|d| d.is_main)
                .or_else(|| self.observed.displays.values().find(|d| d.focused))
                .map(|d| d.id)
                .ok_or(EngineError::NoFocusedSpace)?,
            Some(n) => {
                let id = n
                    .parse::<u32>()
                    .map(DisplayId)
                    .map_err(|_| EngineError::DisplayNotFound(n.to_string()))?;
                if !self.observed.displays.contains_key(&id) {
                    return Err(EngineError::DisplayNotFound(n.to_string()));
                }
                id
            }
            None => self
                .observed
                .spaces
                .values()
                .find(|s| s.focused)
                .map(|s| s.display_id)
                .ok_or(EngineError::NoFocusedSpace)?,
        };

        let mut on_display: Vec<_> = self
            .observed
            .spaces
            .values()
            .filter(|s| s.display_id == target_display)
            .collect();
        on_display.sort_by_key(|s| (s.position, s.id));
        if on_display.is_empty() {
            return Err(EngineError::DisplayNotFound(target_display.0.to_string()));
        }
        if on_display.len() == 1 {
            return Err(EngineError::NoAdjacentSpace);
        }
        let current_space = self
            .space_cursors
            .get(&target_display)
            .map(|(space, _)| *space)
            .or_else(|| on_display.iter().find(|s| s.focused).map(|s| s.id))
            .unwrap_or(on_display[0].id);
        let current = on_display
            .iter()
            .position(|s| s.id == current_space)
            .unwrap_or(0);
        let next = (current as i32 + delta).rem_euclid(on_display.len() as i32) as usize;
        let target = on_display[next].id;
        let displacement = next as i32 - current as i32;
        self.space_cursors.insert(target_display, (target, true));
        Ok(vec![Action::FocusSpaceStep {
            target,
            delta: displacement,
        }])
    }

    pub fn note_space_focus_dispatched(&mut self, space: SpaceId) {
        if let Some(display) = self.observed.spaces.get(&space).map(|s| s.display_id) {
            self.space_cursors.insert(display, (space, true));
        }
    }

    pub fn cancel_pending_space_focus(&mut self, space: SpaceId) {
        if let Some(display) = self.observed.spaces.get(&space).map(|s| s.display_id) {
            self.space_cursors.remove(&display);
        }
    }

    /// Make the next snapshot authoritative for Space navigation cursors.
    /// Immediate event-driven snapshots intentionally preserve pending targets
    /// so rapid navigation can chain while macOS focus observation is stale.
    pub fn abandon_pending_space_cursors(&mut self) {
        for (_, pending) in self.space_cursors.values_mut() {
            *pending = false;
        }
    }

    fn reconcile_space_cursors(&mut self) {
        for display in self.observed.displays.keys().copied().collect::<Vec<_>>() {
            let observed = self
                .observed
                .spaces
                .values()
                .find(|space| space.display_id == display && space.focused)
                .map(|space| space.id);
            let Some(observed) = observed else { continue };
            match self.space_cursors.get(&display).copied() {
                Some((intended, true)) if intended != observed => {}
                _ => {
                    self.space_cursors.insert(display, (observed, false));
                }
            }
        }
        self.space_cursors.retain(|display, (space, _)| {
            self.observed.displays.contains_key(display)
                && self
                    .observed
                    .spaces
                    .get(space)
                    .is_some_and(|s| s.display_id == *display)
        });
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

    /// Focus a logical workspace. i3-style dynamic spawn: an unknown name is
    /// registered on demand (non-persistent) and its backing Space is created
    /// anchored on the focused Space; the focus fires once a snapshot observes
    /// the new Space. A known workspace with a backing focuses immediately.
    ///
    /// A dynamic workspace whose persisted `backing_space` no longer exists
    /// (the user deleted it, or the Space was lost across a session) is
    /// treated as unknown: forget the stale id and spawn a fresh Space.
    /// Without this, the stale id would always fail `require_space`, the
    /// hotkey would error silently, and alt-N would appear to do nothing.
    pub fn focus_workspace(&mut self, name: &str) -> Result<Vec<Action>, EngineError> {
        if let Some(space) = self
            .workspaces
            .backing_for(name)
            .filter(|s| self.observed.spaces.contains_key(s))
        {
            if self.placement.is_some() {
                if !self.pending_workspace_focus.iter().any(|(n, _)| n == name) {
                    self.pending_workspace_focus.push_back((name.into(), 0));
                }
                return Ok(vec![]);
            }
            return Ok(vec![Action::FocusSpace { space }]);
        }
        if self.workspaces.0.contains_key(name) {
            self.clear_stale_backing(name);
        }
        if !self.pending_workspace_focus.iter().any(|(n, _)| n == name) {
            self.pending_workspace_focus.push_back((name.into(), 0));
        }
        self.request_workspace_creation(name)
    }

    pub fn move_window_to_workspace(
        &mut self,
        window: Option<WindowId>,
        name: &str,
    ) -> Result<Vec<Action>, EngineError> {
        let window = self.resolve_window(window)?;
        let observed = &self.observed.windows[&window];
        if observed.fullscreen == rovr_types::ObservedBool::Yes
            || observed
                .space_id
                .and_then(|s| self.observed.spaces.get(&s))
                .is_some_and(|s| s.is_fullscreen)
        {
            return Err(EngineError::NativeFullscreenMove);
        }
        if let Some(space) = self
            .workspaces
            .backing_for(name)
            .filter(|s| self.observed.spaces.contains_key(s))
        {
            if self.observed.spaces[&space].is_fullscreen {
                return Err(EngineError::NativeFullscreenMove);
            }
            if self.placement.is_some() {
                if !self
                    .pending_workspace_move
                    .iter()
                    .any(|(w, n)| *w == window && n == name)
                {
                    self.pending_workspace_move.push_back((window, name.into()));
                    self.flight_recorder.record(
                        "workspace.move_queued",
                        format!("window {window:?} -> {name}: held until placement settles"),
                    );
                }
                return Ok(vec![]);
            }
            self.dynamic_grace.insert(
                name.into(),
                std::time::Instant::now() + self.dynamic_grace_duration(),
            );
            return Ok(vec![Action::MoveWindowToSpace { window, space }]);
        }
        if self.workspaces.0.contains_key(name) {
            self.clear_stale_backing(name);
        }
        if !self
            .pending_workspace_move
            .iter()
            .any(|(w, n)| *w == window && n == name)
        {
            self.pending_workspace_move.push_back((window, name.into()));
        }
        self.request_workspace_creation(name)
    }

    pub fn workspace_for_space(&self, space: SpaceId) -> Option<&str> {
        self.workspaces.name_for_space(space)
    }

    /// The currently focused window, if observation saw one.
    ///
    /// On macOS each Space remembers its own “focused window”, so the
    /// observation can contain several `focused == true` entries (one per
    /// app, one per Space). Picking the first `HashMap` entry is
    /// non-deterministic and causes hotkey commands like
    /// `window move-to-workspace` to act on a window the user is not looking
    /// at. Prefer the focused window on the currently focused Space, and
    /// among those (or as fallback) pick the most recent (largest WindowId)
    /// deterministically so `query focused` and `resolve_window` agree.
    pub fn focused_window(&self) -> Option<WindowId> {
        let focused_space = self
            .observed
            .spaces
            .values()
            .find(|s| s.focused)
            .map(|s| s.id);
        let mut candidates: Vec<&WindowSnapshot> = self
            .observed
            .windows
            .values()
            .filter(|w| w.focused)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by(|a, b| {
            let a_on = a.space_id == focused_space;
            let b_on = b.space_id == focused_space;
            (!a_on, std::cmp::Reverse(a.id.0)).cmp(&(!b_on, std::cmp::Reverse(b.id.0)))
        });
        candidates.first().map(|w| w.id)
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

        if !snapshot.complete {
            for window in snapshot.windows {
                self.observed.windows.insert(window.id, window);
            }
            for space in snapshot.spaces {
                self.observed.spaces.insert(space.id, space);
            }
            for display in snapshot.displays {
                self.observed.displays.insert(display.id, display);
            }
            return;
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
                    && candidate.space_id == source.space_id
                    && candidate.display_id == source.display_id
                    && candidate.managed == rovr_types::ObservedBool::Yes
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
                    is_fullscreen: false,
                    is_system: false,
                },
                SpaceSnapshot {
                    id: SpaceId(22),
                    display_id: DisplayId(2),
                    label: None,
                    focused: false,
                    generation: 0,
                    position: 1,
                    is_fullscreen: false,
                    is_system: false,
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
                    is_main: false,
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
                    is_main: false,
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
                is_main: false,
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
                is_fullscreen: false,
                is_system: false,
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
            false,
            &std::collections::HashSet::new(),
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
            false,
            &std::collections::HashSet::new(),
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
            is_fullscreen: false,
            is_system: false,
        }
    }

    fn ws_config(name: &str, persistent: bool) -> rovr_config::WorkspaceConfig {
        rovr_config::WorkspaceConfig {
            name: name.into(),
            layout: rovr_types::LayoutKind::Bsp,
            display: None,
            persistent,
            plugin: None,
        }
    }

    fn window_on_space(id: u32, space: u64) -> WindowSnapshot {
        WindowSnapshot {
            id: WindowId(id),
            pid: ProcessId(1),
            app: "App".into(),
            bundle_id: None,
            title: String::new(),
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            space_id: Some(SpaceId(space)),
            display_id: Some(DisplayId(1)),
            focused: false,
            minimized: rovr_types::ObservedBool::No,
            fullscreen: rovr_types::ObservedBool::No,
            managed: rovr_types::ObservedBool::Yes,
            generation: 0,
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
            is_main: false,
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
        ws.backing = Some(crate::workspace::WorkspaceBacking::Normal { space: SpaceId(11) });
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
        engine.workspaces.0.get_mut("code").unwrap().backing =
            Some(crate::workspace::WorkspaceBacking::Normal { space: SpaceId(11) });
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
            None,
            "native handles are learned from observation, never persistence"
        );
        assert!(restored.restored_workspace_layouts.contains_key("code"));
        let area = Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let expected = restored.restored_workspace_layouts["code"]
            .bsp
            .placements(area, 8.0);

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

    // ---- i3-style dynamic workspaces: spawn on focus, die when empty ----

    /// alt-N on an unknown workspace registers it dynamically and requests a
    /// CreateSpace; once the Space materializes and remap binds it, the queued
    /// FocusSpace fires.
    #[test]
    fn dynamic_workspace_focus_spawns_and_fills() {
        let mut engine = Engine::new(Config {
            workspaces: vec![ws_config("1", true)],
            ..Default::default()
        });
        engine.capabilities.create_space = true;
        engine.capabilities.destroy_space = true;
        engine.capabilities.reorder_space = true;

        let snap1 = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        engine.apply_event(Event::Snapshot(snap1));

        let actions = engine.focus_workspace("2").expect("dynamic spawn");
        assert_eq!(
            actions,
            vec![Action::CreateSpace {
                anchor: SpaceId(11)
            }],
            "unknown workspace must register + request CreateSpace, not error"
        );
        assert!(engine.workspaces.backing_for("2").is_none());

        // macOS created Space 20; the next snapshot binds it to "2" and the
        // queued focus fires once (the bound Space is not yet reported as
        // focused by AX, but that is the normal macOS path).
        let snap2 = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, false), space_snap(20, 1, 1, false)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let actions2 = engine.apply_event(Event::Snapshot(snap2));
        assert_eq!(engine.workspaces.backing_for("2"), Some(SpaceId(20)));
        assert_eq!(
            actions2
                .iter()
                .filter(|a| matches!(a, Action::FocusSpace { .. }))
                .count(),
            1,
            "pending focus must fire exactly once when the spawned space binds"
        );
        assert!(
            !actions2
                .iter()
                .any(|action| matches!(action, Action::MoveSpace { .. })),
            "a workspace created directly after its predecessor must not pay for a no-op reorder"
        );

        // A second snapshot that still reports Space 20 as unfocused must
        // NOT keep dispatching — that would hijack the user's manual focus.
        let snap3 = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, false), space_snap(20, 1, 1, false)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let actions3 = engine.apply_event(Event::Snapshot(snap3));
        assert!(
            !actions3
                .iter()
                .any(|a| matches!(a, Action::FocusSpace { .. })),
            "one-shot focus: no re-dispatch until the user re-issues the hotkey"
        );

        // The user re-pressing alt-2 focuses immediately (no duplicate creation).
        let again = engine.focus_workspace("2").unwrap();
        assert_eq!(again, vec![Action::FocusSpace { space: SpaceId(20) }]);
    }

    /// If the user alt-tabs away before a spawned focus lands, the engine
    /// must not steal focus back on the next reconcile. The next alt-N
    /// reissues the focus through the normal hotkey path.
    #[test]
    fn dynamic_workspace_focus_does_not_steal_after_user_switches_away() {
        let mut engine = Engine::new(Config {
            workspaces: vec![ws_config("1", true)],
            ..Default::default()
        });
        engine.capabilities.create_space = true;
        engine.capabilities.destroy_space = true;

        let snap1 = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        engine.apply_event(Event::Snapshot(snap1));
        let _ = engine.focus_workspace("2").expect("dynamic spawn");

        // Spawn binds, the queued focus fires once.
        let snap2 = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, false), space_snap(20, 1, 1, false)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let _ = engine.apply_event(Event::Snapshot(snap2));

        // User alt-tabs to a different Space (back to 11, which is focused).
        let user_overrides = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, true), space_snap(20, 1, 1, false)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let actions = engine.apply_event(Event::Snapshot(user_overrides));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::FocusSpace { .. })),
            "engine must not re-focus the spawned Space after the user moved away"
        );
    }

    /// A dynamic workspace whose persisted backing Space no longer exists
    /// (the user deleted it manually, or the Space was lost across a prior
    /// session) must NOT error on alt-N. The stale id is forgotten and a
    /// fresh Space is created. This is the regression for the "alt 2/3 is
    /// unreachable" report.
    #[test]
    fn focus_workspace_with_stale_dynamic_binding_respawns() {
        let mut engine = Engine::new(Config {
            workspaces: vec![ws_config("1", true)],
            ..Default::default()
        });
        engine.capabilities.create_space = true;
        engine.capabilities.destroy_space = true;
        // Seed observed with the persistent Space plus a fresh Space that
        // the dynamic "2" is NOT bound to, then bind "2" to a stale id that
        // is not in the observed set.
        let snap = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, true), space_snap(7, 1, 1, false)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let _ = engine.apply_event(Event::Snapshot(snap));
        // Simulate a persisted dynamic workspace with a stale backing id.
        let stale = SpaceId(1778);
        let mut stale_state = crate::workspace::WorkspaceState::new("2".into(), None, false);
        stale_state.dynamic = true;
        stale_state.backing = Some(crate::workspace::WorkspaceBacking::Normal { space: stale });
        engine.workspaces.0.insert("2".into(), stale_state);
        engine.layouts.insert(stale, Default::default());

        let actions = engine
            .focus_workspace("2")
            .expect("stale id must not error");
        assert!(
            matches!(actions.as_slice(), &[Action::CreateSpace { .. }]),
            "stale dynamic binding must fall through to the spawn path; got {actions:?}"
        );
        assert_eq!(engine.workspaces.backing_for("2"), None);
        assert!(
            !engine.layouts.contains_key(&stale),
            "stale SpaceId layout must be dropped"
        );
        assert!(
            engine.restored_workspace_layouts.contains_key("2"),
            "stale tree parks under the logical name so the respawn reattaches it"
        );
    }

    /// If desktop 3 is manually deleted from 1,2,3,4, macOS compacts desktop
    /// 4 into the third slot. Recreating logical workspace 3 initially appends
    /// it, so it must be moved behind workspace 2 before it is focused. This
    /// ordering keeps the repair entirely off-screen.
    #[test]
    fn recreated_middle_numeric_workspace_reorders_before_focus() {
        let mut engine = Engine::new(Config {
            workspaces: vec![ws_config("1", true)],
            ..Default::default()
        });
        engine.capabilities.create_space = true;
        engine.capabilities.reorder_space = true;

        let initial = PlatformSnapshot {
            windows: vec![],
            spaces: vec![
                space_snap(11, 1, 0, true),
                space_snap(12, 1, 1, false),
                space_snap(13, 1, 2, false),
                space_snap(14, 1, 3, false),
            ],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let _ = engine.apply_event(Event::Snapshot(initial));
        for (name, id, ordinal) in [("2", 12, 1), ("3", 13, 2), ("4", 14, 3)] {
            let mut workspace = crate::workspace::WorkspaceState::new(name.into(), None, false);
            workspace.dynamic = true;
            workspace.ordinal = ordinal;
            workspace.backing =
                Some(crate::workspace::WorkspaceBacking::Normal { space: SpaceId(id) });
            engine.workspaces.0.insert(name.into(), workspace);
        }

        // The user deletes workspace 3. Workspace 4 keeps its SpaceId while
        // its observed position compacts from 3 to 2.
        let after_delete = PlatformSnapshot {
            windows: vec![],
            spaces: vec![
                space_snap(11, 1, 0, true),
                space_snap(12, 1, 1, false),
                space_snap(14, 1, 2, false),
            ],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let _ = engine.apply_event(Event::Snapshot(after_delete));
        assert_eq!(engine.workspaces.backing_for("3"), None);
        assert_eq!(engine.workspaces.backing_for("4"), Some(SpaceId(14)));

        assert_eq!(
            engine.focus_workspace("3").unwrap(),
            vec![Action::CreateSpace {
                anchor: SpaceId(11)
            }]
        );

        // macOS appends the replacement after workspace 4. Rovr repairs its
        // logical slot before issuing the one visible focus operation.
        let appended = PlatformSnapshot {
            windows: vec![],
            spaces: vec![
                space_snap(11, 1, 0, true),
                space_snap(12, 1, 1, false),
                space_snap(14, 1, 2, false),
                space_snap(20, 1, 3, false),
            ],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let actions = engine.apply_event(Event::Snapshot(appended));
        assert_eq!(
            actions,
            vec![Action::MoveSpace {
                space: SpaceId(20),
                after: SpaceId(12),
            },],
            "replacement must be reordered and observed before focus"
        );
        assert_eq!(engine.workspaces.backing_for("3"), Some(SpaceId(20)));
        assert_eq!(engine.workspaces.backing_for("4"), Some(SpaceId(14)));
    }

    /// A bad mapping created by an older daemon must heal on the next hotkey
    /// press; otherwise upgrading the spawn path would leave existing users
    /// permanently stuck with alt-2 pointing at desktop 7.
    #[test]
    fn existing_workspace_focus_does_not_heal_remembered_ids() {
        let mut engine = Engine::new(Config {
            workspaces: vec![ws_config("1", true)],
            ..Default::default()
        });
        engine.capabilities.reorder_space = true;
        let snapshot = PlatformSnapshot {
            windows: vec![],
            spaces: vec![
                space_snap(11, 1, 0, true),
                space_snap(21, 1, 1, false),
                space_snap(22, 1, 2, false),
                space_snap(23, 1, 3, false),
                space_snap(24, 1, 4, false),
                space_snap(25, 1, 5, false),
                space_snap(20, 1, 6, false),
            ],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let _ = engine.apply_event(Event::Snapshot(snapshot));
        let mut workspace = crate::workspace::WorkspaceState::new("2".into(), None, false);
        workspace.dynamic = true;
        workspace.ordinal = 1;
        workspace.backing = Some(crate::workspace::WorkspaceBacking::Normal { space: SpaceId(20) });
        engine.workspaces.0.insert("2".into(), workspace);

        assert_eq!(
            engine.focus_workspace("2").unwrap(),
            vec![Action::FocusSpace { space: SpaceId(20) },]
        );
    }

    /// Switching away from a dynamic workspace whose backing Space has no
    /// windows destroys the Space and forgets the workspace (i3 semantics).
    #[test]
    fn dynamic_workspace_destroyed_when_empty_and_unfocused() {
        let mut engine = Engine::new(Config {
            workspaces: vec![ws_config("1", true)],
            ..Default::default()
        });
        engine.capabilities.create_space = true;
        engine.capabilities.destroy_space = true;

        // Desktop 1 holds the only window; alt-2 spawns an empty desktop.
        let snap1 = PlatformSnapshot {
            windows: vec![window_on_space(1, 11)],
            spaces: vec![space_snap(11, 1, 0, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        engine.apply_event(Event::Snapshot(snap1));
        let spawn = engine.focus_workspace("2").unwrap();
        assert!(matches!(spawn.as_slice(), &[Action::CreateSpace { .. }]));

        // Space 20 materializes and is bound; the queued focus dispatches.
        let snap2 = PlatformSnapshot {
            windows: vec![window_on_space(1, 11)],
            spaces: vec![space_snap(11, 1, 0, false), space_snap(20, 1, 1, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        engine.apply_event(Event::Snapshot(snap2));
        assert_eq!(engine.workspaces.backing_for("2"), Some(SpaceId(20)));

        // User switches back to desktop 1 (space 20 now empty + unfocused).
        // Simulate the post-spawn grace having elapsed.
        engine.dynamic_grace.clear();
        let leave = PlatformSnapshot {
            windows: vec![window_on_space(1, 11)],
            spaces: vec![space_snap(11, 1, 0, true), space_snap(20, 1, 1, false)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let actions = engine.apply_event(Event::Snapshot(leave));
        assert!(
            actions.contains(&Action::DestroySpace { space: SpaceId(20) }),
            "empty unfocused dynamic workspace must be destroyed"
        );
        assert!(
            engine.workspaces.0.contains_key("2"),
            "workspace kept pending until Space gone"
        );
        assert!(
            engine.pending_destroy.contains_key(&SpaceId(20)),
            "pending destroy"
        );
        let gone = PlatformSnapshot {
            windows: vec![window_on_space(1, 11)],
            spaces: vec![space_snap(11, 1, 0, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        engine.apply_event(Event::Snapshot(gone));
        assert!(
            !engine.workspaces.0.contains_key("2"),
            "forgotten after destruction confirmed"
        );

        // ...and alt-2 spawns it fresh again.
        let respawn = engine.focus_workspace("2").unwrap();
        assert!(matches!(respawn.as_slice(), &[Action::CreateSpace { .. }]));
    }

    /// Occupied or focused dynamic workspaces — and persistent/configured
    /// ones — are never destroyed by the empty-space sweep.
    #[test]
    fn dynamic_workspace_sweep_respects_occupied_focused_persistent() {
        let config = Config {
            workspaces: vec![ws_config("1", true), ws_config("keep", false)],
            ..Default::default()
        };
        let mut engine = Engine::new(config);
        engine.capabilities.create_space = true;
        engine.capabilities.destroy_space = true;

        // "keep" is non-persistent with a window on its backing; ws "2" is a
        // dynamic spawn target still pending its Space.
        let snap = PlatformSnapshot {
            windows: vec![window_on_space(7, 22)],
            spaces: vec![space_snap(11, 1, 0, true), space_snap(22, 1, 1, false)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        engine.apply_event(Event::Snapshot(snap.clone()));
        engine.focus_workspace("2").expect("pending spawn");

        let actions = engine.apply_event(Event::Snapshot(snap));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::DestroySpace { .. })),
            "occupied dynamic, pending dynamic, and configured workspaces must survive"
        );
        assert!(engine.workspaces.0.contains_key("keep"));
        assert!(engine.workspaces.0.contains_key("2"));

        // Empty but FOCUSED dynamic also survives.
        engine.workspaces.0.get_mut("keep").unwrap().backing =
            Some(crate::workspace::WorkspaceBacking::Normal { space: SpaceId(22) });
        let focused_empty = PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, false), space_snap(22, 1, 1, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let actions = engine.apply_event(Event::Snapshot(focused_empty));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::DestroySpace { .. })),
            "the focused empty workspace must not be destroyed"
        );
    }

    /// move-to-workspace to an unknown workspace spawns its Space and queues
    /// the MoveWindowToSpace until the Space is observed.
    #[test]
    fn dynamic_workspace_move_spawns_and_defers_move() {
        let mut engine = Engine::new(Config {
            workspaces: vec![ws_config("1", true)],
            ..Default::default()
        });
        engine.capabilities.create_space = true;
        engine.capabilities.destroy_space = true;

        let snap = PlatformSnapshot {
            windows: vec![window_on_space(9, 11)],
            spaces: vec![space_snap(11, 1, 0, true)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        engine.apply_event(Event::Snapshot(snap));

        let actions = engine
            .move_window_to_workspace(Some(WindowId(9)), "5")
            .expect("dynamic spawn for move");
        assert!(matches!(actions.as_slice(), &[Action::CreateSpace { .. }]));

        let snap2 = PlatformSnapshot {
            windows: vec![window_on_space(9, 11)],
            spaces: vec![space_snap(11, 1, 0, true), space_snap(30, 1, 1, false)],
            displays: vec![display_snap(1)],
            complete: true,
        };
        let actions2 = engine.apply_event(Event::Snapshot(snap2));
        assert!(actions2.contains(&Action::MoveWindowToSpace {
            window: WindowId(9),
            space: SpaceId(30),
        }));
    }

    #[test]
    fn topology_heal_prunes_only_empty_unclaimed_spaces_one_at_a_time() {
        let mut engine = Engine::new(Config {
            workspaces: vec![ws_config("1", true)],
            ..Default::default()
        });
        engine.capabilities.destroy_space = true;
        let snapshot = PlatformSnapshot {
            windows: vec![window_on_space(9, 12)],
            spaces: vec![
                space_snap(11, 1, 0, true),
                space_snap(12, 1, 1, false),
                space_snap(13, 1, 2, false),
                space_snap(14, 1, 3, false),
            ],
            displays: vec![display_snap(1)],
            complete: true,
        };
        engine.apply_event(Event::Snapshot(snapshot));

        for until in engine.external_spaces.values_mut() {
            *until = std::time::Instant::now();
        }
        assert_eq!(engine.workspaces.backing_for("1"), Some(SpaceId(11)));
        assert_eq!(
            engine.next_topology_heal_action(),
            Some(Action::DestroySpace { space: SpaceId(14) }),
            "heal removes only the highest empty orphan and re-observes before the next"
        );
        assert_ne!(
            engine.next_topology_heal_action(),
            Some(Action::DestroySpace { space: SpaceId(12) }),
            "occupied orphan space must be preserved"
        );
    }

    #[test]
    fn topology_heal_never_removes_last_space_on_display() {
        let mut engine = Engine::new(Config::default());
        engine.capabilities.destroy_space = true;
        engine.apply_event(Event::Snapshot(PlatformSnapshot {
            windows: vec![],
            spaces: vec![space_snap(11, 1, 0, false)],
            displays: vec![display_snap(1)],
            complete: true,
        }));
        assert_eq!(engine.next_topology_heal_action(), None);
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

    /// Config reload updates policy without assigning surviving logical
    /// workspaces to different SpaceIds.
    #[test]
    fn reload_config_preserves_ids_after_drag() {
        let workspace = |name: &str| rovr_config::WorkspaceConfig {
            name: name.into(),
            layout: rovr_types::LayoutKind::Bsp,
            display: None,
            persistent: true,
            plugin: None,
        };
        let mut engine = Engine::new(Config {
            workspaces: vec![workspace("code"), workspace("chat")],
            ..Default::default()
        });
        engine.capabilities.create_space = false; // no lifecycle noise

        let spaces_at = |id_a: u64, pos_a: u32, id_b: u64, pos_b: u32| PlatformSnapshot {
            windows: vec![],
            spaces: vec![
                space_snap(id_a, 1, pos_a, false),
                space_snap(id_b, 1, pos_b, true),
            ],
            displays: vec![display_snap(1)],
            complete: true,
        };

        // Healthy session: code→11@position0, chat→12@position1.
        let _ = engine.apply_event(Event::Snapshot(spaces_at(11, 0, 12, 1)));
        assert_eq!(engine.workspaces.backing_for("code"), Some(SpaceId(11)));
        assert_eq!(engine.workspaces.backing_for("chat"), Some(SpaceId(12)));

        // User drags chat ahead of code in Mission Control: positions swap.
        // With drag handling, alt-N follows visual position, so code
        // (ordinal 0) now at pos0 should be Space 12, chat at pos1 should be
        // Space 11.
        let _ = engine.apply_event(Event::Snapshot(spaces_at(12, 0, 11, 1)));
        assert_eq!(
            (
                engine.workspaces.backing_for("code"),
                engine.workspaces.backing_for("chat")
            ),
            (Some(SpaceId(12)), Some(SpaceId(11))),
            "drag must make alt-N follow visual position"
        );

        // Explicit reload with the same config must keep the dragged mapping
        // stable, not flip back.
        engine.reload_config(Config {
            workspaces: vec![workspace("code"), workspace("chat")],
            ..Default::default()
        });
        let _ = engine.apply_event(Event::Snapshot(spaces_at(12, 0, 11, 1)));
        assert_eq!(
            (
                engine.workspaces.backing_for("code"),
                engine.workspaces.backing_for("chat")
            ),
            (Some(SpaceId(12)), Some(SpaceId(11))),
            "reload must keep dragged mapping"
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
            is_fullscreen: false,
            is_system: false,
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
            is_main: false,
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
            false,
            &std::collections::HashSet::new(),
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

    /// focus_space_step: alt+arrow navigation across a display's spaces,
    /// including targeting a NON-active display (per-display focused Spaces
    /// make the external display's current Space visible from anywhere).
    #[test]
    fn focus_space_steps_and_wraps_per_display() {
        let mut engine = Engine::default();
        let mut snap = snapshot(vec![]);
        snap.spaces = vec![
            SpaceSnapshot {
                id: SpaceId(1),
                display_id: DisplayId(2),
                label: None,
                focused: false,
                generation: 0,
                position: 0,
                is_fullscreen: false,
                is_system: false,
            },
            SpaceSnapshot {
                id: SpaceId(2),
                display_id: DisplayId(2),
                label: None,
                focused: true,
                generation: 0,
                position: 1,
                is_fullscreen: false,
                is_system: false,
            },
            SpaceSnapshot {
                id: SpaceId(3),
                display_id: DisplayId(2),
                label: None,
                focused: false,
                generation: 0,
                position: 2,
                is_fullscreen: false,
                is_system: false,
            },
            // decoy on the other display, globally "focused"
            SpaceSnapshot {
                id: SpaceId(9),
                display_id: DisplayId(1),
                label: None,
                focused: true,
                generation: 0,
                position: 3,
                is_fullscreen: false,
                is_system: false,
            },
        ];
        snap.displays = vec![
            DisplaySnapshot {
                id: DisplayId(1),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                label: None,
                focused: true,
                is_main: true,
                generation: 0,
            },
            DisplaySnapshot {
                id: DisplayId(2),
                frame: Rect {
                    x: 100.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                label: None,
                focused: false,
                is_main: false,
                generation: 0,
            },
        ];
        engine.apply_event(Event::Snapshot(snap));

        // next on display 2: current is 2 -> 3
        assert_eq!(
            engine.focus_space_step(Some("2"), 1).unwrap(),
            vec![Action::FocusSpaceStep {
                target: SpaceId(3),
                delta: 1,
            }]
        );
        // Observation is still stale at 2, but rapid presses continue from
        // the intended target and wrap: 3 -> 1 -> 2.
        assert_eq!(
            engine.focus_space_step(Some("2"), 1).unwrap(),
            vec![Action::FocusSpaceStep {
                target: SpaceId(1),
                delta: -2,
            }]
        );
        assert_eq!(
            engine.focus_space_step(Some("2"), 1).unwrap(),
            vec![Action::FocusSpaceStep {
                target: SpaceId(2),
                delta: 1,
            }]
        );
        assert_eq!(
            engine.focus_space_step(Some("2"), -1).unwrap(),
            vec![Action::FocusSpaceStep {
                target: SpaceId(1),
                delta: -1,
            }]
        );
        assert_eq!(
            engine.focus_space_step(Some("2"), -1).unwrap(),
            vec![Action::FocusSpaceStep {
                target: SpaceId(3),
                delta: 2,
            }]
        );
    }

    #[test]
    fn recovery_snapshot_replaces_stale_pending_space_cursor() {
        let mut engine = Engine::default();
        let mut snap = snapshot(vec![]);
        snap.spaces = (1..=3)
            .map(|id| SpaceSnapshot {
                id: SpaceId(id),
                display_id: DisplayId(1),
                label: None,
                focused: id == 1,
                generation: 0,
                position: id as u32 - 1,
                is_fullscreen: false,
                is_system: false,
            })
            .collect();
        snap.displays = vec![DisplaySnapshot {
            id: DisplayId(1),
            frame: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            label: None,
            focused: true,
            is_main: true,
            generation: 0,
        }];
        engine.apply_event(Event::Snapshot(snap.clone()));

        assert_eq!(
            engine.focus_space_step(None, 1).unwrap(),
            vec![Action::FocusSpaceStep {
                target: SpaceId(2),
                delta: 1,
            }]
        );
        engine.apply_event(Event::Snapshot(snap.clone()));
        assert_eq!(
            engine.focus_space_step(None, 1).unwrap(),
            vec![Action::FocusSpaceStep {
                target: SpaceId(3),
                delta: 1,
            }]
        );

        engine.abandon_pending_space_cursors();
        engine.apply_event(Event::Snapshot(snap));
        assert_eq!(
            engine.focus_space_step(None, 1).unwrap(),
            vec![Action::FocusSpaceStep {
                target: SpaceId(2),
                delta: 1,
            }]
        );
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

#[cfg(test)]
#[path = "workspace_lifecycle_tests.rs"]
mod workspace_lifecycle_tests;
