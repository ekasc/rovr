use rovr_types::{Direction, Rect, SpaceId, WindowId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    RefreshAll,
    RefreshWindow {
        window: WindowId,
    },
    SetWindowFrame {
        window: WindowId,
        frame: Rect,
    },
    MoveWindowToSpace {
        window: WindowId,
        space: SpaceId,
    },
    FocusWindow {
        window: WindowId,
    },
    FocusDirection {
        from: WindowId,
        direction: Direction,
    },
    FocusSpace {
        space: SpaceId,
    },
    /// Focus `target` by moving `delta` positions from the optimistic current Space.
    FocusSpaceStep {
        target: SpaceId,
        delta: i32,
    },
    CreateSpace {
        anchor: SpaceId,
    },
    DestroySpace {
        space: SpaceId,
    },
    MoveSpace {
        space: SpaceId,
        after: SpaceId,
    },
    SetWindowLayer {
        window: WindowId,
        layer: i32,
    },
    SetWindowSticky {
        window: WindowId,
        sticky: bool,
    },
    SetWindowShadow {
        window: WindowId,
        shadow: bool,
    },
    SetWindowOpacity {
        window: WindowId,
        opacity: f64,
        duration_ms: u64,
    },
    SetWindowScale {
        window: WindowId,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    SetWindowMinimized {
        window: WindowId,
        minimized: bool,
    },
    /// Close a window by pressing its AX close button.
    CloseWindow {
        window: WindowId,
    },
    /// Toggle the native (green-button) fullscreen state via AX.
    ToggleNativeFullscreen {
        window: WindowId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_space_step_serializes_with_target_and_delta() {
        let value = serde_json::to_value(Action::FocusSpaceStep {
            target: SpaceId(42),
            delta: -3,
        })
        .unwrap();
        assert_eq!(value["type"], "focus_space_step");
        assert_eq!(value["target"], 42);
        assert_eq!(value["delta"], -3);
    }
}
