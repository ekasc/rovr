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
use serde_json::Value;

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
    Doctor(DoctorArgs),
    Sa(SaArgs),
    Service(ServiceArgs),
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
struct DoctorArgs {
    /// Print the raw doctor payload as JSON instead of the readable report.
    /// Machine consumers (menu bar, GUI, scripts) should pass this so their
    /// parsing never depends on the report's layout.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
struct QueryArgs {
    #[command(subcommand)]
    command: Option<QuerySubcommand>,
    /// Print the canonical current snapshot (`space`/`display`/`window`) as
    /// bare JSON. Same as `rovr query current`; exists so SketchyBar-style
    /// consumers can init with a flag: `rovr query --current`.
    #[arg(long)]
    current: bool,
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
    #[command(
        name = "move-to-display",
        about = "Send window to the adjacent display (left/right/up/down) by physical arrangement; defaults to the focused window"
    )]
    MoveToDisplay {
        direction: DirectionArg,
        window: Option<u32>,
    },
    #[command(
        name = "move-to-new-space",
        about = "Create a fresh Space on the adjacent display (left/right/up/down) and send the window there with focus; defaults to the focused window"
    )]
    MoveToNewSpace {
        direction: DirectionArg,
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
        // Negative deltas shrink the edge; without allow_hyphen_values clap
        // reads `-20` as an unknown flag and the bind never reaches the daemon.
        #[arg(allow_hyphen_values = true)]
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
    #[command(
        name = "next",
        about = "Step to the next Space (wrapping); defaults to the focused display"
    )]
    Next {
        display: Option<String>,
    },
    #[command(
        name = "prev",
        about = "Step to the previous Space; defaults to the focused display"
    )]
    Prev {
        display: Option<String>,
    },
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
    #[value(alias = "right")]
    East,
    #[value(alias = "left")]
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

#[derive(Debug, Args)]
struct ServiceArgs {
    #[command(subcommand)]
    command: ServiceSubcommand,
}

/// Daemon process lifecycle. The daemon is a launchd LaunchAgent; these
/// commands drive `launchctl` so a crashed or stopped daemon can be brought
/// back without re-running the installer.
#[derive(Debug, Subcommand)]
enum ServiceSubcommand {
    /// Show whether launchd has the daemon loaded and whether its socket answers.
    Status,
    /// Load and start the daemon LaunchAgent.
    Start,
    /// Stop and unload the daemon LaunchAgent.
    Stop,
    /// Restart the running daemon in place.
    Restart,
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
            ConfigSubcommand::Reload { .. } | ConfigSubcommand::Check { .. } => {}
        }
    }
    if let TopCommand::Sa(args) = &cli.command {
        return run_sa(args);
    }
    if let TopCommand::Service(args) = &cli.command {
        return run_service(args);
    }
    if let TopCommand::Doctor(args) = &cli.command {
        let socket = cli.socket.clone().unwrap_or_else(default_socket_path);
        return run_doctor(&socket, args.json);
    }
    let socket = cli.socket.unwrap_or_else(default_socket_path);
    if matches!(cli.command, TopCommand::Subscribe) {
        return run_subscribe(&socket);
    }
    let current_only = is_current_query(&cli.command);
    let command = map_command(cli.command);
    let request = Request::new(NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed), command);
    let response = send(&socket, &request)?;
    if current_only {
        // Bare snapshot for status-bar consumers: valid JSON, stable fields,
        // no envelope, no logs on stdout. Failures go to stderr + non-zero.
        return print_current_response(&response);
    }
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

/// True for `rovr query current` and `rovr query --current` — both map to the
/// same `QueryCommand::Current`; only the output framing differs (bare JSON).
fn is_current_query(command: &TopCommand) -> bool {
    matches!(
        command,
        TopCommand::Query(args) if args.current
            || matches!(args.command, Some(QuerySubcommand::Current))
    )
}

/// Render a `query --current` response as bare snapshot JSON for stdout.
/// `Ok` → pretty snapshot; `Err` → message for stderr (caller exits non-zero).
fn render_current_response(response: &Response) -> Result<String, String> {
    match &response.outcome {
        ResponseOutcome::Ok { result } => {
            serde_json::to_string_pretty(result).map_err(|e| format!("encode snapshot: {e}"))
        }
        ResponseOutcome::Error { error } => Err(format!(
            "query --current failed ({}): {}",
            error.code, error.message
        )),
    }
}

