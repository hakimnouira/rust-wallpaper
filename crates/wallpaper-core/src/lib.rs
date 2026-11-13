//! Platform-independent domain types for the wallpaper engine.

pub mod library;
pub mod runtime;

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

impl SuspensionReason {
    /// Every reason, in the fixed order used for deterministic reporting.
    pub const ALL: [SuspensionReason; 6] = [
        SuspensionReason::UserPaused,
        SuspensionReason::FullscreenApplication,
        SuspensionReason::SessionLocked,
        SuspensionReason::DisplayPoweredOff,
        SuspensionReason::SystemSuspended,
        SuspensionReason::RuntimeError,
    ];

    /// Stable snake_case name, identical to the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            SuspensionReason::UserPaused => "user_paused",
            SuspensionReason::FullscreenApplication => "fullscreen_application",
            SuspensionReason::SessionLocked => "session_locked",
            SuspensionReason::DisplayPoweredOff => "display_powered_off",
            SuspensionReason::SystemSuspended => "system_suspended",
            SuspensionReason::RuntimeError => "runtime_error",
        }
    }
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

    /// Returns the active reasons in the fixed [`SuspensionReason::ALL`] order.
    pub fn active_reasons(&self) -> Vec<SuspensionReason> {
        SuspensionReason::ALL
            .into_iter()
            .filter(|reason| self.reasons.contains(reason))
            .collect()
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

/// What the most relevant window on a monitor looks like, ordered by how strongly
/// it suggests the wallpaper is hidden or the user is busy with something demanding.
///
/// The derived `Ord` is load-bearing: a policy pauses when the observed kind is
/// `>=` its threshold, so the declaration order below is the severity order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForegroundKind {
    /// Nothing relevant: empty desktop, shell/taskbar/Start, or our own windows.
    None,
    /// An ordinary window that does not fill the monitor.
    Normal,
    /// A maximized window that keeps its normal chrome (title bar / frame).
    /// Typical productivity app: browser, editor, file manager.
    Maximized,
    /// A window covering the whole monitor with its chrome stripped, or a
    /// recognised game-engine window: borderless-fullscreen games, F11 video.
    BorderlessFullscreen,
    /// Exclusive / Direct3D fullscreen, as reported by the shell itself.
    ExclusiveFullscreen,
}

/// Performance profiles governing resource usage and playback pause triggers.
///
/// | Profile     | Pauses for foreground...                | pause / resume debounce | poll   |
/// |-------------|------------------------------------------|-------------------------|--------|
/// | Eco         | maximized, borderless, exclusive         | 1000 ms / 2000 ms       | 500 ms |
/// | Balanced    | borderless, exclusive                    |  500 ms / 1500 ms       | 250 ms |
/// | Performance | exclusive fullscreen only                |  300 ms /  750 ms       | 200 ms |
///
/// All profiles pause for session lock, display power-off and system suspend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceProfile {
    /// Aggressive pause behavior: pauses on any maximized or fullscreen window and
    /// polls the foreground window less often.
    Eco,
    /// Default profile: pauses during borderless/exclusive fullscreen applications and
    /// games but keeps playing behind ordinary maximized productivity apps.
    Balanced,
    /// Maximizes uptime: keeps playing behind maximized and borderless apps and only
    /// pauses for exclusive fullscreen (or the session/power events every profile honours).
    Performance,
}

impl Default for PerformanceProfile {
    fn default() -> Self {
        Self::Balanced
    }
}

impl PerformanceProfile {
    /// Parses a profile name as typed on a CLI or control channel.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "eco" => Some(Self::Eco),
            "balanced" | "default" => Some(Self::Balanced),
            "performance" | "perf" => Some(Self::Performance),
            _ => None,
        }
    }
}

/// Policy configuration controlling when wallpaper playback automatically pauses or resumes.
///
/// This type only describes *decisions*; it knows nothing about MPV or Win32.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybackPolicy {
    /// Active performance profile.
    pub profile: PerformanceProfile,
    /// Lowest [`ForegroundKind`] that suspends a monitor, or `None` to never suspend
    /// for foreground windows.
    pub foreground_pause_threshold: Option<ForegroundKind>,
    /// Whether to pause when the workstation is locked.
    pub pause_on_lock: bool,
    /// Whether to pause while display(s) are powered off.
    pub pause_on_display_power_off: bool,
    /// Whether to pause while the system is suspended / sleeping.
    pub pause_on_system_suspend: bool,
    /// How long a foreground condition must hold before playback is paused.
    pub pause_debounce_millis: u64,
    /// How long the condition must stay cleared before playback resumes.
    /// Longer than the pause delay so alt-tabbing does not oscillate.
    pub resume_debounce_millis: u64,
    /// How often the foreground window is sampled.
    pub poll_interval_millis: u64,
}

