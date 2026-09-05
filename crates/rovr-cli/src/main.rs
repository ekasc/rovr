use std::{
    io::{BufRead, BufReader, Read, Write},
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
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const RESPONSE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

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
    #[command(about = "Run one observation pass now (internal)")]
    Refresh,
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
    #[command(about = "Focus nearest window in a direction (default: focused window)")]
    FocusDirection {
        direction: DirectionArg,
        from: Option<u32>,
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
        about = "Move window to named workspace (logical); defaults to the focused window"
    )]
    MoveToWorkspace {
        workspace: String,
        window: Option<u32>,
    },
    SetLayer {
        layer: i32,
        window: Option<u32>,
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
    #[command(about = "Close a window (defaults to the focused window)")]
    Close {
        window: Option<u32>,
    },
    #[command(about = "Toggle native fullscreen (defaults to the focused window)")]
    ToggleFullscreen {
        window: Option<u32>,
    },
    #[command(about = "Toggle tiling for a window (defaults to the focused window)")]
    ToggleFloat {
        window: Option<u32>,
    },
    #[command(
        name = "swap-dir",
        visible_alias = "swap-direction",
        about = "Swap with nearest neighbor in a direction"
    )]
    SwapDirection {
        direction: DirectionArg,
        #[arg(long)]
        window: Option<u32>,
    },
    #[command(
        name = "warp-dir",
        visible_alias = "warp-direction",
        about = "Insert at neighbor's tree position in a direction"
    )]
    WarpDirection {
        direction: DirectionArg,
        #[arg(long)]
        window: Option<u32>,
    },
    #[command(about = "Resize one edge by delta points (positive = outward)")]
    Resize {
        #[arg(long)]
        window: Option<u32>,
        edge: DirectionArg,
        delta: i32,
    },
}

#[derive(Debug, Args)]
struct SpaceArgs {
    #[command(subcommand)]
    command: SpaceSubcommand,
}