fn print_current_response(response: &Response) -> Result<()> {
    match render_current_response(response) {
        Ok(snapshot) => {
            println!("{snapshot}");
            Ok(())
        }
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}

/// `rovr doctor`: one human-readable diagnostics report. `--json` keeps the raw
/// payload for machine consumers, which must never parse the report's layout.
fn run_doctor(socket: &Path, json: bool) -> Result<()> {
    let request = Request::new(
        NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed),
        Command::Doctor,
    );
    let response = send(socket, &request)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    match render_doctor(&response) {
        Ok(report) => {
            print!("{report}");
            Ok(())
        }
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}

/// Print an already-fetched doctor response as the readable report (best
/// effort: an error outcome goes to stderr, never into the report).
fn print_doctor(response: &Response) {
    match render_doctor(response) {
        Ok(report) => print!("{report}"),
        Err(message) => eprintln!("{message}"),
    }
}

/// One titled group of aligned `key  value` rows. The report reads as a set of
/// small tables without pulling in a table/color dependency.
struct Section {
    title: &'static str,
    rows: Vec<(String, String)>,
}

impl Section {
    fn new(title: &'static str) -> Self {
        Self {
            title,
            rows: Vec::new(),
        }
    }

    fn row(mut self, key: impl Into<String>, value: impl std::fmt::Display) -> Self {
        self.rows.push((key.into(), value.to_string()));
        self
    }

    fn render(&self) -> String {
        let width = self
            .rows
            .iter()
            .map(|(key, _)| key.len())
            .max()
            .unwrap_or_default();
        let mut out = format!("{}\n", self.title);
        for (key, value) in &self.rows {
            out.push_str(&format!("  {key:<width$}  {value}\n"));
        }
        out
    }
}

fn doctor_str(field: &Value, key: &str) -> Option<String> {
    field.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn doctor_u64(field: &Value, key: &str) -> Option<u64> {
    field.get(key).and_then(Value::as_u64)
}

fn doctor_bool(field: &Value, key: &str) -> Option<bool> {
    field.get(key).and_then(Value::as_bool)
}

/// Render the daemon's doctor response. Ok = the report text; Err = a stderr
/// message for an error outcome (the daemon could not produce diagnostics).
fn render_doctor(response: &Response) -> Result<String, String> {
    match &response.outcome {
        ResponseOutcome::Ok { result } => Ok(render_doctor_report(result)),
        ResponseOutcome::Error { error } => {
            Err(format!("doctor failed ({}): {}", error.code, error.message))
        }
    }
}

/// Render the doctor payload as a readable report.
///
/// Pure and defensive: every field is read as optional because older daemons
/// omit sections, so a missing field renders as `unknown` instead of failing.
/// Problems discovered while rendering are also echoed in a trailing summary,
/// which is the part of the report a human actually reads.
fn render_doctor_report(result: &Value) -> String {
    if !result.is_object() {
        return "rovr doctor\n\ndoctor payload is not an object; use `rovr doctor --json`\n"
            .to_string();
    }

    let mut issues: Vec<String> = Vec::new();
    let accessibility = result.get("accessibility");
    let health = result.get("health");
    let sa = result.get("sa");
    let sa_reinject = result.get("sa_reinject").filter(|value| !value.is_null());
    let capabilities = result.get("capabilities");
    let reconcile_interval_ms = doctor_u64(result, "reconcile_interval_ms").unwrap_or(1000);
    if [health, accessibility, sa, capabilities]
        .iter()
        .all(|section| section.is_none())
    {
        // Nothing recognisable came back: say so instead of claiming health.
        issues.push(
            "doctor payload has no diagnostics sections; the CLI and daemon may be different versions"
                .to_owned(),
        );
    }

    // A state loop that is alive but not progressing shows up as a stale
    // heartbeat. Three ticks of slack keeps a slow machine from crying wolf.
    let stale_heartbeat_ms = (reconcile_interval_ms * 3).max(3000);

    let daemon = Section::new("daemon")
        .row(
            "protocol",
            doctor_u64(result, "protocol").unwrap_or_default(),
        )
        .row(
            "generation",
            doctor_u64(result, "generation").unwrap_or_default(),
        )
        .row("windows", doctor_u64(result, "windows").unwrap_or_default())
        .row("spaces", doctor_u64(result, "spaces").unwrap_or_default())
        .row(
            "displays",
            doctor_u64(result, "displays").unwrap_or_default(),
        )
        .row(
            "refresh",
            if doctor_bool(result, "refresh_required").unwrap_or(false) {
                "required"
            } else {
                "not required"
            },
        );

    let mut config = Section::new("config").row(
        "path",
        doctor_str(result, "config").unwrap_or_else(|| "unknown".to_owned()),
    );
    config = config.row(
        "layout",
        doctor_str(result, "layout").unwrap_or_else(|| "unknown".to_owned()),
    );
    if let Some(gap) = doctor_u64(result, "gap") {
        config = config.row("gap", format!("{gap} px"));
    }
    if doctor_u64(result, "reconcile_interval_ms").is_some() {
        config = config.row("reconcile", format!("every {reconcile_interval_ms} ms"));
    }

    let mut health_section = Section::new("health");
    match health.and_then(|h| doctor_u64(h, "state_loop_heartbeat_age_ms")) {
        Some(age) => {
            health_section
                .rows
                .push(("state loop".to_owned(), format!("heartbeat {age} ms ago")));
            if age > stale_heartbeat_ms {
                issues.push(format!(
                    "state loop heartbeat is {age} ms old (expected at most {stale_heartbeat_ms} ms): the daemon may be wedged"
                ));
            }
        }
        None => health_section
            .rows
            .push(("state loop".to_owned(), "unknown".to_owned())),
    }
    match health.and_then(|h| h.get("last_observation_age_ms")) {
        Some(age) if !age.is_null() => health_section.rows.push((
            "observation".to_owned(),
            format!("last pass {} ms ago", age.as_u64().unwrap_or_default()),
        )),
        Some(_) => {
            health_section
                .rows
                .push(("observation".to_owned(), "no pass yet".to_owned()));
            issues.push("no observation pass has completed yet".to_owned());
        }
        None => health_section
            .rows
            .push(("observation".to_owned(), "unknown".to_owned())),
    }
    match health.and_then(|h| doctor_u64(h, "reconcile_failure_streak")) {
        Some(streak) => {
            health_section.rows.push((
                "reconcile".to_owned(),
                format!("{streak} consecutive failures"),
            ));
            if streak > 0 {
                issues.push(format!("reconcile has failed {streak} times in a row"));
            }
        }
        None => health_section
            .rows
            .push(("reconcile".to_owned(), "unknown".to_owned())),
    }
    match result.get("snapshot_wedged_ms").and_then(Value::as_u64) {
        Some(ms) => {
            health_section
                .rows
                .push(("worker".to_owned(), format!("wedged {ms} ms")));
            issues.push(format!("observation worker has been wedged for {ms} ms"));
        }
        None => health_section
            .rows
            .push(("worker".to_owned(), "idle".to_owned())),
    }

    let mut platform = Section::new("platform");
    match accessibility.and_then(|a| doctor_bool(a, "control_available")) {
        Some(true) => platform
            .rows
            .push(("accessibility".to_owned(), "control available".to_owned())),
        Some(false) => {
            platform
                .rows
                .push(("accessibility".to_owned(), "unavailable".to_owned()));
            let message = accessibility
                .and_then(|a| doctor_str(a, "degraded_message"))
                .unwrap_or_else(|| "accessibility control is unavailable".to_owned());
            issues.push(message);
        }
        None => platform
            .rows
            .push(("accessibility".to_owned(), "unknown".to_owned())),
    }
    match sa {
        // Non-macOS daemons report the scripting addition as unavailable.
        Some(sa) if doctor_bool(sa, "available") == Some(false) => platform.rows.push((
            "scripting addition".to_owned(),
            "unavailable on this platform".to_owned(),
        )),
        Some(sa) => {
            let present = doctor_bool(sa, "present").unwrap_or(false);
            let version = doctor_str(sa, "version");
            if !present {
                platform.rows.push((
                    "scripting addition".to_owned(),
                    "not injected in Dock".to_owned(),
                ));
                issues.push(
                    "scripting addition is not injected into Dock: run `sudo rovr sa install` (see docs/SA_SIP.md)"
                        .to_owned(),
                );
            } else {
                let mut parts = vec!["injected".to_owned()];
                if let Some(version) = version.clone() {
                    parts.push(version);
                }
                let prefix = doctor_str(sa, "expected_prefix");
                match doctor_bool(sa, "compatible") {
                    Some(true) => parts.push("compatible".to_owned()),
                    Some(false) => {
                        parts.push("incompatible".to_owned());
                        issues.push(format!(
                            "scripting addition is incompatible (answering version {}, expected {}): reinstall with `sudo rovr sa install`",
                            version.unwrap_or_else(|| "unknown".to_owned()),
                            prefix.unwrap_or_else(|| "a rovr-sa- prefix".to_owned())
                        ));
                    }
                    None => {}
                }
                if let Some(attribs) = doctor_u64(sa, "attribs") {
                    parts.push(format!("attribs 0x{attribs:08x}"));
                }
                platform
                    .rows
                    .push(("scripting addition".to_owned(), parts.join(" · ")));
            }
        }
        None => platform
            .rows
            .push(("scripting addition".to_owned(), "unknown".to_owned())),
    }
    match sa_reinject {
        Some(reinject) => {
            let phase = doctor_str(reinject, "phase").unwrap_or_else(|| "unknown".to_owned());
            let mut parts = vec![phase.clone()];
            if let Some(pid) = doctor_u64(reinject, "dock_pid") {
                parts.push(format!("dock pid {pid}"));
            }
            if let Some(generation) = doctor_u64(reinject, "generation") {
                parts.push(format!("generation {generation}"));
            }
            if doctor_bool(reinject, "pending").unwrap_or(false) {
                parts.push("in flight".to_owned());
            }
            if let Some(secs) = doctor_u64(reinject, "retry_in_secs") {
                parts.push(format!("retry in {secs} s"));
            }
            let last_error = doctor_str(reinject, "last_error");
            platform
                .rows
                .push(("reinjection".to_owned(), parts.join(" · ")));
            if phase == "failed" {
                issues.push(format!(
                    "automatic SA reinjection failed{}: run `sudo rovr sa install`",
                    last_error
                        .map(|error| format!(" ({error})"))
                        .unwrap_or_default()
                ));
            }
        }
        None => platform
            .rows
            .push(("reinjection".to_owned(), "not tracked".to_owned())),
    }

    let mut capabilities_section = Section::new("capabilities");
    if let Some(capabilities) = capabilities.and_then(Value::as_object) {
        let mut missing: Vec<&str> = capabilities
            .iter()
            .filter(|(_, value)| !value.as_bool().unwrap_or(false))
            .map(|(name, _)| name.as_str())
            .collect();
        missing.sort_unstable();
        let available = capabilities.len() - missing.len();
        capabilities_section.rows.push((
            "available".to_owned(),
            format!("{available} of {}", capabilities.len()),
        ));
        if !missing.is_empty() {
            capabilities_section
                .rows
                .push(("missing".to_owned(), missing.join(", ")));
            if missing.contains(&"observe_windows") {
                issues.push("window observation is unavailable: nothing can be tiled".to_owned());
            } else {
                issues.push(format!("capabilities unavailable: {}", missing.join(", ")));
            }
        }
    } else {
        capabilities_section
            .rows
            .push(("available".to_owned(), "unknown".to_owned()));
    }

    let mut report = format!(
        "rovr doctor\n\n{}\n{}\n{}\n{}\n{}\n",
        daemon.render(),
        config.render(),
        health_section.render(),
        platform.render(),
        capabilities_section.render(),
    );

    if issues.is_empty() {
        report.push_str("\nall checks passed\n");
    } else {
        report.push_str(&format!(
            "\n{} issue{}\n",
            issues.len(),
            if issues.len() == 1 { "" } else { "s" }
        ));
        for issue in &issues {
            report.push_str(&format!("  - {issue}\n"));
        }
    }
    report
}

fn generate_completions(shell: Shell) {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut std::io::stdout());
}

fn run_subscribe(socket: &Path) -> Result<()> {
    let mut stream = connect_daemon(socket)?;
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
        TopCommand::Doctor(_) => Command::Doctor,
        TopCommand::Query(args) => Command::Query(if args.current {
            QueryCommand::Current
        } else {
            match args.command {
                Some(QuerySubcommand::Windows) => QueryCommand::Windows,
                Some(QuerySubcommand::Spaces) => QueryCommand::Spaces,
                Some(QuerySubcommand::Displays) => QueryCommand::Displays,
                Some(QuerySubcommand::State) => QueryCommand::State,
                Some(QuerySubcommand::Focused) => QueryCommand::Focused,
                Some(QuerySubcommand::Current) => QueryCommand::Current,
                // Clap's arg_required_else_help prevents this; flag-first
                // mapping above keeps `--current` working without a subcommand.
                None => QueryCommand::Current,
            }
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
            WindowSubcommand::MoveToDisplay { window, direction } => WindowCommand::MoveToDisplay {
                window: window.map(WindowId),
                direction: direction.into(),
            },
            WindowSubcommand::MoveToNewSpace { window, direction } => {
                WindowCommand::MoveToNewSpace {
                    window: window.map(WindowId),
                    direction: direction.into(),
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
            SpaceSubcommand::Next { display } => SpaceCommand::Next { display },
            SpaceSubcommand::Prev { display } => SpaceCommand::Prev { display },
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
        TopCommand::Service(_) => {
            unreachable!("service is handled in main() before map_command")
        }
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
        use rovr_platform::macos::sa::ROVR_SA_VERSION_PREFIX;
        let client = sa_console_client()?;
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
        print_doctor(&resp);
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

/// Which artifact set `sa install` should use.
///
/// A build produced by THIS source tree always wins over a previously
/// installed set. The permanent-install locations hold whatever an earlier
/// install put there, so preferring them makes `sa install` silently reinstall
/// the stale payload already on disk and report success. Only fall back to the
/// installed set for a globally installed CLI that has no local build tree.
#[cfg(target_os = "macos")]
fn choose_sa_artifacts(
    built: (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>),
    installed: (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>),
) -> Option<(PathBuf, PathBuf, PathBuf)> {
    match built {
        (Some(dylib), Some(loader), Some(helper)) => Some((dylib, loader, helper)),
        _ => match installed {
            (Some(dylib), Some(loader), Some(helper)) => Some((dylib, loader, helper)),
            _ => None,
        },
    }
}

/// Members missing from a build tree, but only when the tree is genuinely
/// partial. A tree with nothing in it means "no build tree here" and is a
/// legitimate reason to fall back to the installed set; a tree with one or two
/// pieces is a broken `cargo build` that must be completed by building, not
/// papered over with a stale loader or helper from the installed set.
#[cfg(target_os = "macos")]
fn incomplete_build_members(
    built: &(Option<PathBuf>, Option<PathBuf>, Option<PathBuf>),
) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if built.0.is_none() {
        missing.push("rovr-sa-payload");
    }
    if built.1.is_none() {
        missing.push("rovr-sa-loader");
    }
    if built.2.is_none() {
        missing.push("rovr-sa-helper");
    }
    if missing.len() == 3 {
        // Nothing built here: a fallback candidate, not an error.
        return Vec::new();
    }
    missing
}

/// Locate build artifacts: env overrides, then a build tree next to this
/// executable, then permanent-install locations.
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

    // 1) Build tree next to this executable: the artifacts this source tree
    //    just produced. This is the set that ships with this rovr, so it comes
    //    first. `cargo build -p rovr-sa-*` puts the clang outputs under
    //    <target>/<profile>/build/<crate>-<hash>/out.
    let target_root = exe_canon
        .ancestors()
        .nth(1)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    // A copied permanent binary has no build tree of its own, so also look next
    // to the non-canonical path (covers symlinked installs).
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
    let built = (
        find("rovr-sa-payload", "librovr_sa_payload.dylib")
            .or_else(|| find_fallback("rovr-sa-payload", "librovr_sa_payload.dylib")),
        find("rovr-sa-loader", "rovr-sa-loader")
            .or_else(|| find_fallback("rovr-sa-loader", "rovr-sa-loader")),
        find("rovr-sa-helper", "rovr-sa-helper")
            .or_else(|| find_fallback("rovr-sa-helper", "rovr-sa-helper")),
    );

    // 2) Permanent install locations, for a globally installed CLI with no
    //    local build tree. These hold whatever a previous install wrote, so
    //    they only answer when there is nothing newer to install.
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
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".local/lib/rovr/librovr_sa_payload.dylib"));
        candidates.push(home.join(".local/lib/rovr/rovr-sa-loader"));
        candidates.push(home.join(".local/lib/rovr/rovr-sa-helper"));
        candidates.push(home.join(".local/share/rovr/librovr_sa_payload.dylib"));
    }
    let installed = (
        candidates
            .iter()
            .find(|p| p.ends_with("librovr_sa_payload.dylib") && p.exists())
            .cloned(),
        candidates
            .iter()
            .find(|p| p.ends_with("rovr-sa-loader") && p.exists())
            .cloned(),
        candidates
            .iter()
            .find(|p| p.ends_with("rovr-sa-helper") && p.exists())
            .cloned(),
    );

    let incomplete = incomplete_build_members(&built);
    if !incomplete.is_empty() {
        anyhow::bail!(
            "SA build tree next to {} is incomplete: missing {}. \
             Run `cargo build -p rovr-sa-payload -p rovr-sa-loader -p rovr-sa-helper` \
             so a fresh payload is not paired with a stale loader or helper",
            exe.display(),
            incomplete.join(", ")
        );
    }

    choose_sa_artifacts(built, installed).with_context(|| {
        format!(
            "SA install artifacts not found: no build tree next to {} and no installed set \
             in the usual locations. Run `cargo build -p rovr-sa-payload -p rovr-sa-loader \
             -p rovr-sa-helper` first, or set ROVR_SA_PAYLOAD/ROVR_SA_LOADER/ROVR_SA_HELPER",
            exe.display()
        )
    })
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

/// An SA client for the GUI session's user, not for whoever is asking.
///
/// The payload binds `<console user home>/Library/Caches/rovr/sa.sock` with the
/// identity of the Dock process that loaded it, which is the console session
/// user. A client keyed on `$HOME` therefore probes root's cache directory
/// under `sudo` and reports a live payload as `present: false`. Since
/// `sa install` must run as root, every CLI path that talks to the payload has
/// to resolve the console user's home.
#[cfg(target_os = "macos")]
fn sa_console_client() -> Result<rovr_platform::macos::sa::SaClient> {
    let uid = console_uid().context("determine console uid from /dev/console")?;
    let path = rovr_platform::macos::sa::SaClient::socket_path_for_uid(uid)
        .context("resolve console user home directory")?;
    Ok(rovr_platform::macos::sa::SaClient::with_socket_path_for_uid(path, uid))
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

        // The failure mode that looks like success: the source payload is
        // byte-identical to the one already installed, so nothing changes while
        // the command reports "installed". Say so before anything is written.
        let source = dylib.to_str().and_then(|p| sha256_file(p).ok());
        let already = sha256_file(SA_INSTALLED_DYLIB).ok();
        if let (Some(source), Some(already)) = (source, already) {
            if source == already {
                eprintln!(
                    "warning: {} is byte-identical to the installed payload;",
                    dylib.display()
                );
                eprintln!("         installing it will not change any capability. Rebuild first:");
                eprintln!(
                    "         cargo build --release -p rovr-sa-payload -p rovr-sa-loader -p rovr-sa-helper"
                );
            }
        }

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
        // Keyed on the console uid, not ours: `sudo` would otherwise probe
        // root's cache directory and report a live payload as missing.
        let client = sa_console_client()?;
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
                "sa install: injection ran but the payload socket did not answer at {}. \
                 The loader injects a content-addressed copy, so an already-mapped \
                 payload is not the cause; check Console.app for [rovr-sa] logs and \
                 `rovr sa status`. The daemon keeps retrying through the helper.",
                client.socket_path().display()
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
        // Content-addressed payload copies created by the loader at injection
        // time (one per build). Only touch files matching that exact pattern in
        // the install directory.
        if let Ok(entries) = std::fs::read_dir(SA_INSTALL_DIR) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("librovr_sa_payload.") && name.ends_with(".dylib") {
                    let path = entry.path();
                    std::fs::remove_file(&path)
                        .with_context(|| format!("remove {}", path.display()))?;
                    println!("removed {}", path.display());
                    removed = true;
                }
            }
        }

        // 3. Remove stale Rovr SA socket for the console user.
        if let Some(uid) = console_uid() {
            if let Some(sock) = rovr_platform::macos::sa::SaClient::socket_path_for_uid(uid) {
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

/// Does the privileged helper answer at all?
///
/// A refusal is a response. The helper's caller check runs before it reads any
/// request byte and replies `ROVR_SA_ST_UNAUTHORIZED`, which the client maps to
/// `Rejected`. Counting that as "not answering" is how `sudo rovr sa status`
/// came to report a healthy service as `installed` with `hint: the helper does
/// not answer` — a root caller can never match the console session uid, so it is
/// always refused. Only a transport failure means the service is really gone.
fn helper_is_answering(
    result: &Result<
        rovr_platform::macos::reinject::HelperResponse,
        rovr_platform::macos::reinject::HelperError,
    >,
) -> bool {
    use rovr_platform::macos::reinject::HelperError;
    match result {
        Ok(_) | Err(HelperError::Rejected { .. }) => true,
        Err(HelperError::Unavailable(_) | HelperError::Protocol(_)) => false,
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
    let helper_ok = helper_is_answering(
        &rovr_platform::macos::reinject::HelperClient::new().status(Duration::from_secs(1)),
    );
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

fn send(path: &Path, request: &Request) -> Result<Response> {
    let mut stream = connect_daemon(path)?;
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

/// LaunchAgent label for the daemon (kept in sync with scripts/install.sh).
const DAEMON_LABEL: &str = "com.rovr.daemon";

/// `~/Library/LaunchAgents/com.rovr.daemon.plist`, or an error if HOME is unset.
#[cfg(target_os = "macos")]
fn daemon_plist_path() -> Result<PathBuf> {
    let home =
        std::env::var("HOME").context("HOME is not set; cannot locate the daemon LaunchAgent")?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents/com.rovr.daemon.plist"))
}

/// launchctl arguments (after the program name) for a lifecycle operation.
/// `loaded` is whether launchd already knows the label. Returns `None` when
/// there is nothing to do (stopping an unloaded service, or `status`, which
/// only inspects). Pure so the mapping is unit-tested without running launchd.
fn lifecycle_launchctl_args(
    sub: &ServiceSubcommand,
    uid: u32,
    label: &str,
    plist: &Path,
    loaded: bool,
) -> Option<Vec<String>> {
    let domain = format!("gui/{uid}");
    let target = format!("gui/{uid}/{label}");
    match sub {
        ServiceSubcommand::Status => None,
        ServiceSubcommand::Start if loaded => Some(vec!["kickstart".into(), target]),
        ServiceSubcommand::Start => Some(vec![
            "bootstrap".into(),
            domain,
            plist.display().to_string(),
        ]),
        ServiceSubcommand::Restart if loaded => Some(vec!["kickstart".into(), "-k".into(), target]),
        ServiceSubcommand::Restart => Some(vec![
            "bootstrap".into(),
            domain,
            plist.display().to_string(),
        ]),
        ServiceSubcommand::Stop if loaded => Some(vec!["bootout".into(), target]),
        ServiceSubcommand::Stop => None,
    }
}

/// Whether a connect error means "no daemon is listening": a stale socket file
/// (ConnectionRefused) or no socket file at all (NotFound).
fn daemon_missing(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
    )
}

/// Connect to the daemon socket, turning the "daemon is not running" errno
/// cases into an actionable message instead of a bare connection error.
fn connect_daemon(path: &Path) -> Result<UnixStream> {
    UnixStream::connect(path).map_err(|err| {
        if daemon_missing(&err) {
            anyhow::anyhow!(
                "rovr daemon is not running ({}: {}).\n\
                 Start it with `rovr service start`, or restart it with `rovr service restart`.",
                path.display(),
                err
            )
        } else {
            anyhow::Error::new(err).context(format!("connect to rovr daemon at {}", path.display()))
        }
    })
}

#[cfg(target_os = "macos")]
fn socket_answers(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

/// Poll until the socket matches `want_answers`, bounded by `timeout`. No
/// infinite polling (PRODUCT.md: every OS wait has a deadline).
#[cfg(target_os = "macos")]
fn wait_for_socket(path: &Path, want_answers: bool, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if socket_answers(path) == want_answers {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(target_os = "macos")]
fn daemon_service_loaded(uid: u32) -> bool {
    std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/{DAEMON_LABEL}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn print_service_status(uid: u32, socket: &Path, plist: &Path) -> Result<()> {
    let loaded = daemon_service_loaded(uid);
    let listening = socket_answers(socket);
    println!("service: {DAEMON_LABEL}");
    println!(
        "plist:   {} ({})",
        plist.display(),
        if plist.exists() { "present" } else { "missing" }
    );
    println!("launchd: {}", if loaded { "loaded" } else { "not loaded" });
    println!(
        "socket:  {} ({})",
        socket.display(),
        if listening {
            "listening"
        } else {
            "no listener"
        }
    );
    if !plist.exists() && !loaded {
        println!("hint: the daemon is not installed — run scripts/install.sh");
    } else if !loaded {
        println!("hint: start it with `rovr service start`");
    } else if !listening {
        println!("hint: loaded but not answering — `rovr service restart`");
    }
    Ok(())
}

fn run_service(args: &ServiceArgs) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = args;
        anyhow::bail!("service management is only available on macOS");
    }

    #[cfg(target_os = "macos")]
    {
        let uid = rovr_platform::unix_uid();
        let socket = default_socket_path();
        let plist = daemon_plist_path()?;

        if matches!(&args.command, ServiceSubcommand::Status) {
            return print_service_status(uid, &socket, &plist);
        }

        let loaded = daemon_service_loaded(uid);
        let Some(argv) = lifecycle_launchctl_args(&args.command, uid, DAEMON_LABEL, &plist, loaded)
        else {
            println!("service {DAEMON_LABEL}: already stopped");
            return Ok(());
        };

        if argv.first().map(String::as_str) == Some("bootstrap") && !plist.exists() {
            anyhow::bail!(
                "LaunchAgent plist not found at {} — run scripts/install.sh to install the daemon",
                plist.display()
            );
        }

        let output = std::process::Command::new("launchctl")
            .args(&argv)
            .output()
            .with_context(|| format!("run `launchctl {}`", argv.join(" ")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "`launchctl {}` failed ({}): {}",
                argv.join(" "),
                output.status,
                stderr.trim()
            );
        }

        let want_up = !matches!(&args.command, ServiceSubcommand::Stop);
        let reached = wait_for_socket(&socket, want_up, std::time::Duration::from_secs(3));
        let listening = socket_answers(&socket);
        println!(
            "service {DAEMON_LABEL}: {}",
            if want_up { "started" } else { "stopped" }
        );
        println!(
            "socket:  {} ({})",
            socket.display(),
            if listening {
                "listening"
            } else {
                "no listener"
            }
        );
        if want_up && !reached {
            println!(
                "hint: not answering yet — run `rovr service status`, or check /tmp/rovr.err.log"
            );
        }
        Ok(())
    }
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

    /// A build next to this executable wins over the permanent-install
    /// locations. Those hold whatever a PREVIOUS install wrote, so preferring
    /// them made `sa install` silently reinstall a stale payload and report
    /// success: the exact failure that left macOS 27.2 running the Oct 9 SA
    /// payload with space ops disabled.
    #[test]
    fn sa_install_prefers_a_fresh_build_over_the_installed_set() {
        let built = (
            Some(PathBuf::from(
                "/repo/target/release/build/out/librovr_sa_payload.dylib",
            )),
            Some(PathBuf::from(
                "/repo/target/release/build/out/rovr-sa-loader",
            )),
            Some(PathBuf::from(
                "/repo/target/release/build/out/rovr-sa-helper",
            )),
        );
        let installed = (
            Some(PathBuf::from(
                "/usr/local/lib/rovr/librovr_sa_payload.dylib",
            )),
            Some(PathBuf::from("/usr/local/lib/rovr/rovr-sa-loader")),
            Some(PathBuf::from("/usr/local/lib/rovr/rovr-sa-helper")),
        );
        let chosen = choose_sa_artifacts(built, installed).expect("a complete build set wins");
        assert_eq!(
            chosen.0,
            PathBuf::from("/repo/target/release/build/out/librovr_sa_payload.dylib")
        );
        assert_eq!(
            chosen.1,
            PathBuf::from("/repo/target/release/build/out/rovr-sa-loader")
        );
        assert_eq!(
            chosen.2,
            PathBuf::from("/repo/target/release/build/out/rovr-sa-helper")
        );
    }

    /// A globally installed CLI has no build tree next to it, so the installed
    /// set is the only answer. Reinstalling the current files must still work.
    #[test]
    fn sa_install_falls_back_to_the_installed_set_without_a_build() {
        let chosen = choose_sa_artifacts(
            (None, None, None),
            (
                Some(PathBuf::from(
                    "/usr/local/lib/rovr/librovr_sa_payload.dylib",
                )),
                Some(PathBuf::from("/usr/local/lib/rovr/rovr-sa-loader")),
                Some(PathBuf::from("/usr/local/lib/rovr/rovr-sa-helper")),
            ),
        )
        .expect("the installed set is the fallback");
        assert_eq!(
            chosen.0,
            PathBuf::from("/usr/local/lib/rovr/librovr_sa_payload.dylib")
        );
    }

    /// A partial build tree is a broken build, not a licence to complete the
    /// set from the installed one. Pairing a fresh payload with a stale helper
    /// is exactly the kind of silent mismatch this ordering fix exists to stop.
    #[test]
    fn sa_install_refuses_a_partial_build_tree() {
        let some = || Some(PathBuf::from("/repo/out/x"));
        assert_eq!(
            incomplete_build_members(&(some(), None, None)),
            vec!["rovr-sa-loader", "rovr-sa-helper"],
            "payload-only build names the missing binaries"
        );
        assert_eq!(
            incomplete_build_members(&(some(), some(), None)),
            vec!["rovr-sa-helper"],
            "loader present but helper missing is still incomplete"
        );
        assert!(
            incomplete_build_members(&(None, None, None)).is_empty(),
            "an absent build tree is a fallback candidate, not an error"
        );
        assert!(
            incomplete_build_members(&(some(), some(), some())).is_empty(),
            "a complete build tree has nothing missing"
        );
    }

    /// `rovr service` exposes the daemon lifecycle as subcommands.
    #[test]
    fn service_subcommands_parse() {
        for sub in ["status", "start", "stop", "restart"] {
            let cli = Cli::try_parse_from(["rovr", "service", sub])
                .unwrap_or_else(|e| panic!("`service {sub}` must parse: {e}"));
            assert!(
                matches!(cli.command, TopCommand::Service(_)),
                "`service {sub}` must map to TopCommand::Service"
            );
        }
        let TopCommand::Service(args) = Cli::try_parse_from(["rovr", "service", "status"])
            .expect("parses")
            .command
        else {
            panic!("expected a service command");
        };
        assert!(matches!(args.command, ServiceSubcommand::Status));
    }

    /// The mutation each lifecycle subcommand performs is a pure launchctl argv
    /// mapping, verifiable without running launchd.
    #[test]
    fn service_lifecycle_maps_to_launchctl() {
        let plist = Path::new("/Users/me/Library/LaunchAgents/com.rovr.daemon.plist");
        let boot = || {
            Some(vec![
                "bootstrap".to_string(),
                "gui/501".to_string(),
                plist.display().to_string(),
            ])
        };
        assert_eq!(
            lifecycle_launchctl_args(&ServiceSubcommand::Start, 501, DAEMON_LABEL, plist, false),
            boot(),
            "start on an unloaded service must bootstrap the LaunchAgent"
        );
        assert_eq!(
            lifecycle_launchctl_args(&ServiceSubcommand::Restart, 501, DAEMON_LABEL, plist, false),
            boot(),
            "restart on an unloaded service must bootstrap too"
        );
        assert_eq!(
            lifecycle_launchctl_args(&ServiceSubcommand::Start, 501, DAEMON_LABEL, plist, true),
            Some(vec![
                "kickstart".to_string(),
                "gui/501/com.rovr.daemon".to_string()
            ])
        );
        assert_eq!(
            lifecycle_launchctl_args(&ServiceSubcommand::Restart, 501, DAEMON_LABEL, plist, true),
            Some(vec![
                "kickstart".to_string(),
                "-k".to_string(),
                "gui/501/com.rovr.daemon".to_string()
            ])
        );
        assert_eq!(
            lifecycle_launchctl_args(&ServiceSubcommand::Stop, 501, DAEMON_LABEL, plist, true),
            Some(vec![
                "bootout".to_string(),
                "gui/501/com.rovr.daemon".to_string()
            ])
        );
        assert_eq!(
            lifecycle_launchctl_args(&ServiceSubcommand::Stop, 501, DAEMON_LABEL, plist, false),
            None,
            "stopping an unloaded service is a no-op"
        );
        assert_eq!(
            lifecycle_launchctl_args(&ServiceSubcommand::Status, 501, DAEMON_LABEL, plist, true),
            None,
            "status never mutates the service"
        );
    }

    /// Only the two "no daemon" errnos get the actionable diagnostic; a live
    /// daemon that resets a connection must not be misreported as down.
    #[test]
    fn daemon_missing_classifies_only_absence() {
        use std::io::{Error, ErrorKind};
        assert!(daemon_missing(&Error::from(ErrorKind::ConnectionRefused)));
        assert!(daemon_missing(&Error::from(ErrorKind::NotFound)));
        assert!(!daemon_missing(&Error::from(ErrorKind::PermissionDenied)));
        assert!(!daemon_missing(&Error::from(ErrorKind::ConnectionReset)));
        assert!(!daemon_missing(&Error::from(ErrorKind::TimedOut)));
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

    /// A shrink bind such as `alt - y : rovr window resize west -20` must reach
    /// the daemon. clap reads a bare `-20` as an unknown flag unless the delta
    /// arg allows hyphen values, which silently kills the bind.
    #[test]
    fn window_resize_parses_a_negative_delta() {
        let cli = Cli::try_parse_from(["rovr", "window", "resize", "west", "-20"])
            .expect("negative delta parses");
        let TopCommand::Window(args) = cli.command else {
            panic!("expected window command");
        };
        assert!(matches!(
            args.command,
            WindowSubcommand::Resize {
                edge: DirectionArg::West,
                delta: -20,
                ..
            }
        ));
        assert_eq!(
            map_command(TopCommand::Window(WindowArgs {
                command: WindowSubcommand::Resize {
                    window: None,
                    edge: DirectionArg::West,
                    delta: -20,
                },
            })),
            Command::Window(WindowCommand::Resize {
                window: None,
                edge: rovr_types::Direction::West,
                delta: -20,
            })
        );
    }

    /// `space next` / `space prev` exist so the alt+tab binds can live in skhd
    /// as ordinary CLI calls; the protocol supported them but the CLI did not.
    #[test]
    fn space_next_and_prev_parse_and_map() {
        for (arg, expected) in [
            ("next", SpaceCommand::Next { display: None }),
            ("prev", SpaceCommand::Prev { display: None }),
        ] {
            let cli = Cli::try_parse_from(["rovr", "space", arg]).expect("parses");
            let TopCommand::Space(_) = cli.command else {
                panic!("expected space command");
            };
            let cli = Cli::try_parse_from(["rovr", "space", arg]).unwrap();
            assert_eq!(map_command(cli.command), Command::Space(expected));
        }

        let cli = Cli::try_parse_from(["rovr", "space", "next", "main"]).unwrap();
        assert_eq!(
            map_command(cli.command),
            Command::Space(SpaceCommand::Next {
                display: Some("main".into())
            })
        );
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

    /// The bug this pins: `sudo rovr sa status` reported a healthy helper as
    /// `installed` with "the helper does not answer", because the helper
    /// refusing a root caller (which can never match the console session uid)
    /// was being read as a dead service. A refusal is a response.
    #[cfg(target_os = "macos")]
    #[test]
    fn helper_refusal_proves_liveness_but_transport_failure_does_not() {
        use rovr_platform::macos::reinject::{HelperError, HelperResponse};
        assert!(helper_is_answering(&Ok(HelperResponse {
            status: 0,
            dock_pid: 1
        })));
        assert!(
            helper_is_answering(&Err(HelperError::Rejected { status: 4 })),
            "UNAUTHORIZED is a real response from a live helper"
        );
        assert!(!helper_is_answering(&Err(HelperError::Unavailable(
            "connect failed".into()
        ))));
        assert!(!helper_is_answering(&Err(HelperError::Protocol(
            "connection closed mid-response".into()
        ))));
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

    /// `rovr query --current` and `rovr query current` map to the same typed
    /// command — one command system, two spellings (flag for SketchyBar init
    /// scripts, subcommand for consistency with other queries).
    #[test]
    fn query_current_flag_and_subcommand_agree() {
        let via_flag = Cli::try_parse_from(["rovr", "query", "--current"]).expect("flag parses");
        assert!(is_current_query(&via_flag.command));
        assert_eq!(
            map_command(via_flag.command),
            Command::Query(QueryCommand::Current)
        );

        let via_sub = Cli::try_parse_from(["rovr", "query", "current"]).expect("sub parses");
        assert!(is_current_query(&via_sub.command));
        assert_eq!(
            map_command(via_sub.command),
            Command::Query(QueryCommand::Current)
        );

        let other = Cli::try_parse_from(["rovr", "query", "windows"]).expect("other parses");
        assert!(!is_current_query(&other.command));
    }

    /// `--current` prints the bare snapshot (not the response envelope) as
    /// valid JSON with stable fields; absent focus is null, never a placeholder.
    #[test]
    fn query_current_renders_bare_snapshot_json() {
        let snapshot = serde_json::json!({
            "space": 3,
            "display": 1,
            "window": {"id": 482, "pid": 1234, "app": "Ghostty", "title": "~/src/rovr"}
        });
        let ok = Response::ok(7, snapshot.clone());
        let rendered = render_current_response(&ok).expect("ok renders");
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert_eq!(value, snapshot);
        assert_eq!(value["space"], 3);
        assert_eq!(value["window"]["app"], "Ghostty");
        // No envelope keys leak into stdout.
        assert!(value.get("status").is_none());
        assert!(value.get("result").is_none());

        let unfocused = Response::ok(
            8,
            serde_json::json!({"space": 3, "display": 1, "window": null}),
        );
        let rendered = render_current_response(&unfocused).expect("unfocused renders");
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert!(value["window"].is_null());

        let err = Response::error(9, "SNAPSHOT_ERROR", "no observation yet");
        let message = render_current_response(&err).expect_err("error must not render JSON");
        assert!(
            message.contains("SNAPSHOT_ERROR"),
            "stderr names the code: {message}"
        );
    }

    /// A doctor payload with everything healthy, mirroring the daemon's shape.
    fn healthy_doctor_result() -> Value {
        serde_json::json!({
            "protocol": rovr_protocol::PROTOCOL_VERSION,
            "capabilities": {
                "observe_windows": true,
                "set_window_frame": true,
                "focus_window": true,
                "move_window_to_space": true,
                "create_space": true,
                "destroy_space": true,
                "focus_space": true,
                "reorder_space": true,
                "set_window_layer": true,
                "set_window_sticky": true,
                "set_window_shadow": true,
                "set_window_opacity": true,
                "set_window_scale": true,
                "scripting_addition": true,
            },
            "accessibility": {"control_available": true, "degraded_message": null},
            "sa": {
                "socket": "/tmp/rovr-501/sa.sock",
                "present": true,
                "version": "rovr-sa-2.1",
                "compatible": true,
                "attribs": 2047,
                "expected_prefix": "rovr-sa-2."
            },
            "sa_reinject": {
                "phase": "healthy",
                "generation": 3,
                "dock_pid": 812,
                "attempts_this_generation": 1,
                "retry_in_secs": null,
                "pending": false,
                "last_result": "injected",
                "last_error": null,
                "helper_socket": "/tmp/rovr-501/sa-helper.sock"
            },
            "snapshot_wedged_ms": null,
            "health": {
                "state_loop_heartbeat_age_ms": 12,
                "last_observation_age_ms": 20,
                "reconcile_failure_streak": 0
            },
            "generation": 41,
            "refresh_required": false,
            "windows": 7,
            "spaces": 2,
            "displays": 1,
            "config": "/Users/me/.config/rovr/rovr.toml",
            "layout": "bsp",
            "gap": 8,
            "reconcile_interval_ms": 1000
        })
    }

    /// Section values align, so the report reads as a set of small tables.
    #[test]
    fn doctor_sections_align_their_values() {
        let section = Section::new("demo").row("a", 1).row("longest", "value");
        assert_eq!(section.render(), "demo\n  a        1\n  longest  value\n");
    }

    /// The whole point of the change: `rovr doctor` prints grouped prose with
    /// every surface represented, and says so when nothing is wrong.
    #[test]
    fn doctor_renders_a_readable_report_when_everything_is_healthy() {
        let report =
            render_doctor(&Response::ok(1, healthy_doctor_result())).expect("ok outcome renders");
        for section in ["daemon", "config", "health", "platform", "capabilities"] {
            assert!(
                report.contains(section),
                "report must contain a {section} section:\n{report}"
            );
        }
        for expected in [
            "protocol",
            "generation",
            "bsp",
            "8 px",
            "every 1000 ms",
            "control available",
            "injected",
            "compatible",
            "healthy",
            "14 of 14",
        ] {
            assert!(
                report.contains(expected),
                "report must mention `{expected}`:\n{report}"
            );
        }
        assert!(
            report.ends_with("all checks passed\n"),
            "a healthy payload must end with a clear verdict:\n{report}"
        );
        assert!(
            !report.contains("\"windows\""),
            "the report is prose, not echoed JSON:\n{report}"
        );
    }

    /// A missing scripting addition must name the fix and reduce the
    /// capability count, so the report is actionable instead of a flag dump.
    #[test]
    fn doctor_report_names_the_fix_for_a_missing_scripting_addition() {
        let mut payload = healthy_doctor_result();
        payload["sa"]["present"] = serde_json::json!(false);
        payload["sa"]["version"] = serde_json::json!(null);
        payload["capabilities"]["scripting_addition"] = serde_json::json!(false);
        let report = render_doctor(&Response::ok(2, payload)).expect("renders");
        assert!(report.contains("not injected in Dock"), "{report}");
        assert!(
            report.contains("sudo rovr sa install"),
            "the fix must be in the report: {report}"
        );
        assert!(
            report.contains("13 of 14"),
            "capability count must reflect the loss: {report}"
        );
        assert!(
            report.contains("2 issues"),
            "the summary must count the sa and capability losses: {report}"
        );
    }

    /// Every degraded surface doctor knows about must show up both as a row
    /// state and in the trailing issue summary.
    #[test]
    fn doctor_report_surfaces_degraded_accessibility_and_stale_health() {
        let mut payload = healthy_doctor_result();
        payload["accessibility"]["control_available"] = serde_json::json!(false);
        payload["accessibility"]["degraded_message"] =
            serde_json::json!("accessibility permission unavailable");
        payload["health"]["reconcile_failure_streak"] = serde_json::json!(3);
        payload["health"]["state_loop_heartbeat_age_ms"] = serde_json::json!(9000);
        payload["health"]["last_observation_age_ms"] = serde_json::json!(null);
        payload["snapshot_wedged_ms"] = serde_json::json!(4200);
        let report = render_doctor(&Response::ok(3, payload)).expect("renders");
        for expected in [
            "accessibility permission unavailable",
            "reconcile has failed 3 times in a row",
            "heartbeat is 9000 ms old",
            "wedged 4200 ms",
            "no pass yet",
        ] {
            assert!(
                report.contains(expected),
                "report must surface `{expected}`:\n{report}"
            );
        }
        assert!(
            !report.contains("all checks passed"),
            "a degraded payload must not claim success:\n{report}"
        );
        assert!(
            report.contains("5 issues"),
            "summary must count them: {report}"
        );
    }

    /// An unrecognised or older payload degrades to `unknown` rows instead of
    /// panicking: doctor is diagnostics, it must render even when odd.
    #[test]
    fn doctor_report_tolerates_an_empty_payload() {
        let report = render_doctor(&Response::ok(4, serde_json::json!({}))).expect("renders");
        assert!(report.contains("rovr doctor"), "{report}");
        assert!(report.contains("unknown"), "{report}");
        assert!(
            report.contains("1 issue\n"),
            "a payload with no sections must not claim health:\n{report}"
        );
    }

    /// An error outcome is a stderr message, never a report: the daemon could
    /// not produce diagnostics, so there is nothing to print as one.
    #[test]
    fn doctor_error_outcome_becomes_a_stderr_message() {
        let err = Response::error(5, "DAEMON_STARTING", "no observation yet");
        let message = render_doctor(&err).expect_err("error must not render a report");
        assert!(message.contains("DAEMON_STARTING"), "{message}");
        assert!(message.contains("no observation yet"), "{message}");
    }

    /// `--json` is the machine contract; the readable report is the human one.
    /// Both must keep mapping to the same typed command.
    #[test]
    fn doctor_json_flag_opts_out_of_the_readable_report() {
        let plain = Cli::try_parse_from(["rovr", "doctor"]).expect("doctor parses");
        assert!(
            matches!(plain.command, TopCommand::Doctor(ref args) if !args.json),
            "the report is the default output"
        );
        let json = Cli::try_parse_from(["rovr", "doctor", "--json"]).expect("--json parses");
        assert!(
            matches!(json.command, TopCommand::Doctor(ref args) if args.json),
            "--json keeps the raw payload for scripts"
        );
        assert_eq!(map_command(json.command), Command::Doctor);
    }
}
