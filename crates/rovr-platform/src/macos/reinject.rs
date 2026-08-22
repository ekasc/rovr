// Automatic SA reinjection — unprivileged side.
//
// The NORMAL Rovr daemon owns everything about lifecycle here: Dock PID
// change detection, SA health probing, retry policy and verification. This
// module contains that decision logic as a pure, unit-testable state machine
// plus the thin IPC client for the privileged helper.
//
// Privileged side (crates/rovr-sa-helper) owns ONLY: caller authentication,
// self-resolved Dock pid, fixed-path artifact validation, and running the
// fixed loader against the fixed payload. It has no timers, no polling and no
// policy.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Fixed socket path of the privileged helper's launchd listener. Must match
/// `ROVR_HELPER_SOCKET_PATH` in crates/rovr-sa-helper/src/helper.m and the
/// SockPathName in the installed LaunchDaemon plist.
pub const HELPER_SOCKET_PATH: &str = "/var/run/rovr-sa-helper.sock";

pub const HELPER_MAGIC: u32 = u32::from_le_bytes(*b"RVH1");
const HELPER_PROTO: u32 = 1;

pub const HELPER_OP_INJECT: u32 = 1;
pub const HELPER_OP_STATUS: u32 = 2;

pub const HELPER_ST_OK: u32 = 0;
pub const HELPER_ST_UNAUTHORIZED: u32 = 1;
pub const HELPER_ST_BAD_REQUEST: u32 = 2;
pub const HELPER_ST_DOCK_NOT_FOUND: u32 = 3;
pub const HELPER_ST_ARTIFACTS_INVALID: u32 = 4;
pub const HELPER_ST_INJECTION_FAILED: u32 = 5;
pub const HELPER_ST_INTERNAL: u32 = 6;

/// Bounded handshake-verification window after a successful injection request.
const VERIFY_WINDOW: Duration = Duration::from_secs(8);
/// Backoff schedule between failed attempts within one Dock generation.
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(45),
];

#[cfg(target_os = "macos")]
extern "C" {
    fn getuid() -> u32;
}

#[cfg(target_os = "macos")]
fn current_uid() -> u32 {
    unsafe { getuid() }
}

#[cfg(not(target_os = "macos"))]
fn current_uid() -> u32 {
    0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReinjectionPhase {
    /// SA handshake valid for the current Dock generation.
    Healthy,
    /// One privileged injection request is in flight for this generation.
    Injecting,
    /// Injection ran; waiting (bounded) for the new payload's handshake.
    Verifying,
    /// Attempts exhausted or hard-failed for this generation. No further
    /// requests until the Dock generation changes. Non-SA Rovr keeps working.
    Failed,
}

impl ReinjectionPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReinjectionPhase::Healthy => "healthy",
            ReinjectionPhase::Injecting => "injecting",
            ReinjectionPhase::Verifying => "verifying",
            ReinjectionPhase::Failed => "failed",
        }
    }
}

/// What the platform layer should do after an `observe` tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickAction {
    None,
    /// Spawn exactly one privileged injection request (single-flight).
    RequestInjection,
}

/// Snapshot of the reinjection lifecycle for diagnostics (`rovr doctor`).
#[derive(Debug, Clone)]
pub struct ReinjectionStatus {
    pub phase: ReinjectionPhase,
    /// Monotonic counter bumped on every observed Dock PID change; every
    /// attempt and success is keyed to this so stale results can never mark a
    /// newer Dock healthy.
    pub generation: u64,
    pub dock_pid: Option<i32>,
    pub attempts_this_generation: u32,
    /// Seconds until the next permitted retry, if backoff is active.
    pub retry_in_secs: Option<u64>,
    pub last_result: Option<&'static str>,
    pub last_error: Option<String>,
}

struct Inner {
    phase: ReinjectionPhase,
    generation: u64,
    dock_pid: Option<i32>,
    attempts: u32,
    verify_deadline: Option<Instant>,
    next_retry_at: Option<Instant>,
    last_result: Option<&'static str>,
    last_error: Option<String>,
}

/// Pure decision state machine. Not Send/Sync-bound; owned by the platform
/// observation path (single thread).
pub struct Reinjector {
    inner: Inner,
}

impl Default for Reinjector {
    fn default() -> Self {
        Self::new()
    }
}

