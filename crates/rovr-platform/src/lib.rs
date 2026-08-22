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
    /// Automatic SA reinjection lifecycle diagnostics; None on platforms
    /// without the macOS scripting addition.
    fn sa_reinject_diagnostics(&self) -> Option<SaReinjectDiag> {
        None
    }
}
