// Scripting-addition client transport.
//
// The SA payload (when loaded into Dock by yabai) listens on
// /tmp/yabai-sa_<username>.socket and speaks a length-prefixed binary
// protocol. Wire format, adapted from yabai src/sa.m / src/osax/common.h
// (MIT, © 2019 Åsmund Vikane):
//
//   request:  [i16 LE length][u8 opcode][payload...]
//             where length = 1 + payload size (excludes the length field)
//   handshake: request opcode 0x01 with no payload
//              response: version cstring, NUL, u32 LE capability attributes
//   ops: payload is packed little-endian; the payload echoes one byte ACK.
//
// Rovr connects to a SA installed and injected by yabai; rovr does not
// (yet) ship its own payload. Every transition carries a hard deadline.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

const SA_SOCKET_PATH_FMT: &str = "/tmp/yabai-sa_{}.socket";
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

/// Result of a successful handshake: the capability attribute bits the
/// payload reports for this macOS version.
#[derive(Debug, Clone, Copy)]
pub struct SaInfo {
    pub attribs: u32,
}

pub struct SaClient {
    socket_path: PathBuf,
}

impl SaClient {
    pub fn new() -> Self {
        let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
        Self {
            socket_path: PathBuf::from(SA_SOCKET_PATH_FMT.replace("{}", &user)),
        }
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
        let attribs = u32::from_le_bytes(buffer[nul + 1..nul + 5].try_into().ok()?);
        Some(SaInfo { attribs })
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
