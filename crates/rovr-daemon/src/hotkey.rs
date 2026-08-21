//! Built-in global hotkey backend (macOS).
//!
//! Architecture (blocker 6): the macOS global-hotkey backend installs a
//! Carbon event handler on `GetApplicationEventTarget()` and a CGEvent tap on
//! the MAIN run loop. Both are only serviced while the main thread runs an
//! AppKit/CFRunLoop event loop — so the daemon's MAIN thread runs that event
//! loop (`run_appkit_event_loop`), while socket accept and state processing
//! run on worker threads. The manager is created on the main thread.
//!
//! This component contains no window-management policy: it parses key strings,
//! maps them to commands via the ONE shared parser
//! (`rovr_protocol::command_parser`), and dispatches them over public IPC to
//! the daemon's own socket. An invalid command is logged and executes NOTHING
//! (blocker 8) — it is never substituted with another command; invalid binds
//! are already rejected at config load/reload time.
#![allow(clippy::question_mark)]
use std::path::PathBuf;

use anyhow::Result;
use rovr_config::Config;
use tracing::{error, info, warn};

#[cfg(target_os = "macos")]
use {
    global_hotkey::{
        hotkey::{Code, HotKey, Modifiers},
        GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    },
    rovr_protocol::{command_parser::parse_command, Request},
    std::io::Write,
    std::os::unix::net::UnixStream,
};

/// Create and register the global hotkey manager. MUST be called on the main
/// thread (Carbon event target + main-run-loop event tap). Returns the manager
/// which the caller must keep alive for the daemon's lifetime. If no binds or
/// on non-macOS, returns None and does nothing (skhd remains supported).
#[cfg(target_os = "macos")]
pub fn create_hotkey_manager(config: Config, socket_path: PathBuf) -> Option<GlobalHotKeyManager> {
    if config.binds.is_empty() {
        info!("hotkey: no [[bind]] entries, built-in hotkey disabled");
        return None;
    }
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            warn!(%e, "hotkey: failed to create manager, built-in hotkey disabled");
            return None;
        }
    };
    let mut id_to_command: std::collections::HashMap<u32, String> =
        std::collections::HashMap::new();
    for bind in &config.binds {
        match parse_skhd_hotkey(&bind.key) {
            Some(hotkey) => {
                let id = hotkey.id();
                match manager.register(hotkey) {
                    Ok(()) => {
                        info!(key=%bind.key, command=%bind.command, id, "hotkey: registered");
                        id_to_command.insert(id, bind.command.clone());
                    }
                    Err(e) => warn!(key=%bind.key, %e, "hotkey: register failed"),
                }
            }
            None => warn!(key=%bind.key, "hotkey: failed to parse skhd key, skipping"),
        }
    }
    if id_to_command.is_empty() {
        return Some(manager);
    }
    let socket = socket_path.clone();
    std::thread::spawn(move || {
        let receiver = GlobalHotKeyEvent::receiver();
        info!("hotkey: listener started ({} binds)", id_to_command.len());
        while let Ok(event) = receiver.recv() {
            if event.state == HotKeyState::Pressed {
                if let Some(cmd_str) = id_to_command.get(&event.id) {
                    info!(id=event.id, cmd=%cmd_str, "hotkey: triggered");
                    // Blocker 7+8: parse with the ONE shared parser. An
                    // invalid command logs an error and executes NOTHING —
                    // never a substitute command like Ping.
                    match parse_command(cmd_str) {
                        Ok(command) => {
                            if let Err(e) = dispatch_via_ipc(&socket, command) {
                                warn!(%e, cmd=%cmd_str, "hotkey: dispatch failed");
                            }
                        }
                        Err(parse_err) => {
                            error!(cmd=%cmd_str, reason=%parse_err, "hotkey: invalid bind command — executing nothing");
                        }
                    }
                }
            }
        }
    });
    Some(manager)
}

#[cfg(not(target_os = "macos"))]
pub fn create_hotkey_manager(_config: Config, _socket_path: PathBuf) -> Option<()> {
    None
}

