use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use rovr_protocol::{
    Command, ConfigCommand, DebugCommand, LayoutCommand, Notification, QueryCommand, Request,
    Response, ResponseOutcome, ScratchpadCommand, SpaceCommand, WindowCommand, WorkspaceCommand,
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
    Sa(SaArgs),
    Query(QueryArgs),
    Window(WindowArgs),
    Space(SpaceArgs),
    Layout(LayoutArgs),
    Scratchpad(ScratchpadArgs),
    Workspace(WorkspaceArgs),
    Config(ConfigArgs),
    Debug(DebugArgs),
    Subscribe,
    #[command(about = "Generate shell completion scripts (bash/zsh/fish/powershell/elvish)")]
    Completions {
        shell: Shell,
    },
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
    #[command(
        name = "move-to-workspace",
        about = "Move window to named workspace (logical)"
    )]
    MoveToWorkspace {
        window: u32,
        workspace: String,
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
    Swap {
        a: u32,
        b: u32,
    },
    Warp {
        window: u32,
        target: u32,
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
    Reload {
        path: Option<String>,
    },
    Check {
        path: String,
    },
    #[command(name = "gen-skhd", about = "Generate skhd config from rovr.toml binds")]
    GenSkhd {
        path: Option<String>,
    },
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
    Balance { space: u64 },
    SetRatio { space: u64, ratio: f64 },
}
#[derive(Debug, Args)]
struct ScratchpadArgs {
    #[command(subcommand)]
    command: ScratchpadSubcommand,
}

#[derive(Debug, Subcommand)]
enum ScratchpadSubcommand {
    Toggle { name: String },
}

#[derive(Debug, Args)]
struct WorkspaceArgs {
    #[command(subcommand)]
    command: WorkspaceSubcommand,
}

#[derive(Debug, Subcommand)]
enum WorkspaceSubcommand {
    Focus {
        name: String,
    },
    #[command(name = "move-window", about = "Move window to named workspace")]
    MoveWindow {
        window: u32,
        workspace: String,
    },
}

#[derive(Debug, Args)]
struct SaArgs {
    #[command(subcommand)]
    command: SaSubcommand,
}

#[derive(Debug, Subcommand)]
enum SaSubcommand {
    /// Show Rovr SA socket, payload version, compatibility and per-capability status.
    Status,
    /// Install / update the Rovr scripting-addition payload into Dock (requires SIP party disabled for injection).
    Install,
    /// Uninstall the Rovr scripting-addition payload.
    Uninstall,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let TopCommand::Completions { shell } = cli.command {
        generate_completions(shell);
        return Ok(());
    }
    if let TopCommand::Config(args) = &cli.command {
        if let ConfigSubcommand::GenSkhd { path } = &args.command {
            return run_gen_skhd(path.as_deref());
        }
    }
    if let TopCommand::Sa(args) = &cli.command {
        return run_sa(args);
    }
    let socket = cli.socket.unwrap_or_else(default_socket_path);
    if matches!(cli.command, TopCommand::Subscribe) {
        return run_subscribe(&socket);
    }
    let command = map_command(cli.command);
    let request = Request::new(NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed), command);
    let response = send(&socket, &request)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn generate_completions(shell: Shell) {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut std::io::stdout());
}

fn run_subscribe(socket: &Path) -> Result<()> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connect to rovr daemon at {}", socket.display()))?;
    let request = Request::new(
        NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
        Command::Subscribe,
    );
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    consume_subscribe_stream(&mut reader, &mut handle)
}