#[derive(Debug, Subcommand)]
enum SpaceSubcommand {
    Focus {
        space: u64,
    },
    #[command(name = "focus-recent", about = "Focus the previously active Space")]
    FocusRecent,
    Create {
        anchor: Option<u64>,
    },
    Destroy {
        space: u64,
    },
    Move {
        space: u64,
        after: u64,
    },
    #[command(
        name = "toggle-insets",
        about = "Collapse/restore all gap+padding for the session"
    )]
    ToggleInsets,
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
    #[command(about = "Print a minimal starter config or all resolved defaults")]
    Dump {
        #[arg(long, help = "Include every resolved default value")]
        full: bool,
    },
    #[command(about = "Reload config and self-heal workspace/Space topology")]
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
    /// All layout commands default to the focused space.
    Rotate {
        #[arg(long)]
        space: Option<u64>,
    },
    Mirror {
        #[arg(long)]
        space: Option<u64>,
    },
    Balance {
        #[arg(long)]
        space: Option<u64>,
    },
    SetRatio {
        ratio: f64,
        #[arg(long)]
        space: Option<u64>,
    },
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
    #[command(
        name = "move-window",
        about = "Move window to named workspace (defaults to the focused window)"
    )]
    MoveWindow {
        workspace: String,
        window: Option<u32>,
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
        match &args.command {
            ConfigSubcommand::Dump { full } => return run_config_dump(*full),
            ConfigSubcommand::GenSkhd { path } => return run_gen_skhd(path.as_deref()),
            ConfigSubcommand::Reload { .. } | ConfigSubcommand::Check { .. } => {}
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
    stream.set_read_timeout(Some(RESPONSE_DEADLINE))?;
    stream.set_write_timeout(Some(RESPONSE_DEADLINE))?;
    let request = Request::new(
        NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
        Command::Subscribe,
    );
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    read_subscription_ack(&mut reader)?;
    reader.get_mut().set_read_timeout(None)?;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    consume_notifications(&mut reader, &mut handle)
}

/// Reads a subscription stream: consumes the first line as the ACK `Response`
/// (returning an error if it is not `ok`), then prints each subsequent
/// notification line. Unknown/future notification variants are silently skipped
/// so the client stays forward-compatible.
#[cfg(test)]
fn consume_subscribe_stream<R: BufRead, W: Write>(reader: &mut R, out: &mut W) -> Result<()> {
    read_subscription_ack(reader)?;
    consume_notifications(reader, out)
}

fn read_subscription_ack(reader: &mut impl BufRead) -> Result<()> {
    let line = read_bounded_line(reader, MAX_RESPONSE_BYTES as usize)?;
    if line.is_empty() {
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
    Ok(())
}

fn consume_notifications(reader: &mut impl BufRead, out: &mut impl Write) -> Result<()> {
    loop {
        let buf = read_bounded_line(reader, MAX_RESPONSE_BYTES as usize)?;
        if buf.is_empty() {
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

fn read_bounded_line(reader: &mut impl BufRead, max: usize) -> Result<String> {
    let mut output = Vec::new();
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        let take = chunk
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(chunk.len(), |index| index + 1);
        if output.len().saturating_add(take) > max {
            anyhow::bail!("daemon response exceeds {max} bytes");
        }
        output.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        if output.last() == Some(&b'\n') {
            break;
        }
    }
    String::from_utf8(output).context("daemon response is not UTF-8")
}

fn map_command(command: TopCommand) -> Command {
    match command {
        TopCommand::Ping => Command::Ping,
        TopCommand::Refresh => Command::Refresh,
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
                from: from.map(WindowId),
                direction: direction.into(),
            },
            WindowSubcommand::Close { window } => WindowCommand::Close {
                window: window.map(WindowId),
            },
            WindowSubcommand::ToggleFullscreen { window } => WindowCommand::ToggleFullscreen {
                window: window.map(WindowId),
            },
            WindowSubcommand::ToggleFloat { window } => WindowCommand::ToggleFloat {
                window: window.map(WindowId),
            },
            WindowSubcommand::SwapDirection { direction, window } => WindowCommand::SwapDirection {
                direction: direction.into(),
                window: window.map(WindowId),
            },
            WindowSubcommand::WarpDirection { direction, window } => WindowCommand::WarpDirection {
                direction: direction.into(),
                window: window.map(WindowId),
            },
            WindowSubcommand::Resize {
                window,
                edge,
                delta,
            } => WindowCommand::Resize {
                window: window.map(WindowId),
                edge: edge.into(),
                delta,
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
                    window: window.map(WindowId),
                    workspace,
                }
            }
            WindowSubcommand::SetLayer { window, layer } => WindowCommand::SetLayer {
                window: window.map(WindowId),
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
            SpaceSubcommand::FocusRecent => SpaceCommand::FocusRecent,
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
            SpaceSubcommand::ToggleInsets => SpaceCommand::ToggleInsets,
        }),
        TopCommand::Config(args) => Command::Config(match args.command {
            ConfigSubcommand::Dump { .. } => {
                unreachable!("config dump is handled in main() before map_command")
            }
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
                space: space.map(SpaceId),
            },
            LayoutSubcommand::Mirror { space } => LayoutCommand::Mirror {
                space: space.map(SpaceId),
            },
            LayoutSubcommand::Balance { space } => LayoutCommand::Balance {
                space: space.map(SpaceId),
            },
            LayoutSubcommand::SetRatio { space, ratio } => LayoutCommand::SetRatio {
                space: space.map(SpaceId),
                ratio,
            },
        }),
        TopCommand::Scratchpad(args) => Command::Scratchpad(match args.command {
            ScratchpadSubcommand::Toggle { name } => ScratchpadCommand::Toggle { name },
        }),
        TopCommand::Workspace(args) => Command::Workspace(match args.command {
            WorkspaceSubcommand::Focus { name } => WorkspaceCommand::Focus { name },
            WorkspaceSubcommand::MoveWindow { window, workspace } => WorkspaceCommand::MoveWindow {
                window: window.map(WindowId),
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

fn run_config_dump(full: bool) -> Result<()> {
    print!("{}", render_config_dump(full)?);
    Ok(())
}

fn render_config_dump(full: bool) -> Result<String> {
    if full {
        let mut output = toml::to_string_pretty(&rovr_config::Config::default())?;
        if !output.ends_with('\n') {
            output.push('\n');
        }
        Ok(output)
    } else {
        Ok(format!(
            "config-version = {}\n",
            rovr_config::CURRENT_CONFIG_VERSION
        ))
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

        let state = sa_state(&client);
        match &state {
            SaState::NotInstalled => {
                println!("state: not_installed");
                println!("present: false");
                println!("hint: run `sudo rovr sa install` (see docs/SA_SIP.md)");
            }
            SaState::InstalledNotInjected => {
                println!("state: installed_not_injected");
                println!("present: false");
                println!("hint: payload files are installed but Dock is not running the payload; run `sudo rovr sa install` to inject");
            }
            SaState::InjectedCompatible(info) => {
                println!("state: injected_compatible");
                println!("present: true");
                println!("version: {}", info.version);
                println!("compatible: true");
                println!("attribs: 0x{:08x}", info.attribs);
                let add = (info.attribs & 0x04) != 0;
                let rem = (info.attribs & 0x08) != 0;
                let mov_ = (info.attribs & 0x10) != 0;
                println!("capabilities:");
                println!("  create_space: {}", add);
                println!("  destroy_space: {}", rem);
                println!("  reorder_space: {}", mov_);
                println!(
                    "  focus/layer/sticky/shadow/opacity/scale: {}/{}/{}/{}/{}/{}",
                    info.attribs & 0x20 != 0,
                    info.attribs & 0x80 != 0,
                    info.attribs & 0x100 != 0,
                    info.attribs & 0x200 != 0,
                    info.attribs & 0x40 != 0,
                    info.attribs & 0x400 != 0
                );
            }
            SaState::IncompatibleProtocol(version) => {
                println!("state: incompatible_protocol");
                println!("present: true");
                println!("version: {}", version);
                println!("compatible: false");
                println!("hint: a non-Rovr or outdated payload is answering on the Rovr socket; reinstall via `sudo rovr sa install`");
            }
            SaState::CapabilityMissing(info, missing) => {
                println!("state: capability_missing");
                println!("present: true");
                println!("version: {}", info.version);
                println!("compatible: true");
                println!("attribs: 0x{:08x}", info.attribs);
                println!("missing: {}", missing.join(", "));
                println!("hint: the payload resolved fewer Dock internals on this macOS build; affected operations will fail until supported");
            }
        }

        print_sa_lifecycle_status(&client);
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

/// Privileged install locations for the Rovr SA artifacts.
#[cfg(target_os = "macos")]
const SA_INSTALL_DIR: &str = "/Library/Application Support/rovr";
#[cfg(target_os = "macos")]
const SA_INSTALLED_DYLIB: &str = "/Library/Application Support/rovr/librovr_sa_payload.dylib";
#[cfg(target_os = "macos")]
const SA_INSTALLED_LOADER: &str = "/Library/Application Support/rovr/rovr-sa-loader";
/// Privileged helper binary installed by `rovr sa install`.
#[cfg(target_os = "macos")]
const SA_INSTALLED_HELPER: &str = "/Library/Application Support/rovr/rovr-sa-helper";
/// LaunchDaemon plist for the privileged helper. SMAppService is NOT usable
/// here because it requires the executable to live inside an .app bundle
/// (`Contents/Library/LaunchDaemons`); Rovr ships as cargo-built CLI binaries.
/// This explicit launchd registration is the minimal fallback — see docs/SA.md.
#[cfg(target_os = "macos")]
const SA_PLIST_PATH: &str = "/Library/LaunchDaemons/com.rovr.sa-helper.plist";
#[cfg(target_os = "macos")]
const SA_PLIST_LABEL: &str = "com.rovr.sa-helper";
/// Install-time payload identity marker (sha256 of the installed dylib plus
/// the handshake version observed after the last successful injection). Lets
/// `rovr sa status` distinguish installed-payload != injected-payload.
#[cfg(target_os = "macos")]
const SA_MARKER_PATH: &str = "/Library/Application Support/rovr/payload.installed.json";

/// The five honest SA states surfaced by `rovr sa status` (blocker 1).
#[cfg(target_os = "macos")]
enum SaState {
    NotInstalled,
    InstalledNotInjected,
    InjectedCompatible(rovr_platform::macos::sa::SaInfo),
    IncompatibleProtocol(String),
    CapabilityMissing(rovr_platform::macos::sa::SaInfo, Vec<String>),
}

#[cfg(target_os = "macos")]
fn sa_installed_files_present() -> bool {
    std::path::Path::new(SA_INSTALLED_DYLIB).exists()
}

#[cfg(target_os = "macos")]
fn sa_state(client: &rovr_platform::macos::sa::SaClient) -> SaState {
    use rovr_platform::macos::sa::{
        OSAX_ATTRIB_ADD_SPACE, OSAX_ATTRIB_MOV_SPACE, OSAX_ATTRIB_REM_SPACE,
    };
    match client.probe() {
        Some(info) => {
            if !info.is_compatible() {
                return SaState::IncompatibleProtocol(info.version);
            }
            let mut missing = Vec::new();
            if info.attribs & OSAX_ATTRIB_ADD_SPACE == 0 {
                missing.push("create_space".to_string());
            }
            if info.attribs & OSAX_ATTRIB_REM_SPACE == 0 {
                missing.push("destroy_space".to_string());
            }
            if info.attribs & OSAX_ATTRIB_MOV_SPACE == 0 {
                missing.push("reorder_space".to_string());
            }
            for (bit, name) in [
                (0x20, "focus_space"),
                (0x40, "set_window_opacity"),
                (0x80, "set_window_layer"),
                (0x100, "set_window_sticky"),
                (0x200, "set_window_shadow"),
                (0x400, "set_window_scale"),
            ] {
                if info.attribs & bit == 0 {
                    missing.push(name.to_string());
                }
            }
            if missing.is_empty() {
                SaState::InjectedCompatible(info)
            } else {
                SaState::CapabilityMissing(info, missing)
            }
        }
        None => {
            if sa_installed_files_present() {
                SaState::InstalledNotInjected
            } else {
                SaState::NotInstalled
            }
        }
    }
}

/// Locate build artifacts: env overrides, then cargo target build dirs
/// relative to this executable, then permanent-install locations.
#[cfg(target_os = "macos")]
fn find_sa_artifacts() -> Result<(PathBuf, PathBuf, PathBuf)> {
    if let (Ok(dylib), Ok(loader), Ok(helper)) = (
        std::env::var("ROVR_SA_PAYLOAD"),
        std::env::var("ROVR_SA_LOADER"),
        std::env::var("ROVR_SA_HELPER"),
    ) {
        return Ok((
            PathBuf::from(dylib),
            PathBuf::from(loader),
            PathBuf::from(helper),
        ));
    }
    let exe = std::env::current_exe().context("locate rovr executable")?;
    let exe_canon = std::fs::canonicalize(&exe).unwrap_or_else(|_| exe.clone());
    // 1) Permanent install: SA artifacts installed alongside rovr
    //    ($PREFIX/bin -> $PREFIX/lib/rovr or $PREFIX/share/rovr).
    //    scripts/install.sh copies them there.
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(bin_dir) = exe_canon.parent() {
        candidates.push(bin_dir.join("librovr_sa_payload.dylib"));
        candidates.push(bin_dir.join("rovr-sa-loader"));
        candidates.push(bin_dir.join("rovr-sa-helper"));
        if let Some(prefix) = bin_dir.parent() {
            candidates.push(prefix.join("lib/rovr/librovr_sa_payload.dylib"));
            candidates.push(prefix.join("lib/rovr/rovr-sa-loader"));
            candidates.push(prefix.join("lib/rovr/rovr-sa-helper"));
            candidates.push(prefix.join("share/rovr/librovr_sa_payload.dylib"));
            candidates.push(prefix.join("share/rovr/rovr-sa-loader"));
            candidates.push(prefix.join("share/rovr/rovr-sa-helper"));
        }
    }
    // Also check XDG-ish permanent locations directly.
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".local/lib/rovr/librovr_sa_payload.dylib"));
        candidates.push(home.join(".local/lib/rovr/rovr-sa-loader"));
        candidates.push(home.join(".local/lib/rovr/rovr-sa-helper"));
        candidates.push(home.join(".local/share/rovr/librovr_sa_payload.dylib"));
    }
    let dylib_perm = candidates
        .iter()
        .find(|p| p.ends_with("librovr_sa_payload.dylib") && p.exists())
        .cloned();
    let loader_perm = candidates
        .iter()
        .find(|p| p.ends_with("rovr-sa-loader") && p.exists())
        .cloned();
    let helper_perm = candidates
        .iter()
        .find(|p| p.ends_with("rovr-sa-helper") && p.exists())
        .cloned();
    if let (Some(d), Some(l), Some(h)) = (dylib_perm, loader_perm, helper_perm) {
        return Ok((d, l, h));
    }
    // 2) Dev install: cargo target dir relative to the (canonical) exe.
    //    exe is <target>/<profile>/rovr; ancestors()[1] is the profile dir
    //    where cargo puts build-script output (<target>/<profile>/build/<crate>-<hash>/out).
    let target_root = exe_canon
        .ancestors()
        .nth(1)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let find = |crate_prefix: &str, artifact: &str| -> Option<PathBuf> {
        let base = target_root.join("build");
        if !base.is_dir() {
            return None;
        }
        let mut hits: Vec<PathBuf> = std::fs::read_dir(&base)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(crate_prefix))
                    .unwrap_or(false)
            })
            .map(|p| p.join("out").join(artifact))
            .filter(|p| p.exists())
            .collect();
        hits.sort();
        hits.pop()
    };
    // 3) Fallback: also check non-canonical exe's build dir (covers copied
    //    permanent binary where canonical == ~/.local/bin/rovr and has no build).
    let find_fallback = |crate_prefix: &str, artifact: &str| -> Option<PathBuf> {
        let alt_root = exe
            .ancestors()
            .nth(1)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        if alt_root == target_root {
            return None;
        }
        let base = alt_root.join("build");
        if !base.is_dir() {
            return None;
        }
        let mut hits: Vec<PathBuf> = std::fs::read_dir(&base)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(crate_prefix))
                    .unwrap_or(false)
            })
            .map(|p| p.join("out").join(artifact))
            .filter(|p| p.exists())
            .collect();
        hits.sort();
        hits.pop()
    };
    let dylib = find("rovr-sa-payload", "librovr_sa_payload.dylib")
        .or_else(|| find_fallback("rovr-sa-payload", "librovr_sa_payload.dylib"))
        .context("payload dylib not built — run `cargo build -p rovr-sa-payload` first")?;
    let loader = find("rovr-sa-loader", "rovr-sa-loader")
        .or_else(|| find_fallback("rovr-sa-loader", "rovr-sa-loader"))
        .context("loader binary not built — run `cargo build -p rovr-sa-loader` first")?;
    let helper = find("rovr-sa-helper", "rovr-sa-helper")
        .or_else(|| find_fallback("rovr-sa-helper", "rovr-sa-helper"))
        .context("helper binary not built — run `cargo build -p rovr-sa-helper` first")?;
    Ok((dylib, loader, helper))
}

