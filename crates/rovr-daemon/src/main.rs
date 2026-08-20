use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use rovr_config::Config;
use rovr_core::{Action, Engine, Event};
#[cfg(target_os = "macos")]
use rovr_platform::MacPlatform;
#[cfg(not(target_os = "macos"))]
use rovr_platform::MockPlatform;
use rovr_platform::Platform;
use rovr_protocol::{
    Command, ConfigCommand, DebugCommand, LayoutCommand, Notification, QueryCommand, Request,
    Response, ScratchpadCommand, SpaceCommand, WindowCommand, PROTOCOL_VERSION,
};
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
}

struct Daemon {
    engine: Engine,
    platform: Box<dyn Platform>,
    config: Config,
    config_path: PathBuf,
    state_path: PathBuf,
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
    let mut engine = Engine::new(config.clone());
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
    };

    run_socket_server(socket_path, daemon)
}

#[cfg(target_os = "macos")]
fn make_platform() -> Result<Box<dyn Platform>> {
    Ok(Box::new(MacPlatform::new()?))
}

#[cfg(not(target_os = "macos"))]
fn make_platform() -> Result<Box<dyn Platform>> {
    Ok(Box::new(MockPlatform::default()))
}

fn run_socket_server(path: PathBuf, daemon: Daemon) -> Result<()> {
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("remove stale socket {}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("bind rovr socket at {}", path.display()))?;
    info!(socket = %path.display(), "rovr daemon listening");

    let subscribers: Arc<Mutex<Vec<SyncSender<Notification>>>> = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::channel::<Envelope>();
    let subs_for_loop = subscribers.clone();
    thread::spawn(move || state_loop(daemon, rx, subs_for_loop));

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let tx = tx.clone();
                let subs = subscribers.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, tx, subs) {
                        error!(%err, "IPC client failed");
                    }
                });
            }
            Err(err) => error!(%err, "socket accept failed"),
        }
    }

    Ok(())
}

