use rovr_core::Action;
use rovr_types::{Capabilities, PlatformSnapshot};
use thiserror::Error;

pub mod bounded_worker;
mod mock;
pub use mock::MockPlatform;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacPlatform;

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("operation is not supported by this platform: {0}")]
    Unsupported(&'static str),
    #[error("platform operation failed: {0}")]
    Operation(String),
}

/// A recoverable platform-layer failure that did not fail the enclosing
/// operation. The daemon drains these into its bounded flight recorder so
/// partial snapshots remain diagnosable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformDiagnostic {
    pub kind: &'static str,
    pub detail: String,
}

/// Diagnostics for the automatic SA reinjection lifecycle. Exposed by
/// `rovr doctor`; never contains secrets or privileged internals.
#[derive(Debug, Clone)]
pub struct SaReinjectDiag {
    /// healthy | injecting | verifying | failed
    pub phase: &'static str,
    /// Bumped on every observed Dock PID change; attempts/successes are keyed
    /// to it so a stale success can never mark a newer Dock healthy.
    pub generation: u64,
    pub dock_pid: Option<i32>,
    pub attempts_this_generation: u32,
    /// Seconds until the next permitted retry, if backoff is active.
    pub retry_in_secs: Option<u64>,
    /// True while an injection request is in flight or being verified.
    pub pending: bool,
    pub last_result: Option<&'static str>,
    pub last_error: Option<String>,
    /// Fixed helper socket path (diagnostics only).
    pub helper_socket: String,
}

/// Real UNIX uid of this process (never the $UID environment variable, which
/// may be unset or spoofed in GUI-agent contexts like skhd/launchd). The
/// daemon socket, CLI discovery and the SA socket namespace must all key on
/// this SAME value so components agree regardless of their inherited env.
pub fn unix_uid() -> u32 {
    #[cfg(target_os = "macos")]
    unsafe {
        getuid()
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Non-macOS builds (tests/mock): keep deterministic env-based value.
        std::env::var("UID")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn getuid() -> u32;
}

/// Per-user runtime directory shared by daemon, CLI and Dock payload. The
/// daemon/payload create it as 0700 and refuse unsafe pre-existing entries.
pub fn runtime_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(format!("/tmp/rovr-{}", unix_uid()))
}

pub fn daemon_socket_path() -> std::path::PathBuf {
    runtime_dir().join("daemon.sock")
}

pub trait Platform: Send {
    fn capabilities(&self) -> Capabilities;
    fn snapshot(&mut self) -> Result<PlatformSnapshot, PlatformError>;
    fn execute(&mut self, action: &Action) -> Result<(), PlatformError>;
    fn needs_refresh(&self) -> bool {
        false
    }
    /// Milliseconds the observation worker has been wedged, if it has.
    /// Diagnostics-only; lets `doctor` expose a hung AX/SkyLight worker
    /// instead of hiding it behind generic timeouts.
    fn snapshot_wedged_ms(&self) -> Option<u64> {
        None
    }
    /// Drain recoverable failures accumulated since the previous call.
    fn drain_diagnostics(&mut self) -> Vec<PlatformDiagnostic> {
        Vec::new()
    }
    /// Automatic SA reinjection lifecycle diagnostics; None on platforms
    /// without the macOS scripting addition.
    fn sa_reinject_diagnostics(&self) -> Option<SaReinjectDiag> {
        None
    }
    /// Register a callback invoked (on the platform's event thread) with the
    /// kind of each observed window event. The daemon may use the kind to wake
    /// its state loop immediately; observation remains snapshot-authoritative.
    fn set_event_watcher(&mut self, event_kind_watcher: std::sync::Arc<dyn Fn(u32) + Send + Sync>) {
        let _ = event_kind_watcher;
    }
}

/// An AX timeout cannot establish that a newly observed window is unminimized.
#[cfg(any(target_os = "macos", test))]
fn cached_minimized(
    observed: rovr_types::ObservedBool,
    cached: Option<rovr_types::ObservedBool>,
) -> rovr_types::ObservedBool {
    use rovr_types::ObservedBool;
    match observed {
        ObservedBool::Unknown => cached.unwrap_or(ObservedBool::Unknown),
        known => known,
    }
}

#[cfg(test)]
mod observation_tests {
    use super::cached_minimized;
    use rovr_types::ObservedBool::{No, Unknown, Yes};

    #[test]
    fn first_seen_unknown_minimized_stays_unknown() {
        assert_eq!(cached_minimized(Unknown, None), Unknown);
        assert_eq!(cached_minimized(Unknown, Some(Unknown)), Unknown);
        assert_eq!(cached_minimized(Unknown, Some(Yes)), Yes);
        assert_eq!(cached_minimized(Unknown, Some(No)), No);
        assert_eq!(cached_minimized(Yes, Some(No)), Yes);
    }
}
