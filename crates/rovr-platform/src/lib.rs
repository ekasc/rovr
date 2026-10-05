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

/// Per-user runtime directory for the DAEMON socket, created 0700 by the
/// daemon.
///
/// Deliberately not `/tmp`. `com.apple.tmp_cleaner` runs daily and deletes
/// every `/tmp` entry whose atime, mtime AND ctime are all older than 3 days.
/// A bound Unix socket is never touched through its path after `bind`, so all
/// three timestamps stay frozen at creation time and the socket file is
/// deleted out from under the still-running daemon about three days after it
/// started. The daemon keeps serving on the open fd, but the path is gone, so
/// every CLI call and every in-process dispatch fails with ENOENT until the
/// daemon is restarted.
///
/// `$HOME` is set explicitly by the launchd plist and already backs the config
/// and state paths, so it cannot desync the daemon from its CLI the way
/// `$TMPDIR` or `$UID` can (see [`unix_uid`]).
///
/// The SA socket keeps its own `/tmp/rovr-<uid>/sa.sock` namespace on purpose:
/// it is bound by the payload dylib running inside Dock, so relocating it would
/// require rebuilding and reinstalling that payload.
pub fn daemon_runtime_dir() -> std::path::PathBuf {
    daemon_runtime_dir_for(std::env::var("HOME").ok().as_deref())
}

fn daemon_runtime_dir_for(home: Option<&str>) -> std::path::PathBuf {
    match home {
        Some(home) if !home.is_empty() => {
            std::path::PathBuf::from(home).join("Library/Caches/rovr")
        }
        // No HOME is unusual for a launchd agent (the plist always sets it).
        // Keep the legacy location rather than falling back to a relative
        // path, which would let the daemon and the CLI bind different sockets.
        _ => std::path::PathBuf::from(format!("/tmp/rovr-{}", unix_uid())),
    }
}

pub fn daemon_socket_path() -> std::path::PathBuf {
    daemon_runtime_dir().join("daemon.sock")
}

/// Run the platform event loop on the calling (main) thread.
///
/// On macOS this services the run loop that AX observer and SLS/NSWorkspace
/// notification sources are attached to; it does not return under normal
/// operation. On other platforms there are no event sources, so it parks
/// forever (the daemon's real work runs on its own threads).
pub fn run_event_loop() {
    #[cfg(target_os = "macos")]
    {
        macos::run_event_loop();
    }
    #[cfg(not(target_os = "macos"))]
    {
        loop {
            std::thread::park();
        }
    }
}

pub trait Platform: Send {
    fn capabilities(&self) -> Capabilities;
    fn snapshot(&mut self) -> Result<PlatformSnapshot, PlatformError>;
    fn execute(&mut self, action: &Action) -> Result<(), PlatformError>;
    fn needs_refresh(&self) -> bool {
        false
    }
    /// Publish the canonical public snapshot to external consumers.
    ///
    /// On macOS this posts `com.rovr.state.changed` to
    /// `NSDistributedNotificationCenter` with the full JSON snapshot in
    /// `userInfo["state"]`. The default is a no-op (non-macOS builds, tests).
    /// The daemon calls this only when the snapshot differs from the last
    /// published one — duplicate identical snapshots are suppressed there,
    /// not here. Must never spawn shell processes or touch SketchyBar.
    fn publish_public_state(&self, _state_json: &str) {}
    /// Retarget the focused-window title-change subscription.
    ///
    /// On macOS this subscribes `kAXTitleChangedNotification` on the given
    /// window (`0` = none focused: remove the subscription) so title edits on
    /// the focused window wake the state loop without waiting for the
    /// periodic recovery tick. At most one such subscription exists; the
    /// bridge removes the previous window's registration first and ignores
    /// failures (recovery observation still catches the title). The default
    /// is a no-op. Never publishes or serializes state — the callback only
    /// requests a normal re-observation.
    fn track_focused_window(&self, _window_id: u32) {}
    /// Fire a pre-encoded SketchyBar `--trigger` message at the running bar.
    ///
    /// On macOS this sends `message` (NUL-joined argv + trailing NUL, exactly
    /// SketchyBar CLI wire format) to the bar's Mach bootstrap port
    /// (`git.felix.sketchybar`). Fire-and-forget with a zero-timeout send:
    /// returns `true` on send, `false` when SketchyBar is absent or the send
    /// fails — never blocks, retries, spawns, or crashes. The default
    /// reports absent. Callers send only on deduped state changes, so each
    /// send is already the latest state (no queue, no backlog possible).
    fn sketchybar_trigger(&self, _message: &[u8]) -> bool {
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
mod runtime_dir_tests {
    use super::daemon_runtime_dir_for;

    /// The regression this guards: a daemon socket under `/tmp` is reaped by
    /// the system tmp cleaner after 3 days while the daemon keeps running.
    #[test]
    fn runtime_dir_is_outside_tmp() {
        let dir = daemon_runtime_dir_for(Some("/Users/example"));
        assert_eq!(
            dir,
            std::path::PathBuf::from("/Users/example/Library/Caches/rovr")
        );
        assert!(!dir.starts_with("/tmp"));
    }

    #[test]
    fn empty_home_falls_back_to_the_legacy_uid_dir() {
        let dir = daemon_runtime_dir_for(Some(""));
        assert!(dir.to_string_lossy().starts_with("/tmp/rovr-"));
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
