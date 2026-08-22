use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::{FileTypeExt, PermissionsExt},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
    thread,
    time::Duration,
};
const MAX_REQUEST_BYTES: usize = 64 * 1024;

use anyhow::{Context, Result};
use clap::Parser;
use rovr_config::Config;
use rovr_core::{Action, Engine, EngineError, Event};
#[cfg(target_os = "macos")]
use rovr_platform::MacPlatform;
#[cfg(not(target_os = "macos"))]
use rovr_platform::MockPlatform;
use rovr_platform::Platform;
use rovr_protocol::{
    Command, ConfigCommand, DebugCommand, LayoutCommand, Notification, QueryCommand, Request,
    Response, ScratchpadCommand, SpaceCommand, WindowCommand, WorkspaceCommand, PROTOCOL_VERSION,
};
use rovr_types::SpaceId;

mod hotkey;
use serde_json::json;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
/// Bounded per-subscriber backlog. A subscriber that falls this far behind is
/// evicted (its channel is full), so the state loop never blocks on it.
const SUBSCRIBER_BACKLOG: usize = 64;

#[derive(Debug, Parser)]
#[command(name = "rovr-daemon", version)]
struct Args {
    #[arg(long)]
    foreground: bool,
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
}

struct Envelope {
    request: Request,
    response: Sender<Response>,
    /// Wall-clock instant the request was accepted off the socket — used to
    /// measure state-loop queueing delay (head-of-line blocking behind
    /// periodic observation work).
    queued_at: std::time::Instant,
}

struct Daemon {
    engine: Engine,
    platform: Box<dyn Platform>,
    config: Config,
    config_path: PathBuf,
    state_path: PathBuf,
    /// The space that was current before the current one; updated whenever
    /// observation sees a change. Backs `space focus-recent`.
    previous_space: std::cell::Cell<Option<SpaceId>>,
    /// The currently focused Space as of the last observation.
    current_space_cell: std::cell::Cell<Option<SpaceId>>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "rovr=info".into()))
        .init();

    let args = Args::parse();
    let _foreground = args.foreground;
    let socket_path = args.socket.unwrap_or_else(default_socket_path);
    let config_path = args.config.unwrap_or_else(default_config_path);
    let state_path = default_state_path();
    let config = load_config_or_default(&config_path)?;

    let mut platform: Box<dyn Platform> = make_platform()?;
    let event_watcher: std::sync::Arc<dyn Fn(u32) + Send + Sync> =
        std::sync::Arc::new(|_window_id| {
            if let Some(event_tx) = EVENT_TX.get() {
                let (response, response_rx) = mpsc::channel();
                let request = Request::new(0, Command::Refresh);
                let _ = event_tx.try_send(Envelope {
                    request,
                    response,
                    queued_at: std::time::Instant::now(),
                });
                drop(response_rx);
            }
        });
    platform.set_event_watcher(event_watcher);
    let mut engine = Engine::new(config.clone());
    engine.capabilities = platform.capabilities();
    if let Err(err) = engine.load_state(&state_path) {
        warn!(%err, "no persisted state (first run expected)");
    }
    match platform.snapshot() {
        Ok(snapshot) => {
            execute_actions(
                &mut *platform,
                engine.apply_event(Event::Snapshot(snapshot)),
            );
        }
        Err(err) => warn!(%err, "initial platform snapshot failed"),
    }

    let daemon = Daemon {
        engine,
        platform,
        config,
        config_path,
        state_path,
        previous_space: std::cell::Cell::new(None),
        current_space_cell: std::cell::Cell::new(None),
    };

    run_daemon(socket_path, daemon)
}

#[cfg(target_os = "macos")]
fn make_platform() -> Result<Box<dyn Platform>> {
    Ok(Box::new(MacPlatform::new()?))
}

#[cfg(not(target_os = "macos"))]
fn make_platform() -> Result<Box<dyn Platform>> {
    Ok(Box::new(MockPlatform::default()))
}

/// Binds the IPC socket, then splits work across threads (blocker 6):
/// - MAIN thread: creates the global hotkey manager (Carbon event target
///   requires the main thread) and runs the AppKit event loop so hotkeys fire.
/// - accept thread: UnixListener::incoming + per-client handler threads.
/// - state thread: the single owner of engine/platform mutable state.
fn run_daemon(path: PathBuf, daemon: Daemon) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(&path) {
        let ft = meta.file_type();
        if ft.is_symlink() {
            anyhow::bail!("refusing to remove symlink at {}", path.display());
        }
        if ft.is_socket() {
            fs::remove_file(&path)
                .with_context(|| format!("remove stale socket {}", path.display()))?;
        } else {
            anyhow::bail!(
                "stale non-socket file at {} — refusing to remove",
                path.display()
            );
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("bind rovr socket at {}", path.display()))?;
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    info!(socket = %path.display(), "rovr daemon listening");

    // Built-in hotkey manager MUST be created on the main thread. It is kept
    // alive by the AppKit event loop below for the daemon's lifetime.
    let hotkey_manager = hotkey::create_hotkey_manager(daemon.config.clone(), path.clone());

    let subscribers: Arc<Mutex<Vec<SyncSender<Notification>>>> = Arc::new(Mutex::new(Vec::new()));
    // Bounded request queue: a flood of clients applies backpressure at the
    // socket instead of growing memory without limit while the state loop is
    // busy (256 in-flight requests is far beyond any real session).
    let (tx, rx) = mpsc::sync_channel::<Envelope>(256);

    // AX event trampolines push a Refresh envelope through this so the state
    // loop wakes instantly instead of waiting for the next tick. try_send
    // only: a full queue must never block the AppKit event loop.
    let _ = EVENT_TX.set(tx.clone());

    // State loop on its own thread (single owner of daemon state).
    let subs_for_loop = subscribers.clone();
    thread::spawn(move || state_loop(daemon, rx, subs_for_loop));

    // Socket accept loop off the main thread.
    let subs_for_accept = subscribers.clone();
    let _accept_handle = thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let tx = tx.clone();
                    let subs = subs_for_accept.clone();
                    thread::spawn(move || {
                        if let Err(err) = handle_client(stream, tx, subs) {
                            error!(%err, "IPC client failed");
                        }
                    });
                }
                Err(err) => error!(%err, "socket accept failed"),
            }
        }
    });

    // Main thread: pump the AppKit/CFRunLoop event loop forever so global
    // hotkeys are delivered. Never returns while the daemon lives.
    #[cfg(target_os = "macos")]
    hotkey::run_appkit_event_loop(hotkey_manager);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = hotkey_manager;
        let _ = _accept_handle.join();
    }
    #[cfg(not(target_os = "macos"))]
    Ok(())
}

