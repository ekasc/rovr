use rovr_types::{DisplayId, PlatformSnapshot, SpaceId, WindowId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Snapshot(PlatformSnapshot),
    WindowDestroyed { window: WindowId },
    SpaceDestroyed { space: SpaceId },
    DisplayRemoved { display: DisplayId },
    SystemWillSleep,
    SystemWoke,
    DockRestarted,
    DisplayTopologyChanged,
}
