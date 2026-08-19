use rovr_types::{Direction, Rect, SpaceId, WindowId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub version: u16,
    pub id: u64,
    pub command: Command,
}

impl Request {
    pub fn new(id: u64, command: Command) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            command,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "domain", content = "command", rename_all = "snake_case")]
pub enum Command {
    Ping,
    Doctor,
    Query(QueryCommand),
    Window(WindowCommand),
    Space(SpaceCommand),
    Layout(LayoutCommand),
    Config(ConfigCommand),
    Debug(DebugCommand),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueryCommand {
    Windows,
    Spaces,
    Displays,
    State,
    Focused,
    Current,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WindowCommand {
    Focus {
        window: WindowId,
    },
    FocusDirection {
        from: WindowId,
        direction: Direction,
    },
    SetFrame {
        window: WindowId,
        frame: Rect,
    },
    MoveToSpace {
        window: WindowId,
        space: SpaceId,
    },
    SetLayer {
        window: WindowId,
        layer: i32,
    },
    SetSticky {
        window: WindowId,
        sticky: bool,
    },
    SetShadow {
        window: WindowId,
        shadow: bool,
    },
    SetOpacity {
        window: WindowId,
        opacity: f64,
        duration_ms: u64,
    },
    Pip {
        window: WindowId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpaceCommand {
    Focus { space: SpaceId },
    Create { anchor: Option<SpaceId> },
    Destroy { space: SpaceId },
    Move { space: SpaceId, after: SpaceId },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutCommand {
    Rotate { space: SpaceId },
    Mirror { space: SpaceId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigCommand {
    Reload { path: Option<String> },
    Check { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DebugCommand {
    Events,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub version: u16,
    pub id: u64,
    #[serde(flatten)]
    pub outcome: ResponseOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResponseOutcome {
    Ok { result: Value },
    Error { error: ErrorPayload },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
}

impl Response {
    pub fn ok(id: u64, result: impl Serialize) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            outcome: ResponseOutcome::Ok {
                result: serde_json::to_value(result).unwrap_or(Value::Null),
            },
        }
    }

    pub fn error(id: u64, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            outcome: ResponseOutcome::Error {
                error: ErrorPayload {
                    code: code.into(),
                    message: message.into(),
                },
            },
        }
    }
}
