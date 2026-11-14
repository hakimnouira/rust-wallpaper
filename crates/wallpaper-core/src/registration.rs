//! Event-source registration status and bounded retry schedule.
//!
//! The Windows listener depends on several independent notifications. Any of them can fail to
//! register while the others work, and a half-registered listener must never be reported as
//! full coverage: a daemon that believes it will hear about screen locks, but does not, is
//! worse than one that knows it is blind. This module is platform-independent; the Win32 layer
//! only reports successes and failures into it.

use std::time::{Duration, Instant};

/// Everything the runtime relies on being told about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EventSource {
    /// `WTSRegisterSessionNotification`: lock / unlock.
    SessionNotifications,
    /// `RegisterPowerSettingNotification(GUID_CONSOLE_DISPLAY_STATE)`: display on / off.
    DisplayPower,
    /// `WM_POWERBROADCAST` suspend/resume; needs a live top-level window, no registration.
    SuspendResume,
    /// The registered `TaskbarCreated` broadcast: Explorer restarted.
    ExplorerRestart,
    /// `WM_DISPLAYCHANGE` broadcast; needs a live top-level (not message-only) window.
    DisplayTopology,
}

impl EventSource {
    pub const ALL: [EventSource; 5] = [
        EventSource::SessionNotifications,
        EventSource::DisplayPower,
        EventSource::SuspendResume,
        EventSource::ExplorerRestart,
        EventSource::DisplayTopology,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventSource::SessionNotifications => "session_notifications",
            EventSource::DisplayPower => "display_power",
            EventSource::SuspendResume => "suspend_resume",
            EventSource::ExplorerRestart => "explorer_restart",
            EventSource::DisplayTopology => "display_topology",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// State of one registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationState {
    /// Not attempted yet.
    Pending,
    /// Registered and delivering.
    Active,
    /// Failed; another attempt is scheduled.
    Retrying { attempts: u32, last_error: String },
    /// Failed and the retry budget is spent. Events from this source will not arrive.
    Failed { attempts: u32, last_error: String },
}

/// How much of the event surface is actually covered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// Every source is active.
    Full,
    /// Some sources are active; the listed ones are not.
    Partial { missing: Vec<EventSource> },
    /// Nothing is active.
    None,
}

/// Snapshot of all registrations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationReport {
    states: [RegistrationState; 5],
}

impl Default for RegistrationReport {
    fn default() -> Self {
        Self {
            states: std::array::from_fn(|_| RegistrationState::Pending),
        }
    }
}

impl RegistrationReport {
    pub fn state(&self, source: EventSource) -> &RegistrationState {
        &self.states[source.index()]
    }

    /// Full only when *every* source is active. Partial registration is never full coverage.
    pub fn coverage(&self) -> Coverage {
        let missing: Vec<EventSource> = EventSource::ALL
            .into_iter()
            .filter(|s| !matches!(self.state(*s), RegistrationState::Active))
            .collect();
        if missing.is_empty() {
            Coverage::Full
        } else if missing.len() == EventSource::ALL.len() {
            Coverage::None
        } else {
            Coverage::Partial { missing }
        }
    }

    pub fn is_full(&self) -> bool {
        self.coverage() == Coverage::Full
    }

    /// One human-readable line per source that is not active, for logs and `status` output.
    pub fn diagnostics(&self) -> Vec<String> {
        EventSource::ALL
            .into_iter()
            .filter_map(|s| match self.state(s) {
                RegistrationState::Active => None,
                RegistrationState::Pending => Some(format!("{}: not registered yet", s.as_str())),
                RegistrationState::Retrying {
                    attempts,
                    last_error,
                } => Some(format!(
                    "{}: registration failed (attempt {attempts}), retrying: {last_error}",
                    s.as_str()
                )),
                RegistrationState::Failed {
                    attempts,
                    last_error,
                } => Some(format!(
                    "{}: registration FAILED after {attempts} attempts, events from this source will NOT arrive: {last_error}",
                    s.as_str()
                )),
            })
            .collect()
    }
}

/// Bounded exponential retry schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts including the first one.
    pub max_attempts: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
        }
    }
}

impl RetryPolicy {
    /// Delay before the attempt following the `attempts`-th failure (1-based).
    pub fn delay_after(&self, attempts: u32) -> Duration {
        let shift = attempts.saturating_sub(1).min(16);
        self.initial_delay
            .saturating_mul(1u32 << shift)
            .min(self.max_delay)
    }
}

/// What to do after reporting a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    RetryAfter(Duration),
    GiveUp,
}

/// Tracks registrations and when each failed one should be retried.
#[derive(Debug, Clone)]
pub struct RegistrationTracker {
    report: RegistrationReport,
    policy: RetryPolicy,
    attempts: [u32; 5],
    next_retry: [Option<Instant>; 5],
}

impl RegistrationTracker {
    pub fn new(policy: RetryPolicy) -> Self {
        Self {
            report: RegistrationReport::default(),
            policy,
            attempts: [0; 5],
            next_retry: [None; 5],
        }
    }

    pub fn report(&self) -> &RegistrationReport {
        &self.report
    }

    pub fn policy(&self) -> &RetryPolicy {
        &self.policy
    }

    pub fn attempts(&self, source: EventSource) -> u32 {
        self.attempts[source.index()]
    }

    pub fn mark_active(&mut self, source: EventSource) {
        let i = source.index();
        self.attempts[i] += 1;
        self.next_retry[i] = None;
        self.report.states[i] = RegistrationState::Active;
    }

