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
use rovr_core::{Action, Engine, Event};
#[cfg(target_os = "macos")]
use rovr_platform::MacPlatform;
#[cfg(not(target_os = "macos"))]
use rovr_platform::MockPlatform;
use rovr_platform::Platform;
use rovr_protocol::{
    Command, ConfigCommand, DebugCommand, QueryCommand, Request, Response, SpaceCommand,
    WindowCommand, PROTOCOL_VERSION,
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
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "rovr=info".into()))
        .init();

    let args = Args::parse();
    let _foreground = args.foreground;
    let socket_path = args.socket.unwrap_or_else(default_socket_path);
    let config_path = args.config.unwrap_or_else(default_config_path);
    let config = load_config_or_default(&config_path)?;

    let mut platform: Box<dyn Platform> = make_platform()?;
    let mut engine = Engine::new(config.clone());
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

    let (tx, rx) = mpsc::channel::<Envelope>();
    thread::spawn(move || state_loop(daemon, rx));

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let tx = tx.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, tx) {
                        error!(%err, "IPC client failed");
                    }
                });
            }
            Err(err) => error!(%err, "socket accept failed"),
        }
    }

    Ok(())
}

fn handle_client(mut stream: UnixStream, tx: Sender<Envelope>) -> Result<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let request: Request = serde_json::from_str(&line).context("decode request")?;
    let request_id = request.id;

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

fn state_loop(mut daemon: Daemon, rx: Receiver<Envelope>) {
    loop {
        let interval = Duration::from_millis(daemon.config.general.reconcile_interval_ms.max(100));
        match rx.recv_timeout(interval) {
            Ok(envelope) => {
                let response = daemon.handle(envelope.request);
                let _ = envelope.response.send(response);
            }
            Err(RecvTimeoutError::Timeout) => daemon.refresh_observation(),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
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