/// Reads a subscription stream: consumes the first line as the ACK `Response`
/// (returning an error if it is not `ok`), then prints each subsequent
/// notification line. Unknown/future notification variants are silently skipped
/// so the client stays forward-compatible.
fn consume_subscribe_stream<R: BufRead, W: Write>(reader: &mut R, out: &mut W) -> Result<()> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(anyhow::anyhow!("subscription stream closed before ACK"));
    }
    let ack: Response = serde_json::from_str(line.trim()).context("decode subscription ACK")?;
    if !matches!(ack.outcome, ResponseOutcome::Ok { .. }) {
        let msg = match &ack.outcome {
            ResponseOutcome::Error { error } => error.message.clone(),
            ResponseOutcome::Ok { .. } => String::new(),
        };
        return Err(anyhow::anyhow!("subscription rejected: {msg}"));
    }
    let mut buf = String::new();
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        match serde_json::from_str::<Notification>(buf.trim()) {
            Ok(Notification::Unknown) => continue,
            Ok(_) => {
                out.write_all(buf.trim().as_bytes())?;
                out.write_all(b"\n")?;
            }
            Err(err) => return Err(err.into()),
        }
    }
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
            WindowSubcommand::MoveToWorkspace { window, workspace } => {
                WindowCommand::MoveToWorkspace {
                    window: WindowId(window),
                    workspace,
                }
            }
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
            WindowSubcommand::Swap { a, b } => WindowCommand::Swap {
                a: WindowId(a),
                b: WindowId(b),
            },
            WindowSubcommand::Warp { window, target } => WindowCommand::Warp {
                window: WindowId(window),
                target: WindowId(target),
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
            ConfigSubcommand::GenSkhd { .. } => {
                unreachable!("gen-skhd is handled in main() before map_command")
            }
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
            LayoutSubcommand::Balance { space } => LayoutCommand::Balance {
                space: SpaceId(space),
            },
            LayoutSubcommand::SetRatio { space, ratio } => LayoutCommand::SetRatio {
                space: SpaceId(space),
                ratio,
            },
        }),
        TopCommand::Scratchpad(args) => Command::Scratchpad(match args.command {
            ScratchpadSubcommand::Toggle { name } => ScratchpadCommand::Toggle { name },
        }),
        TopCommand::Workspace(args) => Command::Workspace(match args.command {
            WorkspaceSubcommand::Focus { name } => WorkspaceCommand::Focus { name },
            WorkspaceSubcommand::MoveWindow { window, workspace } => WorkspaceCommand::MoveWindow {
                window: WindowId(window),
                workspace,
            },
        }),
        TopCommand::Sa(_) => unreachable!("sa is handled in main() before map_command"),
        TopCommand::Completions { .. } => {
            unreachable!("completions are handled in main() before map_command is called")
        }
        TopCommand::Subscribe => {
            unreachable!("subscribe is handled in main() before map_command is called")
        }
    }
}

fn run_sa(args: &SaArgs) -> Result<()> {
    match &args.command {
        SaSubcommand::Status => run_sa_status(),
        SaSubcommand::Install => run_sa_install(),
        SaSubcommand::Uninstall => run_sa_uninstall(),
    }
}

