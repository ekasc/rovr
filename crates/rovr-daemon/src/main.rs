use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::{
    collections::HashMap,
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
use rovr_types::{DisplayId, SpaceId, WindowId};

mod hotkey;
use serde_json::json;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
/// Bounded per-subscriber backlog. A subscriber that falls this far behind is
/// evicted (its channel is full), so the state loop never blocks on it.
const SUBSCRIBER_BACKLOG: usize = 64;
const MIN_RECOVERY_INTERVAL: Duration = Duration::from_secs(5);
const WINDOW_CREATED_EVENT_KIND: u32 = 1;
const WINDOW_DESTROYED_EVENT_KIND: u32 = 4;

fn event_requests_immediate_refresh(event_kind: u32) -> bool {
    event_kind == WINDOW_CREATED_EVENT_KIND || event_kind == WINDOW_DESTROYED_EVENT_KIND
}

#[derive(Default)]
struct RefreshWake(std::sync::atomic::AtomicBool);

impl RefreshWake {
    fn request(&self) -> bool {
        !self.0.swap(true, std::sync::atomic::Ordering::AcqRel)
    }

    fn acknowledge(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[derive(Debug, Parser)]
#[command(name = "rovr-daemon", version)]
struct Args {
    #[arg(long)]
    foreground: bool,
    #[arg(long)]
    socket: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(
        long,
        help = "Override persisted state path (useful for isolated verification)"
    )]
    state: Option<PathBuf>,
}

struct Envelope {
    request: Request,
    response: Sender<Response>,
    /// Wall-clock instant the request was accepted off the socket — used to
    /// measure state-loop queueing delay (head-of-line blocking behind
    /// periodic observation work).
    queued_at: std::time::Instant,
    event_wake: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct SpaceHistory {
    current: Option<SpaceId>,
    previous: Option<SpaceId>,
}

struct Daemon {
    engine: Engine,
    platform: Box<dyn Platform>,
    config: Config,
    config_path: PathBuf,
    state_path: PathBuf,
    /// Per-display current/previous Spaces for `space focus-recent`.
    space_history: std::cell::RefCell<HashMap<DisplayId, SpaceHistory>>,
    refresh_wake: Arc<RefreshWake>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "rovr=info".into()))
        .init();

    let args = Args::parse();
    let _foreground = args.foreground;
    let socket_path = args.socket.unwrap_or_else(default_socket_path);
    let config_path = args.config.unwrap_or_else(default_config_path);
    let state_path = args.state.unwrap_or_else(default_state_path);
    let config = load_config_or_default(&config_path)?;

    let mut platform: Box<dyn Platform> = make_platform()?;
    let refresh_wake = std::sync::Arc::new(RefreshWake::default());
    let watcher_wake = refresh_wake.clone();
    let event_watcher: std::sync::Arc<dyn Fn(u32) + Send + Sync> =
        std::sync::Arc::new(move |event_kind| {
            if !event_requests_immediate_refresh(event_kind) || !watcher_wake.request() {
                return;
            }
            let Some(event_tx) = EVENT_TX.get() else {
                watcher_wake.acknowledge();
                return;
            };
            let (response, response_rx) = mpsc::channel();
            let request = Request::new(0, Command::Refresh);
            if event_tx
                .try_send(Envelope {
                    request,
                    response,
                    queued_at: std::time::Instant::now(),
                    event_wake: true,
                })
                .is_err()
            {
                watcher_wake.acknowledge();
            }
            drop(response_rx);
        });
    platform.set_event_watcher(event_watcher);
    let mut engine = Engine::new(config.clone());
    engine.capabilities = platform.capabilities();
    if let Err(err) = engine.load_state(&state_path) {
        warn!(%err, "no persisted state (first run expected)");
    }
    let mut daemon = Daemon {
        engine,
        platform,
        config,
        config_path,
        state_path,
        space_history: std::cell::RefCell::new(HashMap::new()),
        refresh_wake,
    };
    match daemon.platform.snapshot() {
        Ok(snapshot) => {
            let actions = daemon.engine.apply_event(Event::Snapshot(snapshot));
            if let Err(err) = execute_actions_result(&mut *daemon.platform, actions) {
                warn!(%err, "initial platform reconciliation failed");
            }
        }
        Err(err) => warn!(%err, "initial platform snapshot failed"),
    }

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
        event_wake: false,
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
    // Full snapshots are an idle recovery watchdog. Window creation events
    // enqueue an immediate Refresh for spawn tiling; focus events are resolved
    // on demand by focus-defaulting window commands and recovered here when
    // idle. Interactive Space/Workspace commands remain observation-free.
    let interval = Duration::from_millis(daemon.config.general.reconcile_interval_ms.max(100))
        .max(MIN_RECOVERY_INTERVAL);
    let mut last_observed_at = std::time::Instant::now();
    loop {
        let envelope = match rx.recv_timeout(interval) {
            Ok(envelope) => envelope,
            Err(RecvTimeoutError::Timeout) => {
                let t_obs = std::time::Instant::now();
                daemon.engine.abandon_pending_space_cursors();
                if daemon.refresh_observation() {
                    deliver_notification(&subscribers, &Notification::StateChanged);
                }
                let obs_ms = t_obs.elapsed().as_millis() as u64;
                if obs_ms > 100 {
                    tracing::info!(obs_ms, "slow periodic observation");
                }
                last_observed_at = std::time::Instant::now();
                deliver_notification(&subscribers, &heartbeat_notification());
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if envelope.event_wake {
            daemon.refresh_wake.acknowledge();
        }

        // Some commands must NOT pay the pre-handle observation:
        // - Refresh observes inside handle() — two enumerations per
        //   AX window-create event visibly delayed spawn→tile.
        // - Workspace/Space focus are direct id switches; a full
        //   snapshot (~100-500ms) before posting the swipe was THE
        //   external-display switch lag.
        // - Focus-defaulting window commands re-observe themselves
        //   inside handle() at resolution time anyway.
        let skip_pre_observe = match &envelope.request.command {
            Command::Refresh => true,
            Command::Workspace(_) | Command::Space(_) => true,
            Command::Window(wc) if window_command_defaults_to_focus(wc) => true,
            _ => false,
        };
        if !skip_pre_observe && last_observed_at.elapsed() >= interval {
            let t_obs = std::time::Instant::now();
            daemon.engine.abandon_pending_space_cursors();
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
        let observes_internally = match &envelope.request.command {
            Command::Refresh => true,
            Command::Window(command) if window_command_defaults_to_focus(command) => true,
            _ => false,
        };
        let t_handle = std::time::Instant::now();
        let result = daemon.handle(envelope.request);
        if observes_internally {
            last_observed_at = std::time::Instant::now();
        }
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
}

fn heartbeat_notification() -> Notification {
    let unix_ms = std::time::SystemTime::UNIX_EPOCH
        .elapsed()
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();
    Notification::Heartbeat { unix_ms }
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
                    Ok(actions) => {
                        let close_window = actions.iter().find_map(|a| match a {
                            Action::CloseWindow { window } => Some(*window),
                            _ => None,
                        });
                        let exec = if let Some(wid) = close_window {
                            self.execute_close_and_refresh(wid, actions)
                        } else {
                            self.execute_and_refresh(actions)
                        };
                        match exec {
                            Ok(()) => HandleResult::ok(id, json!({ "accepted": true }))
                                .with_notifications(vec![Notification::StateChanged]),
                            Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string()),
                        }
                    }
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
                            // A dynamic spawn (i3-style alt-N) registered a new
                            // workspace — persist so it survives a restart.
                            let spawned = actions
                                .iter()
                                .any(|a| matches!(a, Action::CreateSpace { .. }));
                            let t1 = std::time::Instant::now();
                            // A known-workspace focus is one-shot and the next
                            // periodic tick will confirm it. A dynamic spawn
                            // is different: the CreateSpace must land, the
                            // engine must OBSERVE the new Space, the awaited
                            // binding must fire, and only then does the
                            // queued FocusSpace dispatch. Without the
                            // follow-up snapshot+apply_event the user is
                            // stuck waiting up to one reconcile interval
                            // (~1s) for the focus to land, with the new
                            // Space visibly flashing in and out of focus as
                            // the window server settles. Drive the loop
                            // synchronously on the spawn path.
                            let exec = if spawned {
                                match execute_actions_result(&mut *self.platform, actions) {
                                    Ok(()) => match self.platform.snapshot() {
                                        Ok(snap) => {
                                            let observed_spaces: Vec<SpaceId> =
                                                snap.spaces.iter().map(|s| s.id).collect();
                                            let followup =
                                                self.engine.apply_event(Event::Snapshot(snap));
                                            let focus_target = self
                                                .engine
                                                .workspaces
                                                .backing_for(name);
                                            tracing::info!(
                                                workspace = %name,
                                                observed_spaces = ?observed_spaces,
                                                focus_target = ?focus_target,
                                                followup_count = followup.len(),
                                                followup = ?followup,
                                                "workspace focus spawn followup"
                                            );
                                            execute_actions_result(
                                                &mut *self.platform,
                                                followup,
                                            )
                                        }
                                        Err(err) => Err(err.into()),
                                    },
                                    Err(err) => Err(err),
                                }
                            } else {
                                execute_actions_result(&mut *self.platform, actions)
                            };
                            let target_space = self.engine.workspaces.backing_for(name);
                            if exec.is_ok() {
                                if let Some(space) = target_space {
                                    self.engine.note_space_focus_dispatched(space);
                                    self.note_space_switched_to(space);
                                }
                                if spawned {
                                    self.persist_state();
                                }
                            }
                            let total_ms = t0.elapsed().as_millis() as u64;
                            // INFO when slow so switch lag is visible without
                            // debug logging; the hotkey path pays this on
                            // every alt-N press.
                            if total_ms > 50 {
                                tracing::info!(
                                    workspace = %name,
                                    engine_ms,
                                    execute_ms = t1.elapsed().as_millis() as u64,
                                    total_ms,
                                    "workspace focus timing (slow)"
                                );
                            } else {
                                tracing::debug!(
                                    workspace = %name,
                                    engine_ms,
                                    execute_ms = t1.elapsed().as_millis() as u64,
                                    total_ms,
                                    "workspace focus timing"
                                );
                            }
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
                    Ok(actions) => {
                        let spawned = actions
                            .iter()
                            .any(|a| matches!(a, Action::CreateSpace { .. }));
                        match self.execute_and_refresh(actions) {
                            Ok(()) => {
                                if spawned {
                                    self.persist_state();
                                }
                                HandleResult::ok(id, json!({ "accepted": true }))
                                    .with_notifications(vec![Notification::StateChanged])
                            }
                            Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string()),
                        }
                    }
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
                if let SpaceCommand::Next { display } | SpaceCommand::Prev { display } = &command {
                    let delta = if matches!(command, SpaceCommand::Next { .. }) { 1 } else { -1 };
                    return match self.engine.focus_space_step(display.as_deref(), delta) {
                        Ok(actions) => {
                            let target = actions.iter().find_map(|action| match action {
                                Action::FocusSpaceStep { target, .. } => Some(*target),
                                _ => None,
                            });
                            match execute_actions_result(&mut *self.platform, actions) {
                                Ok(()) => {
                                    if let Some(space) = target {
                                        self.engine.note_space_focus_dispatched(space);
                                        self.note_space_switched_to(space);
                                    }
                                    HandleResult::ok(id, json!({ "accepted": true }))
                                        .with_notifications(vec![Notification::StateChanged])
                                }
                                Err(err) => {
                                    if let Some(space) = target {
                                        self.engine.cancel_pending_space_focus(space);
                                    }
                                    HandleResult::err(id, "PLATFORM_ERROR", err.to_string())
                                }
                            }
                        }
                        Err(
                            EngineError::NoAdjacentSpace
                            | EngineError::DisplayNotFound(_)
                            | EngineError::NoFocusedSpace,
                        ) => HandleResult::err(id, "ENGINE_ERROR", "no adjacent space"),
                        Err(err) => HandleResult::err(id, "ENGINE_ERROR", err.to_string()),
                    };
                }
                if let SpaceCommand::FocusRecent = command {
                    return match self.focus_recent_space() {
                        Some((result, previous)) => {
                            match execute_actions_result(&mut *self.platform, result) {
                                Ok(()) => {
                                    self.engine.note_space_focus_dispatched(previous);
                                    self.note_space_switched_to(previous);
                                    HandleResult::ok(id, json!({ "accepted": true }))
                                        .with_notifications(vec![Notification::StateChanged])
                                }
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
                    let result = self.engine.focus_space(space);
                    match result {
                        Ok(actions) => match execute_actions_result(&mut *self.platform, actions) {
                            Ok(()) => {
                                self.engine.note_space_focus_dispatched(space);
                                self.note_space_switched_to(space);
                                HandleResult::ok(id, json!({ "accepted": true }))
                                    .with_notifications(vec![Notification::StateChanged])
                            }
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
                    SpaceCommand::ToggleInsets
                    | SpaceCommand::Focus { .. }
                    | SpaceCommand::FocusRecent
                    | SpaceCommand::Next { .. }
                    | SpaceCommand::Prev { .. } => unreachable!("handled above"),
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
                            // Hotkeys must follow the reloaded config — without
                            // this, new/changed [[bind]] entries only applied
                            // on daemon restart (the dead-code warnings were
                            // pointing at exactly this gap).
                            if let Err(err) = hotkey::reload(&config) {
                                return HandleResult::err(
                                    id,
                                    "CONFIG_ERROR",
                                    format!("hotkey update failed: {err}"),
                                );
                            }
                            self.config = config.clone();
                            self.config_path = raw;
                            match self.reload_config_and_self_heal(config) {
                                Ok(healed_spaces) => {
                                    self.persist_state();
                                    HandleResult::ok(
                                        id,
                                        json!({
                                            "reloaded": true,
                                            "healed_spaces": healed_spaces,
                                        }),
                                    )
                                    .with_notifications(vec![Notification::ConfigReloaded])
                                }
                                Err(err) => HandleResult::err(
                                    id,
                                    "PLATFORM_ERROR",
                                    format!("config reloaded but topology heal failed: {err}"),
                                )
                                .with_notifications(vec![Notification::ConfigReloaded]),
                            }
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
        // Capabilities are probed live by the platform (SA attribs follow
        // install/uninstall/reinjection). Re-sync the engine's copy every
        // tick so lifecycle decisions — e.g. CreateSpace for missing
        // persistent workspaces — use CURRENT capabilities, not the ones
        // captured once at daemon startup.
        let fresh_caps = self.platform.capabilities();
        if fresh_caps != self.engine.capabilities {
            self.engine.capabilities = fresh_caps;
            changed = true;
        }
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
        for diagnostic in self.platform.drain_diagnostics() {
            self.engine
                .flight_recorder
                .record(diagnostic.kind, diagnostic.detail);
        }
        // macOS reports one focused Space per display. Process every display
        // in sorted order so HashMap iteration cannot affect recent history.
        let mut focused_by_display: HashMap<DisplayId, SpaceId> = HashMap::new();
        for space in self.engine.observed.spaces.values().filter(|s| s.focused) {
            focused_by_display
                .entry(space.display_id)
                .and_modify(|current| {
                    let current_space = self.engine.observed.spaces.get(current).unwrap();
                    if (space.position, space.id) < (current_space.position, current_space.id) {
                        *current = space.id;
                    }
                })
                .or_insert(space.id);
        }
        let mut displays: Vec<_> = focused_by_display.into_iter().collect();
        displays.sort_by_key(|(display, _)| *display);
        let mut history = self.space_history.borrow_mut();
        for (display, current) in displays {
            let entry = history.entry(display).or_default();
            if entry.current != Some(current) {
                entry.previous = entry.current;
                entry.current = Some(current);
            }
        }
        changed
    }

    fn active_display(&self) -> Option<DisplayId> {
        self.engine
            .observed
            .displays
            .values()
            .filter(|display| display.focused)
            .map(|display| display.id)
            .min()
            .or_else(|| {
                self.engine
                    .observed
                    .displays
                    .values()
                    .find(|display| display.is_main)
                    .map(|display| display.id)
            })
            .or_else(|| self.engine.observed.displays.keys().copied().min())
            .or_else(|| {
                self.engine
                    .observed
                    .spaces
                    .values()
                    .filter(|space| space.focused)
                    .map(|space| space.display_id)
                    .min()
            })
    }

    fn current_space(&self) -> Option<SpaceId> {
        let history = self.space_history.borrow();
        self.active_display()
            .and_then(|display| history.get(&display).and_then(|state| state.current))
            .or_else(|| {
                self.engine
                    .observed
                    .spaces
                    .values()
                    .filter(|space| space.focused)
                    .min_by_key(|space| (space.display_id, space.position, space.id))
                    .map(|space| space.id)
            })
    }

    /// Record a deliberate Space switch AT DISPATCH TIME, on the display that
    /// contains the target. This preserves rapid two-way toggles before the
    /// next observation catches up.
    fn note_space_switched_to(&self, new_current: SpaceId) {
        let Some(display) = self
            .engine
            .observed
            .spaces
            .get(&new_current)
            .map(|space| space.display_id)
        else {
            return;
        };
        let mut history = self.space_history.borrow_mut();
        let entry = history.entry(display).or_default();
        if entry.current != Some(new_current) {
            entry.previous = entry.current;
            entry.current = Some(new_current);
        }
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
        let display = self.active_display()?;
        let previous = self
            .space_history
            .borrow()
            .get(&display)
            .and_then(|state| state.previous)?;
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

    fn execute_close_and_refresh(&mut self, window: WindowId, actions: Vec<Action>) -> Result<()> {
        execute_actions_result(&mut *self.platform, actions)?;
        // Optimistically exclude the closing window so the remaining windows
        // retile instantly, even though CGWindowList may still report it
        // during the AppKit close animation.
        let mut snapshot = self.platform.snapshot()?;
        snapshot.windows.retain(|w| w.id != window);
        let followup = self.engine.apply_event(Event::Snapshot(snapshot));
        if !followup.is_empty() {
            execute_actions_result(&mut *self.platform, followup)?;
            self.engine
                .flight_recorder
                .record("reconcile.verification", "followup actions executed");
        }
        Ok(())
    }

    /// Reset config-scoped runtime state, re-observe the real topology, and
    /// remove empty unclaimed Spaces one at a time. Space IDs remain
    /// authoritative; every deletion is observed as absent before another
    /// decision, so Mission Control position compaction cannot corrupt logical
    /// workspace identity.
    fn reload_config_and_self_heal(&mut self, config: Config) -> Result<usize> {
        const MAX_HEALED_SPACES: usize = 64;
        const VERIFY_POLLS: usize = 20;
        const VERIFY_INTERVAL: Duration = Duration::from_millis(50);
        self.engine.reload_config(config);

        for healed_spaces in 0..MAX_HEALED_SPACES {
            self.engine.capabilities = self.platform.capabilities();
            let snapshot = self.platform.snapshot()?;
            if !snapshot.complete {
                anyhow::bail!("cannot self-heal from an incomplete platform snapshot");
            }
            let actions = self.engine.apply_event(Event::Snapshot(snapshot));
            let topology_mutated = actions.iter().any(|action| {
                matches!(
                    action,
                    Action::CreateSpace { .. }
                        | Action::DestroySpace { .. }
                        | Action::MoveSpace { .. }
                )
            });
            if !actions.is_empty() {
                execute_actions_result(&mut *self.platform, actions)?;
            }
            if topology_mutated {
                // Persistent-workspace creation and ordinary dynamic cleanup
                // are asynchronous. Execute once and let the normal refresh
                // loop verify them; never issue the same topology mutation in
                // a tight reload loop against a stale snapshot.
                return Ok(healed_spaces);
            }

            let Some(heal) = self.engine.next_topology_heal_action() else {
                return Ok(healed_spaces);
            };
            let Action::DestroySpace { space } = heal else {
                unreachable!("topology heal only emits DestroySpace")
            };
            execute_actions_result(&mut *self.platform, vec![Action::DestroySpace { space }])?;

            let mut verified = false;
            for _ in 0..VERIFY_POLLS {
                let observed = self.platform.snapshot()?;
                if observed.complete && observed.spaces.iter().all(|item| item.id != space) {
                    verified = true;
                    break;
                }
                thread::sleep(VERIFY_INTERVAL);
            }
            if !verified {
                anyhow::bail!("space {} still observed after self-heal deletion", space.0);
            }
        }

        anyhow::bail!("topology did not converge after {MAX_HEALED_SPACES} deletions")
    }
    fn persist_state(&self) {
        if let Err(err) = self.engine.save_state(&self.state_path) {
            warn!(%err, "failed to persist daemon state");
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
    // Shared helper: /tmp/rovr-<getuid()>/daemon.sock — the SAME runtime
    // directory and uid keying as the SA socket. ($UID is not consulted:
    // launchd environments omit it, so env-based keying would desync the
    // daemon from its CLI.)
    rovr_platform::daemon_socket_path()
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
    use rovr_platform::{MockPlatform, PlatformDiagnostic, PlatformError};
    use rovr_protocol::ResponseOutcome;
    use rovr_types::{
        Capabilities, DisplayId, DisplaySnapshot, PlatformSnapshot, Rect, SpaceId, SpaceSnapshot,
        WindowId, WindowSnapshot,
    };
    use std::sync::mpsc::sync_channel;

    #[test]
    fn refresh_wake_coalesces_until_acknowledged() {
        let wake = RefreshWake::default();
        assert!(wake.request());
        assert!(!wake.request());
        wake.acknowledge();
        assert!(wake.request());
    }

    #[test]
    fn subscription_heartbeat_is_typed_and_timestamped() {
        assert!(matches!(
            heartbeat_notification(),
            Notification::Heartbeat { unix_ms } if unix_ms > 0
        ));
    }

    #[test]
    fn only_window_creation_requests_an_immediate_refresh() {
        assert!(event_requests_immediate_refresh(WINDOW_CREATED_EVENT_KIND));
        assert!(event_requests_immediate_refresh(
            WINDOW_DESTROYED_EVENT_KIND
        ));
        assert!(!event_requests_immediate_refresh(2));
    }

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
            space_history: std::cell::RefCell::new(HashMap::new()),
            refresh_wake: Arc::new(RefreshWake::default()),
            engine: Engine::new(Config::default()),
            platform: Box::new(MockPlatform::default()),
            config: Config::default(),
            config_path: PathBuf::from("/dev/null/rovr-test-config.toml"),
            state_path: PathBuf::from("/dev/null/rovr-test-state.json"),
        }
    }

    /// Platform whose capabilities change at runtime — models the SA appearing
    /// (install/reinjection) or disappearing (uninstall) while the daemon runs.
    /// Capability state and the execution log live behind shared handles so
    /// the test can flip capabilities after the platform is boxed into a Daemon.
    struct MutableCapPlatform {
        create_space: Arc<std::sync::atomic::AtomicBool>,
        snapshot: PlatformSnapshot,
        executed: Arc<std::sync::Mutex<Vec<Action>>>,
    }

    fn create_space_requests(executed: &std::sync::Mutex<Vec<Action>>) -> Vec<SpaceId> {
        executed
            .lock()
            .unwrap()
            .iter()
            .filter_map(|a| match a {
                Action::CreateSpace { anchor } => Some(*anchor),
                _ => None,
            })
            .collect()
    }

    impl Platform for MutableCapPlatform {
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                create_space: self.create_space.load(std::sync::atomic::Ordering::Relaxed),
                ..Capabilities::default()
            }
        }
        fn snapshot(&mut self) -> Result<PlatformSnapshot, PlatformError> {
            Ok(self.snapshot.clone())
        }
        fn execute(&mut self, action: &Action) -> Result<(), PlatformError> {
            self.executed.lock().unwrap().push(action.clone());
            Ok(())
        }
    }

    struct HealingPlatform {
        snapshot: Arc<std::sync::Mutex<PlatformSnapshot>>,
        destroyed: Arc<std::sync::Mutex<Vec<SpaceId>>>,
    }

    impl Platform for HealingPlatform {
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                destroy_space: true,
                ..Capabilities::default()
            }
        }

        fn snapshot(&mut self) -> Result<PlatformSnapshot, PlatformError> {
            Ok(self.snapshot.lock().unwrap().clone())
        }

        fn execute(&mut self, action: &Action) -> Result<(), PlatformError> {
            if let Action::DestroySpace { space } = action {
                self.destroyed.lock().unwrap().push(*space);
                self.snapshot
                    .lock()
                    .unwrap()
                    .spaces
                    .retain(|candidate| candidate.id != *space);
            }
            Ok(())
        }
    }

    #[test]
    fn config_reload_self_heals_empty_orphan_spaces_one_by_one() {
        let snapshot = Arc::new(std::sync::Mutex::new(PlatformSnapshot {
            windows: vec![],
            spaces: vec![
                SpaceSnapshot {
                    id: SpaceId(11),
                    display_id: DisplayId(1),
                    label: None,
                    focused: true,
                    generation: 1,
                    position: 0,
                    is_fullscreen: false,
                    is_system: false,
                },
                SpaceSnapshot {
                    id: SpaceId(12),
                    display_id: DisplayId(1),
                    label: None,
                    focused: false,
                    generation: 1,
                    position: 1,
                    is_fullscreen: false,
                    is_system: false,
                },
                SpaceSnapshot {
                    id: SpaceId(13),
                    display_id: DisplayId(1),
                    label: None,
                    focused: false,
                    generation: 1,
                    position: 2,
                    is_fullscreen: false,
                    is_system: false,
                },
            ],
            displays: vec![DisplaySnapshot {
                id: DisplayId(1),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1000.0,
                    height: 800.0,
                },
                label: None,
                focused: true,
                is_main: true,
                generation: 1,
            }],
            complete: true,
        }));
        let destroyed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut daemon = test_daemon();
        daemon.platform = Box::new(HealingPlatform {
            snapshot: snapshot.clone(),
            destroyed: destroyed.clone(),
        });
        let config = Config {
            workspaces: vec![rovr_config::WorkspaceConfig {
                name: "1".into(),
                layout: rovr_types::LayoutKind::Bsp,
                display: None,
                persistent: true,
                plugin: None,
            }],
            ..Config::default()
        };

        let healed = daemon.reload_config_and_self_heal(config).unwrap();

        assert_eq!(healed, 2);
        assert_eq!(*destroyed.lock().unwrap(), vec![SpaceId(13), SpaceId(12)]);
        assert_eq!(snapshot.lock().unwrap().spaces.len(), 1);
        assert_eq!(daemon.engine.workspaces.backing_for("1"), Some(SpaceId(11)));
    }

    /// i3-style alt-N must dispatch the CreateSpace, observe the new Space,
    /// bind it to the dynamic workspace, and emit the queued FocusSpace
    /// synchronously — not wait one reconcile interval for the periodic
    /// state loop to notice. Regression for the "huge delay" reported when
    /// the fast path skipped the post-snapshot apply_event.
    #[test]
    fn workspace_focus_spawn_synchronous_focus_path() {
        struct SpawnFocusPlatform {
            snapshot: Arc<std::sync::Mutex<PlatformSnapshot>>,
            executed: Arc<std::sync::Mutex<Vec<Action>>>,
        }
        impl Platform for SpawnFocusPlatform {
            fn capabilities(&self) -> Capabilities {
                Capabilities {
                    create_space: true,
                    destroy_space: true,
                    focus_space: true,
                    ..Capabilities::default()
                }
            }
            fn snapshot(&mut self) -> Result<PlatformSnapshot, PlatformError> {
                Ok(self.snapshot.lock().unwrap().clone())
            }
            fn execute(&mut self, action: &Action) -> Result<(), PlatformError> {
                let mut snap = self.snapshot.lock().unwrap();
                let mut executed = self.executed.lock().unwrap();
                executed.push(action.clone());
                match action {
                    Action::CreateSpace { .. } => {
                        let display_id = snap.spaces.first().map(|s| s.display_id);
                        let next_id = snap.spaces.iter().map(|s| s.id.0).max().unwrap_or(0) + 1;
                        if let Some(display) = display_id {
                            let new_pos = snap.spaces.len() as u32;
                            snap.spaces.push(SpaceSnapshot {
                                id: SpaceId(next_id),
                                display_id: display,
                                label: None,
                                focused: false,
                                generation: 1,
                                position: new_pos,
                                is_fullscreen: false,
                                is_system: false,
                            });
                        }
                    }
                    Action::FocusSpace { space } => {
                        for s in snap.spaces.iter_mut() {
                            s.focused = s.id == *space;
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
        }
        let snapshot = Arc::new(std::sync::Mutex::new(PlatformSnapshot {
            windows: vec![],
            spaces: vec![SpaceSnapshot {
                id: SpaceId(1),
                display_id: DisplayId(1),
                label: None,
                focused: true,
                generation: 1,
                position: 0,
                is_fullscreen: false,
                is_system: false,
            }],
            displays: vec![DisplaySnapshot {
                id: DisplayId(1),
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1000.0,
                    height: 800.0,
                },
                label: None,
                focused: true,
                is_main: true,
                generation: 1,
            }],
            complete: true,
        }));
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut daemon = test_daemon();
        daemon.engine = Engine::new(Config {
            workspaces: vec![rovr_config::WorkspaceConfig {
                name: "1".into(),
                layout: rovr_types::LayoutKind::Bsp,
                display: None,
                persistent: true,
                plugin: None,
            }],
            ..Config::default()
        });
        daemon.config = daemon.engine.config.clone();
        daemon.platform = Box::new(SpawnFocusPlatform {
            snapshot: snapshot.clone(),
            executed: executed.clone(),
        });
        // Seed the engine's observed state from the platform snapshot so
        // creation_anchor can find a focused Space.
        let _ = daemon
            .engine
            .apply_event(Event::Snapshot(snapshot.lock().unwrap().clone()));

        let result = daemon.handle(Request::new(
            1,
            Command::Workspace(WorkspaceCommand::Focus { name: "2".into() }),
        ));
        let outcome_ok = matches!(result.response.outcome, ResponseOutcome::Ok { .. });
        if !outcome_ok {
            let err = match result.response.outcome {
                ResponseOutcome::Error { ref error } => {
                    format!("{}: {}", error.code, error.message)
                }
                _ => "non-ok".to_string(),
            };
            panic!("spawn + focus must succeed, got {err}");
        }

        let executed = executed.lock().unwrap().clone();
        let create_indexes: Vec<usize> = executed
            .iter()
            .enumerate()
            .filter_map(|(i, a)| matches!(a, Action::CreateSpace { .. }).then_some(i))
            .collect();
        assert_eq!(
            create_indexes.len(),
            1,
            "exactly one CreateSpace, got {executed:?}"
        );
        let focus_indexes: Vec<usize> = executed
            .iter()
            .enumerate()
            .filter_map(|(i, a)| matches!(a, Action::FocusSpace { .. }).then_some(i))
            .collect();
        assert_eq!(
            focus_indexes.len(),
            1,
            "exactly one FocusSpace must fire synchronously after the spawn, got {executed:?}"
        );
        assert!(
            focus_indexes[0] > create_indexes[0],
            "FocusSpace must come after CreateSpace: {executed:?}"
        );
    }

    /// The SA can appear after startup (install/reinjection). Capabilities
    /// must be re-probed every tick; the next reconcile cycle must emit the
    /// CreateSpace that was previously gated off for the missing persistent
    /// workspace (regression for the stale-caps half of c4a8c69).
    #[test]
    fn capabilities_refresh_at_runtime_unlocks_persistent_creation() {
        let config = Config {
            workspaces: vec![
                rovr_config::WorkspaceConfig {
                    name: "code".into(),
                    layout: rovr_types::LayoutKind::Bsp,
                    display: None,
                    persistent: true,
                    plugin: None,
                },
                rovr_config::WorkspaceConfig {
                    name: "chat".into(),
                    layout: rovr_types::LayoutKind::Bsp,
                    display: None,
                    persistent: true,
                    plugin: None,
                },
            ],
            ..Default::default()
        };
        // One space exists: "code" claims it, "chat" stays missing and needs
        // CreateSpace — exactly the situation gated by capabilities.
        let create_space = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut daemon = test_daemon();
        daemon.engine = Engine::new(config);
        daemon.config = daemon.engine.config.clone();
        daemon.platform = Box::new(MutableCapPlatform {
            create_space: create_space.clone(),
            snapshot: PlatformSnapshot {
                windows: vec![],
                spaces: vec![SpaceSnapshot {
                    id: SpaceId(11),
                    display_id: DisplayId(1),
                    label: None,
                    focused: true,
                    generation: 0,
                    position: 0,
                    is_fullscreen: false,
                    is_system: false,
                }],
                displays: vec![DisplaySnapshot {
                    id: DisplayId(1),
                    frame: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 1000.0,
                        height: 800.0,
                    },
                    label: None,
                    focused: true,
                    is_main: true,
                    generation: 0,
                }],
                complete: true,
            },
            executed: executed.clone(),
        });

        // Tick 1: create_space unavailable (pre-SA). No CreateSpace may be
        // requested even though a persistent workspace has no backing.
        daemon.refresh_observation();
        assert!(
            create_space_requests(executed.as_ref()).is_empty(),
            "gated capability must not produce CreateSpace"
        );
        assert_eq!(
            daemon.engine.workspaces.backing_for("code"),
            Some(SpaceId(11))
        );

        // SA install/reinjection happens out of band: create_space appears.
        create_space.store(true, std::sync::atomic::Ordering::Relaxed);

        // Tick 2: the refreshed capability must unlock exactly one CreateSpace
        // for the still-missing workspace, anchored on the existing space.
        daemon.refresh_observation();
        assert_eq!(
            create_space_requests(executed.as_ref()),
            vec![SpaceId(11)],
            "capability refresh must let the next reconcile emit the previously gated CreateSpace"
        );
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
                is_fullscreen: false,
                is_system: false,
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
                is_main: false,
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

    #[test]
    fn recoverable_platform_diagnostics_enter_flight_recorder_once() {
        let mut daemon = test_daemon();
        daemon.platform = Box::new(MockPlatform {
            diagnostics: vec![PlatformDiagnostic {
                kind: "ax.refine_timeout",
                detail: "pid=42 operation=refine error=platform worker timed out".to_string(),
            }],
            ..MockPlatform::default()
        });

        daemon.refresh_observation();
        daemon.refresh_observation();

        let records = daemon.engine.flight_recorder.snapshot();
        let matching: Vec<_> = records
            .iter()
            .filter(|record| record.kind == "ax.refine_timeout")
            .collect();
        assert_eq!(matching.len(), 1, "drained diagnostics must not replay");
        assert!(matching[0].detail.contains("pid=42"));
        assert!(matching[0].detail.contains("operation=refine"));
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
                        is_fullscreen: false,
                        is_system: false,
                    },
                    rovr_types::SpaceSnapshot {
                        id: SpaceId(7),
                        display_id: DisplayId(1),
                        label: None,
                        focused: false,
                        generation: 1,
                        position: 1,
                        is_fullscreen: false,
                        is_system: false,
                    },
                ],
                displays: vec![],
                complete: true,
            },
            executed: vec![],
            diagnostics: vec![],
        };
        let mut daemon = Daemon {
            space_history: std::cell::RefCell::new(HashMap::new()),
            refresh_wake: Arc::new(RefreshWake::default()),
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

        daemon.space_history.borrow_mut().insert(
            DisplayId(1),
            SpaceHistory {
                current: Some(SpaceId(3)),
                previous: Some(SpaceId(7)),
            },
        );

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
        assert_eq!(
            daemon
                .space_history
                .borrow()
                .get(&DisplayId(1))
                .and_then(|state| state.previous),
            Some(SpaceId(7))
        );

        // A tracked previous Space that no longer exists errors instead of
        // emitting an action against a ghost Space.
        daemon
            .space_history
            .borrow_mut()
            .get_mut(&DisplayId(1))
            .unwrap()
            .previous = Some(SpaceId(999));
        let result = daemon.handle(Request::new(3, Command::Space(SpaceCommand::FocusRecent)));
        assert!(matches!(
            result.response.outcome,
            ResponseOutcome::Error { .. }
        ));
    }

    #[test]
    fn focus_recent_is_scoped_to_the_active_display() {
        let mut daemon = test_daemon();
        daemon.platform = Box::new(MockPlatform {
            snapshot: PlatformSnapshot {
                windows: vec![],
                spaces: vec![
                    SpaceSnapshot {
                        id: SpaceId(11),
                        display_id: DisplayId(1),
                        label: None,
                        focused: true,
                        generation: 1,
                        position: 0,
                        is_fullscreen: false,
                        is_system: false,
                    },
                    SpaceSnapshot {
                        id: SpaceId(12),
                        display_id: DisplayId(1),
                        label: None,
                        focused: false,
                        generation: 1,
                        position: 1,
                        is_fullscreen: false,
                        is_system: false,
                    },
                    SpaceSnapshot {
                        id: SpaceId(21),
                        display_id: DisplayId(2),
                        label: None,
                        focused: true,
                        generation: 1,
                        position: 0,
                        is_fullscreen: false,
                        is_system: false,
                    },
                    SpaceSnapshot {
                        id: SpaceId(22),
                        display_id: DisplayId(2),
                        label: None,
                        focused: false,
                        generation: 1,
                        position: 1,
                        is_fullscreen: false,
                        is_system: false,
                    },
                ],
                displays: vec![
                    DisplaySnapshot {
                        id: DisplayId(1),
                        frame: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 1.0,
                            height: 1.0,
                        },
                        label: None,
                        focused: true,
                        is_main: true,
                        generation: 1,
                    },
                    DisplaySnapshot {
                        id: DisplayId(2),
                        frame: Rect {
                            x: 1.0,
                            y: 0.0,
                            width: 1.0,
                            height: 1.0,
                        },
                        label: None,
                        focused: false,
                        is_main: false,
                        generation: 1,
                    },
                ],
                complete: true,
            },
            executed: vec![],
            diagnostics: vec![],
        });
        daemon.refresh_observation();
        daemon.note_space_switched_to(SpaceId(12));
        daemon.note_space_switched_to(SpaceId(22));
        daemon.note_space_switched_to(SpaceId(12));

        let (_, target) = daemon.focus_recent_space().expect("display 1 history");
        assert_eq!(target, SpaceId(11));
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
