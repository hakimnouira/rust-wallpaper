//! Versioned, bounded messages shared between the daemon and desktop clients.
//! The wire format is one UTF-8 JSON envelope per line. The framing helpers enforce
//! a hard limit before growing the input buffer without bound.

#[cfg(windows)]
pub mod client;

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use uuid::Uuid;
use wallpaper_core::{PerformanceProfile, PlaybackState, Wallpaper, WallpaperId};

/// Increment when an incompatible wire-contract change is made.
pub const PROTOCOL_VERSION: u16 = 1;
/// Maximum encoded envelope size (excluding the trailing newline).
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub command: Command,
}

impl RequestEnvelope {
    pub fn new(command: Command) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            command,
        }
    }

    /// Validate compatibility before dispatching a request.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ProtocolError::VersionMismatch {
                received: self.protocol_version,
                supported: PROTOCOL_VERSION,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", content = "payload", rename_all = "snake_case")]
pub enum Command {
    Ping,
    ListMonitors,
    ListWallpapers,
    ImportWallpaper {
        path: String,
        title: Option<String>,
    },
    RemoveWallpaper {
        wallpaper_id: WallpaperId,
    },
    SetProfile {
        profile: PerformanceProfile,
    },
    ApplyWallpaper {
        wallpaper_id: WallpaperId,
        monitor: Option<String>,
    },
    Pause {
        monitor: Option<String>,
    },
    Resume {
        monitor: Option<String>,
    },
    Stop {
        monitor: Option<String>,
    },
    SetVolume {
        monitor: Option<String>,
        volume: u32,
    },
    SetMute {
        monitor: Option<String>,
        muted: bool,
    },
    GetStatus {
        monitor: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub id: String,
    pub device_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerStatusInfo {
    pub monitor: String,
    pub state: PlaybackState,
    pub active_decoder: String,
    pub memory_bytes: u64,
    pub suspended: bool,
    pub suspension_reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ProtocolMismatch,
    InvalidRequest,
    NotFound,
    Unsupported,
    RuntimeFailure,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub result: Response,
}

impl ResponseEnvelope {
    pub fn new(request_id: Uuid, result: Response) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result,
        }
    }

    pub fn error(request_id: Uuid, code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(
            request_id,
            Response::Error {
                code,
                message: message.into(),
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", content = "payload", rename_all = "snake_case")]
pub enum Response {
    Pong,
    MonitorList(Vec<MonitorInfo>),
    WallpaperList(Vec<Wallpaper>),
    PlaybackChanged {
        wallpaper_id: Option<WallpaperId>,
        monitor: Option<String>,
        state: PlaybackState,
    },
    PlayerStatus(Vec<PlayerStatusInfo>),
    Accepted,
    Error {
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("protocol version mismatch: client sent {received}, server supports {supported}")]
    VersionMismatch { received: u16, supported: u16 },
    #[error("frame exceeds maximum size of {MAX_FRAME_BYTES} bytes")]
    FrameTooLarge,
    #[error("unexpected end of stream while reading a frame")]
    UnexpectedEof,
    #[error("invalid JSON envelope: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Read one newline-terminated frame without allowing unbounded buffering.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Vec<u8>, ProtocolError> {
    let mut frame = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte)? {
            0 if frame.is_empty() => return Err(ProtocolError::UnexpectedEof),
            0 => return Err(ProtocolError::UnexpectedEof),
            _ if byte[0] == b'\n' => return Ok(frame),
            _ => {
                if frame.len() == MAX_FRAME_BYTES {
                    return Err(ProtocolError::FrameTooLarge);
                }
                frame.push(byte[0]);
            }
        }
    }
}

pub fn read_request<R: Read>(reader: &mut R) -> Result<RequestEnvelope, ProtocolError> {
    let frame = read_frame(reader)?;
    Ok(serde_json::from_slice(&frame)?)
}

pub fn write_response<W: Write>(
    writer: &mut W,
    response: &ResponseEnvelope,
) -> Result<(), ProtocolError> {
    let encoded = serde_json::to_vec(response)?;
    if encoded.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    writer.write_all(&encoded)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn request_round_trips_and_validates_version() {
        let request = RequestEnvelope::new(Command::Ping);
        let encoded = serde_json::to_vec(&request).unwrap();
        let decoded: RequestEnvelope = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, request);
        decoded.validate().unwrap();
    }

    #[test]
    fn library_mutation_commands_round_trip() {
        let import = RequestEnvelope::new(Command::ImportWallpaper {
            path: r"C:\\Wallpapers\\demo.mp4".into(),
            title: Some("Demo".into()),
        });
        let encoded = serde_json::to_vec(&import).unwrap();
        let decoded: RequestEnvelope = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, import);

        let remove = RequestEnvelope::new(Command::RemoveWallpaper {
            wallpaper_id: WallpaperId::new(),
        });
        let encoded = serde_json::to_vec(&remove).unwrap();
        let decoded: RequestEnvelope = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, remove);
    }

    #[test]
    fn version_mismatch_is_reported() {
        let mut request = RequestEnvelope::new(Command::Ping);
        request.protocol_version = PROTOCOL_VERSION + 1;
        assert!(matches!(
            request.validate(),
            Err(ProtocolError::VersionMismatch { .. })
        ));
    }

    #[test]
    fn bounded_frame_reader_accepts_one_line() {
        let mut input = Cursor::new(br#"{"ok":true}"#.to_vec());
        input.get_mut().extend_from_slice(b"\nnext");
        assert_eq!(read_frame(&mut input).unwrap(), br#"{"ok":true}"#.to_vec());
    }

    #[test]
    fn frame_reader_rejects_oversized_input() {
        let input = vec![b'x'; MAX_FRAME_BYTES + 1];
        assert!(matches!(
            read_frame(&mut Cursor::new(input)),
            Err(ProtocolError::FrameTooLarge)
        ));
    }

    #[test]
    fn response_writer_emits_newline_delimited_json() {
        let request_id = Uuid::new_v4();
        let response = ResponseEnvelope::new(request_id, Response::Pong);
        let mut output = Vec::new();
        write_response(&mut output, &response).unwrap();
        assert_eq!(output.last(), Some(&b'\n'));
        let decoded: ResponseEnvelope =
            serde_json::from_slice(&output[..output.len() - 1]).unwrap();
        assert_eq!(decoded, response);
    }
}
