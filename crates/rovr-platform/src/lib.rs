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
}
