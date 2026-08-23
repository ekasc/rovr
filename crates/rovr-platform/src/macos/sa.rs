// Scripting-addition client transport — Rovr-owned.
//
// Rovr speaks the same length-prefixed binary framing as yabai's SA
// (src/sa.m / src/osax/common.h, MIT © 2019 Åsmund Vikane) but over its
// OWN socket namespace `/tmp/rovr-<uid>/sa.sock` and with a versioned
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
//              `rovr-sa-2.0`). The client probes, parses and version-checks
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
pub const SA_SOCKET_PATH_FMT: &str = "/tmp/rovr-{}/sa.sock";
/// Expected payload version prefix. Bump the minor on wire-compatible
/// extensions; bump the major on breaking changes. `probe()` preserves raw
/// mismatched versions for status, while capabilities reject them.
pub const ROVR_SA_VERSION_PREFIX: &str = "rovr-sa-2.";
const SA_SOCKET_BUFF_LEN: usize = 0x1000;
const SA_DEADLINE: Duration = Duration::from_secs(2);
/// Deadline for PERIODIC health probes: a wedged payload must never hold the
/// state loop longer than this. Operational paths keep the full SA_DEADLINE.
const SA_PROBE_DEADLINE: Duration = Duration::from_millis(250);

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
pub const OSAX_ATTRIB_FOCUS_SPACE: u32 = 0x20;
pub const OSAX_ATTRIB_WINDOW_OPACITY: u32 = 0x40;
pub const OSAX_ATTRIB_WINDOW_LAYER: u32 = 0x80;
pub const OSAX_ATTRIB_WINDOW_STICKY: u32 = 0x100;
pub const OSAX_ATTRIB_WINDOW_SHADOW: u32 = 0x200;
pub const OSAX_ATTRIB_WINDOW_SCALE: u32 = 0x400;

#[cfg(target_os = "macos")]
extern "C" {
    fn getuid() -> u32;
    fn getpeereid(socket: i32, euid: *mut u32, egid: *mut u32) -> i32;
}

#[cfg(target_os = "macos")]
unsafe fn libc_getuid() -> u32 {
    getuid()
}

