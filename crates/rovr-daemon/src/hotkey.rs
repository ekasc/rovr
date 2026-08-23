//! Built-in global hotkey backend (macOS).
//!
//! Architecture (blocker 6): the macOS global-hotkey backend installs a
//! Carbon event handler on `GetApplicationEventTarget()` and a CGEvent tap on
//! the MAIN run loop. Both are only serviced while the main thread runs an
//! AppKit/CFRunLoop event loop — so the daemon's MAIN thread runs that event
//! loop (`run_appkit_event_loop`), while socket accept and state processing
//! run on worker threads. The manager is created on the main thread.
//!
//! This component contains no window-management policy: it consumes parsed
//! key chords from the shared protocol seam and maps commands via the ONE parser
//! (`rovr_protocol::command_parser`), and dispatches them over public IPC to
//! the daemon's own socket. An invalid command is logged and executes NOTHING
//! (blocker 8) — it is never substituted with another command; invalid binds
//! are already rejected at config load/reload time.
#![allow(clippy::question_mark)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::Result;
use rovr_config::Config;
use tracing::{error, info, warn};

#[cfg(target_os = "macos")]
use {
    global_hotkey::{
        hotkey::{Code, HotKey, Modifiers},
        GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    },
    rovr_protocol::{
        command_parser::parse_command,
        hotkey::{parse_hotkey, KeyChord, KeyCode},
        Request,
    },
    std::ffi::c_void,
    std::io::{Read, Write},
    std::os::unix::net::UnixStream,
    std::sync::{mpsc, Arc, Mutex, OnceLock, RwLock},
};

#[cfg(target_os = "macos")]
struct HotkeyRuntime {
    manager: GlobalHotKeyManager,
    hotkeys: Vec<HotKey>,
    commands: Arc<RwLock<std::collections::HashMap<u32, String>>>,
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
pub struct HotkeyHandle {
    #[allow(dead_code)]
    runtime: Arc<Mutex<HotkeyRuntime>>,
}

#[cfg(target_os = "macos")]
static HOTKEY_RUNTIME: OnceLock<Arc<Mutex<HotkeyRuntime>>> = OnceLock::new();

#[cfg(target_os = "macos")]
extern "C" {
    static mut _dispatch_main_q: c_void;
    fn dispatch_async_f(queue: *mut c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
}

/// Create and register the global hotkey manager. MUST be called on the main
/// thread (Carbon event target + main-run-loop event tap). Returns the manager
/// which the caller must keep alive for the daemon's lifetime. If no binds or
/// on non-macOS, returns None and does nothing (skhd remains supported).
#[cfg(target_os = "macos")]
pub fn create_hotkey_manager(config: Config, socket_path: PathBuf) -> Option<HotkeyHandle> {
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(error) => {
            warn!(%error, "hotkey: failed to create manager, built-in hotkey disabled");
            return None;
        }
    };
    let commands = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let runtime = Arc::new(Mutex::new(HotkeyRuntime {
        manager,
        hotkeys: Vec::new(),
        commands: commands.clone(),
    }));
    if let Err(error) = apply_config(&runtime, &config) {
        warn!(%error, "hotkey: initial registration failed");
    }
    let _ = HOTKEY_RUNTIME.set(runtime.clone());

    let (dispatch_tx, dispatch_rx) = mpsc::sync_channel::<(String, rovr_protocol::Command)>(64);
    let dispatch_socket = socket_path.clone();
    std::thread::spawn(move || {
        while let Ok((cmd_str, command)) = dispatch_rx.recv() {
            if let Err(error) = dispatch_via_ipc(&dispatch_socket, command) {
                warn!(%error, cmd=%cmd_str, "hotkey: dispatch failed");
            }
        }
    });

    std::thread::spawn(move || {
        let receiver = GlobalHotKeyEvent::receiver();
        while let Ok(event) = receiver.recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            let command = commands
                .read()
                .ok()
                .and_then(|commands| commands.get(&event.id).cloned());
            let Some(cmd_str) = command else { continue };
            match parse_command(&cmd_str) {
                Ok(command) => {
                    if dispatch_tx.try_send((cmd_str.clone(), command)).is_err() {
                        warn!(cmd=%cmd_str, "hotkey: dispatch queue full; dropping press");
                    }
                }
                Err(parse_err) => {
                    error!(cmd=%cmd_str, reason=%parse_err, "hotkey: invalid bind command — executing nothing");
                }
            }
        }
    });
    Some(HotkeyHandle { runtime })
}

