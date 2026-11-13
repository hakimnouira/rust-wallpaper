//! Playback supervisor managing debounced policy evaluations, system power/session events,
//! and process crash recovery with exponential backoff.

use std::time::{Duration, Instant};
use tracing::info;
use wallpaper_core::{PerformanceProfile, PlaybackPolicy};

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

/// Debouncer to prevent rapid oscillation of pause / resume states
/// (e.g. while launching or alt-tabbing between applications).
#[derive(Debug, Clone)]
pub struct StateDebouncer {
    target_state: bool,
    current_state: bool,
    last_change_intent: Instant,
    debounce_duration: Duration,
}

impl StateDebouncer {
    pub fn new(initial_state: bool, debounce_duration: Duration) -> Self {
        Self {
            target_state: initial_state,
            current_state: initial_state,
            last_change_intent: Instant::now(),
            debounce_duration,
        }
    }

    pub fn set_debounce_duration(&mut self, dur: Duration) {
        self.debounce_duration = dur;
    }

    /// Submits a requested state (e.g. `should_pause`).
    /// Returns `Some(new_state)` if the state change has stabilized for `debounce_duration`,
    /// or `None` if debounce is in progress or state hasn't changed.
    pub fn update(&mut self, requested_state: bool) -> Option<bool> {
        if requested_state != self.target_state {
            self.target_state = requested_state;
            self.last_change_intent = Instant::now();
        }

        if self.target_state != self.current_state {
            if self.last_change_intent.elapsed() >= self.debounce_duration {
                self.current_state = self.target_state;
                return Some(self.current_state);
            }
        }

        None
    }

    pub fn current_state(&self) -> bool {
        self.current_state
    }
}

/// Central supervisor coordinating playback policies, debouncing, session state, and recovery.
pub struct PlaybackSupervisor {
    pub policy: PlaybackPolicy,
    pub debouncer: StateDebouncer,
    pub session_locked: bool,
    pub display_off: bool,
    pub active_pause_reason: Option<&'static str>,
}

impl PlaybackSupervisor {
    pub fn new(policy: PlaybackPolicy) -> Self {
        let debouncer = StateDebouncer::new(false, Duration::from_millis(policy.debounce_millis));
        Self {
            policy,
            debouncer,
            session_locked: false,
            display_off: false,
            active_pause_reason: None,
        }
    }

    pub fn set_profile(&mut self, profile: PerformanceProfile) {
        self.policy = PlaybackPolicy::for_profile(profile);
        self.debouncer
            .set_debounce_duration(Duration::from_millis(self.policy.debounce_millis));
        info!(?profile, "Switched runtime performance profile");
    }

    /// Evaluates current state against policy and updates the debouncer.
    ///
    /// Returns `Some(true)` if playback should transition to paused,
    /// `Some(false)` if playback should transition to active/playing,
    /// or `None` if no transition is needed.
    pub fn evaluate_and_debounce(&mut self, is_fullscreen: bool, is_game: bool) -> Option<bool> {
        let (raw_should_pause, reason) = self.policy.evaluate(
            is_fullscreen,
            is_game,
            self.session_locked,
            self.display_off,
        );

        if raw_should_pause {
            self.active_pause_reason = reason;
        } else if self.debouncer.target_state {
            self.active_pause_reason = None;
        }

        self.debouncer.update(raw_should_pause)
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
    fn test_state_debouncer_filtering() {
        let mut debouncer = StateDebouncer::new(false, Duration::from_millis(50));

        // Rapid flickering should not trigger transition immediately
        assert_eq!(debouncer.update(true), None);
        assert_eq!(debouncer.current_state(), false);

        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(debouncer.update(true), Some(true));
        assert_eq!(debouncer.current_state(), true);
    }
}