impl Reinjector {
    pub fn new() -> Self {
        Reinjector {
            inner: Inner {
                phase: ReinjectionPhase::Failed,
                generation: 0,
                dock_pid: None,
                attempts: 0,
                verify_deadline: None,
                next_retry_at: None,
                last_result: None,
                last_error: None,
            },
        }
    }

    pub fn status(&self) -> ReinjectionStatus {
        let i = &self.inner;
        ReinjectionStatus {
            phase: i.phase,
            generation: i.generation,
            dock_pid: i.dock_pid,
            attempts_this_generation: i.attempts,
            retry_in_secs: i.next_retry_at.map(|at| {
                let d = at.saturating_duration_since(Instant::now());
                d.as_secs() + u64::from(d.subsec_nanos() > 0)
            }),
            last_result: i.last_result,
            last_error: i.last_error.clone(),
        }
    }

    /// Feed one observation tick. `dock_pid` is the current Dock process (None
    /// if it cannot be resolved), `sa_alive` whether the SA handshake just
    /// answered. Returns what to do.
    pub fn observe(&mut self, now: Instant, dock_pid: Option<i32>, sa_alive: bool) -> TickAction {
        // Dock generation change: invalidate ALL cached SA state. A success
        // recorded against an older PID must never mark the new Dock healthy.
        if dock_pid != self.inner.dock_pid {
            self.inner.generation += 1;
            self.inner.dock_pid = dock_pid;
            self.inner.attempts = 0;
            self.inner.verify_deadline = None;
            self.inner.next_retry_at = None;
            self.inner.phase = if sa_alive {
                ReinjectionPhase::Healthy
            } else {
                ReinjectionPhase::Failed
            };
        }

        match self.inner.phase {
            ReinjectionPhase::Healthy => {
                if !sa_alive {
                    // Handshake stopped answering without a Dock change (Dock
                    // wedged, socket removed): same recovery path, tied to the
                    // same generation.
                    self.inner.phase = ReinjectionPhase::Failed;
                    self.inner.verify_deadline = None;
                    return self.maybe_request();
                }
                TickAction::None
            }
            ReinjectionPhase::Injecting => TickAction::None,
            ReinjectionPhase::Verifying => {
                if sa_alive {
                    self.inner.phase = ReinjectionPhase::Healthy;
                    self.inner.verify_deadline = None;
                    self.inner.last_result = Some("injected");
                    self.inner.last_error = None;
                    TickAction::None
                } else if now >= self.inner.verify_deadline.unwrap_or(now) {
                    // Bounded verification window expired: treat as a failed
                    // attempt and fall into backoff.
                    self.inner.phase = ReinjectionPhase::Failed;
                    self.inner.verify_deadline = None;
                    self.inner.last_result = Some("handshake_timeout");
                    self.inner.last_error =
                        Some("injection accepted but handshake did not appear in time".into());
                    self.schedule_backoff(now);
                    TickAction::None
                } else {
                    TickAction::None
                }
            }
            ReinjectionPhase::Failed => {
                if sa_alive {
                    // Recovered externally (e.g. manual install): accept it.
                    self.inner.phase = ReinjectionPhase::Healthy;
                    self.inner.last_error = None;
                    return TickAction::None;
                }
                if let Some(at) = self.inner.next_retry_at {
                    if now < at {
                        return TickAction::None; // backoff: no tight loop
                    }
                }
                self.maybe_request()
            }
        }
    }

    /// Mark that a privileged request was actually dispatched. Call only when
    /// the single-flight guard was acquired.
    pub fn injection_started(&mut self) {
        self.inner.attempts += 1;
        self.inner.phase = ReinjectionPhase::Injecting;
        self.inner.next_retry_at = None;
    }

    /// Record the outcome of an injection request.
    pub fn injection_finished(&mut self, now: Instant, result: Result<(), String>) {
        // Guard against a late result from a previous generation.
        match self.inner.phase {
            ReinjectionPhase::Injecting => {}
            _ => return,
        }
        match result {
            Ok(()) => {
                self.inner.phase = ReinjectionPhase::Verifying;
                self.inner.verify_deadline = Some(now + VERIFY_WINDOW);
                self.inner.last_result = Some("requested");
            }
            Err(err) => {
                self.inner.phase = ReinjectionPhase::Failed;
                self.inner.last_result = Some("failed");
                self.inner.last_error = Some(err);
                self.schedule_backoff(now);
            }
        }
    }

