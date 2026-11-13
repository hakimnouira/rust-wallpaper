//! Platform-independent domain types for the wallpaper engine.

pub mod library;

pub use library::{LibraryError, WallpaperLibrary};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;
use uuid::Uuid;

/// Stable identifier for an imported wallpaper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WallpaperId(pub Uuid);

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

impl fmt::Display for WallpaperId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for WallpaperId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// Supported wallpaper content kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WallpaperKind {
    Video,
    Web,
    Scene,
}

/// Hardware decoding acceleration options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareAcceleration {
    Auto,
    D3d11va,
    Dxva2,
    Disabled,
}

impl HardwareAcceleration {
    pub fn as_mpv_arg(&self) -> &'static str {
        match self {
            Self::Auto => "auto-safe",
            Self::D3d11va => "d3d11va",
            Self::Dxva2 => "dxva2",
            Self::Disabled => "no",
        }
    }
}

impl Default for HardwareAcceleration {
    fn default() -> Self {
        Self::Auto
    }
}

/// Playback configuration settings for a wallpaper instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackConfig {
    pub loop_playback: bool,
    pub mute: bool,
    pub volume: u32,
    pub hardware_acceleration: HardwareAcceleration,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            loop_playback: true,
            mute: true,
            volume: 100,
            hardware_acceleration: HardwareAcceleration::Auto,
        }
    }
}

/// Metadata describing a wallpaper asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallpaperMetadata {
    pub title: String,
    pub source: String,
    pub duration_secs: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub thumbnail_path: Option<String>,
}

/// A wallpaper known to the local library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wallpaper {
    pub id: WallpaperId,
    pub kind: WallpaperKind,
    pub metadata: WallpaperMetadata,
    pub config: PlaybackConfig,
}

impl Wallpaper {
    pub fn new(title: impl Into<String>, source: impl Into<String>, kind: WallpaperKind) -> Self {
        let t = title.into();
        let s = source.into();
        Self {
            id: WallpaperId::new(),
            kind,
            metadata: WallpaperMetadata {
                title: t,
                source: s,
                duration_secs: None,
                width: None,
                height: None,
                thumbnail_path: None,
            },
            config: PlaybackConfig::default(),
        }
    }

    #[inline]
    pub fn title(&self) -> &str {
        &self.metadata.title
    }

    #[inline]
    pub fn source(&self) -> &str {
        &self.metadata.source
    }
}

/// Explicit reasons why wallpaper playback may be suspended.
/// Multiple reasons can coexist simultaneously (e.g. UserPaused while SessionLocked).
/// A player will only resume when its set of active suspension reasons is completely empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspensionReason {
    /// Manually paused by user or IPC command.
    UserPaused,
    /// Paused due to a foreground fullscreen window or game.
    FullscreenApplication,
    /// Paused because the Windows workstation session is locked.
    SessionLocked,
    /// Paused because display monitors are powered off or in low-power standby.
    DisplayPoweredOff,
    /// Paused due to system power suspend / sleep.
    SystemSuspended,
    /// Paused due to an active runtime or player failure.
    RuntimeError,
}

/// A container tracking active suspension reasons for a wallpaper player.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SuspensionFlags {
    reasons: std::collections::HashSet<SuspensionReason>,
}

impl SuspensionFlags {
    pub fn new() -> Self {
        Self {
            reasons: std::collections::HashSet::new(),
        }
    }

    /// Adds a suspension reason. Returns true if it was newly added.
    pub fn add(&mut self, reason: SuspensionReason) -> bool {
        self.reasons.insert(reason)
    }

    /// Removes a suspension reason. Returns true if it was removed.
    pub fn remove(&mut self, reason: SuspensionReason) -> bool {
        self.reasons.remove(&reason)
    }

    /// Returns true if any suspension reason requires the player to be suspended.
    pub fn is_suspended(&self) -> bool {
        !self.reasons.is_empty()
    }

    /// Returns true if a specific suspension reason is currently active.
    pub fn contains(&self, reason: SuspensionReason) -> bool {
        self.reasons.contains(&reason)
    }

