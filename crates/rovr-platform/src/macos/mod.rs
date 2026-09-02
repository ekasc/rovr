pub mod reinject;
pub mod sa;

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{c_char, c_void, CStr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rovr_core::Action;
use rovr_types::{
    Capabilities, DisplayId, DisplaySnapshot, PlatformSnapshot, ProcessId, Rect, SpaceId,
    SpaceSnapshot, WindowId, WindowSnapshot,
};

use crate::bounded_worker::BoundedWorker;
use crate::{Platform, PlatformDiagnostic, PlatformError};

use reinject::{HelperClient, InjectionJob, ReinjectionPhase, Reinjector, TickAction};
use sa::{
    SaClient, SaInfo, OSAX_ATTRIB_ADD_SPACE, OSAX_ATTRIB_FOCUS_SPACE, OSAX_ATTRIB_MOV_SPACE,
    OSAX_ATTRIB_REM_SPACE, OSAX_ATTRIB_WINDOW_LAYER, OSAX_ATTRIB_WINDOW_OPACITY,
    OSAX_ATTRIB_WINDOW_SCALE, OSAX_ATTRIB_WINDOW_SHADOW, OSAX_ATTRIB_WINDOW_STICKY,
};

// AX event plumbing: the C observer handler runs on the main thread and
// calls the trampoline below. Events accumulate as a bitmask consumed by
// needs_refresh(); an optional watcher (registered by the daemon) receives the
// event kind synchronously so the state loop can decide whether to wake immediately.
static PENDING_EVENTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static EVENT_WATCHER: std::sync::OnceLock<std::sync::Arc<dyn Fn(u32) + Send + Sync>> =
    std::sync::OnceLock::new();

#[allow(non_camel_case_types)]
pub type rovr_ax_event_trampoline_fn = extern "C" fn(event_kind: i32, window_id: u32);

extern "C" fn rovr_ax_event_trampoline(event_kind: i32, _window_id: u32) {
    PENDING_EVENTS.fetch_or(event_kind as u32, std::sync::atomic::Ordering::SeqCst);
    if let Some(watcher) = EVENT_WATCHER.get() {
        watcher(event_kind as u32);
    }
}

const ROVR_APP_MAX: usize = 256;
const ROVR_TITLE_MAX: usize = 512;
const ROVR_BUNDLE_MAX: usize = 256;
const AX_JOB_TIMEOUT: Duration = Duration::from_millis(500);

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

fn valid_space_step_delta(delta: i32) -> bool {
    delta != 0
}

#[allow(dead_code)]
fn focused_space_for_display(spaces: &[SpaceSnapshot], display: DisplayId) -> Option<SpaceId> {
    spaces
        .iter()
        .filter(|space| space.display_id == display && space.focused)
        .min_by_key(|space| (space.position, space.id))
        .map(|space| space.id)
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
struct BridgeAxWindow {
    id: u32,
    focused: u8,
    minimized: u8,
    fullscreen: u8,
    managed: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BridgeDisplay {
    id: u32,
    focused: u8,
    is_main: u8,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

type WindowCallback = unsafe extern "C" fn(*const BridgeWindow, *mut c_void);
type AxWindowCallback = unsafe extern "C" fn(*const BridgeAxWindow, *mut c_void);
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
    is_system: u8,
}

extern "C" {
    fn rovr_bridge_init() -> i32;
    fn rovr_bridge_capabilities() -> u64;
    fn rovr_bridge_enumerate_window_candidates(
        callback: WindowCallback,
        context: *mut c_void,
    ) -> i32;
    fn rovr_bridge_refine_windows_for_pid(
        pid: i32,
        callback: AxWindowCallback,
        context: *mut c_void,
    ) -> i32;
    fn rovr_bridge_enumerate_displays(callback: DisplayCallback, context: *mut c_void) -> i32;
    fn rovr_bridge_set_window_frame(window_id: u32, x: f64, y: f64, width: f64, height: f64)
        -> i32;
    fn rovr_bridge_focus_window(window_id: u32) -> i32;
    fn rovr_bridge_set_window_minimized(window_id: u32, minimized: i32) -> i32;
    fn rovr_bridge_close_window(window_id: u32) -> i32;
    fn rovr_bridge_toggle_fullscreen(window_id: u32) -> i32;
    fn rovr_bridge_install_event_handlers(callback: Option<rovr_ax_event_trampoline_fn>);
    fn rovr_bridge_needs_refresh() -> i32;
    fn rovr_bridge_enumerate_spaces(callback: SpaceCallback, context: *mut c_void) -> i32;
    fn rovr_bridge_move_window_to_space(window_id: u32, space_id: u64) -> i32;
    fn rovr_bridge_focus_space(space_id: u64) -> i32;
    fn rovr_bridge_focus_space_step(target_space_id: u64, delta: i32) -> i32;
    fn rovr_bridge_window_space_id(window_id: u32) -> u64;
    fn rovr_bridge_window_pid(window_id: u32) -> i32;
    fn rovr_bridge_current_space_for_space(space_id: u64) -> u64;
    fn rovr_bridge_display_for_space(space_id: u64) -> u32;
    fn rovr_bridge_space_is_fullscreen(space_id: u64) -> i32;
    fn rovr_bridge_space_is_system(space_id: u64) -> i32;
    fn rovr_bridge_is_display_animating(display_id: u32) -> i32;
    fn rovr_bridge_sls_managed_for_window(window_id: u32) -> i32;
    fn rovr_bridge_dock_pid() -> i32;
}

#[derive(Clone, Copy)]
struct AxRefinement {
    id: u32,
    focused: bool,
    minimized: u8,
    fullscreen: u8,
    managed: u8,
}

enum AxWorkerResult {
    Refinements(Vec<AxRefinement>),
    Mutation(i32),
}

struct AxWorkerPool {
    workers: HashMap<i32, BoundedWorker<AxWorkerResult>>,
    diagnostics: VecDeque<PlatformDiagnostic>,
}

const AX_DIAGNOSTIC_CAPACITY: usize = 256;

impl Default for AxWorkerPool {
    fn default() -> Self {
        Self {
            workers: HashMap::new(),
            diagnostics: VecDeque::with_capacity(AX_DIAGNOSTIC_CAPACITY),
        }
    }
}

impl AxWorkerPool {
    fn worker(&mut self, pid: i32) -> &BoundedWorker<AxWorkerResult> {
        self.workers.entry(pid).or_insert_with(|| {
            BoundedWorker::new(
                AX_JOB_TIMEOUT,
                crate::bounded_worker::DEFAULT_RETRY_INTERVAL,
            )
        })
    }

    fn refine(&mut self, pids: &HashSet<i32>) -> Vec<AxRefinement> {
        self.workers
            .retain(|pid, worker| pids.contains(pid) || !worker.is_idle());
        let deadline = Instant::now() + AX_JOB_TIMEOUT;
        let mut submitted = Vec::with_capacity(pids.len());
        for &pid in pids {
            let submission = self.worker(pid).submit(move || {
                unsafe extern "C" fn collect(value: *const BridgeAxWindow, context: *mut c_void) {
                    if value.is_null() || context.is_null() {
                        return;
                    }
                    let value = *value;
                    (&mut *(context as *mut Vec<AxRefinement>)).push(AxRefinement {
                        id: value.id,
                        focused: value.focused != 0,
                        minimized: value.minimized,
                        fullscreen: value.fullscreen,
                        managed: value.managed,
                    });
                }
                let mut values = Vec::new();
                let status = unsafe {
                    rovr_bridge_refine_windows_for_pid(
                        pid,
                        collect,
                        &mut values as *mut _ as *mut c_void,
                    )
                };
                if status != 0 {
                    values.clear();
                }
                AxWorkerResult::Refinements(values)
            });
            match submission {
                Ok(epoch) => submitted.push((pid, epoch)),
                Err(err) => self.record_diagnostic(
                    "ax.worker_unavailable",
                    format!("pid={pid} operation=refine error={err}"),
                ),
            }
        }

        let mut refinements = Vec::new();
        for (pid, epoch) in submitted {
            match self.worker(pid).wait_until(epoch, deadline) {
                Ok(AxWorkerResult::Refinements(mut values)) => {
                    refinements.append(&mut values);
                }
                Ok(AxWorkerResult::Mutation(_)) => {
                    unreachable!("worker epoch returned wrong job kind")
                }
                Err(err) => self.record_diagnostic(
                    "ax.refine_timeout",
                    format!("pid={pid} operation=refine error={err}"),
                ),
            }
        }
        refinements
    }

    fn record_diagnostic(&mut self, kind: &'static str, detail: String) {
        if self.diagnostics.len() == AX_DIAGNOSTIC_CAPACITY {
            self.diagnostics.pop_front();
        }
        self.diagnostics
            .push_back(PlatformDiagnostic { kind, detail });
    }

    fn drain_diagnostics(&mut self) -> Vec<PlatformDiagnostic> {
        self.diagnostics.drain(..).collect()
    }

    fn mutate(
        &mut self,
        pid: i32,
        operation: &'static str,
        mutation: impl FnOnce() -> i32 + Send + 'static,
    ) -> Result<i32, crate::bounded_worker::BoundedError> {
        let worker = self.worker(pid);
        let result = worker
            .submit(move || AxWorkerResult::Mutation(mutation()))
            .and_then(|epoch| worker.wait_until(epoch, Instant::now() + AX_JOB_TIMEOUT));
        match result {
            Ok(AxWorkerResult::Mutation(status)) => Ok(status),
            Ok(AxWorkerResult::Refinements(_)) => {
                unreachable!("worker epoch returned wrong job kind")
            }
            Err(err) => {
                self.record_diagnostic(
                    "ax.mutation_timeout",
                    format!("pid={pid} operation={operation} error={err}"),
                );
                Err(err)
            }
        }
    }
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
    /// One global snapshot coordinator. Per-application AX IPC is delegated to
    /// the persistent PID-keyed workers below.
    snapshot_worker: BoundedWorker<Result<PlatformSnapshot, PlatformError>>,
    ax_workers: Arc<Mutex<AxWorkerPool>>,
    last_known_minimized: std::cell::RefCell<HashMap<WindowId, rovr_types::ObservedBool>>,
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
        unsafe {
            rovr_bridge_install_event_handlers(Some(rovr_ax_event_trampoline));
        }
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
            ax_workers: Arc::new(Mutex::new(AxWorkerPool::default())),
            last_known_minimized: std::cell::RefCell::new(HashMap::new()),
        })
    }

    fn set_event_watcher(&mut self, event_kind_watcher: std::sync::Arc<dyn Fn(u32) + Send + Sync>) {
        let _ = EVENT_WATCHER.set(event_kind_watcher);
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
            .filter(|info| info.is_compatible())
            .map(|info| info.attribs)
            .unwrap_or(0)
    }

    fn execute_sa(
        &self,
        required_capability: u32,
        name: &'static str,
        op: impl Fn(&SaClient) -> Result<(), sa::SaError>,
    ) -> Result<(), PlatformError> {
        let info = self.sa_info.borrow();
        if !info
            .as_ref()
            .is_some_and(|info| info.is_compatible() && info.attribs & required_capability != 0)
        {
            return Err(PlatformError::Unsupported(name));
        }
        drop(info);
        op(&self.sa).map_err(|err| PlatformError::Operation(err.to_string()))
    }

    fn execute_focus_space(&self, space: &SpaceId) -> Result<(), PlatformError> {
        // A logical workspace focus is ID-exact. When the SA is available,
        // never fall back to positional gestures after a transient failure:
        // deletion compacts Mission Control positions, so that fallback can
        // land alt-2 on desktop 3/4. Freshly created Spaces can take a
        // noticeable moment to appear in Dock's internal `dock_spaces`
        // model — `space_for_display_with_id` returns nil and the SA focus
        // returns false until the Dock data structure catches up. Retry
        // the exact ID for up to a couple of seconds so a spawn-and-focus
        // that the user issued a few hundred ms apart still lands without
        // a hotkey re-press.
        if self.sa_attribs() & OSAX_ATTRIB_FOCUS_SPACE != 0 {
            const SA_FOCUS_ATTEMPTS: u32 = 30;
            const SA_FOCUS_INTERVAL: Duration = Duration::from_millis(100);
            let mut last_error = None;
            for attempt in 0..SA_FOCUS_ATTEMPTS {
                match self.sa.focus_space(space.0) {
                    Ok(()) => {
                        self.last_focus_target.set(space.0);
                        return Ok(());
                    }
                    Err(err) => last_error = Some(err),
                }
                if attempt + 1 < SA_FOCUS_ATTEMPTS {
                    std::thread::sleep(SA_FOCUS_INTERVAL);
                }
            }
            // SA claimed failure after a 3 second retry window. The most
            // common cause is a Space the SA's `dock_spaces` cache has not
            // yet observed. Fall through to the gesture path so the user
            // gets a working switch instead of a silent no-op: the bridge
            // posts a swipe via the WindowServer, which goes through
            // Mission Control directly and reaches even brand-new Spaces.
            tracing::debug!(
                space = space.0,
                error = ?last_error,
                "SA focus exhausted; falling back to gesture"
            );
        }

        // SA absent: gesture focus is the best available degraded path.
        // Gesture path: posting a swipe sequence while the previous Mission
        // Control animation is still in flight leaves WindowServer between
        // Spaces (blank screen). Wait — bounded — until the previous target
        // has actually landed, exiting the moment it does. A stale previous
        // post (idle-then-switch, manual Space change) never waits.
        // CROSS-DISPLAY posts skip the gate: the previous animation runs on
        // the OTHER display and cannot leave this display between Spaces, so
        // waiting only added latency to external-display switches.
        let now = Instant::now();
        let t_start = now;
        let mut settled_ms = 0u64;
        let prev_target = self.last_focus_target.get();
        let cross_display = prev_target != 0
            && unsafe { rovr_bridge_display_for_space(space.0) }
                != unsafe { rovr_bridge_display_for_space(prev_target) };
        if gesture_settle_needed(self.last_focus_posted_at.get(), now)
            && !cross_display
            && prev_target != 0
        {
            let deadline = now + GESTURE_LAND_CAP;
            while unsafe { rovr_bridge_current_space_for_space(prev_target) } != prev_target {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(GESTURE_LAND_POLL);
            }
            settled_ms = t_start.elapsed().as_millis() as u64;
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

    fn execute_focus_space_step(&self, target: &SpaceId, delta: i32) -> Result<(), PlatformError> {
        if !valid_space_step_delta(delta) {
            return Err(PlatformError::Operation(
                "space step displacement must be nonzero".to_string(),
            ));
        }
        if self.sa_attribs() & OSAX_ATTRIB_FOCUS_SPACE != 0 {
            // Same Dock-catchup retry as execute_focus_space: a freshly
            // created target Space may not yet be wired into `dock_spaces`.
            const ATTEMPTS: u32 = 30;
            const INTERVAL: Duration = Duration::from_millis(100);
            for _ in 0..ATTEMPTS {
                if self.sa.focus_space(target.0).is_ok() {
                    self.last_focus_target.set(target.0);
                    self.last_focus_posted_at.set(Some(Instant::now()));
                    return Ok(());
                }
                std::thread::sleep(INTERVAL);
            }
        }

        let status = unsafe { rovr_bridge_focus_space_step(target.0, delta) };
        if status == 0 {
            self.last_focus_target.set(target.0);
            self.last_focus_posted_at.set(Some(Instant::now()));
            Ok(())
        } else {
            Err(PlatformError::Operation(format!(
                "focus space step failed with status {status}"
            )))
        }
    }

    fn ensure_bridge_idle(&self) -> Result<(), PlatformError> {
        self.snapshot_worker
            .run(|| Ok(PlatformSnapshot::default()))
            .map(|_| ())
            .map_err(|err| {
                PlatformError::Operation(format!("Objective-C bridge unavailable: {err}"))
            })
    }

    fn snapshot_inner(
        bridge_capabilities: u64,
        ax_workers: &Mutex<AxWorkerPool>,
    ) -> Result<PlatformSnapshot, PlatformError> {
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
                is_main: display.is_main != 0,
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
                is_fullscreen: space.type_ == 4,
                is_system: space.is_system != 0,
            });
        }
        let mut windows: Vec<WindowSnapshot> = Vec::new();
        let window_status = unsafe {
            rovr_bridge_enumerate_window_candidates(
                collect_window,
                &mut windows as *mut _ as *mut c_void,
            )
        };
        if window_status != 0 {
            return Err(PlatformError::Operation(format!(
                "window enumeration failed with status {window_status}"
            )));
        }
        let pids: HashSet<i32> = windows
            .iter()
            .filter(|window| {
                window.space_id.is_some() || (window.frame.height > 300.0 && window.frame.y < 100.0)
            })
            .map(|window| window.pid.0)
            .collect();
        let refinements = if pids.is_empty() {
            Vec::new()
        } else {
            ax_workers
                .lock()
                .map_err(|_| PlatformError::Operation("AX worker pool poisoned".to_string()))?
                .refine(&pids)
        };
        let refinements: HashMap<u32, AxRefinement> = refinements
            .into_iter()
            .map(|refinement| (refinement.id, refinement))
            .collect();
        for window in &mut windows {
            let Some(refinement) = refinements.get(&window.id.0) else {
                continue;
            };
            window.focused = refinement.focused;
            window.minimized = observed_bool(refinement.minimized);
            window.fullscreen = observed_bool(refinement.fullscreen);
            window.managed = observed_bool(refinement.managed);
        }
        // SLS fallback for background apps where AX returns Unknown — uses level/parent (yabai). Keeps tiling on multi-display without frontmost.
        for window in &mut windows {
            if window.managed == rovr_types::ObservedBool::Unknown {
                let sls = unsafe { rovr_bridge_sls_managed_for_window(window.id.0) };
                match sls {
                    0 => window.managed = rovr_types::ObservedBool::No,
                    1 => window.managed = rovr_types::ObservedBool::Yes,
                    _ => {}
                }
            }
        }
        let mut displays: Vec<DisplaySnapshot> = Vec::new();
        let display_status = unsafe {
            rovr_bridge_enumerate_displays(collect_display, &mut displays as *mut _ as *mut c_void)
        };
        if display_status != 0 {
            return Err(PlatformError::Operation(format!(
                "display enumeration failed with status {display_status}"
            )));
        }
        let mut spaces: Vec<SpaceSnapshot> = Vec::new();
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
        // Per-display focused spaces: the bridge now reports one focused space
        // per display via SLSManagedDisplayGetCurrentSpace (multi-display
        // correct). Keep them as-is; do not collapse to a single global
        // focused space, which would discard the secondary display's current
        // space and break workspace/tiling on that display.
        Ok(PlatformSnapshot {
            windows,
            spaces,
            displays,
            complete: true,
        })
    }

    fn execute_ax_mutation(
        &self,
        window: WindowId,
        operation: &'static str,
        mutation: impl FnOnce() -> i32 + Send + 'static,
    ) -> Result<(), PlatformError> {
        let pid = unsafe { rovr_bridge_window_pid(window.0) };
        if pid <= 0 {
            return Err(PlatformError::Operation(format!(
                "window {} no longer has an owning process",
                window.0
            )));
        }
        let status = self
            .ax_workers
            .lock()
            .map_err(|_| PlatformError::Operation("AX worker pool poisoned".to_string()))?
            .mutate(pid, operation, mutation)
            .map_err(|err| {
                PlatformError::Operation(format!(
                    "AX mutation unavailable: pid={pid} window={} operation={operation}: {err}",
                    window.0
                ))
            })?;
        if status == 0 {
            Ok(())
        } else {
            Err(PlatformError::Operation(format!(
                "bridge operation failed with status {status}"
            )))
        }
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
            set_window_layer: sa_attribs & OSAX_ATTRIB_WINDOW_LAYER != 0,
            set_window_sticky: sa_attribs & OSAX_ATTRIB_WINDOW_STICKY != 0,
            set_window_shadow: sa_attribs & OSAX_ATTRIB_WINDOW_SHADOW != 0,
            set_window_opacity: sa_attribs & OSAX_ATTRIB_WINDOW_OPACITY != 0,
            set_window_scale: sa_attribs & OSAX_ATTRIB_WINDOW_SCALE != 0,
            scripting_addition: self
                .sa_info
                .borrow()
                .as_ref()
                .is_some_and(SaInfo::is_compatible),
        }
    }

    fn snapshot(&mut self) -> Result<PlatformSnapshot, PlatformError> {
        let bridge_capabilities = self.bridge_capabilities;
        let ax_workers = Arc::clone(&self.ax_workers);
        let res = self
            .snapshot_worker
            .run(move || Self::snapshot_inner(bridge_capabilities, &ax_workers));
        match res {
            Ok(Ok(mut snap)) => {
                let mut last = self.last_known_minimized.borrow_mut();
                for w in &mut snap.windows {
                    if w.minimized == rovr_types::ObservedBool::Unknown {
                        if let Some(&known) = last.get(&w.id) {
                            if known != rovr_types::ObservedBool::Unknown {
                                w.minimized = known;
                            } else {
                                w.minimized = rovr_types::ObservedBool::No;
                            }
                        } else {
                            w.minimized = rovr_types::ObservedBool::No;
                        }
                    }
                    if w.minimized != rovr_types::ObservedBool::Unknown {
                        last.insert(w.id, w.minimized);
                    }
                }
                let present: HashSet<WindowId> = snap.windows.iter().map(|w| w.id).collect();
                last.retain(|k, _| present.contains(k));
                Ok(snap)
            }
            Ok(Err(e)) => Err(e),
            Err(err) => Err(PlatformError::Operation(format!(
                "snapshot unavailable: {err}"
            ))),
        }
    }

    fn execute(&mut self, action: &Action) -> Result<(), PlatformError> {
        // A timed-out observation closure continues on the sole worker. Do
        // not overlap any mutation, including SA-backed Dock mutations, with
        // that closure. Refresh actions do not touch macOS.
        if !matches!(action, Action::RefreshAll | Action::RefreshWindow { .. }) {
            self.ensure_bridge_idle()?;
        }
        let status = match action {
            Action::RefreshAll | Action::RefreshWindow { .. } => return Ok(()),
            Action::SetWindowFrame { window, frame } => {
                let window = *window;
                let frame = *frame;
                return self.execute_ax_mutation(window, "set_window_frame", move || unsafe {
                    rovr_bridge_set_window_frame(
                        window.0,
                        frame.x,
                        frame.y,
                        frame.width,
                        frame.height,
                    )
                });
            }
            Action::FocusWindow { window } => {
                // A window on a non-current Space cannot take AX focus — the
                // raise below fails (status 2) unless its Space is current
                // first. Switch via the normal focus-Space path (SA fast path
                // or gesture, incl. the settle gate), then wait — bounded —
                // for WindowServer to report the switch as landed before
                // raising the window.
                let target_space = unsafe { rovr_bridge_window_space_id(window.0) };
                if target_space != 0
                    && target_space != unsafe { rovr_bridge_current_space_for_space(target_space) }
                {
                    self.execute_focus_space(&SpaceId(target_space))?;
                    let deadline = Instant::now() + GESTURE_LAND_CAP;
                    while unsafe { rovr_bridge_current_space_for_space(target_space) }
                        != target_space
                    {
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(GESTURE_LAND_POLL);
                    }
                }
                let window = *window;
                return self.execute_ax_mutation(window, "focus_window", move || unsafe {
                    rovr_bridge_focus_window(window.0)
                });
            }
            Action::SetWindowMinimized { window, minimized } => {
                let window = *window;
                let minimized = *minimized;
                return self.execute_ax_mutation(window, "set_window_minimized", move || unsafe {
                    rovr_bridge_set_window_minimized(window.0, if minimized { 1 } else { 0 })
                });
            }
            Action::CloseWindow { window } => {
                let window = *window;
                return self.execute_ax_mutation(window, "close_window", move || unsafe {
                    rovr_bridge_close_window(window.0)
                });
            }
            Action::ToggleNativeFullscreen { window } => {
                let target_space = unsafe { rovr_bridge_window_space_id(window.0) };
                if target_space != 0
                    && target_space != unsafe { rovr_bridge_current_space_for_space(target_space) }
                {
                    self.execute_focus_space(&SpaceId(target_space))?;
                    let deadline = Instant::now() + GESTURE_LAND_CAP;
                    while unsafe { rovr_bridge_current_space_for_space(target_space) }
                        != target_space
                    {
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(GESTURE_LAND_POLL);
                    }
                }
                let window = *window;
                self.execute_ax_mutation(window, "toggle_native_fullscreen", move || unsafe {
                    rovr_bridge_toggle_fullscreen(window.0)
                })?;
                let did = if target_space != 0 {
                    unsafe { rovr_bridge_display_for_space(target_space) }
                } else {
                    0
                };
                if did != 0 {
                    let deadline = Instant::now() + Duration::from_millis(1500);
                    while unsafe { rovr_bridge_is_display_animating(did) } != 0 {
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(40));
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(200));
                }
                return Ok(());
            }
            Action::MoveWindowToSpace { window, space } => {
                if unsafe { rovr_bridge_space_is_fullscreen(space.0) } != 0 {
                    return Err(PlatformError::Operation(
                        "cannot move window to a macOS fullscreen space".to_string(),
                    ));
                }
                if unsafe { rovr_bridge_space_is_system(space.0) } != 0 {
                    return Err(PlatformError::Operation(
                        "cannot move window to a macOS system space".to_string(),
                    ));
                }
                unsafe { rovr_bridge_move_window_to_space(window.0, space.0) }
            }
            Action::FocusDirection { .. } => {
                return Err(PlatformError::Unsupported("focus_direction"))
            }
            Action::FocusSpace { space } => {
                return self.execute_focus_space(space);
            }
            Action::FocusSpaceStep { target, delta } => {
                return self.execute_focus_space_step(target, *delta);
            }
            Action::CreateSpace { anchor } => {
                return self.execute_sa(OSAX_ATTRIB_ADD_SPACE, "create_space", |sa| {
                    sa.create_space(anchor.0)
                });
            }
            Action::DestroySpace { space } => {
                return self.execute_sa(OSAX_ATTRIB_REM_SPACE, "destroy_space", |sa| {
                    sa.destroy_space(space.0)
                });
            }
            Action::MoveSpace { space, after } => {
                return self.execute_sa(OSAX_ATTRIB_MOV_SPACE, "reorder_space", |sa| {
                    sa.move_space(space.0, after.0)
                });
            }
            Action::SetWindowLayer { window, layer } => {
                return self.execute_sa(OSAX_ATTRIB_WINDOW_LAYER, "set_window_layer", |sa| {
                    sa.set_layer(window.0, *layer)
                });
            }
            Action::SetWindowSticky { window, sticky } => {
                return self.execute_sa(OSAX_ATTRIB_WINDOW_STICKY, "set_window_sticky", |sa| {
                    sa.set_sticky(window.0, *sticky)
                });
            }
            Action::SetWindowShadow { window, shadow } => {
                return self.execute_sa(OSAX_ATTRIB_WINDOW_SHADOW, "set_window_shadow", |sa| {
                    sa.set_shadow(window.0, *shadow)
                });
            }
            Action::SetWindowOpacity {
                window,
                opacity,
                duration_ms,
            } => {
                return self.execute_sa(OSAX_ATTRIB_WINDOW_OPACITY, "set_window_opacity", |sa| {
                    sa.set_opacity(window.0, *opacity as f32, *duration_ms as f32 / 1000.0)
                });
            }
            Action::SetWindowScale { window, x, y, w, h } => {
                return self.execute_sa(OSAX_ATTRIB_WINDOW_SCALE, "set_window_scale", |sa| {
                    sa.scale_window(window.0, *x, *y, *w, *h)
                });
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

    fn snapshot_wedged_ms(&self) -> Option<u64> {
        MacPlatform::snapshot_wedged_ms(self)
    }

    fn drain_diagnostics(&mut self) -> Vec<PlatformDiagnostic> {
        self.ax_workers
            .lock()
            .map(|mut workers| workers.drain_diagnostics())
            .unwrap_or_else(|_| {
                vec![PlatformDiagnostic {
                    kind: "ax.worker_pool_poisoned",
                    detail: "AX worker pool lock poisoned while draining diagnostics".to_string(),
                }]
            })
    }

    fn sa_reinject_diagnostics(&self) -> Option<crate::SaReinjectDiag> {
        Some(MacPlatform::sa_reinject_diagnostics(self))
    }

    fn set_event_watcher(&mut self, event_kind_watcher: std::sync::Arc<dyn Fn(u32) + Send + Sync>) {
        MacPlatform::set_event_watcher(self, event_kind_watcher);
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
        // AX push events (window created / focus changed): any pending event
        // forces a reconcile this tick so new windows tile immediately
        // instead of waiting for the periodic snapshot.
        if PENDING_EVENTS.swap(0, std::sync::atomic::Ordering::SeqCst) != 0 {
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
        let compatible_sa = self
            .sa_info
            .borrow()
            .as_ref()
            .is_some_and(SaInfo::is_compatible);
        // Automatic reinjection lifecycle: Dock PID change, SA loss or failed
        // handshake feed the bounded state machine; it requests privileged
        // injection at most once per generation with backoff. An incompatible
        // responder remains visible to diagnostics but is not considered live.
        self.drive_reinjection(current_dock, compatible_sa);
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

fn observed_bool(value: u8) -> rovr_types::ObservedBool {
    match value {
        0 => rovr_types::ObservedBool::No,
        1 => rovr_types::ObservedBool::Yes,
        _ => rovr_types::ObservedBool::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settle gate: waits only for a RECENT previous post. Successive
    /// switching exits as soon as the animation lands (bounded), and an
    /// idle-then-switch never waits at all.
    #[test]
    fn focused_space_selection_is_scoped_to_active_display() {
        let spaces = vec![
            SpaceSnapshot {
                id: SpaceId(11),
                display_id: DisplayId(1),
                label: None,
                focused: true,
                generation: 0,
                position: 0,
                is_fullscreen: false,
                is_system: false,
            },
            SpaceSnapshot {
                id: SpaceId(22),
                display_id: DisplayId(2),
                label: None,
                focused: true,
                generation: 0,
                position: 1,
                is_fullscreen: false,
                is_system: false,
            },
        ];
        assert_eq!(
            focused_space_for_display(&spaces, DisplayId(2)),
            Some(SpaceId(22))
        );
    }

    #[test]
    fn relative_space_step_accepts_nonzero_displacements() {
        assert!(valid_space_step_delta(-3));
        assert!(valid_space_step_delta(-1));
        assert!(valid_space_step_delta(1));
        assert!(valid_space_step_delta(3));
        assert!(!valid_space_step_delta(0));
    }

    #[test]
    fn ax_diagnostics_are_bounded_and_drained() {
        let mut pool = AxWorkerPool::default();
        for index in 0..(AX_DIAGNOSTIC_CAPACITY + 10) {
            pool.record_diagnostic("ax.refine_timeout", format!("pid={index}"));
        }

        let diagnostics = pool.drain_diagnostics();
        assert_eq!(diagnostics.len(), AX_DIAGNOSTIC_CAPACITY);
        assert_eq!(diagnostics[0].detail, "pid=10");
        assert!(pool.drain_diagnostics().is_empty());
    }

    #[test]
    fn unavailable_ax_mutation_records_pid_and_operation() {
        let mut pool = AxWorkerPool::default();
        pool.worker(42)
            .submit(|| {
                std::thread::sleep(Duration::from_millis(50));
                AxWorkerResult::Mutation(0)
            })
            .unwrap();

        assert!(pool.mutate(42, "focus_window", || 0).is_err());
        let diagnostics = pool.drain_diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].kind, "ax.mutation_timeout");
        assert!(diagnostics[0].detail.contains("pid=42"));
        assert!(diagnostics[0].detail.contains("operation=focus_window"));
    }

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