    fn maybe_request(&mut self) -> TickAction {
        if self.inner.attempts > BACKOFF.len() as u32 {
            // Attempts exhausted for this generation: quiet until the Dock
            // changes again. Prevents retry storms when Dock crash-loops.
            return TickAction::None;
        }
        // Phase stays Failed/Healthy here: `injection_started` moves to
        // Injecting only once the single-flight guard has actually been
        // acquired, so a refused spawn can never wedge the machine.
        TickAction::RequestInjection
    }

    fn schedule_backoff(&mut self, now: Instant) {
        let idx = (self.inner.attempts.min(BACKOFF.len() as u32) - 1) as usize;
        self.inner.next_retry_at = Some(now + BACKOFF[idx]);
    }
}

/// Single-flight guard + result slot for background injection jobs. Guarantees
/// at most ONE privileged request in flight regardless of callers.
#[derive(Default)]
pub struct InjectionJob {
    inflight: Arc<AtomicBool>,
    slot: Arc<Mutex<Option<Result<(), String>>>>,
}

impl InjectionJob {
    /// True if the job was spawned; false if another injection is in flight.
    pub fn spawn<F>(&self, job: F) -> bool
    where
        F: FnOnce() -> Result<(), String> + Send + 'static,
    {
        if self.inflight.swap(true, Ordering::SeqCst) {
            return false;
        }
        let slot = self.slot.clone();
        let inflight = self.inflight.clone();
        std::thread::spawn(move || {
            let result = job();
            if let Ok(mut guard) = slot.lock() {
                *guard = Some(result);
            }
            inflight.store(false, Ordering::SeqCst);
        });
        true
    }

    pub fn is_inflight(&self) -> bool {
        self.inflight.load(Ordering::SeqCst)
    }

    /// Take the finished result, if any.
    pub fn poll(&self) -> Option<Result<(), String>> {
        self.slot.lock().ok().and_then(|mut g| g.take())
    }
}

/// Response from the privileged helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperResponse {
    pub status: u32,
    pub dock_pid: i32,
}

#[derive(Debug)]
pub enum HelperError {
    /// Service not reachable (not installed, bootout out, launchd down).
    Unavailable(String),
    /// Helper refused the request or reported failure.
    Rejected { status: u32 },
    /// Transport-level protocol violation.
    Protocol(String),
}

impl std::fmt::Display for HelperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HelperError::Unavailable(e) => write!(f, "helper service unavailable: {e}"),
            HelperError::Rejected { status } => {
                write!(f, "helper rejected request (status {status})")
            }
            HelperError::Protocol(e) => write!(f, "helper protocol error: {e}"),
        }
    }
}

/// Thin client for the privileged helper's fixed 16-byte frame protocol.
#[derive(Debug, Clone)]
pub struct HelperClient {
    socket_path: PathBuf,
}

impl Default for HelperClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HelperClient {
    pub fn new() -> Self {
        Self {
            socket_path: PathBuf::from(HELPER_SOCKET_PATH),
        }
    }

    pub fn with_socket_path(path: PathBuf) -> Self {
        Self { socket_path: path }
    }

    /// Ask the helper to inject the fixed payload into the Dock it resolves
    /// itself. The request carries ONLY the opcode and our real uid (which the
    /// helper cross-checks against kernel peer credentials).
    pub fn inject(&self, timeout: Duration) -> Result<HelperResponse, HelperError> {
        self.request(HELPER_OP_INJECT, timeout)
    }

    /// Liveness/artifact probe of the privileged service.
    pub fn status(&self, timeout: Duration) -> Result<HelperResponse, HelperError> {
        self.request(HELPER_OP_STATUS, timeout)
    }

