use rovr_types::SpaceId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Primary split axis for a Space's BSP tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Axis {
    #[default]
    Vertical,
    Horizontal,
}

/// Orientation of a Space's BSP tree. `reversed` flips the window order
/// (180°/270° rotations); `axis` is the root split direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Orientation {
    pub axis: Axis,
    pub reversed: bool,
}

impl Orientation {
    /// Cycle through four 90° rotations:
    /// (V,false) -> (H,false) -> (V,true) -> (H,true) -> (V,false).
    pub fn rotate(self) -> Self {
        match (self.axis, self.reversed) {
            (Axis::Vertical, false) => Orientation {
                axis: Axis::Horizontal,
                reversed: false,
            },
            (Axis::Horizontal, false) => Orientation {
                axis: Axis::Vertical,
                reversed: true,
            },
            (Axis::Vertical, true) => Orientation {
                axis: Axis::Horizontal,
                reversed: true,
            },
            (Axis::Horizontal, true) => Orientation {
                axis: Axis::Vertical,
                reversed: false,
            },
        }
    }

    /// Flip the primary axis, keeping the reversal.
    pub fn mirror(self) -> Self {
        Orientation {
            axis: match self.axis {
                Axis::Vertical => Axis::Horizontal,
                Axis::Horizontal => Axis::Vertical,
            },
            reversed: self.reversed,
        }
    }
}

/// Per-Space layout state. Currently just orientation; window order is derived
/// from the observed snapshot each cycle (M3a recomputes), so order is not
/// stored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LayoutState {
    pub orientation: Orientation,
}

pub type Layouts = HashMap<SpaceId, LayoutState>;
/// Per-named-scratchpad open/closed state. A scratchpad is "open" when its
/// members should be excluded from tiling. Toggling flips the bool.
#[derive(Debug, Clone, Default)]
pub struct ScratchpadState(pub HashMap<String, bool>);

impl ScratchpadState {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn is_open(&self, name: &str) -> bool {
        self.0.get(name).copied().unwrap_or(false)
    }

    pub fn toggle(&mut self, name: &str) {
        let entry = self.0.entry(name.to_string()).or_insert(false);
        *entry = !*entry;
    }
}