/// UID of the console user (the uid Dock runs as). Under `sudo`, $UID is 0,
/// but the payload socket is created with the console user's uid.
#[cfg(target_os = "macos")]
fn console_uid() -> Option<u32> {
    let out = std::process::Command::new("stat")
        .args(["-f", "%u", "/dev/console"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<u32>().ok()
}

#[cfg(target_os = "macos")]
fn check_sip_for_install() -> Result<()> {
    let out = std::process::Command::new("csrutil")
        .arg("status")
        .output()
        .context("run csrutil status")?;
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    let debug_ok = text.contains("debugging restrictions: disabled")
        || text.contains("system integrity protection status: disabled");
    let fs_ok = text.contains("filesystem protections: disabled")
        || text.contains("system integrity protection status: disabled");
    if !(debug_ok && fs_ok) {
        anyhow::bail!(
            "SIP is too strict for injection. Required (in recovery mode):\n               csrutil enable --without debug --without fs\n             On Apple Silicon additionally: sudo nvram bootarg=\"-arm64e_preview_abi\" boot-args=\"-arm64e_preview_abi\"\n             See docs/SA_SIP.md for why each relaxation is needed."
        );
    }
    Ok(())
}

fn run_sa_install() -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        println!("sa install is only available on macOS");
        anyhow::bail!("sa install: unsupported platform");
    }

    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt;

        let (dylib, loader, helper) = find_sa_artifacts()?;
        println!("payload: {}", dylib.display());
        println!("loader:  {}", loader.display());
        println!("helper:  {}", helper.display());

        if unsafe { libc_getuid() } != 0 {
            eprintln!(
                "sa install must run as root — it writes to {SA_INSTALL_DIR}, registers the privileged LaunchDaemon and injects into Dock:"
            );
            eprintln!("  sudo rovr sa install");
            anyhow::bail!("sa install: root required");
        }
        check_sip_for_install()?;

        // Pin every user-provided/discovered source before changing any
        // privileged destination. Copies and hashes below use only these file
        // descriptors, so a path swap cannot replace an artifact mid-install.
        let mut pinned = [
            open_pinned_artifact(&dylib)?,
            open_pinned_artifact(&loader)?,
            open_pinned_artifact(&helper)?,
        ];

        // ---- 1. Install root-owned immutable copies -------------------------
        std::fs::create_dir_all(SA_INSTALL_DIR)
            .with_context(|| format!("create {SA_INSTALL_DIR}"))?;
        // Root-only write on the directory; group/other read-only.
        std::fs::set_permissions(SA_INSTALL_DIR, std::fs::Permissions::from_mode(0o755))?;
        let destinations = [
            (SA_INSTALLED_DYLIB, 0o644),
            (SA_INSTALLED_LOADER, 0o744),
            (SA_INSTALLED_HELPER, 0o744),
        ];
        let mut installed_sha = String::new();
        for (index, (dst, mode)) in destinations.into_iter().enumerate() {
            let sha = install_pinned_artifact(&mut pinned[index], dst, mode)?;
            if index == 0 {
                installed_sha = sha;
            }
        }
        println!("installed: payload + loader + helper in {SA_INSTALL_DIR}");

        // ---- 2. Register the privileged service (LaunchDaemon) --------------
        std::fs::write(SA_PLIST_PATH, launchd_plist_xml())
            .with_context(|| format!("write {SA_PLIST_PATH}"))?;
        std::fs::set_permissions(SA_PLIST_PATH, std::fs::Permissions::from_mode(0o644))?;
        let bootout = std::process::Command::new("launchctl")
            .args(["bootout", &format!("system/{SA_PLIST_LABEL}")])
            .output();
        if let Ok(out) = bootout {
            if out.status.success() {
                println!("removed previous service registration");
            }
        }
        let bootstrap = std::process::Command::new("launchctl")
            .args(["bootstrap", "system", SA_PLIST_PATH])
            .output()
            .context("run launchctl bootstrap")?;
        if !bootstrap.status.success() {
            let stderr = String::from_utf8_lossy(&bootstrap.stderr);
            eprintln!(
                "service registration FAILED: launchctl bootstrap: {}",
                stderr.trim()
            );
            eprintln!(
                "files are installed but automatic reinjection is NOT active until this succeeds."
            );
            anyhow::bail!("sa install: service registration failed");
        }
        println!(
            "service registered: {SA_PLIST_LABEL} (socket-activated, no approval prompt required)"
        );

        // ---- 3. Trigger initial injection directly (we ARE root) ------------
        let out = std::process::Command::new(SA_INSTALLED_LOADER)
            .arg(SA_INSTALLED_DYLIB)
            .output()
            .context("run rovr-sa-loader")?;
        print!("{}", String::from_utf8_lossy(&out.stderr));
        if !out.status.success() {
            anyhow::bail!(
                "sa install: injection failed (exit {:?}) — files + service are installed; the daemon will retry via the helper",
                out.status.code()
            );
        }

        // ---- 4. Verify the SA handshake ------------------------------------
        let uid = console_uid().context("determine console uid")?;
        let client = rovr_platform::macos::sa::SaClient::with_socket_path_for_uid(
            rovr_platform::macos::sa::SaClient::socket_path_for_uid(&uid.to_string()),
            uid,
        );
        let mut handshake_version = None;
        for _ in 0..20 {
            if let Some(info) = client.probe() {
                println!(
                    "handshake ok: version={} attribs=0x{:08x}",
                    info.version, info.attribs
                );
                handshake_version = Some(info.version);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        let Some(version) = handshake_version else {
            anyhow::bail!(
                "sa install: injection ran but the payload socket did not answer — check `rovr sa status` and Console.app for [rovr-sa] logs"
            )
        };

        // ---- 5. Persist identity marker + report capabilities ---------------
        let marker = serde_json::json!({
            "installed_sha256": installed_sha,
            "handshake_version": version,
            "injected_identity_observable": false,
        });
        std::fs::write(
            SA_MARKER_PATH,
            serde_json::to_vec_pretty(&marker).unwrap_or_default(),
        )
        .with_context(|| format!("write {SA_MARKER_PATH}"))?;

        let attribs = client.probe().map(|i| i.attribs).unwrap_or(0);
        println!("capabilities:");
        println!("  create_space:  {}", attribs & 0x04 != 0);
        println!("  destroy_space: {}", attribs & 0x08 != 0);
        println!("  reorder_space: {}", attribs & 0x10 != 0);
        println!("  focus_space:   {}", attribs & 0x20 != 0);
        println!("  opacity:       {}", attribs & 0x40 != 0);
        println!("  layer:         {}", attribs & 0x80 != 0);
        println!("  sticky:        {}", attribs & 0x100 != 0);
        println!("  shadow:        {}", attribs & 0x200 != 0);
        println!("  scale:         {}", attribs & 0x400 != 0);
        println!("sa install complete.");
        Ok(())
    }
}

/// The LaunchDaemon plist for the privileged helper. Socket-activated:
/// launchd owns /var/run/rovr-sa-helper.sock and starts the helper on demand,
/// so no root code runs (and nothing polls) until an injection is requested.
#[cfg(target_os = "macos")]
fn launchd_plist_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{SA_PLIST_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{SA_INSTALLED_HELPER}</string>
    </array>
    <key>Sockets</key>
    <dict>
        <key>Listener</key>
        <dict>
            <key>SockPathName</key>
            <string>{}</string>
            <key>SockPathMode</key>
            <integer>146</integer>
            <key>SockType</key>
            <string>stream</string>
        </dict>
    </dict>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        rovr_platform::macos::reinject::HELPER_SOCKET_PATH
    )
}

