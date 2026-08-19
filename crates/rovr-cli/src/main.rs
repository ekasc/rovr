use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use rovr_protocol::{
    Command, ConfigCommand, DebugCommand, LayoutCommand, QueryCommand, Request, Response,
    SpaceCommand, WindowCommand,
};
use rovr_types::{Direction, Rect, SpaceId, WindowId};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Parser)]
#[command(name = "rovr", version, about = "Rovr macOS window manager client")]
struct Cli {
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: TopCommand,
}

#[derive(Debug, Subcommand)]
enum TopCommand {
    Ping,
    Doctor,
    Query(QueryArgs),
    Window(WindowArgs),
    Space(SpaceArgs),
    Layout(LayoutArgs),
    Config(ConfigArgs),
    Debug(DebugArgs),
}

#[derive(Debug, Args)]
struct QueryArgs {
    #[command(subcommand)]
    command: QuerySubcommand,
}

#[derive(Debug, Subcommand)]
enum QuerySubcommand {
    Windows,
    Spaces,
    Displays,
    State,
    Focused,
    Current,
}

#[derive(Debug, Args)]
struct WindowArgs {
    #[command(subcommand)]
    command: WindowSubcommand,
}

#[derive(Debug, Subcommand)]
enum WindowSubcommand {
    Focus {
        window: u32,
    },
    FocusDirection {
        from: u32,
        direction: DirectionArg,
    },
    SetFrame {
        window: u32,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    MoveToSpace {
        window: u32,
        space: u64,
    },
    SetLayer {
        window: u32,
        layer: i32,
    },
    SetSticky {
        window: u32,
        #[arg(action = clap::ArgAction::Set)]
        sticky: bool,
    },
    SetShadow {
        window: u32,
        #[arg(action = clap::ArgAction::Set)]
        shadow: bool,
    },
    SetOpacity {
        window: u32,
        opacity: f64,
        duration_ms: u64,
    },
    Pip {
        window: u32,
    },
}

#[derive(Debug, Args)]
struct SpaceArgs {
    #[command(subcommand)]
    command: SpaceSubcommand,
}

#[derive(Debug, Subcommand)]
enum SpaceSubcommand {
    Focus { space: u64 },
    Create { anchor: Option<u64> },
    Destroy { space: u64 },
    Move { space: u64, after: u64 },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DirectionArg {
    North,
    South,
    East,
    West,
}

impl From<DirectionArg> for Direction {
    fn from(value: DirectionArg) -> Self {
        match value {
            DirectionArg::North => Direction::North,
            DirectionArg::South => Direction::South,
            DirectionArg::East => Direction::East,
            DirectionArg::West => Direction::West,
        }
    }
}

#[derive(Debug, Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigSubcommand,
}

#[derive(Debug, Subcommand)]
enum ConfigSubcommand {
    Reload { path: Option<String> },
    Check { path: String },
}

#[derive(Debug, Args)]
struct DebugArgs {
    #[command(subcommand)]
    command: DebugSubcommand,
}

#[derive(Debug, Subcommand)]
enum DebugSubcommand {
    Events,
}
#[derive(Debug, Args)]
struct LayoutArgs {
    #[command(subcommand)]
    command: LayoutSubcommand,
}

#[derive(Debug, Subcommand)]
enum LayoutSubcommand {
    Rotate { space: u64 },
    Mirror { space: u64 },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket = cli.socket.unwrap_or_else(default_socket_path);
    let command = map_command(cli.command);
    let request = Request::new(NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed), command);
    let response = send(&socket, &request)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn map_command(command: TopCommand) -> Command {
    match command {
        TopCommand::Ping => Command::Ping,
        TopCommand::Doctor => Command::Doctor,
        TopCommand::Query(args) => Command::Query(match args.command {
            QuerySubcommand::Windows => QueryCommand::Windows,
            QuerySubcommand::Spaces => QueryCommand::Spaces,
            QuerySubcommand::Displays => QueryCommand::Displays,
            QuerySubcommand::State => QueryCommand::State,
            QuerySubcommand::Focused => QueryCommand::Focused,
            QuerySubcommand::Current => QueryCommand::Current,
        }),
        TopCommand::Window(args) => Command::Window(match args.command {
            WindowSubcommand::Focus { window } => WindowCommand::Focus {
                window: WindowId(window),
            },
            WindowSubcommand::FocusDirection { from, direction } => WindowCommand::FocusDirection {
                from: WindowId(from),
                direction: direction.into(),
            },
            WindowSubcommand::SetFrame {
                window,
                x,
                y,
                width,
                height,
            } => WindowCommand::SetFrame {
                window: WindowId(window),
                frame: Rect {
                    x,
                    y,
                    width,
                    height,
                },
            },
            WindowSubcommand::MoveToSpace { window, space } => WindowCommand::MoveToSpace {
                window: WindowId(window),
                space: SpaceId(space),
            },
            WindowSubcommand::SetLayer { window, layer } => WindowCommand::SetLayer {
                window: WindowId(window),
                layer,
            },
            WindowSubcommand::SetSticky { window, sticky } => WindowCommand::SetSticky {
                window: WindowId(window),
                sticky,
            },
            WindowSubcommand::SetShadow { window, shadow } => WindowCommand::SetShadow {
                window: WindowId(window),
                shadow,
            },
            WindowSubcommand::SetOpacity {
                window,
                opacity,
                duration_ms,
            } => WindowCommand::SetOpacity {
                window: WindowId(window),
                opacity,
                duration_ms,
            },
            WindowSubcommand::Pip { window } => WindowCommand::Pip {
                window: WindowId(window),
            },
        }),
        TopCommand::Space(args) => Command::Space(match args.command {
            SpaceSubcommand::Focus { space } => SpaceCommand::Focus {
                space: SpaceId(space),
            },
            SpaceSubcommand::Create { anchor } => SpaceCommand::Create {
                anchor: anchor.map(SpaceId),
            },
            SpaceSubcommand::Destroy { space } => SpaceCommand::Destroy {
                space: SpaceId(space),
            },
            SpaceSubcommand::Move { space, after } => SpaceCommand::Move {
                space: SpaceId(space),
                after: SpaceId(after),
            },
        }),
        TopCommand::Config(args) => Command::Config(match args.command {
            ConfigSubcommand::Reload { path } => ConfigCommand::Reload { path },
            ConfigSubcommand::Check { path } => ConfigCommand::Check { path },
        }),
        TopCommand::Debug(args) => Command::Debug(match args.command {
            DebugSubcommand::Events => DebugCommand::Events,
        }),
        TopCommand::Layout(args) => Command::Layout(match args.command {
            LayoutSubcommand::Rotate { space } => LayoutCommand::Rotate {
                space: SpaceId(space),
            },
            LayoutSubcommand::Mirror { space } => LayoutCommand::Mirror {
                space: SpaceId(space),
            },
        }),
    }
}

fn send(path: &Path, request: &Request) -> Result<Response> {
    let mut stream = UnixStream::connect(path)
        .with_context(|| format!("connect to rovr daemon at {}", path.display()))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).context("decode daemon response")
}

fn default_socket_path() -> PathBuf {
    let uid = std::env::var("UID").unwrap_or_else(|_| "unknown".into());
    PathBuf::from(format!("/tmp/rovr-{uid}.sock"))
}
