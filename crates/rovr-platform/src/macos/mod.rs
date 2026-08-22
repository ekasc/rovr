pub mod reinject;
pub mod sa;

use std::ffi::{c_char, c_void, CStr};
use std::time::{Duration, Instant};

use rovr_core::Action;
use rovr_types::{
    Capabilities, DisplayId, DisplaySnapshot, PlatformSnapshot, ProcessId, Rect, SpaceId,
    SpaceSnapshot, WindowId, WindowSnapshot,
};

use crate::bounded_worker::BoundedWorker;
use crate::{Platform, PlatformError};

use reinject::{HelperClient, InjectionJob, ReinjectionPhase, Reinjector, TickAction};
use sa::{SaClient, SaInfo, OSAX_ATTRIB_ADD_SPACE, OSAX_ATTRIB_MOV_SPACE, OSAX_ATTRIB_REM_SPACE};

const ROVR_APP_MAX: usize = 256;
const ROVR_TITLE_MAX: usize = 512;
const ROVR_BUNDLE_MAX: usize = 256;

/// Maximum time the gesture path waits for a PREVIOUS swipe animation to
/// land before posting the next sequence. Waiting for actual landing (not a
/// fixed sleep) keeps successive switching fast: the gate exits as soon as
/// WindowServer reports the previous Space as current, typically ~250-350 ms
/// after the post. Posting mid-animation leaves WindowServer between Spaces
/// (blank screen), which is what this prevents.
const GESTURE_LAND_CAP: Duration = Duration::from_millis(450);
/// Poll interval while waiting for the previous animation to land.
const GESTURE_LAND_POLL: Duration = Duration::from_millis(40);
/// A previous post older than this is stale (idle-then-switch, manual Space
/// change): its animation landed long ago or will never be confirmed - never
/// wait on it.
const GESTURE_STALE_AGE: Duration = Duration::from_millis(1200);

/// Periodic SA health probes are throttled to this interval; freshness is
/// forced by clearing the throttle on Dock change / reinjection completion.
const SA_PROBE_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// Whether the next gesture must wait for the previous one to land: only when
/// a post is recorded AND recent.
fn gesture_settle_needed(posted_at: Option<Instant>, now: Instant) -> bool {
    posted_at.is_some_and(|at| now.duration_since(at) < GESTURE_STALE_AGE)
}

const ROVR_CAP_OBSERVE_WINDOWS: u64 = 1 << 0;
const ROVR_CAP_SET_WINDOW_FRAME: u64 = 1 << 1;
const ROVR_CAP_FOCUS_WINDOW: u64 = 1 << 2;
const ROVR_CAP_OBSERVE_SPACES: u64 = 1 << 3;
const ROVR_CAP_MOVE_WINDOW_TO_SPACE: u64 = 1 << 4;
const ROVR_CAP_FOCUS_SPACE: u64 = 1 << 5;