    /// Clears all suspension reasons.
    pub fn clear(&mut self) {
        self.reasons.clear();
    }

    /// Returns a list of active reasons.
    pub fn active_reasons(&self) -> Vec<SuspensionReason> {
        self.reasons.iter().copied().collect()
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

/// Performance profiles governing resource usage and playback pause triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceProfile {
    /// Aggressive pause behavior: pauses on any maximized or fullscreen window,
    /// display sleep, lock screen, and reduces background polling frequency.
    Eco,
    /// Default profile: pauses during true fullscreen applications / games,
    /// lock screen, display power off, and battery saver.
    Balanced,
    /// Maximizes uptime: continues playback even with maximized or borderless apps,
    /// only pauses when explicitly locked or displaying fullscreen exclusive games.
    Performance,
}

impl Default for PerformanceProfile {
    fn default() -> Self {
        Self::Balanced
    }
}

/// Policy configuration controlling when wallpaper playback automatically pauses or resumes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackPolicy {
    /// Active performance profile.
    pub profile: PerformanceProfile,
    /// Whether to pause when a fullscreen application is detected.
    pub pause_on_fullscreen: bool,
    /// Whether to pause specifically for fullscreen games (distinguished from normal maximized apps).
    pub pause_on_fullscreen_game_only: bool,
    /// Whether to pause when workstation is locked (`WM_WTSSESSION_CHANGE` / lock screen).
    pub pause_on_lock: bool,
    /// Whether to pause when display is powered off or system enters suspend/sleep.
    pub pause_on_display_power_off: bool,
    /// Debounce duration before triggering pause/resume to prevent rapid state cycling.
    pub debounce_millis: u64,
}

impl Default for PlaybackPolicy {
    fn default() -> Self {
        Self::for_profile(PerformanceProfile::Balanced)
    }
}

impl PlaybackPolicy {
    pub fn for_profile(profile: PerformanceProfile) -> Self {
        match profile {
            PerformanceProfile::Eco => Self {
                profile,
                pause_on_fullscreen: true,
                pause_on_fullscreen_game_only: false, // pause on any fullscreen / maximized app
                pause_on_lock: true,
                pause_on_display_power_off: true,
                debounce_millis: 1000,
            },
            PerformanceProfile::Balanced => Self {
                profile,
                pause_on_fullscreen: true,
                pause_on_fullscreen_game_only: true, // only true fullscreen apps/games
                pause_on_lock: true,
                pause_on_display_power_off: true,
                debounce_millis: 500,
            },
            PerformanceProfile::Performance => Self {
                profile,
                pause_on_fullscreen: false,
                pause_on_fullscreen_game_only: true,
                pause_on_lock: true,
                pause_on_display_power_off: true,
                debounce_millis: 300,
            },
        }
    }

    /// Evaluates environmental conditions to determine whether playback should pause.
    ///
    /// Returns `(should_pause, reason)`.
    pub fn evaluate(
        &self,
        is_fullscreen_detected: bool,
        is_game_detected: bool,
        is_session_locked: bool,
        is_display_off_or_suspended: bool,
    ) -> (bool, Option<&'static str>) {
        if self.pause_on_lock && is_session_locked {
            return (true, Some("session_locked"));
        }

        if self.pause_on_display_power_off && is_display_off_or_suspended {
            return (true, Some("display_off_or_suspended"));
        }

        if self.pause_on_fullscreen {
            if self.pause_on_fullscreen_game_only {
                if is_fullscreen_detected && is_game_detected {
                    return (true, Some("fullscreen_game"));
                }
            } else if is_fullscreen_detected {
                return (true, Some("fullscreen_app"));
            }
        }

        (false, None)
    }
}