#[cfg(target_os = "macos")]
fn validate_peer(stream: &UnixStream, expected_uid: u32) -> Result<(), SaError> {
    use std::os::fd::AsRawFd;
    let mut uid = u32::MAX;
    let mut gid = 0u32;
    let status = unsafe { getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if status != 0 || uid != expected_uid {
        return Err(SaError::Unavailable("SA socket peer owner mismatch".into()));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
unsafe fn libc_getuid() -> u32 {
    0
}

#[cfg(not(target_os = "macos"))]
fn validate_peer(_stream: &UnixStream, _expected_uid: u32) -> Result<(), SaError> {
    Ok(())
}

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
    expected_uid: u32,
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
        // The payload keys its socket on getuid(); the client must use the
        // SAME value. ($USER is not consulted: macOS does not set a UID env
        // var, so a username fallback would never match the payload.)
        let uid = unsafe { libc_getuid() };
        Self::socket_path_for_uid(&uid.to_string())
    }

    pub fn new() -> Self {
        Self {
            socket_path: Self::default_socket_path(),
            expected_uid: unsafe { libc_getuid() },
        }
    }

    pub fn with_socket_path(path: PathBuf) -> Self {
        Self {
            socket_path: path,
            expected_uid: unsafe { libc_getuid() },
        }
    }

    pub fn with_socket_path_for_uid(path: PathBuf, expected_uid: u32) -> Self {
        Self {
            socket_path: path,
            expected_uid,
        }
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    /// Probe the payload once. Absence of the socket or a failed handshake
    /// means SA capabilities are unavailable; the caller decides whether to
    /// fall back to non-SA paths.
    pub fn probe(&self) -> Option<SaInfo> {
        self.probe_with_deadline(SA_DEADLINE)
    }

    /// Short-deadline probe for PERIODIC health checks: a wedged payload must
    /// never hold a caller longer than this. Operational paths (install,
    /// explicit status) keep the full `SA_DEADLINE` via `probe`.
    pub fn probe_health(&self) -> Option<SaInfo> {
        self.probe_with_deadline(SA_PROBE_DEADLINE)
    }

    /// Probe with an explicit I/O deadline. Operational paths (install,
    /// explicit status) use the full `SA_DEADLINE`; the periodic health check
    /// uses the short one so a hung payload cannot monopolize the state loop.
    pub fn probe_with_deadline(&self, deadline: Duration) -> Option<SaInfo> {
        let mut stream = self.connect_with_deadline(deadline).ok()?;
        // Frame: len = 3 + payload_len (payload empty for handshake).
        stream.write_all(&[0x03, 0x00, SA_OPCODE_HANDSHAKE]).ok()?;

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
        // Preserve the raw version even when incompatible so status can
        // distinguish a protocol mismatch from an absent/stalled payload.
        let attribs = u32::from_le_bytes(buffer[nul + 1..nul + 5].try_into().ok()?);
        Some(SaInfo { version, attribs })
    }

    fn connect(&self) -> Result<UnixStream, SaError> {
        self.connect_with_deadline(SA_DEADLINE)
    }

    fn connect_with_deadline(&self, deadline: Duration) -> Result<UnixStream, SaError> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let metadata = std::fs::symlink_metadata(&self.socket_path).map_err(|err| {
            SaError::Unavailable(format!("{}: {err}", self.socket_path.display()))
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_socket()
            || metadata.uid() != self.expected_uid
        {
            return Err(SaError::Unavailable(format!(
                "unsafe socket identity at {}",
                self.socket_path.display()
            )));
        }
        let stream = UnixStream::connect(&self.socket_path).map_err(|err| {
            SaError::Unavailable(format!("{}: {err}", self.socket_path.display()))
        })?;
        validate_peer(&stream, self.expected_uid)?;
        stream
            .set_read_timeout(Some(deadline))
            .and_then(|()| stream.set_write_timeout(Some(deadline)))
            .map_err(|err| SaError::Operation(format!("set timeout: {err}")))?;
        Ok(stream)
    }

    fn send_op(&self, opcode: u8, payload: &[u8]) -> Result<(), SaError> {
        // Framing matches the payload's reader (and upstream's): `len` counts
        // 2 bytes of length field + 1 opcode byte + payload; the reader
        // consumes `len - 2` bytes after the prefix.
        let length = (3 + payload.len()) as i16;
        let mut bytes = Vec::with_capacity(2 + 1 + payload.len());
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.push(opcode);
        bytes.extend_from_slice(payload);

        let mut stream = self.connect()?;
        stream
            .write_all(&bytes)
            .map_err(|err| SaError::Operation(format!("send opcode {opcode:#04x}: {err}")))?;

        let mut ack = [0u8; 1];
        stream.read_exact(&mut ack).map_err(|err| {
            SaError::Operation(format!("ACK wait for opcode {opcode:#04x}: {err}"))
        })?;
        if ack[0] != 0 {
            return Err(SaError::Operation(format!(
                "opcode {opcode:#04x} rejected with status {}",
                ack[0]
            )));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn socket(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rovr-sa-{name}-{}.sock", std::process::id()))
    }

    #[test]
    fn eof_without_operation_ack_is_failure() {
        let path = socket("eof");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 32];
            let _ = stream.read(&mut request);
        });
        let client = SaClient::with_socket_path(path.clone());
        assert!(client.set_layer(1, 0).is_err());
        server.join().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn probe_preserves_incompatible_raw_version() {
        let path = socket("version");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 3];
            stream.read_exact(&mut request).unwrap();
            stream.write_all(b"rovr-sa-9.0\0\0\0\0\0").unwrap();
        });
        let client = SaClient::with_socket_path(path.clone());
        let info = client.probe().expect("raw handshake remains observable");
        assert_eq!(info.version, "rovr-sa-9.0");
        assert!(!info.is_compatible());
        server.join().unwrap();
        let _ = std::fs::remove_file(path);
    }
}
