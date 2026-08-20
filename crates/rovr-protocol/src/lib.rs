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
    Scratchpad(ScratchpadCommand),
    Config(ConfigCommand),
    Debug(DebugCommand),
    Subscribe,
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
pub enum ScratchpadCommand {
    Toggle { name: String },
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

/// Stable, versioned broadcast event streamed to subscribers over the IPC
/// socket. `type` is the stable discriminator; new variants are additive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Notification {
    /// Sent immediately when a subscription stream is established.
    Hello { protocol_version: u16 },
    /// A reconcile or command cycle mutated observed state; re-query `state`.
    StateChanged { generation: u64 },
    /// A Space's BSP orientation changed.
    LayoutChanged {
        space: SpaceId,
        horizontal: bool,
        reversed: bool,
    },
    /// A scratchpad was toggled open/closed.
    ScratchpadToggled { name: String, open: bool },
    /// The daemon reloaded its configuration.
    ConfigReloaded,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// M4b: the notification wire format is stable and self-describing (tagged
    /// `type`), and every variant round-trips through JSON.
    #[test]
    fn m4b_notification_round_trips() {
        let cases: Vec<Notification> = vec![
            Notification::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
            Notification::StateChanged { generation: 7 },
            Notification::LayoutChanged {
                space: SpaceId(3),
                horizontal: true,
                reversed: false,
            },
            Notification::ScratchpadToggled {
                name: "term".into(),
                open: true,
            },
            Notification::ConfigReloaded,
        ];
        for n in &cases {
            let json = serde_json::to_string(n).expect("serialize notification");
            let back: Notification = serde_json::from_str(&json).expect("deserialize notification");
            assert_eq!(n, &back, "notification must round-trip: {json}");
        }
        // Stable discriminator + field shape.
        let layout = serde_json::to_value(&Notification::LayoutChanged {
            space: SpaceId(1),
            horizontal: true,
            reversed: false,
        })
        .unwrap();
        assert_eq!(layout["type"], "layout_changed");
        assert_eq!(layout["space"], 1);
        assert_eq!(layout["horizontal"], true);
        assert_eq!(layout["reversed"], false);
    }
}