fn handle_client(
    mut stream: UnixStream,
    tx: SyncSender<Envelope>,
    subscribers: Arc<Mutex<Vec<SyncSender<Notification>>>>,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    let mut line = String::new();
    let bytes = BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    if bytes == 0 {
        anyhow::bail!("empty request");
    }
    if line.len() > MAX_REQUEST_BYTES {
        let err = Response::error(0, "BAD_REQUEST", "request too large");
        serde_json::to_writer(&mut stream, &err)?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        return Ok(());
    }
    let request: Request = match serde_json::from_str(&line) {
        Ok(req) => req,
        Err(err) => {
            let resp = Response::error(0, "BAD_REQUEST", format!("invalid request: {err}"));
            serde_json::to_writer(&mut stream, &resp)?;
            stream.write_all(b"\n")?;
            stream.flush()?;
            return Ok(());
        }
    };
    let request_id = request.id;

    if let Some(err) = validate_request(&request) {
        serde_json::to_writer(&mut stream, &err)?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        return Ok(());
    }

    if matches!(request.command, Command::Subscribe) {
        // Acknowledge the subscription before streaming. The CLI consumes this
        // one-shot Response as the subscription ACK, then reads notifications.
        let ack = Response::ok(request_id, json!({ "subscribed": true }));
        serde_json::to_writer(&mut stream, &ack)?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        // Per-subscriber bounded queue. Enqueue Hello before registering so the
        // writer thread always emits Hello first (channel FIFO). The state loop
        // never touches the socket; it only try_sends into this channel.
        let (tx, rx) = mpsc::sync_channel::<Notification>(SUBSCRIBER_BACKLOG);
        let _ = tx.try_send(Notification::Hello {
            protocol_version: PROTOCOL_VERSION,
        });

        // Register the subscriber BEFORE spawning the writer thread. Hello is
        // already buffered in the channel, so the writer still emits it first;
        // but once registered, every notification the state loop delivers queues
        // behind Hello. A client cannot observe Hello until the subscription
        // actually exists, so no transition delivered after registration is lost.
        register_subscriber(&subscribers, tx);

        // Move the socket into a dedicated writer thread that performs all
        // socket I/O off the state-owner path.
        let sub_stream = stream;
        thread::spawn(move || {
            let mut writer = sub_stream;
            while let Ok(notif) = rx.recv() {
                let payload = match serde_json::to_string(&notif) {
                    Ok(mut s) => {
                        s.push('\n');
                        s
                    }
                    Err(_) => continue,
                };
                if writer
                    .write_all(payload.as_bytes())
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
            // rx dropped on exit; the registry entry is reaped on the next try_send.
        });

        return Ok(());
    }

    let (response_tx, response_rx) = mpsc::channel();
    tx.send(Envelope {
        request,
        response: response_tx,
        queued_at: std::time::Instant::now(),
    })?;

    let response = response_rx.recv()?;
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    info!(request_id, "IPC request completed");
    Ok(())
}

fn state_loop(
    mut daemon: Daemon,
    rx: Receiver<Envelope>,
    subscribers: Arc<Mutex<Vec<SyncSender<Notification>>>>,
) {
    // Startup warm-up: run the first observation immediately instead of
    // waiting for the first tick. This establishes the AX/SkyLight per-app
    // connections while the daemon is idle, so the user's FIRST switch is as
    // fast as every other one (cold-start stall regression).
    let t_warm = std::time::Instant::now();
    if daemon.refresh_observation() {
        deliver_notification(&subscribers, &Notification::StateChanged);
    }
    tracing::debug!(
        warmup_ms = t_warm.elapsed().as_millis() as u64,
        "startup observation warm-up complete"
    );
    // Fixed cadence. Deliberately NOT adaptive: faster polling during
    // activity hammers WindowServer with SLS/AX work exactly while the
    // user is switching Spaces, which visibly stutters animations
    // (regression verified live). Observation is serialized here instead.
    let interval = Duration::from_millis(daemon.config.general.reconcile_interval_ms.max(100));
    // Absolute observation deadline. recv_timeout restarts after every
    // request, so a steady request stream (rapid hotkeys) would otherwise
    // postpone observation indefinitely and commands would resolve "the
    // focused window" against stale state (wrong-window regression).
    let mut last_observed_at = std::time::Instant::now();
    loop {
        match rx.recv_timeout(interval) {
            Ok(envelope) => {
                // Observe BEFORE handling so the request sees state no older
                // than one interval, even mid-burst.
                if last_observed_at.elapsed() >= interval {
                    let t_obs = std::time::Instant::now();
                    if daemon.refresh_observation() {
                        deliver_notification(&subscribers, &Notification::StateChanged);
                    }
                    let obs_ms = t_obs.elapsed().as_millis() as u64;
                    if obs_ms > 100 {
                        tracing::info!(obs_ms, "slow periodic observation");
                    }
                    last_observed_at = std::time::Instant::now();
                }
                let queue_wait_ms = envelope.queued_at.elapsed().as_millis() as u64;
                if queue_wait_ms > 50 {
                    tracing::info!(
                        queue_wait_ms,
                        id = envelope.request.id,
                        "request waited on busy state loop"
                    );
                }
                let t_handle = std::time::Instant::now();
                let result = daemon.handle(envelope.request);
                tracing::debug!(
                    handle_ms = t_handle.elapsed().as_millis() as u64,
                    queue_wait_ms,
                    "envelope handled"
                );
                let _ = envelope.response.send(result.response);
                for notif in &result.notifications {
                    deliver_notification(&subscribers, notif);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let t_obs = std::time::Instant::now();
                if daemon.refresh_observation() {
                    deliver_notification(&subscribers, &Notification::StateChanged);
                }
                let obs_ms = t_obs.elapsed().as_millis() as u64;
                if obs_ms > 100 {
                    tracing::info!(obs_ms, "slow periodic observation");
                }
                last_observed_at = std::time::Instant::now();
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn deliver_notification(
    subscribers: &Arc<Mutex<Vec<SyncSender<Notification>>>>,
    notification: &Notification,
) {
    let mut subs = match subscribers.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            error!("subscriber registry poisoned — recovering");
            poisoned.into_inner()
        }
    };
    let mut dead = Vec::new();
    for (i, tx) in subs.iter().enumerate() {
        // Non-blocking: a slow or dead subscriber is evicted immediately so the
        // state loop can never block on client socket I/O.
        if tx.try_send(notification.clone()).is_err() {
            dead.push(i);
        }
    }
    for i in dead.into_iter().rev() {
        subs.remove(i);
    }
}

fn register_subscriber(
    subscribers: &Arc<Mutex<Vec<SyncSender<Notification>>>>,
    tx: SyncSender<Notification>,
) {
    match subscribers.lock() {
        Ok(mut subs) => subs.push(tx),
        Err(poisoned) => {
            error!("subscriber registry poisoned on register — recovering");
            poisoned.into_inner().push(tx);
        }
    }
}

/// Validates the protocol version before any request (including Subscribe) is
/// acted on. Returns an error Response to send back when the version is wrong.
fn validate_request(request: &Request) -> Option<Response> {
    if request.version != PROTOCOL_VERSION {
        Some(Response::error(
            request.id,
            "PROTOCOL_VERSION_MISMATCH",
            format!(
                "client protocol {} is incompatible with daemon protocol {}",
                request.version, PROTOCOL_VERSION
            ),
        ))
    } else {
        None
    }
}

/// Whether this command may resolve a None window id as "the focused window".
/// Those commands need observation to be current at resolution time (see the
/// Command::Window arm in `handle`).
fn window_command_defaults_to_focus(command: &WindowCommand) -> bool {
    match command {
        WindowCommand::FocusDirection { from, .. }
        | WindowCommand::SetLayer { window: from, .. }
        | WindowCommand::MoveToWorkspace { window: from, .. }
        | WindowCommand::Close { window: from }
        | WindowCommand::ToggleFullscreen { window: from }
        | WindowCommand::ToggleFloat { window: from }
        | WindowCommand::SwapDirection { window: from, .. }
        | WindowCommand::WarpDirection { window: from, .. }
        | WindowCommand::Resize { window: from, .. } => from.is_none(),
        _ => false,
    }
}

/// The result of handling one request: the one-shot Response plus any
/// notifications describing state transitions that actually committed.
struct HandleResult {
    response: Response,
    notifications: Vec<Notification>,
}

impl HandleResult {
    fn ok(id: u64, body: impl Serialize) -> Self {
        HandleResult {
            response: Response::ok(id, body),
            notifications: Vec::new(),
        }
    }
    fn err(id: u64, code: &str, msg: impl Into<String>) -> Self {
        HandleResult {
            response: Response::error(id, code, msg),
            notifications: Vec::new(),
        }
    }
    fn with_notifications(mut self, notes: Vec<Notification>) -> Self {
        self.notifications = notes;
        self
    }
}
impl Daemon {
    fn handle(&mut self, request: Request) -> HandleResult {
        let id = request.id;
        match request.command {
            Command::Ping => HandleResult::ok(id, json!({ "pong": true })),
            Command::Refresh => {
                // Internal wake from the AX event trampoline: run one
                // observation pass immediately so newly created windows tile
                // without waiting for the periodic tick.
                let changed = self.refresh_observation();
                let result = HandleResult::ok(id, json!({ "refreshed": changed }));
                if changed {
                    return result.with_notifications(vec![Notification::StateChanged]);
                }
                result
            }
            Command::Doctor => {
                #[cfg(target_os = "macos")]
                let sa_diagnostics = {
                    // Downcast to MacPlatform via `sa_status` by probing directly — avoids
                    // adding a new trait method for this diagnostics-only path.
                    // If the platform is not MacPlatform, this block is not compiled.
                    use rovr_platform::macos::sa::SaClient;
                    let c = SaClient::new();
                    let info = c.probe();
                    json!({
                        "socket": c.socket_path().display().to_string(),
                        "present": info.is_some(),
                        "version": info.as_ref().map(|i| i.version.clone()),
                        "compatible": info.as_ref().is_some_and(|i| i.is_compatible()),
                        "attribs": info.as_ref().map(|i| i.attribs),
                        "expected_prefix": rovr_platform::macos::sa::ROVR_SA_VERSION_PREFIX,
                    })
                };
                #[cfg(not(target_os = "macos"))]
                let sa_diagnostics = json!({ "available": false, "reason": "not_macos" });
                // Automatic reinjection lifecycle (Dock change → one bounded
                // privileged request per generation).
                let sa_reinject = self.platform.sa_reinject_diagnostics();
                #[cfg(not(target_os = "macos"))]
                let sa_reinject = None;
                let snapshot_wedged_ms = self.platform.snapshot_wedged_ms();
                HandleResult::ok(
                id,
                json!({
                    "protocol": PROTOCOL_VERSION,
                    "capabilities": self.platform.capabilities(),
                    "sa": sa_diagnostics,
                    "sa_reinject": sa_reinject.map(|d| json!({
                        "phase": d.phase,
                        "generation": d.generation,
                        "dock_pid": d.dock_pid,
                        "attempts_this_generation": d.attempts_this_generation,
                        "retry_in_secs": d.retry_in_secs,
                        "pending": d.pending,
                        "last_result": d.last_result,
                        "last_error": d.last_error,
                        "helper_socket": d.helper_socket,
                    })),
                    "snapshot_wedged_ms": snapshot_wedged_ms,
                    "generation": self.engine.observed.generation,
                    "refresh_required": self.engine.observed.refresh_required,
                    "windows": self.engine.observed.windows.len(),
                    "spaces": self.engine.observed.spaces.len(),
                    "displays": self.engine.observed.displays.len(),
                    "config": &self.config_path,
                    "layout": self.config.general.layout,
                    "gap": self.config.general.gap,
                    "reconcile_interval_ms": self.config.general.reconcile_interval_ms,
                }),
            )
            }
            Command::Query(command) => match command {
                QueryCommand::Windows => {
                    let mut windows: Vec<_> =
                        self.engine.observed.windows.values().cloned().collect();
                    windows.sort_by_key(|window| window.id);
                    HandleResult::ok(id, windows)
                }
                QueryCommand::Spaces => {
                    let mut spaces: Vec<_> =
                        self.engine.observed.spaces.values().cloned().collect();
                    spaces.sort_by_key(|space| (space.position, space.id));
                    HandleResult::ok(id, spaces)
                }
                QueryCommand::Displays => {
                    let mut displays: Vec<_> =
                        self.engine.observed.displays.values().cloned().collect();
                    displays.sort_by_key(|display| display.id);
                    HandleResult::ok(id, displays)
                }
                QueryCommand::State => HandleResult::ok(
                    id,
                    json!({
                        "observed": &self.engine.observed,
                        "desired": &self.engine.desired,
                    }),
                ),
                QueryCommand::Focused => {
                    let focused = self
                        .engine
                        .observed
                        .windows
                        .values()
                        .find(|w| w.focused)
                        .cloned();
                    HandleResult::ok(id, focused)
                }
                QueryCommand::Current => {
                    let id_val = self
                        .engine
                        .observed
                        .windows
                        .values()
                        .find(|w| w.focused)
                        .map(|w| w.id.0);
                    HandleResult::ok(id, json!({ "id": id_val }))
                }
            },
            Command::Window(command) => {
                // Focus-defaulting commands resolve "the focused window"
                // against observed state. Observe NOW, at resolution time:
                // the previous command's post-action verification snapshot can
                // predate macOS finishing its focus transition, so the next
                // bind in a quick sequence would otherwise target the stale
                // focus and move the WRONG window (regression verified live).
                // Explicit-id commands need no observation.
                if window_command_defaults_to_focus(&command) {
                    self.refresh_observation();
                }
                // Layout-mutating ops (swap/warp) change BSP structure without
                // producing actions; they need snapshot → reconcile → verify.
                let layout_change = match command {
                    WindowCommand::Swap { a, b } => {
                        Some(self.engine.swap_windows(a, b))
                    }
                    WindowCommand::Warp { window, target } => {
                        Some(self.engine.warp_window(window, target))
                    }
                    WindowCommand::SwapDirection { direction, window } => {
                        Some(self.engine.swap_windows_direction(direction, window))
                    }
                    WindowCommand::WarpDirection { direction, window } => {
                        Some(self.engine.warp_window_direction(direction, window))
                    }
                    _ => None,
                };
                if let Some(result) = layout_change {
                    return self.layout_change_result(id, result);
                }
                let result = match command {
                    WindowCommand::Focus { window } => self.engine.focus_window(window),
                    WindowCommand::FocusDirection { from, direction } => {
                        self.engine.focus_direction(from, direction)
                    }
                    WindowCommand::SetFrame { window, frame } => {
                        self.engine.set_window_frame(window, frame)
                    }
                    WindowCommand::MoveToSpace { window, space } => {
                        self.engine.move_window_to_space(window, space)
                    }
                    WindowCommand::MoveToWorkspace { window, workspace } => {
                        self.engine.move_window_to_workspace(window, &workspace)
                    }
                    WindowCommand::SetLayer { window, layer } => {
                        self.engine.set_window_layer(window, layer)
                    }
                    WindowCommand::SetSticky { window, sticky } => {
                        self.engine.set_window_sticky(window, sticky)
                    }
                    WindowCommand::SetShadow { window, shadow } => {
                        self.engine.set_window_shadow(window, shadow)
                    }
                    WindowCommand::SetOpacity {
                        window,
                        opacity,
                        duration_ms,
                    } => self.engine.set_window_opacity(window, opacity, duration_ms),
                    WindowCommand::Pip { window } => self.engine.toggle_window_pip(window),
                    WindowCommand::Close { window } => self.engine.close_window(window),
                    WindowCommand::ToggleFullscreen { window } => {
                        self.engine.toggle_fullscreen(window)
                    }
                    WindowCommand::ToggleFloat { window } => {
                        self.engine.toggle_float(window).map(|()| vec![])
                    }
                    WindowCommand::Resize {
                        window,
                        edge,
                        delta,
                    } => self.engine.resize_window_edge(window, edge, delta),
                    WindowCommand::Swap { .. }
                    | WindowCommand::Warp { .. }
                    | WindowCommand::SwapDirection { .. }
                    | WindowCommand::WarpDirection { .. } => unreachable!(),
                };
                match result {
                    Ok(actions) => match self.execute_and_refresh(actions) {
                        Ok(()) => HandleResult::ok(id, json!({ "accepted": true }))
                            .with_notifications(vec![Notification::StateChanged]),
                        Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string()),
                    },
                    Err(err) => HandleResult::err(id, "ENGINE_ERROR", err.to_string()),
                }
            }
            Command::Layout(command) => {
                // All layout commands default to the focused Space when no
                // explicit id is given.
                let space = match command {
                    LayoutCommand::Rotate { space } => {
                        let Some(space) = self.resolve_space_or_focused(space) else {
                            return HandleResult::err(id, "ENGINE_ERROR", "no focused space");
                        };
                        self.engine.rotate_layout(space);
                        space
                    }
                    LayoutCommand::Mirror { space } => {
                        let Some(space) = self.resolve_space_or_focused(space) else {
                            return HandleResult::err(id, "ENGINE_ERROR", "no focused space");
                        };
                        self.engine.mirror_layout(space);
                        space
                    }
                    LayoutCommand::Balance { space } => {
                        let Some(space) = self.resolve_space_or_focused(space) else {
                            return HandleResult::err(id, "ENGINE_ERROR", "no focused space");
                        };
                        self.engine.balance_layout(space);
                        space
                    }
                    LayoutCommand::SetRatio { space, ratio } => {
                        let Some(space) = self.resolve_space_or_focused(space) else {
                            return HandleResult::err(id, "ENGINE_ERROR", "no focused space");
                        };
                        let res = self.engine.set_split_ratio(space, ratio);
                        if let Err(e) = res {
                            return HandleResult::err(id, "ENGINE_ERROR", e.to_string());
                        }
                        space
                    }
                };
                self.persist_state();
                // The core policy mutation (rotate/mirror) has committed. The
                // typed notification describes that committed transition and must
                // be emitted even if applying it to macOS later fails; the
                // response/error reports the platform problem separately.
                let (horizontal, reversed) = self
                    .engine
                    .layout_orientation(space)
                    .unwrap_or((false, false));
                let notification = Notification::LayoutChanged {
                    space,
                    horizontal,
                    reversed,
                };
                match self.platform.snapshot() {
                    Ok(snapshot) => {
                        let actions = self.engine.apply_event(Event::Snapshot(snapshot));
                        match self.execute_and_refresh(actions) {
                            Ok(()) => HandleResult::ok(id, json!({ "accepted": true }))
                                .with_notifications(vec![notification.clone()]),
                            Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string())
                                .with_notifications(vec![notification]),
                        }
                    }
                    Err(err) => HandleResult::err(id, "SNAPSHOT_ERROR", err.to_string())
                        .with_notifications(vec![notification]),
                }
            }
            Command::Workspace(command) => {
                // Focus-only fast path: focusing a Space mutates no window
                // geometry, so the synchronous verify snapshot (plus its
                // followup reconcile snapshot) only adds ~200 ms of latency
                // to rapid switching. The periodic state loop observes the
                // focus within one reconcile interval anyway.
                if let WorkspaceCommand::Focus { name } = &command {
                    let t0 = std::time::Instant::now();
                    let result = self.engine.focus_workspace(name);
                    let engine_ms = t0.elapsed().as_millis() as u64;
                    return match result {
                        Ok(actions) => {
                            // Record at dispatch so focus-recent and repeated
                            // hotkeys see the new current space immediately.
                            if let Some(Action::FocusSpace { space }) =
                                actions.iter().find(|a| matches!(a, Action::FocusSpace { .. }))
                            {
                                self.note_space_switched_to(*space);
                            }
                            let t1 = std::time::Instant::now();
                            let exec = execute_actions_result(&mut *self.platform, actions);
                            tracing::debug!(
                                workspace = %name,
                                engine_ms,
                                execute_ms = t1.elapsed().as_millis() as u64,
                                total_ms = t0.elapsed().as_millis() as u64,
                                "workspace focus timing"
                            );
                            match exec {
                                Ok(()) => HandleResult::ok(id, json!({ "accepted": true }))
                                    .with_notifications(vec![Notification::StateChanged]),
                                Err(err) => {
                                    HandleResult::err(id, "PLATFORM_ERROR", err.to_string())
                                }
                            }
                        }
                        Err(err) => HandleResult::err(id, "ENGINE_ERROR", err.to_string()),
                    };
                }
                let result = match command {
                    WorkspaceCommand::Focus { .. } => unreachable!("handled above"),
                    WorkspaceCommand::MoveWindow { window, workspace } => {
                        self.engine.move_window_to_workspace(window, &workspace)
                    }
                };
                match result {
                    Ok(actions) => match self.execute_and_refresh(actions) {
                        Ok(()) => HandleResult::ok(id, json!({ "accepted": true }))
                            .with_notifications(vec![Notification::StateChanged]),
                        Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string()),
                    },
                    Err(err) => HandleResult::err(id, "ENGINE_ERROR", err.to_string()),
                }
            }
            Command::Scratchpad(command) => {
                let (name, scratchpad_actions) = match command {
                    ScratchpadCommand::Toggle { name } => {
                        let actions = self.engine.toggle_scratchpad(&name);
                        (name, actions)
                    }
                };
                self.persist_state();
                let open = self.engine.scratchpads.is_open(&name);
                let notification = Notification::ScratchpadToggled {
                    name: name.clone(),
                    open,
                };
                // First execute scratchpad show/hide actions (minimize, move, frame, focus)
                let scratchpad_result = self.execute_and_refresh(scratchpad_actions);
                match self.platform.snapshot() {
                    Ok(snapshot) => {
                        let actions = self.engine.apply_event(Event::Snapshot(snapshot));
                        let combined = match scratchpad_result {
                            Ok(()) => self.execute_and_refresh(actions),
                            Err(e) => Err(e),
                        };
                        match combined {
                            Ok(()) => HandleResult::ok(id, json!({ "accepted": true }))
                                .with_notifications(vec![notification.clone()]),
                            Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string())
                                .with_notifications(vec![notification]),
                        }
                    }
                    Err(err) => HandleResult::err(id, "SNAPSHOT_ERROR", err.to_string())
                        .with_notifications(vec![notification]),
                }
            }
            Command::Space(command) => {
                if let SpaceCommand::FocusRecent = command {
                    return match self.focus_recent_space() {
                        Some((result, previous)) => {
                            // Record the toggle NOW: the second press must see
                            // this space as "previous" even if observation has
                            // not caught up yet.
                            self.note_space_switched_to(previous);
                            match self.execute_and_refresh(result) {
                                Ok(()) => HandleResult::ok(id, json!({ "accepted": true }))
                                    .with_notifications(vec![Notification::StateChanged]),
                                Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string()),
                            }
                        }
                        None => HandleResult::err(
                            id,
                            "ENGINE_ERROR",
                            "no previous space to focus (need at least one observed switch)",
                        ),
                    };
                }
                if let SpaceCommand::Focus { space } = command {
                    // Deliberate focus: record at dispatch (see FocusRecent).
                    self.note_space_switched_to(space);
                    let result = self.engine.focus_space(space);
                    match result {
                        Ok(actions) => match self.execute_and_refresh(actions) {
                            Ok(()) => HandleResult::ok(id, json!({ "accepted": true }))
                                .with_notifications(vec![Notification::StateChanged]),
                            Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string()),
                        },
                        Err(err) => HandleResult::err(id, "ENGINE_ERROR", err.to_string()),
                    }
                } else {
                if let SpaceCommand::ToggleInsets = command {
                    // Session-scoped collapse of gap+padding; the next layout
                    // pass re-frames every tiled window with the new insets.
                    self.engine.insets_off = !self.engine.insets_off;
                    return match self
                        .platform
                        .snapshot()
                        .map_err(|err| (id, err.to_string()))
                    {
                        Ok(snap) => {
                            let actions = self.engine.apply_event(Event::Snapshot(snap));
                            match self.execute_and_refresh(actions) {
                                Ok(()) => HandleResult::ok(
                                    id,
                                    json!({ "accepted": true, "insets_off": self.engine.insets_off }),
                                )
                                .with_notifications(vec![Notification::StateChanged]),
                                Err(err) => {
                                    HandleResult::err(id, "PLATFORM_ERROR", err.to_string())
                                }
                            }
                        }
                        Err((id, msg)) => HandleResult::err(id, "SNAPSHOT_ERROR", msg),
                    };
                }
                let result = match command {
                    SpaceCommand::Create { anchor } => self.engine.create_space(anchor),
                    SpaceCommand::Destroy { space } => self.engine.destroy_space(space),
                    SpaceCommand::Move { space, after } => self.engine.move_space(space, after),
                    SpaceCommand::ToggleInsets | SpaceCommand::Focus { .. } | SpaceCommand::FocusRecent => unreachable!("handled above"),
                };
                match result {
                    Ok(actions) => match self.execute_and_refresh(actions) {
                        Ok(()) => HandleResult::ok(id, json!({ "accepted": true }))
                            .with_notifications(vec![Notification::StateChanged]),
                        Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string()),
                    },
                    Err(err) => HandleResult::err(id, "ENGINE_ERROR", err.to_string()),
                }
                }
            }
            Command::Config(command) => match command {
                ConfigCommand::Reload { path } => {
                    let raw = path
                        .map(PathBuf::from)
                        .unwrap_or_else(|| self.config_path.clone());
                    if raw.to_string_lossy().len() > 4096 {
                        return HandleResult::err(id, "CONFIG_ERROR", "path too long");
                    }
                    match Config::load(&raw) {
                        Ok(config) => {
                            self.config = config.clone();
                            self.config_path = raw;
                            self.engine.reload_config(config);
                            self.persist_state();
                            HandleResult::ok(id, json!({ "reloaded": true }))
                                .with_notifications(vec![Notification::ConfigReloaded])
                        }
                        Err(err) => {
                            let msg = err.to_string();
                            let truncated = if msg.len() > 500 { &msg[..500] } else { &msg };
                            HandleResult::err(id, "CONFIG_ERROR", truncated.to_string())
                        }
                    }
                }
                ConfigCommand::Check { path } => {
                    if path.len() > 4096 {
                        return HandleResult::err(id, "CONFIG_ERROR", "path too long");
                    }
                    match Config::load(&path) {
                        Ok(_) => HandleResult::ok(id, json!({ "valid": true })),
                        Err(err) => {
                            let msg = err.to_string();
                            let truncated = if msg.len() > 500 { &msg[..500] } else { &msg };
                            HandleResult::err(id, "CONFIG_ERROR", truncated.to_string())
                        }
                    }
                },
            },
            Command::Debug(DebugCommand::Events) => {
                HandleResult::ok(id, self.engine.flight_recorder.snapshot())
            }
            Command::Subscribe => HandleResult::err(
                id,
                "SUBSCRIBE_STREAMING",
                "subscribe is served on a streaming connection; the one-shot path does not support it",
            ),
        }
    }

    fn refresh_observation(&mut self) -> bool {
        let mut changed = false;
        // Event-driven: display topology callback sets the flag on reconfiguration.
        if self.platform.needs_refresh() {
            self.engine.observed.bump_generation();
            self.engine
                .flight_recorder
                .record("display.topology_changed", "callback-triggered refresh");
            changed = true;
        }

        // Deliberately SYNCHRONOUS and serialized with actions: observation
        // must never overlap Mission Control animations or SLS mutations —
        // concurrent enumeration during a Space switch makes animations
        // visibly stutter (regression verified live). Snapshots are cheap
        // since per-app AX caching landed, so head-of-line blocking here is
        // bounded by one cheap cycle rather than reintroduced.
        match self.platform.snapshot() {
            Ok(snapshot) => {
                let actions = self.engine.apply_event(Event::Snapshot(snapshot));
                if !actions.is_empty() {
                    execute_actions_result(&mut *self.platform, actions).unwrap_or_else(|err| {
                        self.engine
                            .flight_recorder
                            .record("platform.error", err.to_string());
                        warn!(%err, "periodic reconciliation failed");
                    });
                    changed = true;
                }
            }
            Err(err) => {
                self.engine
                    .flight_recorder
                    .record("snapshot.error", err.to_string());
                warn!(%err, "periodic snapshot failed");
            }
        }
        // Track the previous Space for `space focus-recent`: whenever
        // observation sees the current Space change, the old one becomes
        // "previous". Focused-space lookup mirrors QueryCommand::Current.
        let current_space = self
            .engine
            .observed
            .spaces
            .values()
            .find(|s| s.focused)
            .map(|s| s.id);
        if let (Some(prev_current), Some(new_current)) = (self.current_space(), current_space) {
            if new_current != prev_current {
                self.previous_space.set(Some(prev_current));
            }
        }
        if let Some(cs) = current_space {
            self.current_space_cell.set(Some(cs));
        }
        changed
    }

    fn current_space(&self) -> Option<SpaceId> {
        self.current_space_cell.get()
    }

    /// Record a deliberate Space switch AT DISPATCH TIME. Observation-based
    /// tracking (in `refresh_observation`) only catches up on a later tick;
    /// without this, two quick `focus-recent` presses both read the same
    /// stale "previous" and toggle stops being two-way.
    fn note_space_switched_to(&self, new_current: SpaceId) {
        if self.current_space_cell.get() == Some(new_current) {
            return;
        }
        if let Some(old) = self.current_space_cell.get() {
            self.previous_space.set(Some(old));
        }
        self.current_space_cell.set(Some(new_current));
    }

    /// Resolve an optional Space id against the currently focused one.
    /// `Some(id)` passes through (caller validates existence downstream).
    fn resolve_space_or_focused(&self, space: Option<SpaceId>) -> Option<SpaceId> {
        space.or_else(|| self.current_space())
    }

    /// Actions to focus the previous space, if one is known and still exists.
    /// Also returns the space being focused so the caller can record the
    /// switch immediately (two-way toggle without waiting for observation).
    fn focus_recent_space(&mut self) -> Option<(Vec<Action>, SpaceId)> {
        let previous = self.previous_space.get()?;
        if !self.engine.observed.spaces.contains_key(&previous) {
            return None;
        }
        let actions = self.engine.focus_space(previous).ok()?;
        Some((actions, previous))
    }

    /// Result envelope for layout-tree mutations (swap/warp): they rewrite BSP
    /// structure directly, so the flow is persist → snapshot → reconcile →
    /// verify. Shared by the explicit-id and directional variants.
    fn layout_change_result(&mut self, id: u64, result: Result<(), EngineError>) -> HandleResult {
        match result {
            Ok(()) => {
                self.persist_state();
                match self.platform.snapshot() {
                    Ok(snap) => {
                        let actions = self.engine.apply_event(Event::Snapshot(snap));
                        match self.execute_and_refresh(actions) {
                            Ok(()) => HandleResult::ok(id, json!({ "accepted": true }))
                                .with_notifications(vec![Notification::StateChanged]),
                            Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string())
                                .with_notifications(vec![Notification::StateChanged]),
                        }
                    }
                    Err(err) => HandleResult::err(id, "SNAPSHOT_ERROR", err.to_string()),
                }
            }
            Err(err) => HandleResult::err(id, "ENGINE_ERROR", err.to_string()),
        }
    }

    fn execute_and_refresh(&mut self, actions: Vec<Action>) -> Result<()> {
        execute_actions_result(&mut *self.platform, actions)?;
        // Re-snapshot to verify mutations landed, then reconcile any residual drift.
        let snapshot = self.platform.snapshot()?;
        let followup = self.engine.apply_event(Event::Snapshot(snapshot));
        if !followup.is_empty() {
            execute_actions_result(&mut *self.platform, followup)?;
            self.engine
                .flight_recorder
                .record("reconcile.verification", "followup actions executed");
        }
        Ok(())
    }
    fn persist_state(&self) {
        if let Err(err) = self.engine.save_state(&self.state_path) {
            warn!(%err, "failed to persist daemon state");
        }
    }
}

