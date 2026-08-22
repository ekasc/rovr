//! The ONE reusable command parser for Rovr.
//!
//! CLI, built-in hotkey dispatch, and tests all share this grammar, so bind
//! commands in `rovr.toml` use exactly the syntax the `rovr` CLI accepts —
//! there is no second parser to drift. The CLI binary may still use clap for
//! its argument UX; this module is the shared source of truth for mapping a
//! command STRING to a typed [`Command`].
//!
//! Grammar (whitespace-separated words, no flags):
//!
//! ```text
//! ping
//! doctor
//! query windows|spaces|displays|state|focused|current
//! window focus <id>
//! window focus-direction <from> <north|south|east|west>
//! window set-frame <id> <x> <y> <w> <h>
//! window move-to-space <id> <space>
//! window move-to-workspace <id> <workspace>
//! window set-layer <id> <layer>
//! window set-sticky <id> <true|false>
//! window set-shadow <id> <true|false>
//! window set-opacity <id> <opacity> [duration_ms]
//! window pip <id>
//! window swap <a> <b>
//! window warp <window> <target>
//! space focus <space>
//! space create [anchor]
//! space destroy <space>
//! space move <space> <after>
//! layout rotate|mirror|balance <space>
//! layout set-ratio <space> <ratio>
//! scratchpad toggle <name>
//! workspace focus <name>
//! workspace move-window <window> <workspace>
//! config reload [path]
//! config check <path>
//! debug events
//! ```

use crate::{
    Command, ConfigCommand, DebugCommand, LayoutCommand, QueryCommand, ScratchpadCommand,
    SpaceCommand, WindowCommand, WorkspaceCommand,
};
use rovr_types::{Direction, Rect, SpaceId, WindowId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ParseError {}

fn err<T>(message: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError {
        message: message.into(),
    })
}

fn parse_u32(s: &str, what: &str) -> Result<u32, ParseError> {
    s.parse::<u32>()
        .map_err(|_| ParseError::message(format!("invalid {what}: {s:?}")))
}

fn parse_u64(s: &str, what: &str) -> Result<u64, ParseError> {
    s.parse::<u64>()
        .map_err(|_| ParseError::message(format!("invalid {what}: {s:?}")))
}

fn parse_f64(s: &str, what: &str) -> Result<f64, ParseError> {
    s.parse::<f64>()
        .map_err(|_| ParseError::message(format!("invalid {what}: {s:?}")))
}

fn parse_bool(s: &str, what: &str) -> Result<bool, ParseError> {
    match s.to_lowercase().as_str() {
        "true" | "on" | "yes" => Ok(true),
        "false" | "off" | "no" => Ok(false),
        _ => err(format!("invalid {what}: {s:?} (expected true|false)")),
    }
}

impl ParseError {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Parse a command string into a typed [`Command`]. Unknown or malformed
/// input is an error — callers must never substitute a different command.
pub fn parse_command(input: &str) -> Result<Command, ParseError> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    let Some(head) = parts.first() else {
        return err("empty command");
    };
    let args = &parts[1..];
    match *head {
        "ping" if args.is_empty() => Ok(Command::Ping),
        "refresh" if args.is_empty() => Ok(Command::Refresh),
        "doctor" if args.is_empty() => Ok(Command::Doctor),
        "query" => parse_query(args),
        "window" => parse_window(args),
        "space" => parse_space(args),
        "layout" => parse_layout(args),
        "scratchpad" => parse_scratchpad(args),
        "workspace" => parse_workspace(args),
        "config" => parse_config(args),
        "debug" => parse_debug(args),
        other => err(format!(
            "unknown command {other:?} (expected window|space|layout|scratchpad|workspace|query|config|debug|ping|doctor)"
        )),
    }
}

fn need(args: &[&str], n: usize, usage: &str) -> Result<(), ParseError> {
    if args.len() != n {
        return err(format!("usage: {usage}"));
    }
    Ok(())
}

fn at_least(args: &[&str], n: usize, usage: &str) -> Result<(), ParseError> {
    if args.len() < n {
        return err(format!("usage: {usage}"));
    }
    Ok(())
}