fn handle_client(
    mut stream: UnixStream,
    tx: Sender<Envelope>,
    subscribers: Arc<Mutex<Vec<SyncSender<Notification>>>>,
) -> Result<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let request: Request = serde_json::from_str(&line).context("decode request")?;
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

        register_subscriber(&subscribers, tx);
        return Ok(());
    }

    let (response_tx, response_rx) = mpsc::channel();
    tx.send(Envelope {
        request,
        response: response_tx,
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
    loop {
        let interval = Duration::from_millis(daemon.config.general.reconcile_interval_ms.max(100));
        match rx.recv_timeout(interval) {
            Ok(envelope) => {
                let result = daemon.handle(envelope.request);
                let _ = envelope.response.send(result.response);
                for notif in &result.notifications {
                    deliver_notification(&subscribers, notif);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                daemon.refresh_observation();
                deliver_notification(&subscribers, &Notification::StateChanged);
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
        Err(_) => return,
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
    if let Ok(mut subs) = subscribers.lock() {
        subs.push(tx);
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
            Command::Doctor => HandleResult::ok(
                id,
                json!({
                    "protocol": PROTOCOL_VERSION,
                    "capabilities": self.platform.capabilities(),
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
            ),
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
                };
                match result {
                    Ok(actions) => match self.execute_and_refresh(actions) {
                        Ok(()) => HandleResult::ok(id, json!({ "accepted": true })),
                        Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string()),
                    },
                    Err(err) => HandleResult::err(id, "ENGINE_ERROR", err.to_string()),
                }
            }
            Command::Layout(command) => {
                let space = match command {
                    LayoutCommand::Rotate { space } => {
                        self.engine.rotate_layout(space);
                        space
                    }
                    LayoutCommand::Mirror { space } => {
                        self.engine.mirror_layout(space);
                        space
                    }
                };
                self.persist_state();
                match self.platform.snapshot() {
                    Ok(snapshot) => {
                        let actions = self.engine.apply_event(Event::Snapshot(snapshot));
                        match self.execute_and_refresh(actions) {
                            Ok(()) => {
                                let (horizontal, reversed) = self
                                    .engine
                                    .layout_orientation(space)
                                    .unwrap_or((false, false));
                                HandleResult::ok(id, json!({ "accepted": true }))
                                    .with_notifications(vec![Notification::LayoutChanged {
                                        space,
                                        horizontal,
                                        reversed,
                                    }])
                            }
                            Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string()),
                        }
                    }
                    Err(err) => HandleResult::err(id, "SNAPSHOT_ERROR", err.to_string()),
                }
            }
            Command::Scratchpad(command) => {
                let name = match command {
                    ScratchpadCommand::Toggle { name } => {
                        self.engine.toggle_scratchpad(&name);
                        name
                    }
                };
                self.persist_state();
                match self.platform.snapshot() {
                    Ok(snapshot) => {
                        let actions = self.engine.apply_event(Event::Snapshot(snapshot));
                        match self.execute_and_refresh(actions) {
                            Ok(()) => {
                                let open = self.engine.scratchpads.is_open(&name);
                                HandleResult::ok(id, json!({ "accepted": true }))
                                    .with_notifications(vec![Notification::ScratchpadToggled {
                                        name,
                                        open,
                                    }])
                            }
                            Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string()),
                        }
                    }
                    Err(err) => HandleResult::err(id, "SNAPSHOT_ERROR", err.to_string()),
                }
            }
            Command::Space(command) => {
                let result = match command {
                    SpaceCommand::Focus { space } => self.engine.focus_space(space),
                    SpaceCommand::Create { anchor } => self.engine.create_space(anchor),
                    SpaceCommand::Destroy { space } => self.engine.destroy_space(space),
                    SpaceCommand::Move { space, after } => self.engine.move_space(space, after),
                };
                match result {
                    Ok(actions) => match self.execute_and_refresh(actions) {
                        Ok(()) => HandleResult::ok(id, json!({ "accepted": true })),
                        Err(err) => HandleResult::err(id, "PLATFORM_ERROR", err.to_string()),
                    },
                    Err(err) => HandleResult::err(id, "ENGINE_ERROR", err.to_string()),
                }
            }
            Command::Config(command) => match command {
                ConfigCommand::Reload { path } => {
                    let path = path
                        .map(PathBuf::from)
                        .unwrap_or_else(|| self.config_path.clone());
                    match Config::load(&path) {
                        Ok(config) => {
                            self.engine.config = config.clone();
                            self.config = config;
                            self.config_path = path;
                            HandleResult::ok(id, json!({ "reloaded": true }))
                                .with_notifications(vec![Notification::ConfigReloaded])
                        }
                        Err(err) => HandleResult::err(id, "CONFIG_ERROR", err.to_string()),
                    }
                }
                ConfigCommand::Check { path } => match Config::load(&path) {
                    Ok(_) => HandleResult::ok(id, json!({ "valid": true })),
                    Err(err) => HandleResult::err(id, "CONFIG_ERROR", err.to_string()),
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

    fn refresh_observation(&mut self) {
        // Event-driven: display topology callback sets the flag on reconfiguration.
        if self.platform.needs_refresh() {
            self.engine.observed.bump_generation();
            self.engine
                .flight_recorder
                .record("display.topology_changed", "callback-triggered refresh");
        }

        match self.platform.snapshot() {
            Ok(snapshot) => {
                let actions = self.engine.apply_event(Event::Snapshot(snapshot));
                if let Err(err) = execute_actions_result(&mut *self.platform, actions) {
                    self.engine
                        .flight_recorder
                        .record("platform.error", err.to_string());
                    warn!(%err, "periodic reconciliation failed");
                }
            }
            Err(err) => {
                self.engine
                    .flight_recorder
                    .record("snapshot.error", err.to_string());
                warn!(%err, "periodic snapshot failed");
            }
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

fn default_socket_path() -> PathBuf {
    let uid = std::env::var("UID").unwrap_or_else(|_| "unknown".into());
    PathBuf::from(format!("/tmp/rovr-{uid}.sock"))
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
    use rovr_types::WindowId;
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
            engine: Engine::new(Config::default()),
            platform: Box::new(MockPlatform::default()),
            config: Config::default(),
            config_path: PathBuf::from("/dev/null/rovr-test-config.toml"),
            state_path: PathBuf::from("/dev/null/rovr-test-state.json"),
        }
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
}
