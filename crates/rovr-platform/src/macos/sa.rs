// Scripting-addition client transport — Rovr-owned.
//
// Rovr speaks the same length-prefixed binary framing as yabai's SA
// (src/sa.m / src/osax/common.h, MIT © 2019 Åsmund Vikane) but over its
// OWN socket namespace `/tmp/rovr-sa_<uid>.sock` and with a versioned
// handshake. The payload is Rovr-owned: primitive operations only, no layout
// policy, no config parsing, no desired-state. Every transition carries a
// hard deadline.
//
// Wire format:
//   request:  [i16 LE length][u8 opcode][payload...]
//             where length = 1 + payload size (excludes the length field)
//   handshake: request opcode 0x01 with no payload
//              response: version cstring, NUL, u32 LE capability attributes
//              Rovr payload version strings are `rovr-sa-<semver>` (e.g.
//              `rovr-sa-1.0`). The client probes, parses and version-checks
//              this string; a yabai payload (`yabai-sa-*`) is treated as
//              incompatible (different namespace), not silently accepted.
//   ops: payload is packed little-endian; the payload echoes one byte ACK.
//
// Rovr never connects to `/tmp/yabai-sa_*.socket`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

/// Rovr-owned SA socket namespace. Uses UID (not $USER) so the daemon
/// and CLI agree even when $USER is unset or differs; `$USER` fallback is
/// kept only for error messaging.
pub const SA_SOCKET_PATH_FMT: &str = "/tmp/rovr-sa_{}.sock";
/// Expected payload version prefix. Bump the minor on wire-compatible
/// extensions; bump the major on breaking changes. `probe()` rejects any
/// payload whose version string does not start with this prefix.
pub const ROVR_SA_VERSION_PREFIX: &str = "rovr-sa-1.";
const SA_SOCKET_BUFF_LEN: usize = 0x1000;
const SA_DEADLINE: Duration = Duration::from_secs(2);

const SA_OPCODE_HANDSHAKE: u8 = 0x01;
const SA_OPCODE_SPACE_FOCUS: u8 = 0x02;
const SA_OPCODE_SPACE_CREATE: u8 = 0x03;
const SA_OPCODE_SPACE_DESTROY: u8 = 0x04;
const SA_OPCODE_SPACE_MOVE: u8 = 0x05;
const SA_OPCODE_WINDOW_OPACITY: u8 = 0x07;
const SA_OPCODE_WINDOW_OPACITY_FADE: u8 = 0x08;
const SA_OPCODE_WINDOW_LAYER: u8 = 0x09;
const SA_OPCODE_WINDOW_STICKY: u8 = 0x0A;
const SA_OPCODE_WINDOW_SHADOW: u8 = 0x0B;
const SA_OPCODE_WINDOW_SCALE: u8 = 0x0D;

pub const OSAX_ATTRIB_ADD_SPACE: u32 = 0x04;
pub const OSAX_ATTRIB_REM_SPACE: u32 = 0x08;
pub const OSAX_ATTRIB_MOV_SPACE: u32 = 0x10;

#[derive(Debug, Error)]
pub enum SaError {
    #[error("scripting addition socket is not available: {0}")]
    Unavailable(String),
    #[error("scripting addition operation failed: {0}")]
    Operation(String),
}

/// Result of a successful handshake: the payload's version string plus
/// the capability attribute bits it reports for this macOS build.
#[derive(Debug, Clone)]
pub struct SaInfo {
    pub version: String,
    pub attribs: u32,
}

impl SaInfo {
    pub fn is_compatible(&self) -> bool {
        self.version.starts_with(ROVR_SA_VERSION_PREFIX)
    }
}

pub struct SaClient {
    socket_path: PathBuf,
}

impl Default for SaClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SaClient {
    pub fn socket_path_for_uid(uid: &str) -> PathBuf {
        PathBuf::from(SA_SOCKET_PATH_FMT.replace("{}", uid))
    }