fn run_sa_status() -> Result<()> {
    // Probe SA directly (without daemon) and also query daemon doctor for its view.
    #[cfg(target_os = "macos")]
    {
        use rovr_platform::macos::sa::{SaClient, ROVR_SA_VERSION_PREFIX};
        let client = SaClient::new();
        let socket = client.socket_path().clone();
        println!("socket: {}", socket.display());
        println!("expected_prefix: {}", ROVR_SA_VERSION_PREFIX);
        match client.probe() {
            Some(info) => {
                println!("present: true");
                println!("version: {}", info.version);
                println!("compatible: {}", info.is_compatible());
                println!("attribs: 0x{:08x}", info.attribs);
                let add = (info.attribs & 0x04) != 0;
                let rem = (info.attribs & 0x08) != 0;
                let mov_ = (info.attribs & 0x10) != 0;
                println!("capabilities:");
                println!("  create_space: {}", add);
                println!("  destroy_space: {}", rem);
                println!("  reorder_space: {}", mov_);
                println!("  layer/sticky/shadow/opacity/scale: true (when present)");
            }
            None => {
                println!("present: false");
                println!("hint: run `rovr sa install` then restart Dock, or check SIP / payload injection");
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        println!("sa status is only available on macOS");
    }
    // Also print daemon's view if reachable (best-effort).
    let socket = default_socket_path();
    if let Ok(resp) = send(
        &socket,
        &Request::new(
            NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
            Command::Doctor,
        ),
    ) {
        println!("-- daemon doctor --");
        println!(
            "{}",
            serde_json::to_string_pretty(&resp).unwrap_or_default()
        );
    }
    Ok(())
}

fn run_sa_install() -> Result<()> {
    // Phase-1 slice: document and verify. Full payload injection ships next.
    // We intentionally do NOT weaken SIP beyond what the payload needs: see
    // docs/SA_SIP.md for the scoped requirement (see SA.md for payload scope).
    println!("rovr sa install: Rovr-owned payload injection is not yet bundled in this build.");
    println!("This command will, when the payload ships:");
    println!("  1) install the Rovr SA payload to a privileged location,");
    println!("  2) inject it into Dock (requires SIP with `csrutil enable --without debug` / `filesystem` relaxation depending on macOS),");
    println!("  3) create the Rovr socket at /tmp/rovr-sa_<uid>.sock (never /tmp/yabai-sa_*).");
    println!("See docs/SA_SIP.md (to be added) and `rovr sa status` for diagnostics.");
    println!("For now, verify: yabai must NOT be required; SA socket must be Rovr-owned.");
    anyhow::bail!("sa install: payload not yet bundled — see `rovr sa status` and TODO.md Phase 1");
}

fn run_sa_uninstall() -> Result<()> {
    println!("rovr sa uninstall: payload not yet bundled; nothing to remove in this build.");
    println!("When bundled, this will remove the Rovr payload and restore Dock.");
    Ok(())
}

fn run_gen_skhd(path: Option<&str>) -> Result<()> {
    let cfg_path = path.map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".config/rovr/rovr.toml")
    });
    let content = std::fs::read_to_string(&cfg_path)
        .with_context(|| format!("read config {}", cfg_path.display()))?;
    let cfg: rovr_config::Config =
        toml::from_str(&content).with_context(|| format!("parse {}", cfg_path.display()))?;
    cfg.validate()?;
    for bind in &cfg.binds {
        println!("{} : rovr {}", bind.key, bind.command);
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap_complete::Shell;

    /// M4a: the generated completion script advertises the CLI's known
    /// subcommands (incl. the new `completions` command itself).
    #[test]
    fn m4a_completions_include_top_commands() {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        let mut buf = Vec::new();
        generate(Shell::Zsh, &mut cmd, name, &mut buf);
        let script = String::from_utf8(buf).expect("completion script is utf-8");
        for expected in ["query", "layout", "scratchpad", "completions"] {
            assert!(
                script.contains(expected),
                "completion script missing subcommand `{expected}`"
            );
        }
    }
    /// M4b: `rovr subscribe` consumes the subscription ACK (and errors if it is
    /// not ok) before printing only notification lines.
    #[test]
    fn m4b_cli_consumes_subscription_ack_before_printing_notifications() {
        use std::io::Cursor;

        let ack =
            serde_json::to_string(&Response::ok(1, serde_json::json!({ "subscribed": true })))
                .unwrap();
        let hello = serde_json::to_string(&Notification::Hello {
            protocol_version: rovr_protocol::PROTOCOL_VERSION,
        })
        .unwrap();
        let changed = serde_json::to_string(&Notification::StateChanged).unwrap();
        let stream = format!("{ack}\n{hello}\n{changed}\n");

        let mut input = Cursor::new(stream.into_bytes());
        let mut out = Vec::new();
        consume_subscribe_stream(&mut input, &mut out).expect("consume stream");

        let printed = String::from_utf8(out).expect("utf-8 output");
        assert!(
            printed.contains(&hello),
            "hello notification must be printed"
        );
        assert!(printed.contains(&changed), "state_changed must be printed");
        assert!(
            !printed.contains(&ack),
            "subscription ACK must not be printed as a notification"
        );
    }

    #[test]
    fn m4b_cli_subscription_ack_error_aborts() {
        use std::io::Cursor;
        let err = serde_json::to_string(&Response::error(
            1,
            "PROTOCOL_VERSION_MISMATCH",
            "bad version",
        ))
        .unwrap();
        let mut input = Cursor::new(format!("{err}\n").into_bytes());
        let mut out = Vec::new();
        let result = consume_subscribe_stream(&mut input, &mut out);
        assert!(result.is_err(), "non-ok ACK must abort the subscription");
        assert!(out.is_empty(), "nothing should be printed on ACK error");
    }

    /// M4b: a malformed notification frame must terminate the stream with an
    /// error rather than disappearing silently.
    #[test]
    fn m4b_cli_malformed_notification_frame_aborts() {
        use std::io::Cursor;
        let ack =
            serde_json::to_string(&Response::ok(1, serde_json::json!({ "subscribed": true })))
                .unwrap();
        let hello = serde_json::to_string(&Notification::Hello {
            protocol_version: rovr_protocol::PROTOCOL_VERSION,
        })
        .unwrap();
        // Garbage line after a valid Hello: the stream must error out.
        let stream = format!("{ack}\n{hello}\nnot-json\n");
        let mut input = Cursor::new(stream.into_bytes());
        let mut out = Vec::new();
        let result = consume_subscribe_stream(&mut input, &mut out);
        assert!(
            result.is_err(),
            "malformed notification frame must abort the stream"
        );
        let printed = String::from_utf8(out).expect("utf-8 output");
        assert!(
            printed.contains(&hello),
            "valid frames before the corruption must still be printed"
        );
    }
}
