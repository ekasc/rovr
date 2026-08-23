//! The ONE reusable command parser for Rovr.
//!
//! CLI, built-in hotkey dispatch, and tests all share this grammar, so bind
//! commands in `rovr.toml` use exactly the syntax the `rovr` CLI accepts —
//! there is no second parser to drift. The CLI binary may still use clap for
//! its argument UX; this module is the shared source of truth for mapping a
//! command STRING to a typed [`Command`].
//!
//! Grammar is the clap CLI order. Optional window/space selectors use the
//! same `--window` / `--space` flags as the CLI:
//!
//! ```text
//! ping
//! doctor
//! query windows|spaces|displays|state|focused|current
//! window focus <id>
//! window focus-direction <north|south|east|west> [from]
//! window set-frame <id> <x> <y> <w> <h>
//! window move-to-space <id> <space>
//! window move-to-workspace <workspace> [window]
//! window set-layer <layer> [window]
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
//! layout rotate|mirror|balance [--space <space>]
//! layout set-ratio <ratio> [--space <space>]
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
    need(
        args,
        1,
        "query windows|spaces|displays|state|focused|current",
    )?;
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
        return err("usage: window <focus|focus-direction|set-frame|move-to-space|move-to-workspace|set-layer|set-sticky|set-shadow|set-opacity|pip|swap|warp|close|toggle-fullscreen|toggle-float|swap-direction|warp-direction|resize> ...");
    };
    let rest = &args[1..];
    let command = match *action {
        "focus" => {
            need(rest, 1, "window focus <id>")?;
            WindowCommand::Focus {
                window: WindowId(parse_u32(rest[0], "window id")?),
            }
        }
        "focus-direction" => {
            if !(1..=2).contains(&rest.len()) {
                return err("usage: window focus-direction <north|south|east|west> [from]");
            }
            let direction = parse_direction(rest[0])?;
            let from = rest
                .get(1)
                .map(|id| parse_u32(id, "window id").map(WindowId))
                .transpose()?;
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
            if !(1..=2).contains(&rest.len()) {
                return err("usage: window move-to-workspace <workspace> [window]");
            }
            let window = rest
                .get(1)
                .map(|id| parse_u32(id, "window id").map(WindowId))
                .transpose()?;
            WindowCommand::MoveToWorkspace {
                window,
                workspace: rest[0].to_string(),
            }
        }
        "set-layer" => {
            if !(1..=2).contains(&rest.len()) {
                return err("usage: window set-layer <layer> [window]");
            }
            let layer = parse_i32(rest[0], "layer")?;
            let window = rest
                .get(1)
                .map(|id| parse_u32(id, "window id").map(WindowId))
                .transpose()?;
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
            need(rest, 3, "window set-opacity <id> <opacity> <duration_ms>")?;
            WindowCommand::SetOpacity {
                window: WindowId(parse_u32(rest[0], "window id")?),
                opacity: parse_f64(rest[1], "opacity")?,
                duration_ms: parse_u64(rest[2], "duration_ms")?,
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
            let (direction, window) = parse_direction_with_window_flag(rest, action)?;
            WindowCommand::SwapDirection { direction, window }
        }
        "warp-dir" | "warp-direction" => {
            let (direction, window) = parse_direction_with_window_flag(rest, action)?;
            WindowCommand::WarpDirection { direction, window }
        }
        "close" | "toggle-fullscreen" | "toggle-float" => {
            if rest.len() > 1 {
                return err(format!("usage: window {action} [window]"));
            }
            let window = rest
                .first()
                .map(|id| parse_u32(id, "window id").map(WindowId))
                .transpose()?;
            match *action {
                "close" => WindowCommand::Close { window },
                "toggle-fullscreen" => WindowCommand::ToggleFullscreen { window },
                _ => WindowCommand::ToggleFloat { window },
            }
        }
        "resize" => {
            let (positionals, window) = parse_window_flag(rest)?;
            need(
                &positionals,
                2,
                "window resize [--window <id>] <edge> <delta>",
            )?;
            WindowCommand::Resize {
                window,
                edge: parse_direction(positionals[0])?,
                delta: parse_i32(positionals[1], "delta")?,
            }
        }
        other => return err(format!("unknown window action {other:?}")),
    };
    Ok(Command::Window(command))
}

fn parse_window_flag<'a>(
    args: &'a [&'a str],
) -> Result<(Vec<&'a str>, Option<WindowId>), ParseError> {
    let mut positionals = Vec::new();
    let mut window = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--window" {
            if window.is_some() || i + 1 >= args.len() {
                return err("--window requires exactly one value");
            }
            window = Some(WindowId(parse_u32(args[i + 1], "window id")?));
            i += 2;
        } else if args[i].starts_with("--") {
            return err(format!("unknown flag {:?}", args[i]));
        } else {
            positionals.push(args[i]);
            i += 1;
        }
    }
    Ok((positionals, window))
}