#[repr(C)]
#[derive(Clone, Copy)]
struct BridgeWindow {
    id: u32,
    pid: i32,
    display_id: u32,
    space_id: u64,
    focused: u8,
    minimized: u8,
    fullscreen: u8,
    managed: u8,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    app: [c_char; ROVR_APP_MAX],
    title: [c_char; ROVR_TITLE_MAX],
    bundle_id: [c_char; ROVR_BUNDLE_MAX],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BridgeDisplay {
    id: u32,
    focused: u8,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

type WindowCallback = unsafe extern "C" fn(*const BridgeWindow, *mut c_void);
type DisplayCallback = unsafe extern "C" fn(*const BridgeDisplay, *mut c_void);
type SpaceCallback = unsafe extern "C" fn(*const BridgeSpace, *mut c_void);

#[repr(C)]
#[derive(Clone, Copy)]
struct BridgeSpace {
    id: u64,
    display_id: u32,
    type_: i32,
    focused: u8,
    position: u32,
}

extern "C" {
    fn rovr_bridge_init() -> i32;
    fn rovr_bridge_capabilities() -> u64;
    fn rovr_bridge_enumerate_windows(callback: WindowCallback, context: *mut c_void) -> i32;
    fn rovr_bridge_enumerate_displays(callback: DisplayCallback, context: *mut c_void) -> i32;
    fn rovr_bridge_set_window_frame(window_id: u32, x: f64, y: f64, width: f64, height: f64)
        -> i32;
    fn rovr_bridge_focus_window(window_id: u32) -> i32;
    fn rovr_bridge_set_window_minimized(window_id: u32, minimized: i32) -> i32;
    fn rovr_bridge_needs_refresh() -> i32;
    fn rovr_bridge_enumerate_spaces(callback: SpaceCallback, context: *mut c_void) -> i32;
    fn rovr_bridge_move_window_to_space(window_id: u32, space_id: u64) -> i32;
    fn rovr_bridge_focus_space(space_id: u64) -> i32;
    fn rovr_bridge_current_space_id() -> u64;
    fn rovr_bridge_dock_pid() -> i32;
}

pub struct MacPlatform {
    bridge_capabilities: u64,
    sa: SaClient,
    sa_info: std::cell::RefCell<Option<SaInfo>>,
    last_dock_pid: std::cell::Cell<Option<i32>>,
    /// Target of the most recently posted focus gesture (0 = none pending).
    /// Used to detect that the previous Mission Control swipe animation has
    /// landed before posting another one.
    last_focus_target: std::cell::Cell<u64>,
    /// When that gesture was posted. The settle gate only applies while this
    /// is recent — see `focus_gate_should_wait`.
    last_focus_posted_at: std::cell::Cell<Option<Instant>>,
    last_sa_present: std::cell::Cell<bool>,
    /// When the SA was last probed. Probes are throttled so a hung payload
    /// cannot be re-contacted every tick; cleared whenever freshness matters
    /// (Dock change, reinjection finished).
    last_sa_probe_at: std::cell::Cell<Option<Instant>>,
    /// Automatic SA reinjection lifecycle (Dock change / handshake loss → one
    /// bounded privileged request per generation). Decision logic lives in
    /// `reinject::Reinjector`; the privileged helper does the injection.
    reinjector: std::cell::RefCell<Reinjector>,
    injection_job: InjectionJob,
    /// Blocker 2: ONE platform worker thread for all observation. AX/SkyLight
    /// hangs can never leak threads — repeated timeouts fail fast against the
    /// same lone worker and recovery is an explicit retry on it.
    snapshot_worker: BoundedWorker<Result<PlatformSnapshot, PlatformError>>,
}

#[derive(Debug, Clone)]
pub struct SaStatus {
    pub socket_path: std::path::PathBuf,
    pub present: bool,
    pub version: Option<String>,
    pub attribs: Option<u32>,
    pub compatible: bool,
}

impl MacPlatform {
    pub fn new() -> Result<Self, PlatformError> {
        let status = unsafe { rovr_bridge_init() };
        if status != 0 {
            return Err(PlatformError::Operation(format!(
                "macOS bridge initialization failed with status {status}"
            )));
        }
        let bridge_capabilities = unsafe { rovr_bridge_capabilities() };
        let sa = SaClient::new();
        let sa_info = sa.probe();
        let last_sa_present = std::cell::Cell::new(sa_info.is_some());
        let last_dock_pid = std::cell::Cell::new({
            let pid = unsafe { rovr_bridge_dock_pid() };
            if pid > 0 {
                Some(pid)
            } else {
                None
            }
        });
        Ok(Self {
            bridge_capabilities,
            sa,
            sa_info: std::cell::RefCell::new(sa_info),
            last_dock_pid,
            last_sa_present,
            last_sa_probe_at: std::cell::Cell::new(None),
            last_focus_target: std::cell::Cell::new(0),
            last_focus_posted_at: std::cell::Cell::new(None),
            reinjector: std::cell::RefCell::new(Reinjector::new()),
            injection_job: InjectionJob::default(),
            snapshot_worker: BoundedWorker::new(
                crate::bounded_worker::DEFAULT_JOB_TIMEOUT,
                crate::bounded_worker::DEFAULT_RETRY_INTERVAL,
            ),
        })
    }

    /// How long the observation worker has been wedged, if it has.
    pub fn snapshot_wedged_ms(&self) -> Option<u64> {
        self.snapshot_worker
            .wedged_since()
            .map(|since| since.elapsed().as_millis() as u64)
    }

    /// Diagnostics for the automatic SA reinjection lifecycle (`rovr doctor`).
    pub fn sa_reinject_diagnostics(&self) -> crate::SaReinjectDiag {
        let st = self.reinjector.borrow().status();
        crate::SaReinjectDiag {
            phase: st.phase.as_str(),
            generation: st.generation,
            dock_pid: st.dock_pid,
            attempts_this_generation: st.attempts_this_generation,
            retry_in_secs: st.retry_in_secs,
            pending: matches!(
                st.phase,
                ReinjectionPhase::Injecting | ReinjectionPhase::Verifying
            ),
            last_result: st.last_result,
            last_error: st.last_error,
            helper_socket: reinject::HELPER_SOCKET_PATH.to_string(),
        }
    }

    /// One reinjection-lifecycle tick: consume any finished background job,
    /// feed Dock PID + SA health into the pure state machine, and dispatch at
    /// most ONE privileged request when it asks for one (single-flight guard).
    /// Everything here is bounded; failures degrade to backoff, never to a
    /// tight loop or an unbounded thread pile-up.
    fn drive_reinjection(&self, dock_pid: Option<i32>, sa_alive: bool) {
        let now = Instant::now();
        if !self.injection_job.is_inflight() {
            if let Some(result) = self.injection_job.poll() {
                self.reinjector.borrow_mut().injection_finished(now, result);
                // Freshness matters after a reinjection: probe immediately.
                self.last_sa_probe_at.set(None);
            }
        }
        let action = self
            .reinjector
            .borrow_mut()
            .observe(now, dock_pid, sa_alive);
        if action == TickAction::RequestInjection && !self.injection_job.is_inflight() {
            let client = HelperClient::new();
            let spawned =
                self.injection_job
                    .spawn(move || match client.inject(Duration::from_secs(15)) {
                        Ok(_) => Ok(()),
                        Err(err) => Err(err.to_string()),
                    });
            if spawned {
                self.reinjector.borrow_mut().injection_started();
            }
        }
    }

    pub fn sa_status(&self) -> SaStatus {
        let info = self.sa_info.borrow();
        SaStatus {
            socket_path: self.sa.socket_path().clone(),
            present: info.is_some(),
            version: info.as_ref().map(|i| i.version.clone()),
            attribs: info.as_ref().map(|i| i.attribs),
            compatible: info.as_ref().is_some_and(|i| i.is_compatible()),
        }
    }

    fn sa_attribs(&self) -> u32 {
        self.sa_info
            .borrow()
            .as_ref()
            .map(|info| info.attribs)
            .unwrap_or(0)
    }

    fn execute_sa(
        &self,
        op: impl Fn(&SaClient) -> Result<(), sa::SaError>,
    ) -> Result<(), PlatformError> {
        if self.sa_info.borrow().is_none() {
            return Err(PlatformError::Unsupported("scripting_addition"));
        }
        op(&self.sa).map_err(|err| PlatformError::Operation(err.to_string()))
    }

    fn execute_focus_space(&self, space: &SpaceId) -> Result<(), PlatformError> {
        // Prefer the SA's clean focus (no gesture, no animation) when the
        // payload is live; fall back to gesture synthesis otherwise.
        if self.sa_info.borrow().is_some() && self.sa.focus_space(space.0).is_ok() {
            self.last_focus_target.set(space.0);
            return Ok(());
        }
        // Gesture path: posting a swipe sequence while the previous Mission
        // Control animation is still in flight leaves WindowServer between
        // Spaces (blank screen). Wait — bounded — until the previous target
        // has actually landed, exiting the moment it does. A stale previous
        // post (idle-then-switch, manual Space change) never waits.
        let now = Instant::now();
        let t_start = now;
        let mut settled_ms = 0u64;
        if gesture_settle_needed(self.last_focus_posted_at.get(), now) {
            let target = self.last_focus_target.get();
            if target != 0 {
                let deadline = now + GESTURE_LAND_CAP;
                while unsafe { rovr_bridge_current_space_id() } != target {
                    if Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(GESTURE_LAND_POLL);
                }
                settled_ms = t_start.elapsed().as_millis() as u64;
            }
        }
        let t_pre_post = Instant::now();
        let status = unsafe { rovr_bridge_focus_space(space.0) };
        let post_ms = t_pre_post.elapsed().as_millis() as u64;
        tracing::debug!(
            space = space.0,
            settled_ms,
            post_ms,
            status,
            total_ms = t_start.elapsed().as_millis() as u64,
            "focus_space timing"
        );
        if status == 0 {
            self.last_focus_target.set(space.0);
            self.last_focus_posted_at.set(Some(Instant::now()));
            Ok(())
        } else {
            Err(PlatformError::Operation(format!(
                "focus space failed with status {status}"
            )))
        }
    }

    fn snapshot_inner(bridge_capabilities: u64) -> Result<PlatformSnapshot, PlatformError> {
        unsafe extern "C" fn collect_window(window: *const BridgeWindow, context: *mut c_void) {
            if window.is_null() || context.is_null() {
                return;
            }
            let window = *window;
            let windows = &mut *(context as *mut Vec<WindowSnapshot>);
            let bundle_id = c_string(&window.bundle_id);
            let minimized = match window.minimized {
                0 => rovr_types::ObservedBool::No,
                1 => rovr_types::ObservedBool::Yes,
                _ => rovr_types::ObservedBool::Unknown,
            };
            let fullscreen = match window.fullscreen {
                0 => rovr_types::ObservedBool::No,
                1 => rovr_types::ObservedBool::Yes,
                _ => rovr_types::ObservedBool::Unknown,
            };
            let managed = match window.managed {
                0 => rovr_types::ObservedBool::No,
                1 => rovr_types::ObservedBool::Yes,
                _ => rovr_types::ObservedBool::Unknown,
            };
            windows.push(WindowSnapshot {
                id: WindowId(window.id),
                pid: ProcessId(window.pid),
                app: c_string(&window.app).unwrap_or_default(),
                bundle_id,
                title: c_string(&window.title).unwrap_or_default(),
                frame: Rect {
                    x: window.x,
                    y: window.y,
                    width: window.width,
                    height: window.height,
                },
                space_id: (window.space_id != 0).then_some(SpaceId(window.space_id)),
                display_id: (window.display_id != 0).then_some(DisplayId(window.display_id)),
                focused: window.focused != 0,
                minimized,
                fullscreen,
                managed,
                generation: 0,
            });
        }
        unsafe extern "C" fn collect_display(display: *const BridgeDisplay, context: *mut c_void) {
            if display.is_null() || context.is_null() {
                return;
            }
            let display = *display;
            let displays = &mut *(context as *mut Vec<DisplaySnapshot>);
            displays.push(DisplaySnapshot {
                id: DisplayId(display.id),
                frame: Rect {
                    x: display.x,
                    y: display.y,
                    width: display.width,
                    height: display.height,
                },
                label: None,
                focused: display.focused != 0,
                generation: 0,
            });
        }
        unsafe extern "C" fn collect_space(space: *const BridgeSpace, context: *mut c_void) {
            if space.is_null() || context.is_null() {
                return;
            }
            let space = *space;
            let spaces = &mut *(context as *mut Vec<SpaceSnapshot>);
            spaces.push(SpaceSnapshot {
                id: SpaceId(space.id),
                display_id: DisplayId(space.display_id),
                label: None,
                focused: space.focused != 0,
                generation: 0,
                position: space.position,
            });
        }
        let mut windows = Vec::new();
        let window_status = unsafe {
            rovr_bridge_enumerate_windows(collect_window, &mut windows as *mut _ as *mut c_void)
        };
        if window_status != 0 {
            return Err(PlatformError::Operation(format!(
                "window enumeration failed with status {window_status}"
            )));
        }
        let mut displays = Vec::new();
        let display_status = unsafe {
            rovr_bridge_enumerate_displays(collect_display, &mut displays as *mut _ as *mut c_void)
        };
        if display_status != 0 {
            return Err(PlatformError::Operation(format!(
                "display enumeration failed with status {display_status}"
            )));
        }
        let mut spaces = Vec::new();
        if bridge_capabilities & ROVR_CAP_OBSERVE_SPACES != 0 {
            let space_status = unsafe {
                rovr_bridge_enumerate_spaces(collect_space, &mut spaces as *mut _ as *mut c_void)
            };
            if space_status != 0 {
                return Err(PlatformError::Operation(format!(
                    "space enumeration failed with status {space_status}"
                )));
            }
        }
        Ok(PlatformSnapshot {
            windows,
            spaces,
            displays,
            complete: true,
        })
    }
}

impl Platform for MacPlatform {
    fn capabilities(&self) -> Capabilities {
        let sa_attribs = self.sa_attribs();
        Capabilities {
            observe_windows: self.bridge_capabilities & ROVR_CAP_OBSERVE_WINDOWS != 0,
            set_window_frame: self.bridge_capabilities & ROVR_CAP_SET_WINDOW_FRAME != 0,
            focus_window: self.bridge_capabilities & ROVR_CAP_FOCUS_WINDOW != 0,
            move_window_to_space: self.bridge_capabilities & ROVR_CAP_MOVE_WINDOW_TO_SPACE != 0,
            create_space: sa_attribs & OSAX_ATTRIB_ADD_SPACE != 0,
            destroy_space: sa_attribs & OSAX_ATTRIB_REM_SPACE != 0,
            focus_space: self.bridge_capabilities & ROVR_CAP_FOCUS_SPACE != 0,
            reorder_space: sa_attribs & OSAX_ATTRIB_MOV_SPACE != 0,
            set_window_layer: self.sa_info.borrow().is_some(),
            set_window_sticky: self.sa_info.borrow().is_some(),
            set_window_shadow: self.sa_info.borrow().is_some(),
            set_window_opacity: self.sa_info.borrow().is_some(),
            set_window_scale: self.sa_info.borrow().is_some(),
            scripting_addition: self.sa_info.borrow().is_some(),
        }
    }

    fn snapshot(&mut self) -> Result<PlatformSnapshot, PlatformError> {
        // Blocker 2: bounded observation through the SINGLE platform worker.
        // A hung AX/SkyLight call wedges exactly one thread; callers time out
        // (2 s) or fail fast, and periodic reconciliation can never spawn
        // additional workers. Recovery retries on the same worker after the
        // retry interval; stale responses are discarded by epoch.
        let bridge_capabilities = self.bridge_capabilities;
        match self
            .snapshot_worker
            .run(move || Self::snapshot_inner(bridge_capabilities))
        {
            Ok(inner) => inner,
            Err(err) => Err(PlatformError::Operation(format!(
                "snapshot unavailable: {err}"
            ))),
        }
    }

    fn execute(&mut self, action: &Action) -> Result<(), PlatformError> {
        let status = match action {
            Action::RefreshAll | Action::RefreshWindow { .. } => return Ok(()),
            Action::SetWindowFrame { window, frame } => unsafe {
                rovr_bridge_set_window_frame(window.0, frame.x, frame.y, frame.width, frame.height)
            },
            Action::FocusWindow { window } => unsafe { rovr_bridge_focus_window(window.0) },
            Action::SetWindowMinimized { window, minimized } => unsafe {
                rovr_bridge_set_window_minimized(window.0, if *minimized { 1 } else { 0 })
            },
            Action::MoveWindowToSpace { window, space } => unsafe {
                rovr_bridge_move_window_to_space(window.0, space.0)
            },
            Action::FocusDirection { .. } => {
                return Err(PlatformError::Unsupported("focus_direction"))
            }
            Action::FocusSpace { space } => {
                return self.execute_focus_space(space);
            }
            Action::CreateSpace { anchor } => {
                return self.execute_sa(|sa| sa.create_space(anchor.0));
            }
            Action::DestroySpace { space } => {
                return self.execute_sa(|sa| sa.destroy_space(space.0));
            }
            Action::MoveSpace { space, after } => {
                return self.execute_sa(|sa| sa.move_space(space.0, after.0));
            }
            Action::SetWindowLayer { window, layer } => {
                return self.execute_sa(|sa| sa.set_layer(window.0, *layer));
            }
            Action::SetWindowSticky { window, sticky } => {
                return self.execute_sa(|sa| sa.set_sticky(window.0, *sticky));
            }
            Action::SetWindowShadow { window, shadow } => {
                return self.execute_sa(|sa| sa.set_shadow(window.0, *shadow));
            }
            Action::SetWindowOpacity {
                window,
                opacity,
                duration_ms,
            } => {
                return self.execute_sa(|sa| {
                    sa.set_opacity(window.0, *opacity as f32, *duration_ms as f32 / 1000.0)
                });
            }
            Action::SetWindowScale { window, x, y, w, h } => {
                return self.execute_sa(|sa| sa.scale_window(window.0, *x, *y, *w, *h));
            }
        };

        if status == 0 {
            Ok(())
        } else {
            Err(PlatformError::Operation(format!(
                "bridge operation failed with status {status}"
            )))
        }
    }

    fn needs_refresh(&self) -> bool {
        self.needs_refresh_inner()
    }

    fn sa_reinject_diagnostics(&self) -> Option<crate::SaReinjectDiag> {
        Some(MacPlatform::sa_reinject_diagnostics(self))
    }
}

impl MacPlatform {
    fn needs_refresh_inner(&self) -> bool {
        let mut needs = unsafe { rovr_bridge_needs_refresh() != 0 };
        // Dock restart detection: pid change means spaces/windows were rebuilt
        let current_dock = {
            let pid = unsafe { rovr_bridge_dock_pid() };
            if pid > 0 {
                Some(pid)
            } else {
                None
            }
        };
        if current_dock != self.last_dock_pid.get() {
            self.last_dock_pid.set(current_dock);
            // Freshness matters after a Dock change: probe immediately.
            self.last_sa_probe_at.set(None);
            needs = true;
        }
        // SA payload probe: refresh the cached identity whenever version or
        // capability attribs change (not just presence), so capabilities and
        // reported version follow a reinjection into a new Dock.
        //
        // Probes are throttled and use a SHORT deadline: a payload that is
        // alive answers in microseconds, and one that is wedged must never
        // monopolize the state loop. Between probes the cached state stands
        // in, so throttled ticks neither flip presence nor lose capabilities.
        let now = Instant::now();
        let probe_due = self
            .last_sa_probe_at
            .get()
            .map_or(true, |at| now.duration_since(at) >= SA_PROBE_MIN_INTERVAL);
        if probe_due {
            self.last_sa_probe_at.set(Some(now));
            if let Some(fresh) = self.sa.probe_health() {
                let changed = self
                    .sa_info
                    .borrow()
                    .as_ref()
                    .map(|cached| {
                        cached.version != fresh.version || cached.attribs != fresh.attribs
                    })
                    .unwrap_or(true);
                if changed {
                    *self.sa_info.borrow_mut() = Some(fresh);
                }
            } else if self.sa_info.borrow().is_some() {
                // Live cache went stale: the payload disappeared.
                *self.sa_info.borrow_mut() = None;
            }
        }
        let current_sa = self.sa_info.borrow().is_some();
        if current_sa != self.last_sa_present.get() {
            self.last_sa_present.set(current_sa);
            needs = true;
        }
        // Automatic reinjection lifecycle: Dock PID change, SA loss or failed
        // handshake feed the bounded state machine; it requests privileged
        // injection at most once per generation with backoff. Non-SA Rovr
        // functionality is unaffected by any failure here.
        self.drive_reinjection(current_dock, current_sa);
        needs
    }
}

fn c_string<const N: usize>(buffer: &[c_char; N]) -> Option<String> {
    if buffer[0] == 0 {
        return None;
    }
    let value = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(value.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settle gate: waits only for a RECENT previous post. Successive
    /// switching exits as soon as the animation lands (bounded), and an
    /// idle-then-switch never waits at all.
    #[test]
    fn gesture_settle_only_for_recent_posts() {
        let now = Instant::now();
        // No previous post: never wait (daemon startup path).
        assert!(!gesture_settle_needed(None, now));
        // Posted long ago: never wait — the "first switch after idle"
        // regression; a stale timestamp must not stall anything.
        let ancient = now - Duration::from_secs(60);
        assert!(!gesture_settle_needed(Some(ancient), now));
        // Just past the stale age: no wait.
        let just_old = now - GESTURE_STALE_AGE - Duration::from_millis(1);
        assert!(!gesture_settle_needed(Some(just_old), now));
        // Recent posts DO settle-wait (blank-screen protection).
        assert!(gesture_settle_needed(
            Some(now - Duration::from_millis(50)),
            now
        ));
        assert!(gesture_settle_needed(Some(now), now));
    }
}