fn parse_query(args: &[&str]) -> Result<Command, ParseError> {
    let Some(what) = args.first() else {
        return err("usage: query windows|spaces|displays|state|focused|current");
    };
    let command = match *what {
        "windows" => QueryCommand::Windows,
        "spaces" => QueryCommand::Spaces,
        "displays" => QueryCommand::Displays,
        "state" => QueryCommand::State,
        "focused" => QueryCommand::Focused,
        "current" => QueryCommand::Current,
        other => return err(format!("unknown query target {other:?}")),
    };
    Ok(Command::Query(command))
}

fn parse_window(args: &[&str]) -> Result<Command, ParseError> {
    let Some(action) = args.first() else {
        return err("usage: window <focus|focus-direction|set-frame|move-to-space|move-to-workspace|set-layer|set-sticky|set-shadow|set-opacity|pip|swap|warp|close|toggle-fullscreen|toggle-float|swap-dir|warp-dir|resize> ...");
    };
    let rest = &args[1..];
    // Focused-defaulting commands accept an OPTIONAL leading window id:
    // both `window close` and `window close 42` are valid.
    fn take_optional_window<'a>(rest: &'a [&'a str]) -> (Option<WindowId>, &'a [&'a str]) {
        match rest.first().and_then(|s| s.parse::<u32>().ok()) {
            Some(id) => (Some(WindowId(id)), &rest[1..]),
            None => (None, rest),
        }
    }
    let command = match *action {
        "focus" => {
            need(rest, 1, "window focus <id>")?;
            WindowCommand::Focus {
                window: WindowId(parse_u32(rest[0], "window id")?),
            }
        }
        "focus-direction" => {
            at_least(
                rest,
                1,
                "window focus-direction [from] <north|south|east|west>",
            )?;
            let direction = parse_direction(rest[rest.len() - 1])?;
            let from = if rest.len() >= 2 {
                Some(WindowId(parse_u32(rest[0], "window id")?))
            } else {
                None
            };
            WindowCommand::FocusDirection { from, direction }
        }
        "set-frame" => {
            need(rest, 5, "window set-frame <id> <x> <y> <width> <height>")?;
            WindowCommand::SetFrame {
                window: WindowId(parse_u32(rest[0], "window id")?),
                frame: Rect {
                    x: parse_f64(rest[1], "x")?,
                    y: parse_f64(rest[2], "y")?,
                    width: parse_f64(rest[3], "width")?,
                    height: parse_f64(rest[4], "height")?,
                },
            }
        }
        "move-to-space" => {
            need(rest, 2, "window move-to-space <id> <space>")?;
            WindowCommand::MoveToSpace {
                window: WindowId(parse_u32(rest[0], "window id")?),
                space: SpaceId(parse_u64(rest[1], "space id")?),
            }
        }
        "move-to-workspace" => {
            need(rest, 1, "window move-to-workspace [id] <workspace>")?;
            let (window, workspace) = if rest.len() >= 2 {
                (
                    Some(WindowId(parse_u32(rest[0], "window id")?)),
                    rest[1].to_string(),
                )
            } else {
                (None, rest[0].to_string())
            };
            WindowCommand::MoveToWorkspace { window, workspace }
        }
        "set-layer" => {
            at_least(rest, 1, "window set-layer [id] <layer>")?;
            let layer = parse_i32(rest[rest.len() - 1], "layer")?;
            let (window, _) = take_optional_window(rest);
            WindowCommand::SetLayer { window, layer }
        }
        "set-sticky" => {
            need(rest, 2, "window set-sticky <id> <true|false>")?;
            WindowCommand::SetSticky {
                window: WindowId(parse_u32(rest[0], "window id")?),
                sticky: parse_bool(rest[1], "sticky")?,
            }
        }
        "set-shadow" => {
            need(rest, 2, "window set-shadow <id> <true|false>")?;
            WindowCommand::SetShadow {
                window: WindowId(parse_u32(rest[0], "window id")?),
                shadow: parse_bool(rest[1], "shadow")?,
            }
        }
        "set-opacity" => {
            at_least(rest, 2, "window set-opacity <id> <opacity> [duration_ms]")?;
            WindowCommand::SetOpacity {
                window: WindowId(parse_u32(rest[0], "window id")?),
                opacity: parse_f64(rest[1], "opacity")?,
                duration_ms: if rest.len() > 2 {
                    parse_u64(rest[2], "duration_ms")?
                } else {
                    0
                },
            }
        }
        "pip" => {
            need(rest, 1, "window pip <id>")?;
            WindowCommand::Pip {
                window: WindowId(parse_u32(rest[0], "window id")?),
            }
        }
        "swap" => {
            need(rest, 2, "window swap <a> <b>")?;
            WindowCommand::Swap {
                a: WindowId(parse_u32(rest[0], "window id")?),
                b: WindowId(parse_u32(rest[1], "window id")?),
            }
        }
        "warp" => {
            need(rest, 2, "window warp <window> <target>")?;
            WindowCommand::Warp {
                window: WindowId(parse_u32(rest[0], "window id")?),
                target: WindowId(parse_u32(rest[1], "target window id")?),
            }
        }
        "swap-dir" | "swap-direction" => {
            at_least(rest, 1, "window swap-dir [id] <north|south|east|west>")?;
            let (window, direction) = split_optional_id_direction(rest)?;
            WindowCommand::SwapDirection { direction, window }
        }
        "warp-dir" | "warp-direction" => {
            at_least(rest, 1, "window warp-dir [id] <north|south|east|west>")?;
            let (window, direction) = split_optional_id_direction(rest)?;
            WindowCommand::WarpDirection { direction, window }
        }
        "close" => {
            let (window, _) = take_optional_window(rest);
            WindowCommand::Close { window }
        }
        "toggle-fullscreen" => {
            let (window, _) = take_optional_window(rest);
            WindowCommand::ToggleFullscreen { window }
        }
        "toggle-float" => {
            let (window, _) = take_optional_window(rest);
            WindowCommand::ToggleFloat { window }
        }
        "resize" => {
            at_least(
                rest,
                2,
                "window resize [id] <north|south|east|west> <delta>",
            )?;
            // Trailing delta, direction before it, optional leading id.
            let delta = parse_i32(rest[rest.len() - 1], "delta")?;
            let direction = parse_direction(rest[rest.len() - 2])?;
            let head = &rest[..rest.len() - 2];
            let (window, _) = take_optional_window(head);
            WindowCommand::Resize {
                window,
                edge: direction,
                delta,
            }
        }
        other => return err(format!("unknown window action {other:?}")),
    };
    Ok(Command::Window(command))
}

/// `[id] <direction>` with the id optional: one token = direction only,
/// two tokens = id then direction.
fn split_optional_id_direction(rest: &[&str]) -> Result<(Option<WindowId>, Direction), ParseError> {
    let direction = parse_direction(rest[rest.len() - 1])?;
    if rest.len() >= 2 {
        Ok((Some(WindowId(parse_u32(rest[0], "window id")?)), direction))
    } else {
        Ok((None, direction))
    }
}

fn parse_direction(s: &str) -> Result<Direction, ParseError> {
    match s.to_lowercase().as_str() {
        "north" => Ok(Direction::North),
        "south" => Ok(Direction::South),
        "east" => Ok(Direction::East),
        "west" => Ok(Direction::West),
        other => Err(ParseError::message(format!("invalid direction {other:?}"))),
    }
}

fn parse_i32(s: &str, what: &str) -> Result<i32, ParseError> {
    s.parse::<i32>()
        .map_err(|_| ParseError::message(format!("invalid {what}: {s:?}")))
}

fn parse_space(args: &[&str]) -> Result<Command, ParseError> {
    let Some(action) = args.first() else {
        return err("usage: space <focus|create|destroy|move|focus-recent> ...");
    };
    let rest = &args[1..];
    let command = match *action {
        "focus" => {
            need(rest, 1, "space focus <space>")?;
            SpaceCommand::Focus {
                space: SpaceId(parse_u64(rest[0], "space id")?),
            }
        }
        "focus-recent" => {
            need(rest, 0, "space focus-recent")?;
            SpaceCommand::FocusRecent
        }
        "toggle-insets" => {
            need(rest, 0, "space toggle-insets")?;
            SpaceCommand::ToggleInsets
        }
        "create" => {
            at_least(rest, 0, "space create [anchor]")?;
            if rest.len() > 1 {
                return err("usage: space create [anchor]");
            }
            let anchor = match rest.first() {
                Some(a) => Some(SpaceId(parse_u64(a, "anchor space id")?)),
                None => None,
            };
            SpaceCommand::Create { anchor }
        }
        "destroy" => {
            need(rest, 1, "space destroy <space>")?;
            SpaceCommand::Destroy {
                space: SpaceId(parse_u64(rest[0], "space id")?),
            }
        }
        "move" => {
            need(rest, 2, "space move <space> <after>")?;
            SpaceCommand::Move {
                space: SpaceId(parse_u64(rest[0], "space id")?),
                after: SpaceId(parse_u64(rest[1], "after space id")?),
            }
        }
        other => return err(format!("unknown space action {other:?}")),
    };
    Ok(Command::Space(command))
}

fn parse_layout(args: &[&str]) -> Result<Command, ParseError> {
    let Some(action) = args.first() else {
        return err("usage: layout <rotate|mirror|balance|set-ratio> [space] [ratio]");
    };
    let rest = &args[1..];
    // All layout commands default to the focused space; an explicit numeric
    // space id is accepted for scripting.
    let (space, rest): (Option<SpaceId>, &[&str]) =
        match rest.first().and_then(|v| v.parse::<u64>().ok()) {
            Some(id) => (Some(SpaceId(id)), &rest[1..]),
            None => (None, rest),
        };
    let command = match *action {
        "rotate" => {
            need(rest, 0, "layout rotate [space]")?;
            LayoutCommand::Rotate { space }
        }
        "mirror" => {
            need(rest, 0, "layout mirror [space]")?;
            LayoutCommand::Mirror { space }
        }
        "balance" => {
            need(rest, 0, "layout balance [space]")?;
            LayoutCommand::Balance { space }
        }
        "set-ratio" => {
            at_least(rest, 1, "layout set-ratio [space] <ratio>")?;
            let ratio = parse_f64(rest[rest.len() - 1], "ratio")?;
            LayoutCommand::SetRatio { space, ratio }
        }
        other => return err(format!("unknown layout action {other:?}")),
    };
    Ok(Command::Layout(command))
}

fn parse_scratchpad(args: &[&str]) -> Result<Command, ParseError> {
    let Some(action) = args.first() else {
        return err("usage: scratchpad toggle <name>");
    };
    let rest = &args[1..];
    match *action {
        "toggle" => {
            need(rest, 1, "scratchpad toggle <name>")?;
            Ok(Command::Scratchpad(ScratchpadCommand::Toggle {
                name: rest[0].to_string(),
            }))
        }
        other => err(format!("unknown scratchpad action {other:?}")),
    }
}

fn parse_workspace(args: &[&str]) -> Result<Command, ParseError> {
    let Some(action) = args.first() else {
        return err("usage: workspace <focus|move-window> ...");
    };
    let rest = &args[1..];
    let command = match *action {
        "focus" => {
            need(rest, 1, "workspace focus <name>")?;
            WorkspaceCommand::Focus {
                name: rest[0].to_string(),
            }
        }
        "move-window" => {
            at_least(rest, 1, "workspace move-window [window] <workspace>")?;
            let (window, workspace) = if rest.len() >= 2 {
                (
                    Some(WindowId(parse_u32(rest[0], "window id")?)),
                    rest[1].to_string(),
                )
            } else {
                (None, rest[0].to_string())
            };
            WorkspaceCommand::MoveWindow { window, workspace }
        }
        other => return err(format!("unknown workspace action {other:?}")),
    };
    Ok(Command::Workspace(command))
}

fn parse_config(args: &[&str]) -> Result<Command, ParseError> {
    let Some(action) = args.first() else {
        return err("usage: config <reload|check> [path]");
    };
    let rest = &args[1..];
    let command = match *action {
        "reload" => {
            at_least(rest, 0, "config reload [path]")?;
            if rest.len() > 1 {
                return err("usage: config reload [path]");
            }
            ConfigCommand::Reload {
                path: rest.first().map(|s| s.to_string()),
            }
        }
        "check" => {
            need(rest, 1, "config check <path>")?;
            ConfigCommand::Check {
                path: rest[0].to_string(),
            }
        }
        other => return err(format!("unknown config action {other:?}")),
    };
    Ok(Command::Config(command))
}

fn parse_debug(args: &[&str]) -> Result<Command, ParseError> {
    let Some(action) = args.first() else {
        return err("usage: debug events");
    };
    match *action {
        "events" if args.len() == 1 => Ok(Command::Debug(DebugCommand::Events)),
        other => err(format!("unknown debug action {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blocker 7: the shared parser understands the real CLI syntax.
    #[test]
    fn blocker7_parses_cli_style_commands() {
        assert_eq!(
            parse_command("window focus 123").unwrap(),
            Command::Window(WindowCommand::Focus {
                window: WindowId(123)
            })
        );
        assert_eq!(
            parse_command("workspace focus code").unwrap(),
            Command::Workspace(WorkspaceCommand::Focus {
                name: "code".into()
            })
        );
        assert_eq!(
            parse_command("layout rotate 42").unwrap(),
            Command::Layout(LayoutCommand::Rotate {
                space: Some(SpaceId(42))
            })
        );
        assert_eq!(
            parse_command("scratchpad toggle terminal").unwrap(),
            Command::Scratchpad(ScratchpadCommand::Toggle {
                name: "terminal".into()
            })
        );
        assert_eq!(parse_command("ping").unwrap(), Command::Ping);
    }

    /// Blocker 8: invalid commands are ERRORS, never another command.
    #[test]
    fn blocker8_invalid_commands_are_errors_not_substitutions() {
        assert!(
            parse_command("window --focus 1").is_err(),
            "flag syntax must be rejected"
        );
        assert!(
            parse_command("layout --rotate 1").is_err(),
            "flag syntax must be rejected"
        );
        assert!(parse_command("nonsense").is_err());
        assert!(parse_command("").is_err());
        assert!(parse_command("window focus notanumber").is_err());
        assert!(parse_command("window focus").is_err());
    }

    #[test]
    fn parses_remaining_grammar() {
        assert!(matches!(
            parse_command("query windows"),
            Ok(Command::Query(QueryCommand::Windows))
        ));
        assert!(matches!(
            parse_command("space create"),
            Ok(Command::Space(SpaceCommand::Create { anchor: None }))
        ));
        assert!(matches!(
            parse_command("space create 5"),
            Ok(Command::Space(SpaceCommand::Create {
                anchor: Some(SpaceId(5))
            }))
        ));
        assert!(matches!(
            parse_command("window swap 1 2"),
            Ok(Command::Window(WindowCommand::Swap {
                a: WindowId(1),
                b: WindowId(2)
            }))
        ));
        assert!(matches!(
            parse_command("window warp 1 2"),
            Ok(Command::Window(WindowCommand::Warp {
                window: WindowId(1),
                target: WindowId(2)
            }))
        ));
        assert!(matches!(
            parse_command("layout set-ratio 3 0.7"),
            Ok(Command::Layout(LayoutCommand::SetRatio { space: Some(SpaceId(3)), ratio }))
                if (ratio - 0.7).abs() < 1e-9
        ));
        assert!(matches!(
            parse_command("window focus-direction 4 west"),
            Ok(Command::Window(WindowCommand::FocusDirection {
                from: Some(WindowId(4)),
                direction: Direction::West
            }))
        ));
        assert!(matches!(
            parse_command("window set-sticky 4 true"),
            Ok(Command::Window(WindowCommand::SetSticky {
                window: WindowId(4),
                sticky: true
            }))
        ));
        let r = parse_command("workspace move-window 4 code");
        assert!(matches!(
            r,
            Ok(Command::Workspace(WorkspaceCommand::MoveWindow { window: Some(WindowId(4)), workspace }))
                if workspace == "code"
        ));
    }

    /// Focused-defaulting window commands parse with and without an explicit
    /// id, and the new directional/float/fullscreen/close commands bind to
    /// the right variants (yabai-parity surface).
    #[test]
    fn parses_focused_defaulting_and_new_commands() {
        use rovr_types::WindowId;

        // Optional id on close / toggle-fullscreen / toggle-float.
        assert!(matches!(
            parse_command("window close"),
            Ok(Command::Window(WindowCommand::Close { window: None }))
        ));
        assert!(matches!(
            parse_command("window close 42"),
            Ok(Command::Window(WindowCommand::Close {
                window: Some(WindowId(42))
            }))
        ));
        assert!(matches!(
            parse_command("window toggle-fullscreen"),
            Ok(Command::Window(WindowCommand::ToggleFullscreen {
                window: None
            }))
        ));
        assert!(matches!(
            parse_command("window toggle-float"),
            Ok(Command::Window(WindowCommand::ToggleFloat { window: None }))
        ));

        // Directional swap/warp with optional trailing id.
        assert!(matches!(
            parse_command("window swap-dir west"),
            Ok(Command::Window(WindowCommand::SwapDirection {
                direction: Direction::West,
                window: None
            }))
        ));
        assert!(matches!(
            parse_command("window warp-dir 5 east"),
            Ok(Command::Window(WindowCommand::WarpDirection {
                direction: Direction::East,
                window: Some(WindowId(5))
            }))
        ));

        // Resize: [id] edge delta.
        assert!(matches!(
            parse_command("window resize north -20"),
            Ok(Command::Window(WindowCommand::Resize {
                window: None,
                edge: Direction::North,
                delta: -20
            }))
        ));
        assert!(matches!(
            parse_command("window resize 7 east 30"),
            Ok(Command::Window(WindowCommand::Resize {
                window: Some(WindowId(7)),
                edge: Direction::East,
                delta: 30
            }))
        ));

        // Focus-direction without the leading id (yabai-style binds).
        assert!(matches!(
            parse_command("window focus-direction west"),
            Ok(Command::Window(WindowCommand::FocusDirection {
                from: None,
                direction: Direction::West
            }))
        ));

        // move-to-workspace without id; space focus-recent.
        assert!(matches!(
            parse_command("window move-to-workspace code"),
            Ok(Command::Window(WindowCommand::MoveToWorkspace {
                window: None,
                workspace
            })) if workspace == "code"
        ));
        assert!(matches!(
            parse_command("space focus-recent"),
            Ok(Command::Space(SpaceCommand::FocusRecent))
        ));
    }

    /// Layout commands default to the focused space when no id is given.
    #[test]
    fn parses_layout_with_focused_space_default() {
        use rovr_types::SpaceId;

        assert!(matches!(
            parse_command("layout rotate"),
            Ok(Command::Layout(LayoutCommand::Rotate { space: None }))
        ));
        assert!(matches!(
            parse_command("layout rotate 42"),
            Ok(Command::Layout(LayoutCommand::Rotate {
                space: Some(SpaceId(42))
            }))
        ));
        assert!(matches!(
            parse_command("layout set-ratio 0.7"),
            Ok(Command::Layout(LayoutCommand::SetRatio {
                space: None,
                ratio
            })) if (ratio - 0.7).abs() < f64::EPSILON
        ));
        assert!(matches!(
            parse_command("layout set-ratio 11 0.5"),
            Ok(Command::Layout(LayoutCommand::SetRatio {
                space: Some(SpaceId(11)),
                ratio
            })) if (ratio - 0.5).abs() < f64::EPSILON
        ));
    }
}