    /// Records a failed attempt and says whether to retry.
    pub fn record_failure(
        &mut self,
        source: EventSource,
        error: impl Into<String>,
        now: Instant,
    ) -> RetryDecision {
        let i = source.index();
        self.attempts[i] += 1;
        let attempts = self.attempts[i];
        let last_error = error.into();

        if attempts >= self.policy.max_attempts {
            self.next_retry[i] = None;
            self.report.states[i] = RegistrationState::Failed {
                attempts,
                last_error,
            };
            RetryDecision::GiveUp
        } else {
            let delay = self.policy.delay_after(attempts);
            self.next_retry[i] = Some(now + delay);
            self.report.states[i] = RegistrationState::Retrying {
                attempts,
                last_error,
            };
            RetryDecision::RetryAfter(delay)
        }
    }

    /// Sources whose retry time has arrived.
    pub fn due(&self, now: Instant) -> Vec<EventSource> {
        EventSource::ALL
            .into_iter()
            .filter(|s| self.next_retry[s.index()].is_some_and(|at| now >= at))
            .collect()
    }

    /// True while any retry is still scheduled (a timer is still needed).
    pub fn has_pending_retries(&self) -> bool {
        self.next_retry.iter().any(Option::is_some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn nothing_registered_is_no_coverage() {
        let t = RegistrationTracker::new(RetryPolicy::default());
        assert_eq!(t.report().coverage(), Coverage::None);
        assert!(!t.report().is_full());
    }

    #[test]
    fn all_active_is_full_coverage() {
        let mut t = RegistrationTracker::new(RetryPolicy::default());
        for s in EventSource::ALL {
            t.mark_active(s);
        }
        assert_eq!(t.report().coverage(), Coverage::Full);
        assert!(t.report().diagnostics().is_empty());
    }

    /// Partial registration must never be reported as full coverage.
    #[test]
    fn partial_registration_is_not_full_coverage() {
        let t0 = Instant::now();
        let mut t = RegistrationTracker::new(RetryPolicy::default());
        t.mark_active(EventSource::SuspendResume);
        t.mark_active(EventSource::DisplayTopology);
        t.mark_active(EventSource::ExplorerRestart);
        t.mark_active(EventSource::DisplayPower);
        t.record_failure(EventSource::SessionNotifications, "access denied", t0);

        assert!(!t.report().is_full());
        assert_eq!(
            t.report().coverage(),
            Coverage::Partial {
                missing: vec![EventSource::SessionNotifications]
            }
        );
        let diag = t.report().diagnostics();
        assert_eq!(diag.len(), 1);
        assert!(diag[0].contains("session_notifications") && diag[0].contains("access denied"));
    }

    #[test]
    fn retries_are_bounded_and_back_off_exponentially() {
        let t0 = Instant::now();
        let policy = RetryPolicy {
            max_attempts: 4,
            initial_delay: ms(500),
            max_delay: ms(1500),
        };
        let mut t = RegistrationTracker::new(policy);
        let src = EventSource::DisplayPower;

        assert_eq!(
            t.record_failure(src, "e1", t0),
            RetryDecision::RetryAfter(ms(500))
        );
        assert_eq!(
            t.record_failure(src, "e2", t0),
            RetryDecision::RetryAfter(ms(1000))
        );
        assert_eq!(
            t.record_failure(src, "e3", t0),
            RetryDecision::RetryAfter(ms(1500)),
            "capped"
        );
        assert_eq!(t.record_failure(src, "e4", t0), RetryDecision::GiveUp);

        assert!(matches!(
            t.report().state(src),
            RegistrationState::Failed { attempts: 4, .. }
        ));
        assert!(!t.has_pending_retries());
        assert!(
            t.report()
                .diagnostics()
                .iter()
                .any(|line| line.contains("display_power")
                    && line.contains("FAILED after 4 attempts"))
        );
    }

    #[test]
    fn due_reports_only_sources_whose_time_has_come() {
        let t0 = Instant::now();
        let mut t = RegistrationTracker::new(RetryPolicy::default());
        t.record_failure(EventSource::DisplayPower, "x", t0); // retry at +500ms
        assert!(t.due(t0 + ms(499)).is_empty());
        assert_eq!(t.due(t0 + ms(500)), vec![EventSource::DisplayPower]);
        assert!(t.has_pending_retries());
    }

    #[test]
    fn retry_that_succeeds_restores_full_coverage() {
        let t0 = Instant::now();
        let mut t = RegistrationTracker::new(RetryPolicy::default());
        for s in EventSource::ALL {
            if s != EventSource::SessionNotifications {
                t.mark_active(s);
            }
        }
        t.record_failure(EventSource::SessionNotifications, "busy", t0);
        assert!(!t.report().is_full());

        t.mark_active(EventSource::SessionNotifications);
        assert!(t.report().is_full());
        assert!(!t.has_pending_retries());
        assert_eq!(t.attempts(EventSource::SessionNotifications), 2);
    }

    #[test]
    fn failed_source_stays_failed_so_coverage_never_silently_recovers() {
        let t0 = Instant::now();
        let mut t = RegistrationTracker::new(RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        });
        for s in EventSource::ALL {
            t.mark_active(s);
        }
        t.record_failure(EventSource::ExplorerRestart, "no message id", t0);
        assert!(matches!(
            t.report().coverage(),
            Coverage::Partial { ref missing } if missing == &vec![EventSource::ExplorerRestart]
        ));
    }
}
