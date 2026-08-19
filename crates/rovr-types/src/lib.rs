use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($name:ident, $inner:ty) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub $inner);

        impl From<$inner> for $name {
            fn from(value: $inner) -> Self {
                Self(value)
            }
        }
    };
}

id_type!(WindowId, u32);
id_type!(SpaceId, u64);
id_type!(DisplayId, u32);
id_type!(ProcessId, i32);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn center(self) -> Point {
        Point {
            x: self.x + self.width / 2.0,
            y: self.y + self.height / 2.0,
        }
    }

    pub fn approx_eq(self, other: Self, epsilon: f64) -> bool {
        (self.x - other.x).abs() <= epsilon
            && (self.y - other.y).abs() <= epsilon
            && (self.width - other.width).abs() <= epsilon
            && (self.height - other.height).abs() <= epsilon
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    North,
    South,
    East,
    West,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutKind {
    Bsp,
    Stack,
    Master,
    Columns,
    Monocle,
    Float,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowSnapshot {
    pub id: WindowId,
    pub pid: ProcessId,
    pub app: String,
    pub bundle_id: Option<String>,
    pub title: String,
    pub frame: Rect,
    pub space_id: Option<SpaceId>,
    pub display_id: Option<DisplayId>,
    pub focused: bool,
    pub minimized: bool,
    pub fullscreen: bool,
    pub managed: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceSnapshot {
    pub id: SpaceId,
    pub display_id: DisplayId,
    pub label: Option<String>,
    pub focused: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplaySnapshot {
    pub id: DisplayId,
    pub frame: Rect,
    pub label: Option<String>,
    pub focused: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PlatformSnapshot {
    pub windows: Vec<WindowSnapshot>,
    pub spaces: Vec<SpaceSnapshot>,
    pub displays: Vec<DisplaySnapshot>,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Capabilities {
    pub observe_windows: bool,
    pub set_window_frame: bool,
    pub focus_window: bool,
    pub move_window_to_space: bool,
    pub create_space: bool,
    pub destroy_space: bool,
    pub focus_space: bool,
    pub set_window_layer: bool,
    pub set_window_opacity: bool,
    pub scripting_addition: bool,
}
