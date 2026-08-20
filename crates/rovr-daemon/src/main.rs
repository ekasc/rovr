use std::sync::{Arc, Mutex};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use rovr_config::Config;
use rovr_core::{layout_state::Axis, Action, Engine, Event};
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

    let subscribers: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));
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
    subscribers: Arc<Mutex<Vec<UnixStream>>>,
) -> Result<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let request: Request = serde_json::from_str(&line).context("decode request")?;
    let request_id = request.id;

    if matches!(request.command, Command::Subscribe) {
        let ack = Response::ok(request_id, json!({ "subscribed": true }));
        serde_json::to_writer(&mut stream, &ack)?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        if let Ok(mut subs) = subscribers.lock() {
            subs.push(stream.try_clone()?);
        }
        broadcast_notification(
            &subscribers,
            &Notification::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        );
        // Keep the connection open. The client sends nothing further, so an EOF
        // (or read error) means it disconnected. The registry clone is pruned
        // lazily by broadcast_notification on the next failed write.
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut buf = String::new();
        loop {
            match reader.read_line(&mut buf) {
                Ok(0) => break,
                Ok(_) => buf.clear(),
                Err(_) => break,
            }
        }
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
    subscribers: Arc<Mutex<Vec<UnixStream>>>,
) {
    loop {
        let interval = Duration::from_millis(daemon.config.general.reconcile_interval_ms.max(100));
        match rx.recv_timeout(interval) {
            Ok(envelope) => {
                let command = envelope.request.command.clone();
                let response = daemon.handle(envelope.request);
                let _ = envelope.response.send(response);
                broadcast_for_command(&daemon, &subscribers, &command);
            }
            Err(RecvTimeoutError::Timeout) => {
                daemon.refresh_observation();
                broadcast_notification(
                    &subscribers,
                    &Notification::StateChanged {
                        generation: daemon.engine.observed.generation,
                    },
                );
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn broadcast_notification(subscribers: &Arc<Mutex<Vec<UnixStream>>>, notification: &Notification) {
    let mut subs = match subscribers.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let mut dead = Vec::new();
    for (i, stream) in subs.iter_mut().enumerate() {
        let payload = match serde_json::to_string(notification) {
            Ok(mut s) => {
                s.push('\n');
                s
            }
            Err(_) => {
                dead.push(i);
                continue;
            }
        };
        if stream
            .write_all(payload.as_bytes())
            .and_then(|_| stream.flush())
            .is_err()
        {
            dead.push(i);
        }
    }
    for i in dead.into_iter().rev() {
        subs.remove(i);
    }
}

fn broadcast_for_command(
    daemon: &Daemon,
    subscribers: &Arc<Mutex<Vec<UnixStream>>>,
    command: &Command,
) {
    let notification = match command {
        Command::Layout(LayoutCommand::Rotate { space })
        | Command::Layout(LayoutCommand::Mirror { space }) => {
            let (horizontal, reversed) = daemon
                .engine
                .layouts
                .get(space)
                .map(|state| {
                    (
                        state.orientation.axis == Axis::Horizontal,
                        state.orientation.reversed,
                    )
                })
                .unwrap_or((false, false));
            Notification::LayoutChanged {
                space: *space,
                horizontal,
                reversed,
            }
        }
        Command::Scratchpad(ScratchpadCommand::Toggle { name }) => {
            Notification::ScratchpadToggled {
                name: name.clone(),
                open: daemon.engine.scratchpads.is_open(name),
            }
        }
        Command::Config(ConfigCommand::Reload { .. }) => Notification::ConfigReloaded,
        _ => Notification::StateChanged {
            generation: daemon.engine.observed.generation,
        },
    };
    broadcast_notification(subscribers, &notification);
}

impl Daemon {
    fn handle(&mut self, request: Request) -> Response {
        if request.version != PROTOCOL_VERSION {
            return Response::error(
                request.id,
                "PROTOCOL_VERSION_MISMATCH",
                format!(
                    "client protocol {} is incompatible with daemon protocol {}",
                    request.version, PROTOCOL_VERSION
                ),
            );
        }

        let id = request.id;
        match request.command {
            Command::Ping => Response::ok(id, json!({ "pong": true })),
            Command::Doctor => Response::ok(
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
                    Response::ok(id, windows)
                }
                QueryCommand::Spaces => {
                    let mut spaces: Vec<_> =
                        self.engine.observed.spaces.values().cloned().collect();
                    spaces.sort_by_key(|space| (space.position, space.id));
                    Response::ok(id, spaces)
                }
                QueryCommand::Displays => {
                    let mut displays: Vec<_> =
                        self.engine.observed.displays.values().cloned().collect();
                    displays.sort_by_key(|display| display.id);
                    Response::ok(id, displays)
                }
                QueryCommand::State => Response::ok(
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
                    Response::ok(id, focused)
                }
                QueryCommand::Current => {
                    let id_val = self
                        .engine
                        .observed
                        .windows
                        .values()
                        .find(|w| w.focused)
                        .map(|w| w.id.0);
                    Response::ok(id, json!({ "id": id_val }))
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
                        Ok(()) => Response::ok(id, json!({ "accepted": true })),
                        Err(err) => Response::error(id, "PLATFORM_ERROR", err.to_string()),
                    },
                    Err(err) => Response::error(id, "ENGINE_ERROR", err.to_string()),
                }
            }
            Command::Layout(command) => {
                match command {
                    LayoutCommand::Rotate { space } => self.engine.rotate_layout(space),
                    LayoutCommand::Mirror { space } => self.engine.mirror_layout(space),
                }
                self.persist_state();
                match self.platform.snapshot() {
                    Ok(snapshot) => {
                        let actions = self.engine.apply_event(Event::Snapshot(snapshot));
                        match self.execute_and_refresh(actions) {
                            Ok(()) => Response::ok(id, json!({ "accepted": true })),
                            Err(err) => Response::error(id, "PLATFORM_ERROR", err.to_string()),
                        }
                    }
                    Err(err) => Response::error(id, "SNAPSHOT_ERROR", err.to_string()),
                }
            }
            Command::Scratchpad(command) => {
                match command {
                    ScratchpadCommand::Toggle { name } => self.engine.toggle_scratchpad(&name),
                }
                self.persist_state();
                match self.platform.snapshot() {
                    Ok(snapshot) => {
                        let actions = self.engine.apply_event(Event::Snapshot(snapshot));
                        match self.execute_and_refresh(actions) {
                            Ok(()) => Response::ok(id, json!({ "accepted": true })),
                            Err(err) => Response::error(id, "PLATFORM_ERROR", err.to_string()),
                        }
                    }
                    Err(err) => Response::error(id, "SNAPSHOT_ERROR", err.to_string()),
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
                        Ok(()) => Response::ok(id, json!({ "accepted": true })),
                        Err(err) => Response::error(id, "PLATFORM_ERROR", err.to_string()),
                    },
                    Err(err) => Response::error(id, "ENGINE_ERROR", err.to_string()),
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
                            Response::ok(id, json!({ "reloaded": true }))
                        }
                        Err(err) => Response::error(id, "CONFIG_ERROR", err.to_string()),
                    }
                }
                ConfigCommand::Check { path } => match Config::load(&path) {
                    Ok(_) => Response::ok(id, json!({ "valid": true })),
                    Err(err) => Response::error(id, "CONFIG_ERROR", err.to_string()),
                },
            },
            Command::Debug(DebugCommand::Events) => {
                Response::ok(id, self.engine.flight_recorder.snapshot())
            }
            Command::Subscribe => Response::error(
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
    use std::io::BufRead;

    /// M4b: broadcast_notification writes a newline-delimited JSON notification
    /// to every registered subscriber stream.
    #[test]
    fn m4b_broadcast_writes_notification_to_subscribers() {
        let (writer, reader) = UnixStream::pair().expect("socket pair");
        let subscribers: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(vec![writer]));

        broadcast_notification(
            &subscribers,
            &Notification::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        );
        // Drop the registry so the clone held inside broadcast is released; the
        // reader end stays open for us to read the written line.
        drop(subscribers);

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read broadcast line");
        let got: Notification =
            serde_json::from_str(line.trim()).expect("parse broadcast notification");
        assert_eq!(
            got,
            Notification::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        );
    }
}