fn execute_actions(platform: &mut dyn Platform, actions: Vec<Action>) {
    for action in actions {
        if let Err(err) = platform.execute(&action) {
            warn!(%err, ?action, "platform action failed");
        }
    }
}

fn execute_actions_result(platform: &mut dyn Platform, actions: Vec<Action>) -> Result<()> {
    for action in actions {
        platform.execute(&action)?;
    }
    Ok(())
}

fn load_config_or_default(path: &Path) -> Result<Config> {
    if path.exists() {
        Ok(Config::load(path)?)
    } else {
        Ok(Config::default())
    }
}

/// Handle for AX-event trampolines to wake the state loop immediately.
/// Set once in run_daemon before any thread that could trigger events.
static EVENT_TX: std::sync::OnceLock<mpsc::SyncSender<Envelope>> = std::sync::OnceLock::new();

fn default_socket_path() -> PathBuf {
    // Keyed on the REAL uid (getuid), never $UID: a daemon started from one
    // environment and a CLI spawned by skhd/launchd from another must agree.
    PathBuf::from(format!("/tmp/rovr-{}.sock", rovr_platform::unix_uid()))
}

fn default_config_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config/rovr/rovr.toml")
    } else {
        PathBuf::from("rovr.toml")
    }
}
fn default_state_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config/rovr/state.json")
    } else {
        PathBuf::from("rovr-state.json")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use rovr_platform::MockPlatform;
    use rovr_protocol::ResponseOutcome;
    use rovr_types::{
        DisplayId, DisplaySnapshot, PlatformSnapshot, Rect, SpaceId, SpaceSnapshot, WindowId,
        WindowSnapshot,
    };
    use std::sync::mpsc::sync_channel;

    /// M4b: a slow or disconnected subscriber is evicted and never blocks the
    /// state loop, because delivery uses a non-blocking try_send into a bounded
    /// per-subscriber channel.
    #[test]
    fn m4b_slow_subscriber_cannot_block_state_loop() {
        let subscribers: Arc<Mutex<Vec<SyncSender<Notification>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = sync_channel::<Notification>(1); // capacity 1
                                                        // Fill the channel so the next try_send must fail (Full).
        tx.try_send(Notification::StateChanged).unwrap();
        subscribers.lock().unwrap().push(tx);

        // Delivering to a full channel must evict immediately (non-blocking).
        deliver_notification(&subscribers, &Notification::StateChanged);

        assert!(
            subscribers.lock().unwrap().is_empty(),
            "full subscriber must be evicted without blocking"
        );
        drop(rx);
    }

    /// M4b: Hello is enqueued before registration, so the writer thread always
    /// emits Hello first on a subscriber's stream (channel FIFO).
    #[test]
    fn m4b_hello_is_first_notification() {
        let (tx, rx) = sync_channel::<Notification>(4);
        tx.try_send(Notification::Hello {
            protocol_version: PROTOCOL_VERSION,
        })
        .unwrap();
        tx.try_send(Notification::StateChanged).unwrap();

        assert_eq!(
            rx.try_recv().unwrap(),
            Notification::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        );
        assert_eq!(rx.try_recv().unwrap(), Notification::StateChanged);
    }

    /// M4b: a newly connected subscriber receives Hello; existing subscribers
    /// must NOT receive a second Hello.
    #[test]
    fn m4b_second_subscriber_does_not_send_hello_to_existing_subscribers() {
        let subscribers: Arc<Mutex<Vec<SyncSender<Notification>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let (a_tx, a_rx) = sync_channel::<Notification>(8);
        register_subscriber(&subscribers, a_tx);

        // B subscribes: enqueue Hello for B only, then register B.
        let (b_tx, b_rx) = sync_channel::<Notification>(8);
        b_tx.try_send(Notification::Hello {
            protocol_version: PROTOCOL_VERSION,
        })
        .unwrap();
        register_subscriber(&subscribers, b_tx);

        // A later broadcast must reach both, but only B got Hello.
        deliver_notification(&subscribers, &Notification::StateChanged);

        // A: only StateChanged, no Hello.
        assert_eq!(a_rx.try_recv().unwrap(), Notification::StateChanged);
        assert!(
            a_rx.try_recv().is_err(),
            "existing subscriber must not receive Hello again"
        );

        // B: Hello first, then StateChanged.
        assert_eq!(
            b_rx.try_recv().unwrap(),
            Notification::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        );
        assert_eq!(b_rx.try_recv().unwrap(), Notification::StateChanged);
    }

    /// M4b: subscribe with a mismatched protocol version is rejected before any
    /// subscription is established.
    #[test]
    fn m4b_subscribe_rejects_protocol_version_mismatch() {
        let good = Request {
            version: PROTOCOL_VERSION,
            id: 1,
            command: Command::Subscribe,
        };
        assert!(
            validate_request(&good).is_none(),
            "matching version must pass"
        );

        let bad = Request {
            version: PROTOCOL_VERSION + 999,
            id: 2,
            command: Command::Subscribe,
        };
        let err = validate_request(&bad).expect("mismatch must produce an error");
        match err.outcome {
            ResponseOutcome::Error { error } => {
                assert_eq!(error.code, "PROTOCOL_VERSION_MISMATCH")
            }
            ResponseOutcome::Ok { .. } => panic!("mismatch must not be accepted"),
        }
    }

    fn test_daemon() -> Daemon {
        Daemon {
            previous_space: std::cell::Cell::new(None),
            current_space_cell: std::cell::Cell::new(None),
            engine: Engine::new(Config::default()),
            platform: Box::new(MockPlatform::default()),
            config: Config::default(),
            config_path: PathBuf::from("/dev/null/rovr-test-config.toml"),
            state_path: PathBuf::from("/dev/null/rovr-test-state.json"),
        }
    }

    /// Builds a daemon whose observed state contains one window on one space on
    /// one display, so window/space mutations can succeed against the engine.
    fn populated_daemon() -> Daemon {
        let mut daemon = test_daemon();
        let snapshot = PlatformSnapshot {
            windows: vec![WindowSnapshot {
                id: WindowId(1),
                pid: rovr_types::ProcessId(10),
                app: "App".into(),
                bundle_id: None,
                title: "w".into(),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                space_id: Some(SpaceId(1)),
                display_id: Some(DisplayId(1)),
                focused: true,
                minimized: rovr_types::ObservedBool::No,
                fullscreen: rovr_types::ObservedBool::No,
                managed: rovr_types::ObservedBool::Yes,
                generation: 1,
            }],
            spaces: vec![SpaceSnapshot {
                id: SpaceId(1),
                display_id: DisplayId(1),
                label: None,
                focused: true,
                generation: 1,
                position: 0,
            }],
            displays: vec![DisplaySnapshot {
                id: DisplayId(1),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1000.0,
                    height: 1000.0,
                },
                label: None,
                focused: true,
                generation: 1,
            }],
            complete: true,
        };
        daemon.engine.apply_event(Event::Snapshot(snapshot));
        daemon
    }

    /// M4b: a failed config reload does not emit ConfigReloaded.
    #[test]
    fn m4b_failed_config_reload_does_not_emit_config_reloaded() {
        let mut daemon = test_daemon();
        let result = daemon.handle(Request::new(
            1,
            Command::Config(ConfigCommand::Reload {
                path: Some("/nonexistent/rovr-does-not-exist-xyz.toml".into()),
            }),
        ));
        assert!(matches!(
            result.response.outcome,
            ResponseOutcome::Error { .. }
        ));
        assert!(
            result.notifications.is_empty(),
            "failed reload must not emit ConfigReloaded"
        );
    }

    /// M4b: a read-only query does not emit any notification.
    #[test]
    fn m4b_query_does_not_emit_state_changed() {
        let mut daemon = test_daemon();
        let result = daemon.handle(Request::new(2, Command::Query(QueryCommand::Windows)));
        assert!(matches!(
            result.response.outcome,
            ResponseOutcome::Ok { .. }
        ));
        assert!(
            result.notifications.is_empty(),
            "query must not emit StateChanged"
        );
    }

    /// M4b: a failed command (unknown window) does not claim a state change.
    #[test]
    fn m4b_failed_command_does_not_claim_state_changed() {
        let mut daemon = test_daemon();
        let result = daemon.handle(Request::new(
            3,
            Command::Window(WindowCommand::Focus {
                window: WindowId(0xdeadbeef),
            }),
        ));
        assert!(matches!(
            result.response.outcome,
            ResponseOutcome::Error { .. }
        ));
        assert!(
            result.notifications.is_empty(),
            "failed command must not emit StateChanged"
        );
    }

    /// Doctor exposes the automatic SA reinjection lifecycle under
    /// `result.sa_reinject`, even when the platform reports none (null).
    #[test]
    fn sa_doctor_exposes_reinject_lifecycle() {
        let mut daemon = test_daemon();
        let result = daemon.handle(Request::new(6, Command::Doctor));
        let value = serde_json::to_value(&result.response).expect("serialize doctor response");
        assert!(
            value
                .get("result")
                .is_some_and(|r| r.get("sa_reinject").is_some()),
            "doctor must include the sa_reinject lifecycle section: {value}"
        );
    }

    /// `space focus-recent` focuses the previously current Space, tracked
    /// from observation. Without a tracked previous Space it errors cleanly.
    #[test]
    fn space_focus_recent_swaps_to_previous_space() {
        // The mock's verify snapshots must keep both spaces observable,
        // otherwise execute_and_refresh clobbers observed state mid-test.
        let platform = MockPlatform {
            snapshot: PlatformSnapshot {
                windows: vec![],
                spaces: vec![
                    rovr_types::SpaceSnapshot {
                        id: SpaceId(3),
                        display_id: DisplayId(1),
                        label: None,
                        focused: true,
                        generation: 1,
                        position: 0,
                    },
                    rovr_types::SpaceSnapshot {
                        id: SpaceId(7),
                        display_id: DisplayId(1),
                        label: None,
                        focused: false,
                        generation: 1,
                        position: 1,
                    },
                ],
                displays: vec![],
                complete: true,
            },
            executed: vec![],
        };
        let mut daemon = Daemon {
            previous_space: std::cell::Cell::new(None),
            current_space_cell: std::cell::Cell::new(None),
            engine: Engine::new(Config::default()),
            platform: Box::new(platform),
            config: Config::default(),
            config_path: PathBuf::from("/dev/null/rovr-test-config.toml"),
            state_path: PathBuf::from("/dev/null/rovr-test-state.json"),
        };
        // Mirror the mock snapshot into observed state (as warm-up would).
        let snap = daemon.platform.snapshot().unwrap();
        daemon.engine.apply_event(Event::Snapshot(snap));
        // No history yet: clean error, no actions executed.
        let result = daemon.handle(Request::new(1, Command::Space(SpaceCommand::FocusRecent)));
        assert!(matches!(
            result.response.outcome,
            ResponseOutcome::Error { .. }
        ));

        daemon.current_space_cell.set(Some(SpaceId(3)));
        daemon.previous_space.set(Some(SpaceId(7)));

        let result = daemon.handle(Request::new(2, Command::Space(SpaceCommand::FocusRecent)));
        assert!(matches!(
            result.response.outcome,
            ResponseOutcome::Ok { .. }
        ));

        // Two-way WITHOUT any observation tick in between: the first press
        // must have recorded Space 3 as previous, so the second press goes
        // back to it. (Regression: stale tracking made the toggle one-way.)
        let result = daemon.handle(Request::new(3, Command::Space(SpaceCommand::FocusRecent)));
        assert!(matches!(
            result.response.outcome,
            ResponseOutcome::Ok { .. }
        ));
        assert_eq!(daemon.current_space(), Some(SpaceId(3)));
        assert_eq!(daemon.previous_space.get(), Some(SpaceId(7)));

        // A tracked previous Space that no longer exists errors instead of
        // emitting an action against a ghost Space.
        daemon.previous_space.set(Some(SpaceId(999)));
        let result = daemon.handle(Request::new(3, Command::Space(SpaceCommand::FocusRecent)));
        assert!(matches!(
            result.response.outcome,
            ResponseOutcome::Error { .. }
        ));
    }

    /// M4b: a successful Window mutation emits StateChanged so subscribers learn
    /// about the verified transition.
    #[test]
    fn successful_window_mutation_emits_state_changed() {
        let mut daemon = populated_daemon();
        let result = daemon.handle(Request::new(
            4,
            Command::Window(WindowCommand::Focus {
                window: WindowId(1),
            }),
        ));
        assert!(matches!(
            result.response.outcome,
            ResponseOutcome::Ok { .. }
        ));
        assert_eq!(
            result.notifications,
            vec![Notification::StateChanged],
            "successful window mutation must emit StateChanged"
        );
    }

    /// M4b: a successful Space mutation emits StateChanged so subscribers learn
    /// about the verified transition.
    #[test]
    fn successful_space_mutation_emits_state_changed() {
        let mut daemon = populated_daemon();
        let result = daemon.handle(Request::new(
            5,
            Command::Space(SpaceCommand::Focus { space: SpaceId(1) }),
        ));
        assert!(matches!(
            result.response.outcome,
            ResponseOutcome::Ok { .. }
        ));
        assert_eq!(
            result.notifications,
            vec![Notification::StateChanged],
            "successful space mutation must emit StateChanged"
        );
    }
}
