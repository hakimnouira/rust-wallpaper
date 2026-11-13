//! Versioned messages shared between the daemon, UI, and player processes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wallpaper_core::{PlaybackState, Wallpaper, WallpaperId};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", content = "payload", rename_all = "snake_case")]
pub enum Command {
    Ping,
    ListWallpapers,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerStatusInfo {
    pub monitor: String,
    pub wallpaper_id: Option<WallpaperId>,
    pub state: PlaybackState,
    pub active_decoder: String,
    pub duration_secs: Option<f64>,
    pub playback_time_secs: Option<f64>,
    pub volume: u32,
    pub muted: bool,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub result: Response,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", content = "payload", rename_all = "snake_case")]
pub enum Response {
    Pong,
    WallpaperList(Vec<Wallpaper>),
    PlaybackChanged {
        wallpaper_id: Option<WallpaperId>,
        monitor: Option<String>,
        state: PlaybackState,
    },
    PlayerStatus(Vec<PlayerStatusInfo>),
    Accepted,
    Error {
        message: String,
    },
}
