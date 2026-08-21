#![allow(clippy::question_mark)]
use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use rovr_config::Config;
use tracing::{info, warn};

#[cfg(target_os = "macos")]
use {
    global_hotkey::{
        hotkey::{Code, HotKey, Modifiers},
        GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    },
    rovr_protocol::{Command, Request},
    std::io::Write,
    std::os::unix::net::UnixStream,
};

/// Spawn optional built-in hotkey listener. Returns manager handle to keep alive.
/// If no binds or on non-macOS, returns None and does nothing (skhd remains the supported path).
#[cfg(target_os = "macos")]
#[allow(clippy::question_mark)]
pub fn spawn_hotkey_listener(config: Config, socket_path: PathBuf) -> Option<GlobalHotKeyManager> {
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
    let mut id_to_command: HashMap<u32, String> = HashMap::new();
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
                    if let Err(e) = dispatch_via_ipc(&socket, cmd_str) {
                        warn!(%e, cmd=%cmd_str, "hotkey: dispatch failed");
                    }
                }
            }
        }
    });
    Some(manager)
}

#[cfg(not(target_os = "macos"))]
pub fn spawn_hotkey_listener(_config: Config, _socket_path: PathBuf) -> Option<()> {
    None
}

#[cfg(target_os = "macos")]
fn dispatch_via_ipc(socket_path: &PathBuf, command_str: &str) -> Result<()> {
    // Parse `command_str` like "window --focus 1" or "workspace focus code"
    // by feeding it through the CLI's clap parser in-process would require
    // linking rovr-cli. Instead we do minimal string→Command mapping here.
    // Supported: window/space/layout/scratchpad/workspace/query/ping/doctor.
    // Fallback: try to connect and send raw, daemon will return BAD_REQUEST if unknown.
    let cmd = parse_bind_command(command_str).unwrap_or_else(|| {
        warn!(cmd=%command_str, "hotkey: unknown command syntax, sending as ping");
        // Use Ping as fallback so we at least exercise IPC
        rovr_protocol::Command::Ping
    });
    let req = Request::new(1, cmd);
    let mut stream = UnixStream::connect(socket_path)?;
    serde_json::to_writer(&mut stream, &req)?;
    stream.write_all(b"\n")?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn parse_bind_command(s: &str) -> Option<Command> {
    // Very small hand-rolled parser for hotkey binds. We accept the same
    // strings as `rovr config gen-skhd` emits: "window --focus 1", "layout --rotate 1", etc.
    // For full fidelity we delegate to `clap` by constructing a fake `rovr` CLI invocation.
    // To avoid depending on rovr-cli crate, we parse manually for common cases.
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.is_empty() {
        return None;
    }
    // Use rovr-cli's Cli parsing if available via feature - for now manual:
    match parts[0] {
        "window" if parts.len() >= 3 && parts[1] == "--focus" => {
            let id = parts[2].parse::<u32>().ok()?;
            Some(Command::Window(rovr_protocol::WindowCommand::Focus {
                window: rovr_types::WindowId(id),
            }))
        }
        "window" if parts.len() >= 5 && parts[1] == "--focus-direction" => {
            // e.g. window --focus-direction --from 1 --direction east
            // Minimal: find --from and --direction
            let from = parts
                .iter()
                .position(|&p| p == "--from")
                .and_then(|i| parts.get(i + 1))?
                .parse::<u32>()
                .ok()?;
            let dir = parts
                .iter()
                .position(|&p| p == "--direction")
                .and_then(|i| parts.get(i + 1))?;
            let direction = match *dir {
                "north" => rovr_types::Direction::North,
                "south" => rovr_types::Direction::South,
                "east" => rovr_types::Direction::East,
                "west" => rovr_types::Direction::West,
                _ => return None,
            };
            Some(Command::Window(
                rovr_protocol::WindowCommand::FocusDirection {
                    from: rovr_types::WindowId(from),
                    direction,
                },
            ))
        }
        "window" if parts.len() >= 4 && parts[1] == "move-to-workspace" => {
            let win = parts[2].parse::<u32>().ok()?;
            let ws = parts[3].to_string();
            Some(Command::Window(
                rovr_protocol::WindowCommand::MoveToWorkspace {
                    window: rovr_types::WindowId(win),
                    workspace: ws,
                },
            ))
        }
        "workspace" if parts.len() >= 3 && parts[1] == "focus" => {
            Some(Command::Workspace(rovr_protocol::WorkspaceCommand::Focus {
                name: parts[2].to_string(),
            }))
        }
        "workspace" if parts.len() >= 4 && parts[1] == "move-window" => {
            let win = parts[2].parse::<u32>().ok()?;
            let ws = parts[3].to_string();
            Some(Command::Workspace(
                rovr_protocol::WorkspaceCommand::MoveWindow {
                    window: rovr_types::WindowId(win),
                    workspace: ws,
                },
            ))
        }
        "scratchpad" if parts.len() >= 3 && parts[1] == "toggle" => Some(Command::Scratchpad(
            rovr_protocol::ScratchpadCommand::Toggle {
                name: parts[2].to_string(),
            },
        )),
        "layout" if parts.len() >= 3 && parts[1] == "--rotate" => {
            let sid = parts[2].parse::<u64>().ok()?;
            Some(Command::Layout(rovr_protocol::LayoutCommand::Rotate {
                space: rovr_types::SpaceId(sid),
            }))
        }
        "query" if parts.len() >= 2 && parts[1] == "--windows" => {
            Some(Command::Query(rovr_protocol::QueryCommand::Windows))
        }
        "ping" => Some(Command::Ping),
        "doctor" => Some(Command::Doctor),
        _ => None,
    }
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

#[cfg(target_os = "macos")]
#[allow(dead_code)]
fn hotkey_id_for_test(key: &str) -> Option<u32> {
    parse_skhd_hotkey(key).map(|h| h.id())
}