    fn request(&self, opcode: u32, timeout: Duration) -> Result<HelperResponse, HelperError> {
        use std::io::{Read, Write};

        let mut stream = UnixStream::connect(&self.socket_path).map_err(|err| {
            HelperError::Unavailable(format!("{}: {err}", self.socket_path.display()))
        })?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|err| HelperError::Unavailable(format!("set read timeout: {err}")))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|err| HelperError::Unavailable(format!("set write timeout: {err}")))?;

        let req = RequestFrame {
            magic: HELPER_MAGIC,
            proto: HELPER_PROTO,
            opcode,
            uid: current_uid(),
        };
        stream
            .write_all(&req.encode())
            .map_err(|err| HelperError::Unavailable(format!("send: {err}")))?;

        let mut buf = [0u8; 16];
        let mut got = 0usize;
        while got < buf.len() {
            let n = stream
                .read(&mut buf[got..])
                .map_err(|err| HelperError::Protocol(format!("read: {err}")))?;
            if n == 0 {
                return Err(HelperError::Protocol(
                    "connection closed mid-response".into(),
                ));
            }
            got += n;
        }
        let resp = ResponseFrame::decode(buf)
            .ok_or_else(|| HelperError::Protocol("malformed response".into()))?;
        if resp.magic != HELPER_MAGIC || resp.proto != HELPER_PROTO {
            return Err(HelperError::Protocol("bad magic/proto in response".into()));
        }
        if resp.status != HELPER_ST_OK {
            return Err(HelperError::Rejected {
                status: resp.status,
            });
        }
        Ok(HelperResponse {
            status: resp.status,
            dock_pid: resp.dock_pid,
        })
    }
}

/// Wire frames kept as explicit structs so tests can pin the layout: there is
/// deliberately NO field for a pid, path, command or environment.
#[repr(C)]
#[derive(Clone, Copy)]
struct RequestFrame {
    magic: u32,
    proto: u32,
    opcode: u32,
    uid: u32,
}

