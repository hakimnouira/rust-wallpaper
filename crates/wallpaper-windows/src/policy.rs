//! Runtime playback policy: re-exports the platform-independent decision engine from
//! `wallpaper-core`, adapts [`WallpaperManager`] to it, and hosts process-crash backoff.
//!
//! The decision logic itself (suspension reasons, debouncing, reconciliation) lives in
//! [`wallpaper_core::runtime`] so it is unit-testable on any host. This module deliberately
//! contains no MPV or Win32 details: policy decisions stay separate from player mechanics.

use crate::manager::WallpaperManager;
use std::time::{Duration, Instant};
use wallpaper_core::PlaybackState;

pub use wallpaper_core::runtime::{
    ActualPlayback, ControlCommand, ForegroundObservation, MonitorStatus, PendingWork,
    PlaybackSink, PlayerCommand, PlayerSuspensionManager, RuntimeEvent, StateDebouncer,
};

/// The playback supervisor: authoritative runtime state machine owning the
/// [`PlayerSuspensionManager`], debouncers and the reconciler.
pub use wallpaper_core::runtime::RuntimeState as PlaybackSupervisor;

/// Lets the reconciler drive the real manager. Carries out commands and reports state;
/// it decides nothing. Uses only the manager's per-monitor API, never `pause_all` /
/// `resume_all`, so one monitor's reasons can never override another's.
impl PlaybackSink for WallpaperManager {
    fn player_states(&self) -> Vec<(String, ActualPlayback)> {
        self.player_statuses()
            .into_iter()
            .map(|(device, _decoder, state, _memory)| {
                let actual = match state {
                    PlaybackState::Playing => ActualPlayback::Playing,
                    PlaybackState::Paused => ActualPlayback::Paused,
                    PlaybackState::Starting | PlaybackState::Stopped | PlaybackState::Failed => {
                        ActualPlayback::Unavailable
                    }
                };
                (device, actual)
            })
            .collect()
    }

    fn pause_player(&mut self, device: &str) {
        self.pause_monitor(device);
    }

    fn resume_player(&mut self, device: &str) {
        self.resume_monitor(device);
    }
}

/// State of an active supervisor retry loop.
#[derive(Debug, Clone)]
pub struct ProcessBackoff {
    pub consecutive_failures: u32,
    pub max_failures: u32,
    pub last_failure_at: Option<Instant>,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for ProcessBackoff {
    fn default() -> Self {
        Self {
            consecutive_failures: 0,
            max_failures: 5,
            last_failure_at: None,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(16),
        }
    }
}

impl ProcessBackoff {
    pub fn new(max_failures: u32, initial_delay: Duration, max_delay: Duration) -> Self {
        Self {
            consecutive_failures: 0,
            max_failures,
            last_failure_at: None,
            initial_delay,
            max_delay,
        }
    }

    /// Records a process crash or launch failure.
    pub fn record_failure(&mut self) -> bool {
        self.consecutive_failures += 1;
        self.last_failure_at = Some(Instant::now());
        self.consecutive_failures <= self.max_failures
    }

    /// Records successful operation, resetting consecutive failure counts.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.last_failure_at = None;
    }

    /// Calculates current required backoff delay before the next retry.
    pub fn current_backoff_delay(&self) -> Duration {
        if self.consecutive_failures == 0 {
            return Duration::ZERO;
        }

        let factor = 2u32.saturating_pow(self.consecutive_failures.saturating_sub(1));
        let computed = self.initial_delay.saturating_mul(factor);
        computed.min(self.max_delay)
    }

    /// Checks if sufficient backoff time has elapsed to attempt another launch/recovery.
    pub fn can_retry(&self) -> bool {
        if self.consecutive_failures == 0 {
            return true;
        }
        if self.consecutive_failures > self.max_failures {
            return false;
        }
        if let Some(last) = self.last_failure_at {
            last.elapsed() >= self.current_backoff_delay()
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_exponential_growth_and_limit() {
        let mut backoff =
            ProcessBackoff::new(3, Duration::from_millis(100), Duration::from_millis(800));

        assert_eq!(backoff.current_backoff_delay(), Duration::ZERO);
        assert!(backoff.can_retry());

        // Failure 1: 100ms
        assert!(backoff.record_failure());
        assert_eq!(backoff.current_backoff_delay(), Duration::from_millis(100));

        // Failure 2: 200ms
        assert!(backoff.record_failure());
        assert_eq!(backoff.current_backoff_delay(), Duration::from_millis(200));

        // Failure 3: 400ms (max_failures reached)
        assert!(backoff.record_failure());
        assert_eq!(backoff.current_backoff_delay(), Duration::from_millis(400));

        // Failure 4: exceeded max_failures
        assert!(!backoff.record_failure());
        assert!(!backoff.can_retry());

        // Recovery reset
        backoff.record_success();
        assert_eq!(backoff.consecutive_failures, 0);
        assert!(backoff.can_retry());
    }

    #[test]
    fn test_player_suspension_manager_coexistence() {
        use wallpaper_core::SuspensionReason;
        let mut mgr = PlayerSuspensionManager::new();
        assert!(!mgr.should_suspend(r"\\.\DISPLAY1"));

        mgr.add_global(SuspensionReason::SessionLocked);
        mgr.add_monitor(r"\\.\DISPLAY1", SuspensionReason::UserPaused);
        mgr.remove_global(SuspensionReason::SessionLocked);

        // DISPLAY1 must remain suspended because of UserPaused; DISPLAY2 resumes.
        assert!(mgr.should_suspend(r"\\.\DISPLAY1"));
        assert!(!mgr.should_suspend(r"\\.\DISPLAY2"));

        mgr.remove_monitor(r"\\.\DISPLAY1", SuspensionReason::UserPaused);
        assert!(!mgr.should_suspend(r"\\.\DISPLAY1"));
    }

    #[test]
    fn test_state_debouncer_filtering() {
        let t0 = Instant::now();
        let mut debouncer = StateDebouncer::new(false, Duration::from_millis(50));

        assert_eq!(debouncer.update_at(true, t0), None);
        assert!(!debouncer.current_state());
        assert_eq!(
            debouncer.update_at(true, t0 + Duration::from_millis(60)),
            Some(true)
        );
        assert!(debouncer.current_state());
    }
}
