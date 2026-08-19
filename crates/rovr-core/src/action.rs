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
}
