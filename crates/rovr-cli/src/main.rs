use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use rovr_protocol::{
    Command, ConfigCommand, DebugCommand, QueryCommand, Request, Response, WindowCommand,
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
        }),
        TopCommand::Config(args) => Command::Config(match args.command {
            ConfigSubcommand::Reload { path } => ConfigCommand::Reload { path },
            ConfigSubcommand::Check { path } => ConfigCommand::Check { path },
        }),
        TopCommand::Debug(args) => Command::Debug(match args.command {
            DebugSubcommand::Events => DebugCommand::Events,
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