impl Default for PlaybackPolicy {
    fn default() -> Self {
        Self::for_profile(PerformanceProfile::Balanced)
    }
}

impl PlaybackPolicy {
    pub fn for_profile(profile: PerformanceProfile) -> Self {
        let (threshold, pause_ms, resume_ms, poll_ms) = match profile {
            PerformanceProfile::Eco => (ForegroundKind::Maximized, 1000, 2000, 500),
            PerformanceProfile::Balanced => (ForegroundKind::BorderlessFullscreen, 500, 1500, 250),
            PerformanceProfile::Performance => (ForegroundKind::ExclusiveFullscreen, 300, 750, 200),
        };
        Self {
            profile,
            foreground_pause_threshold: Some(threshold),
            pause_on_lock: true,
            pause_on_display_power_off: true,
            pause_on_system_suspend: true,
            pause_debounce_millis: pause_ms,
            resume_debounce_millis: resume_ms,
            poll_interval_millis: poll_ms,
        }
    }

    /// Returns true when a foreground window of this kind should suspend its monitor.
    pub fn pauses_for(&self, kind: ForegroundKind) -> bool {
        match self.foreground_pause_threshold {
            Some(threshold) => kind != ForegroundKind::None && kind >= threshold,
            None => false,
        }
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
    fn profiles_pause_for_documented_foreground_kinds() {
        use ForegroundKind::*;
        let eco = PlaybackPolicy::for_profile(PerformanceProfile::Eco);
        let balanced = PlaybackPolicy::for_profile(PerformanceProfile::Balanced);
        let perf = PlaybackPolicy::for_profile(PerformanceProfile::Performance);

        // (kind, eco, balanced, performance)
        let table = [
            (None, false, false, false),
            (Normal, false, false, false),
            (Maximized, true, false, false),
            (BorderlessFullscreen, true, true, false),
            (ExclusiveFullscreen, true, true, true),
        ];
        for (kind, e, b, p) in table {
            assert_eq!(eco.pauses_for(kind), e, "eco {kind:?}");
            assert_eq!(balanced.pauses_for(kind), b, "balanced {kind:?}");
            assert_eq!(perf.pauses_for(kind), p, "performance {kind:?}");
        }
    }

    #[test]
    fn profiles_have_ordered_debounce_and_polling() {
        let eco = PlaybackPolicy::for_profile(PerformanceProfile::Eco);
        let balanced = PlaybackPolicy::for_profile(PerformanceProfile::Balanced);
        let perf = PlaybackPolicy::for_profile(PerformanceProfile::Performance);

        for p in [&eco, &balanced, &perf] {
            assert!(p.pause_on_lock && p.pause_on_display_power_off && p.pause_on_system_suspend);
            assert!(
                p.resume_debounce_millis > p.pause_debounce_millis,
                "resume must be slower than pause to damp oscillation"
            );
        }
        assert!(eco.pause_debounce_millis > balanced.pause_debounce_millis);
        assert!(balanced.pause_debounce_millis > perf.pause_debounce_millis);
        assert!(eco.poll_interval_millis > balanced.poll_interval_millis);
        assert!(balanced.poll_interval_millis > perf.poll_interval_millis);
    }

    #[test]
    fn foreground_kind_order_is_severity_order() {
        use ForegroundKind::*;
        assert!(None < Normal && Normal < Maximized);
        assert!(Maximized < BorderlessFullscreen && BorderlessFullscreen < ExclusiveFullscreen);
    }

    #[test]
    fn profile_names_parse() {
        assert_eq!(
            PerformanceProfile::parse("ECO"),
            Some(PerformanceProfile::Eco)
        );
        assert_eq!(
            PerformanceProfile::parse(" perf "),
            Some(PerformanceProfile::Performance)
        );
        assert_eq!(
            PerformanceProfile::parse("balanced"),
            Some(PerformanceProfile::Balanced)
        );
        assert_eq!(PerformanceProfile::parse("turbo"), Option::None);
    }

    #[test]
    fn active_reasons_are_reported_in_fixed_order() {
        let mut flags = SuspensionFlags::new();
        flags.add(SuspensionReason::RuntimeError);
        flags.add(SuspensionReason::UserPaused);
        flags.add(SuspensionReason::DisplayPoweredOff);
        assert_eq!(
            flags.active_reasons(),
            vec![
                SuspensionReason::UserPaused,
                SuspensionReason::DisplayPoweredOff,
                SuspensionReason::RuntimeError
            ]
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
