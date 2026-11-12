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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", content = "payload", rename_all = "snake_case")]
pub enum Command {
    Ping,
    ListWallpapers,
    ApplyWallpaper { wallpaper_id: WallpaperId, monitor: Option<String> },
    Pause { wallpaper_id: WallpaperId },
    Resume { wallpaper_id: WallpaperId },
    Stop { wallpaper_id: WallpaperId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub result: Response,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", content = "payload", rename_all = "snake_case")]
pub enum Response {
    Pong,
    WallpaperList(Vec<Wallpaper>),
    PlaybackChanged { wallpaper_id: WallpaperId, state: PlaybackState },
    Accepted,
    Error { message: String },
}