/// Run the AppKit application event loop on the MAIN thread. This pumps the
/// Carbon/CGEventTap machinery global-hotkey installed, so registered hotkeys
/// actually fire. Never returns under normal operation.
#[cfg(target_os = "macos")]
pub fn run_appkit_event_loop(manager: Option<GlobalHotKeyManager>) -> ! {
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
    serde_json::to_writer(&mut stream, &req)?;
    stream.write_all(b"\n")?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn parse_skhd_hotkey(s: &str) -> Option<HotKey> {
    // skhd syntax: "cmd - h", "alt + shift - r", "cmd + shift - 1"
    // Split on " - " (space dash space) to separate modifiers+key
    let s = s.trim();
    let (mods_part, key_part) = if let Some(idx) = s.find(" - ") {
        let (a, b) = s.split_at(idx);
        (a, b[3..].trim())
    } else if let Some(idx) = s.find('-') {
        // fallback: "cmd-h"
        let (a, b) = s.split_at(idx);
        (a, b[1..].trim())
    } else {
        return None;
    };
    let mut mods = Modifiers::empty();
    if !mods_part.trim().is_empty() {
        for m in mods_part.split('+') {
            match m.trim().to_lowercase().as_str() {
                "cmd" | "command" | "super" | "meta" => mods |= Modifiers::SUPER,
                "alt" | "option" | "opt" => mods |= Modifiers::ALT,
                "shift" => mods |= Modifiers::SHIFT,
                "ctrl" | "control" => mods |= Modifiers::CONTROL,
                "" => {}
                _ => return None,
            }
        }
    }
    let code = parse_code(key_part)?;
    Some(HotKey::new(Some(mods), code))
}

#[cfg(target_os = "macos")]
fn parse_code(s: &str) -> Option<Code> {
    let lower = s.to_lowercase();
    match lower.as_str() {
        "a" => Some(Code::KeyA),
        "b" => Some(Code::KeyB),
        "c" => Some(Code::KeyC),
        "d" => Some(Code::KeyD),
        "e" => Some(Code::KeyE),
        "f" => Some(Code::KeyF),
        "g" => Some(Code::KeyG),
        "h" => Some(Code::KeyH),
        "i" => Some(Code::KeyI),
        "j" => Some(Code::KeyJ),
        "k" => Some(Code::KeyK),
        "l" => Some(Code::KeyL),
        "m" => Some(Code::KeyM),
        "n" => Some(Code::KeyN),
        "o" => Some(Code::KeyO),
        "p" => Some(Code::KeyP),
        "q" => Some(Code::KeyQ),
        "r" => Some(Code::KeyR),
        "s" => Some(Code::KeyS),
        "t" => Some(Code::KeyT),
        "u" => Some(Code::KeyU),
        "v" => Some(Code::KeyV),
        "w" => Some(Code::KeyW),
        "x" => Some(Code::KeyX),
        "y" => Some(Code::KeyY),
        "z" => Some(Code::KeyZ),
        "0" => Some(Code::Digit0),
        "1" => Some(Code::Digit1),
        "2" => Some(Code::Digit2),
        "3" => Some(Code::Digit3),
        "4" => Some(Code::Digit4),
        "5" => Some(Code::Digit5),
        "6" => Some(Code::Digit6),
        "7" => Some(Code::Digit7),
        "8" => Some(Code::Digit8),
        "9" => Some(Code::Digit9),
        "return" | "enter" => Some(Code::Enter),
        "tab" => Some(Code::Tab),
        "space" => Some(Code::Space),
        "escape" | "esc" => Some(Code::Escape),
        "left" => Some(Code::ArrowLeft),
        "right" => Some(Code::ArrowRight),
        "up" => Some(Code::ArrowUp),
        "down" => Some(Code::ArrowDown),
        "f1" => Some(Code::F1),
        "f2" => Some(Code::F2),
        "f3" => Some(Code::F3),
        "f4" => Some(Code::F4),
        "f5" => Some(Code::F5),
        "f6" => Some(Code::F6),
        "f7" => Some(Code::F7),
        "f8" => Some(Code::F8),
        "f9" => Some(Code::F9),
        "f10" => Some(Code::F10),
        "f11" => Some(Code::F11),
        "f12" => Some(Code::F12),
        _ => None,
    }
}