    pub fn default_socket_path() -> PathBuf {
        // Prefer UID (stable, matches daemon's launchd env) over $USER.
        let uid = std::env::var("UID")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                // Fallback: derive from $USER when UID unset (e.g. tests / non-login shells).
                std::env::var("USER").unwrap_or_else(|_| "unknown".into())
            });
        Self::socket_path_for_uid(&uid)
    }

    pub fn new() -> Self {
        Self {
            socket_path: Self::default_socket_path(),
        }
    }

    pub fn with_socket_path(path: PathBuf) -> Self {
        Self { socket_path: path }
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    /// Probe the payload once. Absence of the socket or a failed handshake
    /// means SA capabilities are unavailable; the caller decides whether to
    /// fall back to non-SA paths.
    pub fn probe(&self) -> Option<SaInfo> {
        let mut stream = self.connect().ok()?;
        stream.write_all(&[0x01, 0x00, SA_OPCODE_HANDSHAKE]).ok()?;

        let mut buffer = [0u8; SA_SOCKET_BUFF_LEN];
        let mut length = 0usize;

        // Response: version cstring, NUL, u32 LE attributes. The version
        // string is skipped; capabilities come from the attribute bits.
        // Loop-read until the attributes are fully buffered, since a
        // localhost read can theoretically split the small response.
        loop {
            let needed = buffer[..length]
                .iter()
                .position(|&byte| byte == 0)
                .map(|nul| nul + 1 + 4)
                .unwrap_or(length + 1);
            if length >= needed {
                break;
            }
            if length >= buffer.len() {
                return None;
            }
            let bytes_read = stream.read(&mut buffer[length..]).ok()?;
            if bytes_read == 0 {
                return None;
            }
            length += bytes_read;
        }

        let nul = buffer[..length].iter().position(|&byte| byte == 0)?;
        let version_bytes = &buffer[..nul];
        let version = String::from_utf8_lossy(version_bytes).into_owned();
        // Require Rovr payload; a yabai payload ("yabai-sa-*") or any
        // unexpected version is treated as incompatible — not silently
        // accepted. This prevents accidentally depending on yabai.
        if !version.starts_with(ROVR_SA_VERSION_PREFIX) {
            return None;
        }
        let attribs = u32::from_le_bytes(buffer[nul + 1..nul + 5].try_into().ok()?);
        Some(SaInfo { version, attribs })
    }

    fn connect(&self) -> Result<UnixStream, SaError> {
        let stream = UnixStream::connect(&self.socket_path).map_err(|err| {
            SaError::Unavailable(format!("{}: {err}", self.socket_path.display()))
        })?;
        stream
            .set_read_timeout(Some(SA_DEADLINE))
            .and_then(|()| stream.set_write_timeout(Some(SA_DEADLINE)))
            .map_err(|err| SaError::Operation(format!("set timeout: {err}")))?;
        Ok(stream)
    }

    fn send_op(&self, opcode: u8, payload: &[u8]) -> Result<(), SaError> {
        let length = (1 + payload.len()) as i16;
        let mut bytes = Vec::with_capacity(2 + 1 + payload.len());
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.push(opcode);
        bytes.extend_from_slice(payload);

        let mut stream = self.connect()?;
        stream
            .write_all(&bytes)
            .map_err(|err| SaError::Operation(format!("send opcode {opcode:#04x}: {err}")))?;

        // The payload sends no ACK for non-handshake opcodes; it processes
        // the message then closes the connection. Reading to EOF is the
        // completion signal and bounds the wait by SA_DEADLINE.
        let mut sink = [0u8; 256];
        loop {
            match stream.read(&mut sink) {
                Ok(0) => break,
                Ok(_) => {}
                Err(err) => {
                    return Err(SaError::Operation(format!(
                        "completion wait for opcode {opcode:#04x}: {err}"
                    )))
                }
            }
        }
        Ok(())
    }

    pub fn focus_space(&self, sid: u64) -> Result<(), SaError> {
        self.send_op(SA_OPCODE_SPACE_FOCUS, &sid.to_le_bytes())
    }

    pub fn create_space(&self, sid: u64) -> Result<(), SaError> {
        self.send_op(SA_OPCODE_SPACE_CREATE, &sid.to_le_bytes())
    }

    pub fn set_opacity(&self, wid: u32, alpha: f32, duration: f32) -> Result<(), SaError> {
        let opcode = if duration > 0.0 {
            SA_OPCODE_WINDOW_OPACITY_FADE
        } else {
            SA_OPCODE_WINDOW_OPACITY
        };
        let mut payload = Vec::with_capacity(12);
        payload.extend_from_slice(&wid.to_le_bytes());
        payload.extend_from_slice(&alpha.to_le_bytes());
        payload.extend_from_slice(&duration.to_le_bytes());
        self.send_op(opcode, &payload)
    }

    pub fn set_layer(&self, wid: u32, layer: i32) -> Result<(), SaError> {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&wid.to_le_bytes());
        payload.extend_from_slice(&layer.to_le_bytes());
        self.send_op(SA_OPCODE_WINDOW_LAYER, &payload)
    }

    pub fn set_sticky(&self, wid: u32, sticky: bool) -> Result<(), SaError> {
        let mut payload = Vec::with_capacity(5);
        payload.extend_from_slice(&wid.to_le_bytes());
        payload.push(u8::from(sticky));
        self.send_op(SA_OPCODE_WINDOW_STICKY, &payload)
    }

    pub fn set_shadow(&self, wid: u32, shadow: bool) -> Result<(), SaError> {
        let mut payload = Vec::with_capacity(5);
        payload.extend_from_slice(&wid.to_le_bytes());
        payload.push(u8::from(shadow));
        self.send_op(SA_OPCODE_WINDOW_SHADOW, &payload)
    }
    pub fn scale_window(&self, wid: u32, x: f32, y: f32, w: f32, h: f32) -> Result<(), SaError> {
        let mut payload = Vec::with_capacity(20);
        payload.extend_from_slice(&wid.to_le_bytes());
        payload.extend_from_slice(&x.to_le_bytes());
        payload.extend_from_slice(&y.to_le_bytes());
        payload.extend_from_slice(&w.to_le_bytes());
        payload.extend_from_slice(&h.to_le_bytes());
        self.send_op(SA_OPCODE_WINDOW_SCALE, &payload)
    }

    pub fn destroy_space(&self, sid: u64) -> Result<(), SaError> {
        self.send_op(SA_OPCODE_SPACE_DESTROY, &sid.to_le_bytes())
    }

    pub fn move_space(&self, src_sid: u64, dst_sid: u64) -> Result<(), SaError> {
        let mut payload = Vec::with_capacity(25);
        payload.extend_from_slice(&src_sid.to_le_bytes());
        payload.extend_from_slice(&dst_sid.to_le_bytes());
        payload.extend_from_slice(&0u64.to_le_bytes());
        payload.push(0u8);
        self.send_op(SA_OPCODE_SPACE_MOVE, &payload)
    }
}