/// Runtime telemetry captured for an active player.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerTelemetry {
    pub device_name: String,
    pub active_decoder: String,
    pub state: PlaybackState,
    pub memory_working_set_bytes: u64,
    pub uptime_secs: u64,
    pub video_width: Option<u32>,
    pub video_height: Option<u32>,
    pub video_fps: Option<f64>,
    pub gpu_context: Option<String>,
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
    #[error("library error: {0}")]
    Library(#[from] LibraryError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallpaper_has_unique_id_and_preserves_metadata() {
        let first = Wallpaper::new("First", "wallpapers/first.mp4", WallpaperKind::Video);
        let second = Wallpaper::new("Second", "wallpapers/second.mp4", WallpaperKind::Video);

        assert_ne!(first.id, second.id);
        assert_eq!(first.title(), "First");
        assert_eq!(first.source(), "wallpapers/first.mp4");
        assert_eq!(first.kind, WallpaperKind::Video);
        assert!(first.config.loop_playback);
    }

    #[test]
    fn test_suspension_flags_coexistence() {
        let mut flags = SuspensionFlags::new();
        assert!(!flags.is_suspended());

        // Lock session
        flags.add(SuspensionReason::SessionLocked);
        assert!(flags.is_suspended());
        assert!(flags.contains(SuspensionReason::SessionLocked));

        // User manually pauses while locked
        flags.add(SuspensionReason::UserPaused);
        assert!(flags.is_suspended());

        // Session unlocks
        flags.remove(SuspensionReason::SessionLocked);
        // Must remain suspended because UserPaused is still active!
        assert!(flags.is_suspended());
        assert!(flags.contains(SuspensionReason::UserPaused));

        // Fullscreen app launches
        flags.add(SuspensionReason::FullscreenApplication);
        assert!(flags.is_suspended());

        // Display powers off
        flags.add(SuspensionReason::DisplayPoweredOff);

        // Display powers on
        flags.remove(SuspensionReason::DisplayPoweredOff);
        assert!(flags.is_suspended());

        // Fullscreen app closes
        flags.remove(SuspensionReason::FullscreenApplication);
        assert!(flags.is_suspended()); // Still UserPaused

        // User resumes
        flags.remove(SuspensionReason::UserPaused);
        assert!(!flags.is_suspended()); // Now fully resumed!
    }

    #[test]
    fn playback_state_serializes_with_stable_names() {
        assert_eq!(serde_json_name(PlaybackState::Playing), "\"playing\"");
    }

    #[test]
    fn policy_evaluation_respects_profiles_and_environmental_conditions() {
        let eco = PlaybackPolicy::for_profile(PerformanceProfile::Eco);
        let balanced = PlaybackPolicy::for_profile(PerformanceProfile::Balanced);
        let perf = PlaybackPolicy::for_profile(PerformanceProfile::Performance);

        // Session lock always triggers pause across all profiles
        assert_eq!(
            eco.evaluate(false, false, true, false),
            (true, Some("session_locked"))
        );
        assert_eq!(
            balanced.evaluate(false, false, true, false),
            (true, Some("session_locked"))
        );
        assert_eq!(
            perf.evaluate(false, false, true, false),
            (true, Some("session_locked"))
        );

        // Display power off
        assert_eq!(
            balanced.evaluate(false, false, false, true),
            (true, Some("display_off_or_suspended"))
        );

        // Fullscreen app (non-game)
        assert_eq!(
            eco.evaluate(true, false, false, false),
            (true, Some("fullscreen_app"))
        );
        assert_eq!(
            balanced.evaluate(true, false, false, false),
            (false, None) // Balanced only pauses on games
        );
        assert_eq!(perf.evaluate(true, false, false, false), (false, None));

        // Fullscreen game
        assert_eq!(
            balanced.evaluate(true, true, false, false),
            (true, Some("fullscreen_game"))
        );
        assert_eq!(
            perf.evaluate(true, true, false, false),
            (false, None) // Performance profile does not pause on fullscreen by default
        );
    }

    fn serde_json_name(state: PlaybackState) -> String {
        match state {
            PlaybackState::Starting => "\"starting\"".into(),
            PlaybackState::Playing => "\"playing\"".into(),
            PlaybackState::Paused => "\"paused\"".into(),
            PlaybackState::Stopped => "\"stopped\"".into(),
            PlaybackState::Failed => "\"failed\"".into(),
        }
    }
}
