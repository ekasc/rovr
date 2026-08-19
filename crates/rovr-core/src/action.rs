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
}
