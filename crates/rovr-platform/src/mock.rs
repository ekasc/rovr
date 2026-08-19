use rovr_core::Action;
use rovr_types::{Capabilities, PlatformSnapshot};

use crate::{Platform, PlatformError};

#[derive(Debug, Default)]
pub struct MockPlatform {
    pub snapshot: PlatformSnapshot,
    pub executed: Vec<Action>,
}

impl MockPlatform {
    pub fn with_snapshot(snapshot: PlatformSnapshot) -> Self {
        Self {
            snapshot,
            executed: vec![],
        }
    }
}

impl Platform for MockPlatform {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            observe_windows: true,
            set_window_frame: true,
            focus_window: true,
            move_window_to_space: true,
            create_space: true,
            destroy_space: true,
            focus_space: true,
            set_window_layer: true,
            set_window_opacity: true,
            scripting_addition: false,
        }
    }

    fn snapshot(&mut self) -> Result<PlatformSnapshot, PlatformError> {
        Ok(self.snapshot.clone())
    }

    fn execute(&mut self, action: &Action) -> Result<(), PlatformError> {
        self.executed.push(action.clone());
        Ok(())
    }
}
