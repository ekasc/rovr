use std::collections::HashMap;

use rovr_types::{
    DisplayId, DisplaySnapshot, Rect, SpaceId, SpaceSnapshot, WindowId, WindowSnapshot,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedState {
    pub generation: u64,
    pub refresh_required: bool,
    pub windows: HashMap<WindowId, WindowSnapshot>,
    pub spaces: HashMap<SpaceId, SpaceSnapshot>,
    pub displays: HashMap<DisplayId, DisplaySnapshot>,
}

impl Default for ObservedState {
    fn default() -> Self {
        Self {
            generation: 1,
            refresh_required: true,
            windows: HashMap::new(),
            spaces: HashMap::new(),
            displays: HashMap::new(),
        }
    }
}

impl ObservedState {
    pub fn bump_generation(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.refresh_required = true;
    }

    pub fn window_is_fresh(&self, id: WindowId) -> bool {
        self.windows
            .get(&id)
            .is_some_and(|window| window.generation == self.generation)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesiredState {
    pub windows: HashMap<WindowId, WindowTarget>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowTarget {
    pub frame: Option<Rect>,
    pub space: Option<SpaceId>,
    pub focused: Option<bool>,
    /// User-requested float: excluded from tiling until toggled back.
    /// Persists with the rest of desired state across daemon restarts.
    #[serde(default)]
    pub floating: bool,
}