fn parse_direction_with_window_flag(
    args: &[&str],
    action: &str,
) -> Result<(Direction, Option<WindowId>), ParseError> {
    let (positionals, window) = parse_window_flag(args)?;
    need(
        &positionals,
        1,
        &format!("window {action} <direction> [--window <id>]"),
    )?;
    Ok((parse_direction(positionals[0])?, window))
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
        "next" | "prev" => {
            at_least(rest, 0, "space next|prev [display]")?;
            if rest.len() > 1 {
                return err("usage: space next|prev [display]");
            }
            // Optional positional display: "main" or numeric id. None steps
            // through the display that currently has keyboard focus.
            let display = rest.first().map(|s| s.to_string());
            if *action == "next" {
                SpaceCommand::Next { display }
            } else {
                SpaceCommand::Prev { display }
            }
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
        return err("usage: layout <rotate|mirror|balance|set-ratio> ...");
    };
    let mut positionals = Vec::new();
    let mut space = None;
    let rest = &args[1..];
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "--space" {
            if space.is_some() || i + 1 >= rest.len() {
                return err("--space requires exactly one value");
            }
            space = Some(SpaceId(parse_u64(rest[i + 1], "space id")?));
            i += 2;
        } else if rest[i].starts_with('-') {
            return err(format!("unknown flag {:?}", rest[i]));
        } else {
            positionals.push(rest[i]);
            i += 1;
        }
    }
    let command = match *action {
        "rotate" => {
            need(&positionals, 0, "layout rotate [--space <space>]")?;
            LayoutCommand::Rotate { space }
        }
        "mirror" => {
            need(&positionals, 0, "layout mirror [--space <space>]")?;
            LayoutCommand::Mirror { space }
        }
        "balance" => {
            need(&positionals, 0, "layout balance [--space <space>]")?;
            LayoutCommand::Balance { space }
        }
        "set-ratio" => {
            need(
                &positionals,
                1,
                "layout set-ratio <ratio> [--space <space>]",
            )?;
            LayoutCommand::SetRatio {
                space,
                ratio: parse_f64(positionals[0], "ratio")?,
            }
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
            if !(1..=2).contains(&rest.len()) {
                return err("usage: workspace move-window <workspace> [window]");
            }
            let window = rest
                .get(1)
                .map(|id| parse_u32(id, "window id").map(WindowId))
                .transpose()?;
            WorkspaceCommand::MoveWindow {
                window,
                workspace: rest[0].to_string(),
            }
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
            parse_command("layout rotate --space 42").unwrap(),
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
        assert_eq!(
            parse_command("space next 2"),
            Ok(Command::Space(SpaceCommand::Next {
                display: Some("2".into())
            }))
        );
        assert_eq!(
            parse_command("space prev"),
            Ok(Command::Space(SpaceCommand::Prev { display: None }))
        );
        assert_eq!(
            parse_command("space next main"),
            Ok(Command::Space(SpaceCommand::Next {
                display: Some("main".into())
            }))
        );
        assert!(parse_command("space next 1 2").is_err());
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
            parse_command("layout set-ratio 0.7 --space 3"),
            Ok(Command::Layout(LayoutCommand::SetRatio { space: Some(SpaceId(3)), ratio }))
                if (ratio - 0.7).abs() < 1e-9
        ));
        assert!(matches!(
            parse_command("window focus-direction west 4"),
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
        let r = parse_command("workspace move-window code 4");
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
        for command in ["window swap-dir west", "window swap-direction west"] {
            assert!(matches!(
                parse_command(command),
                Ok(Command::Window(WindowCommand::SwapDirection {
                    direction: Direction::West,
                    window: None
                }))
            ));
        }
        assert!(matches!(
            parse_command("window warp-direction east --window 5"),
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
            parse_command("window resize --window 7 east 30"),
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
            parse_command("layout rotate --space 42"),
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
            parse_command("layout set-ratio 0.5 --space 11"),
            Ok(Command::Layout(LayoutCommand::SetRatio {
                space: Some(SpaceId(11)),
                ratio
            })) if (ratio - 0.5).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn rejects_trailing_unknown_tokens_instead_of_failing_open() {
        for command in [
            "window close garbage",
            "window resize garbage east 30",
            "layout set-ratio garbage 0.5",
            "query windows garbage",
        ] {
            assert!(parse_command(command).is_err(), "accepted {command:?}");
        }
    }
}