#[cfg(target_os = "macos")]
fn open_pinned_artifact(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    const O_NOFOLLOW: i32 = 0x100;
    let before = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect artifact {}", path.display()))?;
    if before.file_type().is_symlink() || !before.file_type().is_file() {
        anyhow::bail!(
            "artifact {} is not a regular non-symlink file",
            path.display()
        );
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("open artifact {}", path.display()))?;
    let after = file.metadata()?;
    if !after.is_file() || before.dev() != after.dev() || before.ino() != after.ino() {
        anyhow::bail!("artifact {} changed while being opened", path.display());
    }
    Ok(file)
}

#[cfg(target_os = "macos")]
fn install_pinned_artifact(file: &mut std::fs::File, dst: &str, mode: u32) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    file.seek(SeekFrom::Start(0))?;
    let tmp = format!("{dst}.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .with_context(|| format!("create pinned destination {tmp}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
    }
    output.sync_all()?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    // The root-owned install directory is not writable by the source owner;
    // replacement cannot be redirected after this rename.
    std::fs::rename(&tmp, dst).with_context(|| format!("install {dst}"))?;
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(target_os = "macos")]
fn sha256_file(path: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).with_context(|| format!("open {path} for hashing"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn run_sa_uninstall() -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        println!("sa uninstall is only available on macOS");
        anyhow::bail!("sa uninstall: unsupported platform");
    }

    #[cfg(target_os = "macos")]
    {
        if unsafe { libc_getuid() } != 0 {
            eprintln!("sa uninstall must run as root:");
            eprintln!("  sudo rovr sa uninstall");
            anyhow::bail!("sa uninstall: root required");
        }

        // 1. Unregister the privileged service FIRST: guarantees no further
        //    reinjection regardless of what happens below.
        let bootout = std::process::Command::new("launchctl")
            .args(["bootout", &format!("system/{SA_PLIST_LABEL}")])
            .output();
        match bootout {
            Ok(out) if out.status.success() => println!("service unregistered ({SA_PLIST_LABEL})"),
            _ => println!("service was not registered — skipping bootout"),
        }
        if std::path::Path::new(SA_PLIST_PATH).exists() {
            std::fs::remove_file(SA_PLIST_PATH)
                .with_context(|| format!("remove {SA_PLIST_PATH}"))?;
            println!("removed {SA_PLIST_PATH}");
        }
        // Remove the helper's launchd-created socket if one is left behind.
        let helper_sock = rovr_platform::macos::reinject::HELPER_SOCKET_PATH;
        let _ = std::fs::remove_file(helper_sock);

        // 2. Remove installed artifacts.
        let mut removed = false;
        for path in [
            SA_INSTALLED_DYLIB,
            SA_INSTALLED_LOADER,
            SA_INSTALLED_HELPER,
            SA_MARKER_PATH,
        ] {
            if std::path::Path::new(path).exists() {
                std::fs::remove_file(path).with_context(|| format!("remove {path}"))?;
                println!("removed {path}");
                removed = true;
            }
        }

        // 3. Remove stale Rovr SA socket for the console user.
        if let Some(uid) = console_uid() {
            let sock = rovr_platform::macos::sa::SaClient::socket_path_for_uid(&uid.to_string());
            if sock.exists() {
                // Only remove real sockets, never arbitrary paths.
                if let Ok(meta) = std::fs::symlink_metadata(&sock) {
                    use std::os::unix::fs::FileTypeExt;
                    if meta.file_type().is_socket() {
                        let _ = std::fs::remove_file(&sock);
                        println!("removed stale socket {}", sock.display());
                    }
                }
            }
        }

        if !removed {
            println!("nothing installed — nothing to remove.");
            return Ok(());
        }
        // 4. Restart Dock: REQUIRED to unload the currently injected payload —
        //    code already mapped into Dock cannot be evicted any other way.
        //    This is deliberate and only happens during explicit uninstall.
        let _ = std::process::Command::new("killall").arg("Dock").status();
        println!(
            "Dock restarted; payload unloaded. `rovr sa status` should now report not_installed."
        );
        Ok(())
    }
}