impl RequestFrame {
    fn encode(self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&self.magic.to_le_bytes());
        b[4..8].copy_from_slice(&self.proto.to_le_bytes());
        b[8..12].copy_from_slice(&self.opcode.to_le_bytes());
        b[12..16].copy_from_slice(&self.uid.to_le_bytes());
        b
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ResponseFrame {
    magic: u32,
    proto: u32,
    status: u32,
    dock_pid: i32,
}

impl ResponseFrame {
    fn decode(b: [u8; 16]) -> Option<Self> {
        Some(ResponseFrame {
            magic: u32::from_le_bytes(b[0..4].try_into().ok()?),
            proto: u32::from_le_bytes(b[4..8].try_into().ok()?),
            status: u32::from_le_bytes(b[8..12].try_into().ok()?),
            dock_pid: i32::from_le_bytes(b[12..16].try_into().ok()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: u64) -> Instant {
        Instant::now() + Duration::from_secs(secs)
    }

    /// Only one injection may be requested while one is in flight.
    #[test]
    fn reinject_single_flight_no_duplicate_requests() {
        let mut r = Reinjector::new();
        assert_eq!(
            r.observe(t(0), Some(100), false),
            TickAction::RequestInjection
        );
        r.injection_started();
        // While injecting, further ticks never re-request.
        for s in 1..10 {
            assert_eq!(r.observe(t(s), Some(100), false), TickAction::None);
        }
    }

    /// A Dock generation change invalidates any prior success: the new Dock is
    /// NOT considered healthy even though the old one was.
    #[test]
    fn reinject_dock_change_invalidates_stale_success() {
        let mut r = Reinjector::new();
        r.observe(t(0), Some(100), false);
        r.injection_started();
        r.injection_finished(t(1), Ok(()));
        r.observe(t(2), Some(100), true); // verified healthy
        assert_eq!(r.status().phase, ReinjectionPhase::Healthy);

        // Dock restarts: state must reset and require a fresh injection.
        assert_eq!(
            r.observe(t(3), Some(200), false),
            TickAction::RequestInjection
        );
        let st = r.status();
        assert_eq!(st.generation, 2);
        assert_eq!(st.phase, ReinjectionPhase::Failed);
        assert_eq!(st.dock_pid, Some(200));
        r.injection_started();
        assert_eq!(r.status().phase, ReinjectionPhase::Injecting);
    }

    /// Every injection attempt is tied to a Dock generation: a late result
    /// from a previous generation is discarded.
    #[test]
    fn reinject_late_result_from_old_generation_discarded() {
        let mut r = Reinjector::new();
        r.observe(t(0), Some(100), false);
        r.injection_started();
        // Dock changes while the old-generation request is still out: all
        // cached SA state is invalidated and the machine leaves Injecting.
        r.observe(t(1), Some(200), false);
        assert_eq!(r.status().generation, 2);
        // Late OLD-generation result arrives: must be ignored — it must NOT
        // mark the new Dock as verified/injected.
        r.injection_finished(t(2), Ok(()));
        assert_eq!(r.status().phase, ReinjectionPhase::Failed);
        // The fresh generation still gets its own complete cycle.
        assert_eq!(
            r.observe(t(3), Some(200), false),
            TickAction::RequestInjection
        );
        r.injection_started();
        r.injection_finished(t(4), Ok(()));
        assert_eq!(r.status().phase, ReinjectionPhase::Verifying);
    }

    /// Repeated failures hit bounded retries with backoff, then go quiet until
    /// the Dock changes (no retry storm after repeated Dock crashes).
    #[test]
    fn reinject_bounded_retries_then_quiet_until_dock_change() {
        let mut r = Reinjector::new();
        let max_attempts = super::BACKOFF.len() as u32 + 1;
        let base = Instant::now();
        let at = |secs: u64| base + Duration::from_secs(secs);

        // Generation 1 begins with the first observation.
        assert_eq!(
            r.observe(at(0), Some(100), false),
            TickAction::RequestInjection
        );
        let mut clock = 1u64;
        for attempt in 1..=max_attempts {
            if attempt > 1 {
                // Still inside the previous backoff window: nothing requested.
                assert_eq!(r.observe(at(clock), Some(100), false), TickAction::None);
            }
            // Advance well past any backoff step (max step is 45 s).
            clock += 60;
            assert_eq!(
                r.observe(at(clock), Some(100), false),
                TickAction::RequestInjection,
                "attempt {attempt} should eventually be permitted"
            );
            r.injection_started();
            r.injection_finished(at(clock + 1), Err("boom".into()));
        }
        // Exhausted: even far-future ticks stay quiet.
        for s in 1..50 {
            assert_eq!(
                r.observe(at(clock + 60 + s), Some(100), false),
                TickAction::None,
                "no retry storm after exhaustion"
            );
        }
        // New Dock generation re-enables exactly one fresh cycle.
        assert_eq!(
            r.observe(at(clock + 200), Some(300), false),
            TickAction::RequestInjection
        );
        r.injection_started();
        assert_eq!(r.status().attempts_this_generation, 1);
    }

    /// Healthy stays healthy; losing the handshake without a Dock change
    /// triggers exactly one reinjection request.
    #[test]
    fn reinject_handshake_loss_without_dock_change_requests_once() {
        let mut r = Reinjector::new();
        r.observe(t(0), Some(100), true);
        assert_eq!(r.status().phase, ReinjectionPhase::Healthy);
        assert_eq!(
            r.observe(t(1), Some(100), false),
            TickAction::RequestInjection
        );
        r.injection_started();
        assert_eq!(r.observe(t(2), Some(100), false), TickAction::None);
    }

    /// Verification window is bounded: a silent injection degrades to Failed
    /// with backoff instead of waiting forever.
    #[test]
    fn reinject_verification_window_is_bounded() {
        let mut r = Reinjector::new();
        r.observe(t(0), Some(100), false);
        r.injection_started();
        r.injection_finished(t(1), Ok(()));
        assert_eq!(r.status().phase, ReinjectionPhase::Verifying);

        let window = super::VERIFY_WINDOW;
        let before = t(2);
        let deadline = t(1) + window;
        assert_eq!(r.observe(before, Some(100), false), TickAction::None);
        let after = deadline + Duration::from_secs(1);
        r.observe(after, Some(100), false);
        assert_eq!(r.status().phase, ReinjectionPhase::Failed);
        assert_eq!(r.status().last_result, Some("handshake_timeout"));
    }

    /// Successful verification refreshes to Healthy and clears errors.
    #[test]
    fn reinject_verified_handshake_becomes_healthy() {
        let mut r = Reinjector::new();
        r.observe(t(0), Some(100), false);
        r.injection_started();
        r.injection_finished(t(1), Ok(()));
        assert_eq!(r.observe(t(2), Some(100), true), TickAction::None);
        let st = r.status();
        assert_eq!(st.phase, ReinjectionPhase::Healthy);
        assert_eq!(st.last_result, Some("injected"));
        assert!(st.last_error.is_none());
    }

    /// External recovery (manual install) while Failed is accepted.
    #[test]
    fn reinject_external_recovery_accepted() {
        let mut r = Reinjector::new();
        r.observe(t(0), Some(100), false);
        r.injection_started();
        r.injection_finished(t(1), Err("helper unavailable".into()));
        assert_eq!(r.status().phase, ReinjectionPhase::Failed);
        r.observe(t(2), Some(100), true);
        assert_eq!(r.status().phase, ReinjectionPhase::Healthy);
    }

    /// InjectionJob never allows two concurrent spawns.
    #[test]
    fn injection_job_is_single_flight() {
        let job = InjectionJob::default();
        let started = Arc::new(AtomicBool::new(false));
        let started2 = started.clone();
        let spawned = job.spawn(move || {
            started2.store(true, Ordering::SeqCst);
            // Hold the slot briefly so a concurrent spawn attempt is guaranteed
            // to land while the job is still in flight.
            std::thread::sleep(Duration::from_millis(50));
            Ok(())
        });
        assert!(spawned, "first spawn must run");
        while !started.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(job.is_inflight());
        let second = job.spawn(|| Ok(()));
        assert!(!second, "second spawn during flight must be refused");

        while job.is_inflight() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(job.poll(), Some(Ok(())));
        assert_eq!(job.poll(), None, "result consumed exactly once");
        // After completion a new spawn is allowed.
        assert!(job.spawn(|| Ok(())));
    }

    /// Client refuses malformed/magic-mismatched responses instead of
    /// misinterpreting them (helper error handled without daemon crash).
    #[test]
    fn helper_client_rejects_bad_magic_and_short_responses() {
        let dir = std::env::temp_dir().join(format!("rovr-helper-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("t.sock");

        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 16];
            use std::io::Read;
            conn.read_exact(&mut buf).unwrap();
            // Garbage response: wrong magic.
            let bad = [0xFFu8; 16];
            use std::io::Write;
            conn.write_all(&bad).unwrap();
        });

        let client = HelperClient::with_socket_path(sock.clone());
        let err = client.inject(Duration::from_secs(2)).unwrap_err();
        assert!(matches!(err, HelperError::Protocol(_)), "got {err:?}");
        server.join().unwrap();

        // Unreachable socket maps to Unavailable, not a panic.
        let client = HelperClient::with_socket_path(dir.join("missing.sock"));
        let err = client.status(Duration::from_millis(200)).unwrap_err();
        assert!(matches!(err, HelperError::Unavailable(_)), "got {err:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fake helper answering with a rejection status surfaces as Rejected
    /// with the exact code (service unavailable / refusal handled cleanly).
    #[test]
    fn helper_client_surfaces_rejection_status() {
        let dir = std::env::temp_dir().join(format!("rovr-helper-test2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("t.sock");

        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 16];
            use std::io::Read;
            conn.read_exact(&mut buf).unwrap();
            let resp = ResponseFrame {
                magic: HELPER_MAGIC,
                proto: HELPER_PROTO,
                status: HELPER_ST_ARTIFACTS_INVALID,
                dock_pid: 0,
            };
            let mut raw = [0u8; 16];
            raw[0..4].copy_from_slice(&resp.magic.to_le_bytes());
            raw[4..8].copy_from_slice(&resp.proto.to_le_bytes());
            raw[8..12].copy_from_slice(&resp.status.to_le_bytes());
            raw[12..16].copy_from_slice(&resp.dock_pid.to_le_bytes());
            use std::io::Write;
            conn.write_all(&raw).unwrap();
        });

        let client = HelperClient::with_socket_path(sock.clone());
        let err = client.inject(Duration::from_secs(2)).unwrap_err();
        assert!(
            matches!(err, HelperError::Rejected { status } if status == HELPER_ST_ARTIFACTS_INVALID),
            "got {err:?}"
        );
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The wire protocol structurally cannot express an arbitrary PID, path,
    /// command or environment: the request frame is exactly
    /// {magic, proto, opcode, uid}.
    #[test]
    fn helper_protocol_has_no_arbitrary_target_fields() {
        let frame = RequestFrame {
            magic: HELPER_MAGIC,
            proto: HELPER_PROTO,
            opcode: HELPER_OP_INJECT,
            uid: 501,
        };
        let bytes = frame.encode();
        assert_eq!(bytes.len(), 16);
        // Decode round-trip preserves only the four fixed fields.
        let decoded = ResponseFrame::decode(bytes).unwrap();
        assert_eq!(decoded.magic, HELPER_MAGIC);
        assert_eq!(decoded.proto, HELPER_PROTO);
    }
}