#[cfg(target_os = "macos")]
fn candidate_bindings(
    config: &Config,
) -> Result<(Vec<HotKey>, std::collections::HashMap<u32, String>), String> {
    let mut hotkeys = Vec::new();
    let mut commands = std::collections::HashMap::new();
    for bind in &config.binds {
        let chord = parse_hotkey(&bind.key).map_err(|error| error.to_string())?;
        let hotkey = to_global_hotkey(chord);
        commands.insert(hotkey.id(), bind.command.clone());
        hotkeys.push(hotkey);
    }
    Ok((hotkeys, commands))
}

#[cfg(target_os = "macos")]
fn apply_config(runtime: &Arc<Mutex<HotkeyRuntime>>, config: &Config) -> Result<(), String> {
    let (new_hotkeys, new_commands) = candidate_bindings(config)?;
    let mut runtime = runtime.lock().map_err(|_| "hotkey lock poisoned")?;
    let old_hotkeys = runtime.hotkeys.clone();
    runtime
        .manager
        .unregister_all(&old_hotkeys)
        .map_err(|error| error.to_string())?;
    if let Err(error) = runtime.manager.register_all(&new_hotkeys) {
        let _ = runtime.manager.unregister_all(&new_hotkeys);
        let _ = runtime.manager.register_all(&old_hotkeys);
        return Err(error.to_string());
    }
    runtime.hotkeys = new_hotkeys;
    *runtime
        .commands
        .write()
        .map_err(|_| "hotkey command lock poisoned")? = new_commands;
    info!(
        binds = runtime.hotkeys.len(),
        "hotkey: registrations updated"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReloadClaim {
    Claimed,
    Cancelled,
}

struct ReloadGate(AtomicU8);

impl ReloadGate {
    const PENDING: u8 = 0;
    const CLAIMED: u8 = 1;
    const CANCELLED: u8 = 2;

    fn new() -> Self {
        Self(AtomicU8::new(Self::PENDING))
    }

    fn claim(&self) -> ReloadClaim {
        match self.0.compare_exchange(
            Self::PENDING,
            Self::CLAIMED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => ReloadClaim::Claimed,
            Err(_) => ReloadClaim::Cancelled,
        }
    }

    fn cancel(&self) -> ReloadClaim {
        match self.0.compare_exchange(
            Self::PENDING,
            Self::CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => ReloadClaim::Cancelled,
            Err(_) => ReloadClaim::Claimed,
        }
    }
}

#[cfg(target_os = "macos")]
struct ReloadContext {
    runtime: Arc<Mutex<HotkeyRuntime>>,
    config: Config,
    result: std::sync::mpsc::SyncSender<Result<(), String>>,
    gate: Arc<ReloadGate>,
}

#[cfg(target_os = "macos")]
extern "C" fn apply_config_on_main(context: *mut c_void) {
    let context = unsafe { Box::from_raw(context.cast::<ReloadContext>()) };
    let result = match context.gate.claim() {
        ReloadClaim::Claimed => apply_config(&context.runtime, &context.config),
        ReloadClaim::Cancelled => Err("hotkey reload cancelled before application".into()),
    };
    let _ = context.result.send(result);
}

#[cfg(target_os = "macos")]
pub fn reload(config: &Config) -> Result<()> {
    let Some(runtime) = HOTKEY_RUNTIME.get() else {
        return Ok(());
    };
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let gate = Arc::new(ReloadGate::new());
    let context = Box::new(ReloadContext {
        runtime: runtime.clone(),
        config: config.clone(),
        result: tx,
        gate: gate.clone(),
    });
    unsafe {
        dispatch_async_f(
            std::ptr::addr_of_mut!(_dispatch_main_q).cast(),
            Box::into_raw(context).cast(),
            apply_config_on_main,
        );
    }
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(result) => result.map_err(anyhow::Error::msg),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            match gate.cancel() {
                ReloadClaim::Cancelled => {
                    Err(anyhow::anyhow!("timed out updating main-thread hotkeys"))
                }
                // The main-thread callback owns the transaction. Wait for its
                // actual result rather than returning a timeout it can commit
                // after the caller has already observed an error.
                ReloadClaim::Claimed => rx
                    .recv()
                    .map_err(|_| anyhow::anyhow!("main-thread hotkey reload stopped"))?
                    .map_err(anyhow::Error::msg),
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err(anyhow::anyhow!("main-thread hotkey reload stopped"))
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn create_hotkey_manager(_config: Config, _socket_path: PathBuf) -> Option<()> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn reload(_config: &Config) -> Result<()> {
    Ok(())
}

/// Run the AppKit application event loop on the MAIN thread. This pumps the
/// Carbon/CGEventTap machinery global-hotkey installed, so registered hotkeys
/// actually fire. Never returns under normal operation.
#[cfg(target_os = "macos")]
pub fn run_appkit_event_loop(manager: Option<HotkeyHandle>) -> ! {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    // Keep the manager alive for the lifetime of the daemon; dropping it
    // unregisters the hotkeys.
    let _manager = manager;
    let mtm =
        MainThreadMarker::new().expect("run_appkit_event_loop must be called on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    info!("main: running AppKit event loop for global hotkeys");
    app.run();
    unreachable!("NSApplication::run returned");
}

#[cfg(not(target_os = "macos"))]
pub fn run_appkit_event_loop(_manager: Option<()>) -> ! {
    unreachable!("non-macOS builds never run the AppKit loop")
}

#[cfg(target_os = "macos")]
fn dispatch_via_ipc(socket_path: &PathBuf, command: rovr_protocol::Command) -> Result<()> {
    let req = Request::new(1, command);
    let mut stream = UnixStream::connect(socket_path)?;
    let deadline = std::time::Duration::from_secs(5);
    stream.set_read_timeout(Some(deadline))?;
    stream.set_write_timeout(Some(deadline))?;
    serde_json::to_writer(&mut stream, &req)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut response = Vec::new();
    (&mut stream)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut response)?;
    if response.len() > 1024 * 1024 {
        anyhow::bail!("hotkey IPC response too large");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn to_global_hotkey(chord: KeyChord) -> HotKey {
    let mut modifiers = Modifiers::empty();
    if chord.modifiers.command {
        modifiers |= Modifiers::SUPER;
    }
    if chord.modifiers.alt {
        modifiers |= Modifiers::ALT;
    }
    if chord.modifiers.shift {
        modifiers |= Modifiers::SHIFT;
    }
    if chord.modifiers.control {
        modifiers |= Modifiers::CONTROL;
    }
    HotKey::new(Some(modifiers), to_global_code(chord.key))
}

#[cfg(target_os = "macos")]
fn to_global_code(code: KeyCode) -> Code {
    match code {
        KeyCode::A => Code::KeyA,
        KeyCode::B => Code::KeyB,
        KeyCode::C => Code::KeyC,
        KeyCode::D => Code::KeyD,
        KeyCode::E => Code::KeyE,
        KeyCode::F => Code::KeyF,
        KeyCode::G => Code::KeyG,
        KeyCode::H => Code::KeyH,
        KeyCode::I => Code::KeyI,
        KeyCode::J => Code::KeyJ,
        KeyCode::K => Code::KeyK,
        KeyCode::L => Code::KeyL,
        KeyCode::M => Code::KeyM,
        KeyCode::N => Code::KeyN,
        KeyCode::O => Code::KeyO,
        KeyCode::P => Code::KeyP,
        KeyCode::Q => Code::KeyQ,
        KeyCode::R => Code::KeyR,
        KeyCode::S => Code::KeyS,
        KeyCode::T => Code::KeyT,
        KeyCode::U => Code::KeyU,
        KeyCode::V => Code::KeyV,
        KeyCode::W => Code::KeyW,
        KeyCode::X => Code::KeyX,
        KeyCode::Y => Code::KeyY,
        KeyCode::Z => Code::KeyZ,
        KeyCode::Digit0 => Code::Digit0,
        KeyCode::Digit1 => Code::Digit1,
        KeyCode::Digit2 => Code::Digit2,
        KeyCode::Digit3 => Code::Digit3,
        KeyCode::Digit4 => Code::Digit4,
        KeyCode::Digit5 => Code::Digit5,
        KeyCode::Digit6 => Code::Digit6,
        KeyCode::Digit7 => Code::Digit7,
        KeyCode::Digit8 => Code::Digit8,
        KeyCode::Digit9 => Code::Digit9,
        KeyCode::Enter => Code::Enter,
        KeyCode::Tab => Code::Tab,
        KeyCode::Space => Code::Space,
        KeyCode::Escape => Code::Escape,
        KeyCode::Left => Code::ArrowLeft,
        KeyCode::Right => Code::ArrowRight,
        KeyCode::Up => Code::ArrowUp,
        KeyCode::Down => Code::ArrowDown,
        KeyCode::F1 => Code::F1,
        KeyCode::F2 => Code::F2,
        KeyCode::F3 => Code::F3,
        KeyCode::F4 => Code::F4,
        KeyCode::F5 => Code::F5,
        KeyCode::F6 => Code::F6,
        KeyCode::F7 => Code::F7,
        KeyCode::F8 => Code::F8,
        KeyCode::F9 => Code::F9,
        KeyCode::F10 => Code::F10,
        KeyCode::F11 => Code::F11,
        KeyCode::F12 => Code::F12,
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    #[test]
    fn cancel_before_claim_prevents_application() {
        let gate = ReloadGate::new();
        assert_eq!(gate.cancel(), ReloadClaim::Cancelled);
        assert_eq!(gate.claim(), ReloadClaim::Cancelled);
    }

    #[test]
    fn claim_before_timeout_wins_and_must_be_waited_for() {
        let gate = ReloadGate::new();
        assert_eq!(gate.claim(), ReloadClaim::Claimed);
        assert_eq!(gate.cancel(), ReloadClaim::Claimed);
    }

    #[test]
    fn late_callback_is_a_no_op_after_timeout_cancellation() {
        let gate = ReloadGate::new();
        assert_eq!(gate.cancel(), ReloadClaim::Cancelled);
        assert_eq!(gate.claim(), ReloadClaim::Cancelled);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn reloaded_binding_map_drops_removed_commands_and_adds_new_ones() {
        let old = Config {
            binds: vec![rovr_config::KeybindConfig {
                key: "alt - h".into(),
                command: "window focus-direction west".into(),
            }],
            ..Default::default()
        };
        let new = Config {
            binds: vec![rovr_config::KeybindConfig {
                key: "alt - l".into(),
                command: "window focus-direction east".into(),
            }],
            ..Default::default()
        };
        let (old_keys, old_commands) = candidate_bindings(&old).unwrap();
        let (new_keys, new_commands) = candidate_bindings(&new).unwrap();
        assert!(!new_commands.contains_key(&old_keys[0].id()));
        assert_eq!(
            new_commands.get(&new_keys[0].id()).map(String::as_str),
            Some("window focus-direction east")
        );
        assert_ne!(old_commands, new_commands);
    }
}
