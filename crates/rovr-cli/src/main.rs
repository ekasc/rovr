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
                println!("  layer/sticky/shadow/opacity/scale: true");
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
/// relative to this executable.
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
    // exe is <target>/<profile>/rovr; ancestors()[1] is the profile dir where
    // cargo puts build-script output (<target>/<profile>/build/<crate>-<hash>/out).
    let target_root = exe
        .ancestors()
        .nth(1)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let find = |crate_prefix: &str, artifact: &str| -> Option<PathBuf> {
        let base = target_root.join("build");
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
        .context("payload dylib not built — run `cargo build -p rovr-sa-payload` first")?;
    let loader = find("rovr-sa-loader", "rovr-sa-loader")
        .context("loader binary not built — run `cargo build -p rovr-sa-loader` first")?;
    let helper = find("rovr-sa-helper", "rovr-sa-helper")
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

        // ---- 1. Install root-owned immutable copies -------------------------
        std::fs::create_dir_all(SA_INSTALL_DIR)
            .with_context(|| format!("create {SA_INSTALL_DIR}"))?;
        // Root-only write on the directory; group/other read-only.
        std::fs::set_permissions(SA_INSTALL_DIR, std::fs::Permissions::from_mode(0o755))?;
        for (src, dst, mode) in [
            (&dylib, SA_INSTALLED_DYLIB, 0o644),
            (&loader, SA_INSTALLED_LOADER, 0o744),
            (&helper, SA_INSTALLED_HELPER, 0o744),
        ] {
            // Remove first so a stale symlink at dst can never be followed.
            let _ = std::fs::remove_file(dst);
            std::fs::copy(src, dst).with_context(|| format!("copy payload to {dst}"))?;
            std::fs::set_permissions(dst, std::fs::Permissions::from_mode(mode))?;
        }
        println!("installed: payload + loader + helper in {SA_INSTALL_DIR}");

        // Record the installed payload identity BEFORE injection; the injected
        // identity is appended after a verified handshake.
        let installed_sha = sha256_file(SA_INSTALLED_DYLIB)?;

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
        let client = rovr_platform::macos::sa::SaClient::with_socket_path(
            rovr_platform::macos::sa::SaClient::socket_path_for_uid(&uid.to_string()),
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
            "injected_sha256": installed_sha,
            "handshake_version": version,
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
        println!("  layer/sticky/shadow/opacity/scale: true");
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
            <integer>438</integer>
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
            // Installed-vs-injected identity: replacing the dylib on disk does
            // NOT update code already mapped into Dock.
            let injected = m["injected_sha256"].as_str().unwrap_or("");
            let handshake = client.probe().map(|i| i.version);
            if !injected.is_empty() && injected != sha {
                println!("injection: STALE — installed payload differs from the payload last injected into Dock");
                println!("hint: run `sudo rovr sa install` to reinject; until then Dock keeps running the OLD payload build");
            } else {
                println!(
                    "injection: {}",
                    handshake
                        .as_deref()
                        .map(|v| format!("current (handshake {v})"))
                        .unwrap_or_else(|| "not_injected".to_string())
                );
            }
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
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).context("decode daemon response")
}

fn default_socket_path() -> PathBuf {
    // Must match the daemon (real getuid, not $UID — see rovr_platform).
    PathBuf::from(format!("/tmp/rovr-{}.sock", rovr_platform::unix_uid()))
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