/// Lifecycle state of the privileged helper service, derived from three
/// observable facts (kept pure so it can be unit-tested without macOS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceState {
    /// No files, no plist.
    NotInstalled,
    /// Files installed (or plist present) but the helper does not answer.
    Installed,
    /// Reserved for SMAppService-style flows; unused by the launchd fallback
    /// (launchd registration never prompts for approval).
    #[allow(dead_code)]
    AwaitingApproval,
    /// Helper answers STATUS over its socket.
    Registered,
}

#[cfg(target_os = "macos")]
fn derive_service_state(
    files_installed: bool,
    plist_present: bool,
    helper_ok: bool,
) -> ServiceState {
    if helper_ok {
        ServiceState::Registered
    } else if files_installed || plist_present {
        ServiceState::Installed
    } else {
        ServiceState::NotInstalled
    }
}

/// Extend `rovr sa status` with the full lifecycle picture: privileged
/// service state, payload identity, and installed-vs-injected mismatch.
#[cfg(target_os = "macos")]
fn print_sa_lifecycle_status(client: &rovr_platform::macos::sa::SaClient) {
    use std::time::Duration;

    println!("-- lifecycle --");

    // Service state.
    let files_installed = std::path::Path::new(SA_INSTALLED_DYLIB).exists()
        && std::path::Path::new(SA_INSTALLED_HELPER).exists();
    let plist_present = std::path::Path::new(SA_PLIST_PATH).exists();
    let helper_ok = rovr_platform::macos::reinject::HelperClient::new()
        .status(Duration::from_secs(1))
        .is_ok();
    let service_str = match derive_service_state(files_installed, plist_present, helper_ok) {
        ServiceState::Registered => "registered",
        ServiceState::Installed => "installed",
        ServiceState::AwaitingApproval => "awaiting_approval",
        ServiceState::NotInstalled => "not_installed",
    };
    println!(
        "service: {service_str} (socket={} label={SA_PLIST_LABEL})",
        rovr_platform::macos::reinject::HELPER_SOCKET_PATH
    );
    if service_str == "installed" {
        println!(
            "hint: files are present but the helper does not answer — check `sudo launchctl print system/{SA_PLIST_LABEL}`"
        );
    }

    // Payload identity: installed file vs install-time marker.
    let marker: Option<serde_json::Value> = std::fs::read_to_string(SA_MARKER_PATH)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let installed_sha = sha256_file(SA_INSTALLED_DYLIB).ok();
    match (&marker, &installed_sha) {
        (Some(m), Some(sha)) => {
            let marked = m["installed_sha256"].as_str().unwrap_or("");
            println!(
                "payload: installed (sha256={}{})",
                &sha[..8.min(sha.len())],
                if marked == sha {
                    ""
                } else {
                    ", MARKER MISMATCH — reinstall via `sudo rovr sa install`"
                }
            );
            let handshake = client.probe().map(|i| i.version);
            println!(
                "injection: {}",
                handshake
                    .as_deref()
                    .map(|v| format!(
                        "handshake observed ({v}); mapped payload SHA is not observable"
                    ))
                    .unwrap_or_else(|| "not_injected".to_string())
            );
            println!(
                "handshake_version_at_install: {}",
                m["handshake_version"].as_str().unwrap_or("?")
            );
        }
        (None, Some(_)) => {
            println!("payload: installed (identity marker missing — run `sudo rovr sa install` to complete registration)");
            println!("injection: unknown");
        }
        _ => {
            println!("payload: not_installed");
            println!("injection: not_injected");
        }
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn getuid() -> u32;
}

#[cfg(target_os = "macos")]
unsafe fn libc_getuid() -> u32 {
    getuid()
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
    stream.set_read_timeout(Some(RESPONSE_DEADLINE))?;
    stream.set_write_timeout(Some(RESPONSE_DEADLINE))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut line = String::new();
    BufReader::new(stream)
        .take(MAX_RESPONSE_BYTES + 1)
        .read_line(&mut line)?;
    if line.len() as u64 > MAX_RESPONSE_BYTES {
        anyhow::bail!("daemon response exceeds {MAX_RESPONSE_BYTES} bytes");
    }
    serde_json::from_str(&line).context("decode daemon response")
}

fn default_socket_path() -> PathBuf {
    rovr_platform::daemon_socket_path()
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
        for expected in [
            "query",
            "layout",
            "scratchpad",
            "focus-recent",
            "completions",
        ] {
            assert!(
                script.contains(expected),
                "completion script missing subcommand `{expected}`"
            );
        }
    }
    #[test]
    fn space_focus_recent_maps_to_protocol_command() {
        let command = map_command(TopCommand::Space(SpaceArgs {
            command: SpaceSubcommand::FocusRecent,
        }));
        assert_eq!(command, Command::Space(SpaceCommand::FocusRecent));
    }

    #[test]
    fn config_dump_is_small_by_default_and_full_on_request() {
        let minimal = render_config_dump(false).expect("render minimal config");
        assert_eq!(minimal, "config-version = 1\n");

        let full = render_config_dump(true).expect("render full config");
        let parsed = rovr_config::Config::parse(&full).expect("full dump round trips");
        assert_eq!(parsed.config_version, rovr_config::CURRENT_CONFIG_VERSION);
        assert_eq!(parsed.general.layout, rovr_types::LayoutKind::Bsp);
        assert!(full.contains("[general]"));

        let cli =
            Cli::try_parse_from(["rovr", "config", "dump", "--full"]).expect("dump --full parses");
        assert!(matches!(
            cli.command,
            TopCommand::Config(ConfigArgs {
                command: ConfigSubcommand::Dump { full: true }
            })
        ));
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

    /// SA lifecycle: service state derivation distinguishes not_installed,
    /// installed and registered (pure logic — no macOS interaction).
    #[cfg(target_os = "macos")]
    #[test]
    fn sa_service_state_distinctions() {
        use ServiceState::*;
        assert_eq!(
            derive_service_state(false, false, false),
            NotInstalled,
            "no files + no plist + no helper = not installed"
        );
        assert_eq!(
            derive_service_state(true, true, false),
            Installed,
            "files present but helper silent = installed, not registered"
        );
        assert_eq!(
            derive_service_state(true, false, false),
            Installed,
            "payload files alone still count as installed"
        );
        assert_eq!(
            derive_service_state(false, true, false),
            Installed,
            "plist without helper answers = installed (registration incomplete)"
        );
        assert_eq!(
            derive_service_state(true, true, true),
            Registered,
            "helper answering STATUS = registered"
        );
    }

    /// SA lifecycle: uninstall removes every privileged artifact it installs —
    /// pinned by listing exactly what install writes vs what uninstall deletes.
    #[cfg(target_os = "macos")]
    #[test]
    fn sa_install_and_uninstall_artifact_sets_are_symmetric() {
        let installed = [
            SA_INSTALLED_DYLIB,
            SA_INSTALLED_LOADER,
            SA_INSTALLED_HELPER,
            SA_MARKER_PATH,
            SA_PLIST_PATH,
        ];
        let uninstalled_paths = [
            SA_INSTALLED_DYLIB,
            SA_INSTALLED_LOADER,
            SA_INSTALLED_HELPER,
            SA_MARKER_PATH,
            SA_PLIST_PATH,
        ];
        for path in installed {
            assert!(
                uninstalled_paths.contains(&path),
                "{path} is installed but never removed by uninstall"
            );
        }
    }
}
