use rovr_core::Action;
use rovr_types::{Capabilities, PlatformSnapshot};

use crate::{Platform, PlatformDiagnostic, PlatformError};

#[derive(Debug, Default)]
pub struct MockPlatform {
    pub snapshot: PlatformSnapshot,
    pub executed: Vec<Action>,
    pub diagnostics: Vec<PlatformDiagnostic>,
    /// Canonical JSON snapshots passed to `publish_public_state`, in order.
    /// Lets tests assert emission + duplicate suppression without macOS APIs.
    pub published: std::sync::Mutex<Vec<String>>,
    /// Window ids passed to `track_focused_window`, in order (`0` = unfocused).
    /// Lets tests assert title-tracking follows focus without macOS APIs.
    pub tracked: std::sync::Mutex<Vec<u32>>,
    /// Encoded trigger messages passed to `sketchybar_trigger`, in order.
    pub sketchybar_sent: std::sync::Mutex<Vec<Vec<u8>>>,
    /// What `sketchybar_trigger` reports. Flip to `false` to simulate an
    /// absent bar; the mock never fails spuriously and never poisons later
    /// sends, mirroring the stateless bridge.
    pub sketchybar_ok: std::sync::Mutex<bool>,
}

impl MockPlatform {
    pub fn with_snapshot(snapshot: PlatformSnapshot) -> Self {
        Self {
            snapshot,
            executed: vec![],
            diagnostics: vec![],
            published: Default::default(),
            tracked: Default::default(),
            sketchybar_sent: Default::default(),
            sketchybar_ok: std::sync::Mutex::new(true),
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
            reorder_space: true,
            set_window_layer: true,
            set_window_sticky: true,
            set_window_shadow: true,
            set_window_opacity: true,
            set_window_scale: true,
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

    fn drain_diagnostics(&mut self) -> Vec<PlatformDiagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    fn publish_public_state(&self, state_json: &str) {
        if let Ok(mut guard) = self.published.lock() {
            guard.push(state_json.to_string());
        }
    }

    fn track_focused_window(&self, window_id: u32) {
        if let Ok(mut guard) = self.tracked.lock() {
            guard.push(window_id);
        }
    }

    fn sketchybar_trigger(&self, message: &[u8]) -> bool {
        if let Ok(mut guard) = self.sketchybar_sent.lock() {
            guard.push(message.to_vec());
        }
        self.sketchybar_ok.lock().map(|ok| *ok).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_records_canonical_json_in_order() {
        let platform = MockPlatform::default();
        // Default publish is a no-op on the trait; the mock records instead
        // so daemon dedup tests can assert emission without macOS APIs.
        platform.publish_public_state(r#"{"space":3,"display":1,"window":null}"#);
        platform.publish_public_state(r#"{"space":4,"display":1,"window":null}"#);
        let published = platform.published.lock().unwrap();
        assert_eq!(published.len(), 2);
        assert!(published[0].contains(r#""space":3"#));
        assert!(published[1].contains(r#""space":4"#));
        // Each payload must be valid JSON with stable fields.
        for payload in published.iter() {
            let value: serde_json::Value = serde_json::from_str(payload).unwrap();
            assert!(value.get("space").is_some());
            assert!(value.get("display").is_some());
            assert!(value.get("window").is_some());
        }
    }

    #[test]
    fn track_focused_window_records_ids_in_order() {
        let platform = MockPlatform::default();
        platform.track_focused_window(482);
        platform.track_focused_window(483);
        platform.track_focused_window(0);
        assert_eq!(*platform.tracked.lock().unwrap(), vec![482, 483, 0]);
    }
}
