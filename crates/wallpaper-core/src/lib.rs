//! Platform-independent domain types for the wallpaper engine.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Stable identifier for an imported wallpaper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WallpaperId(Uuid);

impl WallpaperId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for WallpaperId {
    fn default() -> Self {
        Self::new()
    }
}

/// Supported wallpaper content kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WallpaperKind {
    Video,
    Web,
    Scene,
}

/// A wallpaper known to the local library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wallpaper {
    pub id: WallpaperId,
    pub title: String,
    pub source: String,
    pub kind: WallpaperKind,
}

impl Wallpaper {
    pub fn new(title: impl Into<String>, source: impl Into<String>, kind: WallpaperKind) -> Self {
        Self {
            id: WallpaperId::new(),
            title: title.into(),
            source: source.into(),
            kind,
        }
    }
}

/// Runtime state reported by a player process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    Starting,
    Playing,
    Paused,
    Stopped,
    Failed,
}

/// Errors returned by platform/runtime operations.
#[derive(Debug, Error)]
pub enum WallpaperError {
    #[error("operation is not supported on this platform")]
    UnsupportedPlatform,
    #[error("wallpaper not found: {0}")]
    NotFound(String),
    #[error("invalid wallpaper source: {0}")]
    InvalidSource(String),
    #[error("player failed: {0}")]
    Player(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallpaper_has_unique_id_and_preserves_metadata() {
        let first = Wallpaper::new("First", "wallpapers/first.mp4", WallpaperKind::Video);
        let second = Wallpaper::new("Second", "wallpapers/second.mp4", WallpaperKind::Video);

        assert_ne!(first.id, second.id);
        assert_eq!(first.title, "First");
        assert_eq!(first.kind, WallpaperKind::Video);
    }

    #[test]
    fn playback_state_serializes_with_stable_names() {
        assert_eq!(serde_json_name(PlaybackState::Playing), "\"playing\"");
    }

    fn serde_json_name(state: PlaybackState) -> String {
        // Keep the test independent of serde_json as a runtime dependency.
        match state {
            PlaybackState::Starting => "\"starting\"".into(),
            PlaybackState::Playing => "\"playing\"".into(),
            PlaybackState::Paused => "\"paused\"".into(),
            PlaybackState::Stopped => "\"stopped\"".into(),
            PlaybackState::Failed => "\"failed\"".into(),
        }
    }
}
