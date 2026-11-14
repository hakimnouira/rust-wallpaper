//! Player health model.
//!
//! "Is the player healthy?" is several independent questions, and conflating them is how an
//! unhealthy player ends up counted as fine. This module keeps them apart:
//!
//! | Dimension                    | Field                         | Source today                      |
//! |------------------------------|-------------------------------|-----------------------------------|
//! | player process exists        | `process_exists`              | host has an entry for the monitor |
//! | player process is alive      | `process_alive`               | host `try_wait` (else `Unknown`)  |
//! | MPV IPC is responsive        | `ipc_responsive`              | probe rounds + failed commands    |
//! | video output is initialized  | `video_initialized`           | probe rounds                      |
//! | playback is Playing/Paused   | `playback`                    | host-reported state               |
//! | recovery is in progress      | `recovery`                    | host, else inferred from absence  |
//! | recovery budget is exhausted | `recovery`                    | host, else time-based budget      |
//!
//! Signals the host cannot supply stay [`Signal::Unknown`]; they are never guessed as healthy
//! *or* unhealthy. The verdict only fails on positive evidence, except that a player which has
//! not reached `Playing`/`Paused` after the startup grace is unhealthy.
//!
//! All time-dependent logic takes `now: Instant`, so tests are deterministic.

use crate::PlaybackState;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A tri-state fact: the host may be unable to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Yes,
    No,
    Unknown,
}

impl Signal {
    pub fn from_bool(value: bool) -> Self {
        if value {
            Signal::Yes
        } else {
            Signal::No
        }
    }
}

/// Recovery progress of a player that stopped being healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryState {
    /// Nothing is being recovered.
    Idle,
    /// A restart is under way or scheduled.
    InProgress { attempt: u32 },
    /// The retry budget is spent: automatic recovery has stopped.
    Exhausted { attempts: u32 },
}

/// Non-blocking facts about one player, as reported by the host.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerReport {
    pub device: String,
    pub state: PlaybackState,
    /// Process uptime if the host knows it. When `None` the tracker measures time since it
    /// first observed the player.
    pub uptime: Option<Duration>,
    pub memory_bytes: u64,
    /// The OS says the child has not exited. `Unknown` when the host cannot tell.
    pub process_alive: Signal,
    /// Host-side recovery progress, when the host exposes it.
    pub recovery: Option<RecoveryState>,
}

/// Outcome of one IPC probe of one player. Probes can block (MPV IPC waits up to 1.5 s per
/// command), so they are scheduled separately from the cheap [`PlayerReport`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub device: String,
    /// At least one property query was answered.
    pub ipc_responded: bool,
    /// Video dimensions were reported, so a video output exists.
    pub video_initialized: bool,
}

/// Why a player is considered unhealthy. Listed in precedence order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnhealthyCause {
    ProcessDead,
    PlaybackFailed,
    PlaybackStopped,
    StartTimeout,
    IpcUnresponsive,
    VideoNotInitialized,
}

/// Overall verdict for one assigned monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    /// Every known dimension is good.
    Healthy,
    /// Launched recently and not yet `Playing`; within the startup grace. Not healthy yet,
    /// but not an error either.
    Starting,
    /// Positive evidence of a fault.
    Unhealthy(UnhealthyCause),
    /// An assignment exists but no player does.
    Missing,
    /// Unhealthy or missing, with the host's automatic recovery still within budget.
    Recovering,
    /// Unhealthy or missing, and recovery has run out of budget.
    RecoveryExhausted,
}

impl HealthStatus {
    pub fn is_healthy(self) -> bool {
        matches!(self, HealthStatus::Healthy)
    }

    /// Whether this verdict maps to [`SuspensionReason::RuntimeError`](crate::SuspensionReason).
    /// `Starting` is deliberately *not* an error: it is neither healthy nor faulty yet.
    pub fn is_error(self) -> bool {
        !matches!(self, HealthStatus::Healthy | HealthStatus::Starting)
    }
}

/// Full per-monitor health picture.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerHealth {
    pub device: String,
    pub status: HealthStatus,
    pub process_exists: bool,
    pub process_alive: Signal,
    pub ipc_responsive: Signal,
    pub video_initialized: Signal,
    pub playback: Option<PlaybackState>,
    pub recovery: RecoveryState,
}

impl PlayerHealth {
    /// Names of dimensions that could not be verified, for honest diagnostics.
    pub fn unverified(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.process_exists {
            if self.process_alive == Signal::Unknown {
                out.push("process_alive");
            }
            if self.ipc_responsive == Signal::Unknown {
                out.push("ipc_responsive");
            }
            if self.video_initialized == Signal::Unknown {
                out.push("video_initialized");
            }
        }
        out
    }
}

/// Tunables for [`HealthTracker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthPolicy {
    /// How long a new player may stay in `Starting` or without video before it is a fault.
    pub startup_grace: Duration,
    /// Consecutive probe rounds with no IPC answer before IPC counts as unresponsive.
    pub ipc_failure_threshold: u32,
    /// Consecutive commands that had no effect before IPC counts as unresponsive.
    pub command_failure_threshold: u32,
    /// How long a monitor may stay unhealthy or missing before recovery counts as exhausted,
    /// when the host does not report its own retry budget.
    pub recovery_budget: Duration,
}

impl Default for HealthPolicy {
    fn default() -> Self {
        Self {
            startup_grace: Duration::from_secs(10),
            ipc_failure_threshold: 3,
            command_failure_threshold: 2,
            // Longer than the manager's own backoff schedule (0.5+1+2+4+8 s) plus launches.
            recovery_budget: Duration::from_secs(45),
        }
    }
}

#[derive(Debug, Clone)]
struct DeviceTrack {
    present_since: Instant,
    last_uptime: Option<Duration>,
    probe_failures: u32,
    probe_ok_seen: bool,
    video: Signal,
    error_since: Option<Instant>,
}

impl DeviceTrack {
    fn new(now: Instant) -> Self {
        Self {
            present_since: now,
            last_uptime: None,
            probe_failures: 0,
            probe_ok_seen: false,
            video: Signal::Unknown,
            error_since: None,
        }
    }

    /// A fresh process: counters from the previous one no longer apply.
    fn reset_for_new_process(&mut self, now: Instant) {
        *self = Self::new(now);
    }
}

/// Stateful evaluator turning reports and probe results into [`PlayerHealth`].
#[derive(Debug, Clone)]
pub struct HealthTracker {
    policy: HealthPolicy,
    tracks: HashMap<String, DeviceTrack>,
    /// Time since which each device has had no player at all.
    missing_since: HashMap<String, Instant>,
}

impl HealthTracker {
    pub fn new(policy: HealthPolicy) -> Self {
        Self {
            policy,
            tracks: HashMap::new(),
            missing_since: HashMap::new(),
        }
    }

    pub fn policy(&self) -> &HealthPolicy {
        &self.policy
    }

    /// Records an IPC probe round for one player.
    pub fn observe_probe(&mut self, probe: &ProbeResult, now: Instant) {
        let track = self
            .tracks
            .entry(probe.device.clone())
            .or_insert_with(|| DeviceTrack::new(now));
        if probe.ipc_responded {
            track.probe_failures = 0;
            track.probe_ok_seen = true;
            track.video = Signal::from_bool(probe.video_initialized);
        } else {
            track.probe_failures = track.probe_failures.saturating_add(1);
        }
    }

    /// Drops all state for a device (monitor removed).
    pub fn forget(&mut self, device: &str) {
        self.tracks.remove(device);
        self.missing_since.remove(device);
    }

    /// Evaluates every assigned device. `command_failures` reports, per device, how many
    /// consecutive pause/resume commands failed to take effect.
    pub fn evaluate(
        &mut self,
        assigned: &[String],
        reports: &[PlayerReport],
        command_failures: impl Fn(&str) -> u32,
        now: Instant,
    ) -> Vec<PlayerHealth> {
        // Devices no longer assigned must not keep stale state.
        let keep: std::collections::HashSet<&str> = assigned.iter().map(String::as_str).collect();
        self.tracks.retain(|d, _| keep.contains(d.as_str()));
        self.missing_since.retain(|d, _| keep.contains(d.as_str()));

        assigned
            .iter()
            .map(|device| {
                let report = reports.iter().find(|r| &r.device == device);
                self.evaluate_one(device, report, command_failures(device), now)
            })
            .collect()
    }

    fn evaluate_one(
        &mut self,
        device: &str,
        report: Option<&PlayerReport>,
        command_failures: u32,
        now: Instant,
    ) -> PlayerHealth {
        let Some(report) = report else {
            return self.evaluate_missing(device, now);
        };
        self.missing_since.remove(device);

        let track = self
            .tracks
            .entry(device.to_string())
            .or_insert_with(|| DeviceTrack::new(now));

        // Uptime going backwards means the host replaced the process.
        if let (Some(prev), Some(cur)) = (track.last_uptime, report.uptime) {
            if cur + Duration::from_secs(1) < prev {
                track.reset_for_new_process(now);
            }
        }
        track.last_uptime = report.uptime;
        let uptime = report
            .uptime
            .unwrap_or_else(|| now.saturating_duration_since(track.present_since));

        // --- individual dimensions --------------------------------------------------
        let ipc = if track.probe_failures >= self.policy.ipc_failure_threshold
            || command_failures >= self.policy.command_failure_threshold
        {
            Signal::No
        } else if track.probe_ok_seen {
            Signal::Yes
        } else {
            Signal::Unknown
        };
        let video = track.video;
        let in_grace = uptime < self.policy.startup_grace;

        // --- verdict: first positive evidence wins, in precedence order -------------
        let cause = if report.process_alive == Signal::No {
            Some(UnhealthyCause::ProcessDead)
        } else {
            match report.state {
                PlaybackState::Failed => Some(UnhealthyCause::PlaybackFailed),
                PlaybackState::Stopped => Some(UnhealthyCause::PlaybackStopped),
                PlaybackState::Starting if !in_grace => Some(UnhealthyCause::StartTimeout),
                _ if ipc == Signal::No => Some(UnhealthyCause::IpcUnresponsive),
                PlaybackState::Playing | PlaybackState::Paused
                    if video == Signal::No && ipc == Signal::Yes && !in_grace =>
                {
                    Some(UnhealthyCause::VideoNotInitialized)
                }
                _ => None,
            }
        };

        let status = match cause {
            Some(cause) => {
                self.error_status(device, report.recovery, now, HealthStatus::Unhealthy(cause))
            }
            None if report.state == PlaybackState::Starting => {
                self.clear_error(device);
                HealthStatus::Starting
            }
            None => {
                self.clear_error(device);
                HealthStatus::Healthy
            }
        };

        PlayerHealth {
            device: device.to_string(),
            status,
            process_exists: true,
            process_alive: report.process_alive,
            ipc_responsive: ipc,
            video_initialized: video,
            playback: Some(report.state),
            recovery: report.recovery.unwrap_or(RecoveryState::Idle),
        }
    }

    fn evaluate_missing(&mut self, device: &str, now: Instant) -> PlayerHealth {
        // The previous process is gone: its counters must not leak into the next one.
        self.tracks.remove(device);
        self.missing_since.entry(device.to_string()).or_insert(now);
        let status = self.error_status(device, None, now, HealthStatus::Missing);
        PlayerHealth {
            device: device.to_string(),
            status,
            process_exists: false,
            process_alive: Signal::No,
            ipc_responsive: Signal::No,
            video_initialized: Signal::No,
            playback: None,
            recovery: match status {
                HealthStatus::RecoveryExhausted => RecoveryState::Exhausted { attempts: 0 },
                _ => RecoveryState::InProgress { attempt: 0 },
            },
        }
    }

    /// Applies the recovery budget to an error verdict.
    fn error_status(
        &mut self,
        device: &str,
        host_recovery: Option<RecoveryState>,
        now: Instant,
        base: HealthStatus,
    ) -> HealthStatus {
        if let Some(RecoveryState::Exhausted { .. }) = host_recovery {
            return HealthStatus::RecoveryExhausted;
        }

        let since = match self.tracks.get_mut(device) {
            Some(track) => *track.error_since.get_or_insert(now),
            None => *self.missing_since.entry(device.to_string()).or_insert(now),
        };
        if now.saturating_duration_since(since) >= self.policy.recovery_budget {
            return HealthStatus::RecoveryExhausted;
        }

        match (base, host_recovery) {
            // The host says it is recovering: that is the more specific truth.
            (_, Some(RecoveryState::InProgress { .. })) => HealthStatus::Recovering,
            (HealthStatus::Missing, _) => HealthStatus::Missing,
            (other, _) => other,
        }
    }

    fn clear_error(&mut self, device: &str) {
        if let Some(track) = self.tracks.get_mut(device) {
            track.error_since = None;
        }
    }
}

/// Adaptive scheduler for blocking IPC probe rounds.
///
/// Healthy players are probed at `base`. After a round in which any probe failed, the interval
/// doubles up to `max`, so a hung MPV cannot keep the daemon loop blocked: the worst-case duty
/// cycle shrinks with every failed round. One fully successful round resets it.
#[derive(Debug, Clone)]
pub struct ProbeScheduler {
    base: Duration,
    max: Duration,
    current: Duration,
    last_round: Option<Instant>,
}

impl ProbeScheduler {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            current: base,
            last_round: None,
        }
    }

    pub fn interval(&self) -> Duration {
        self.current
    }

    pub fn is_due(&self, now: Instant) -> bool {
        match self.last_round {
            None => true,
            Some(last) => now.saturating_duration_since(last) >= self.current,
        }
    }

    /// Records that a round ran. `all_ok` is true when every probed player answered.
    pub fn record_round(&mut self, now: Instant, all_ok: bool) {
        self.last_round = Some(now);
        self.current = if all_ok {
            self.base
        } else {
            (self.current * 2).min(self.max)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D1: &str = r"\\.\DISPLAY1";

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn report(state: PlaybackState) -> PlayerReport {
        PlayerReport {
            device: D1.to_string(),
            state,
            uptime: None,
            memory_bytes: 100 << 20,
            process_alive: Signal::Unknown,
            recovery: None,
        }
    }

    fn probe(ok: bool, video: bool) -> ProbeResult {
        ProbeResult {
            device: D1.to_string(),
            ipc_responded: ok,
            video_initialized: video,
        }
    }

    fn eval(
        tracker: &mut HealthTracker,
        report: Option<PlayerReport>,
        cmd_failures: u32,
        now: Instant,
    ) -> PlayerHealth {
        let reports: Vec<PlayerReport> = report.into_iter().collect();
        tracker
            .evaluate(&[D1.to_string()], &reports, |_| cmd_failures, now)
            .remove(0)
    }

    #[test]
    fn playing_with_working_ipc_and_video_is_healthy() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        t.observe_probe(&probe(true, true), t0);
        let h = eval(
            &mut t,
            Some(report(PlaybackState::Playing)),
            0,
            t0 + ms(100),
        );
        assert_eq!(h.status, HealthStatus::Healthy);
        assert_eq!(h.ipc_responsive, Signal::Yes);
        assert_eq!(h.video_initialized, Signal::Yes);
        assert!(h.process_exists);
    }

    #[test]
    fn unverifiable_dimensions_stay_unknown_and_do_not_fail_a_playing_player() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        let h = eval(&mut t, Some(report(PlaybackState::Playing)), 0, t0);
        assert_eq!(h.status, HealthStatus::Healthy);
        assert_eq!(
            h.unverified(),
            vec!["process_alive", "ipc_responsive", "video_initialized"]
        );
    }

    #[test]
    fn starting_is_not_healthy_but_not_an_error_within_grace() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        let h = eval(&mut t, Some(report(PlaybackState::Starting)), 0, t0);
        assert_eq!(h.status, HealthStatus::Starting);
        assert!(!h.status.is_healthy());
        assert!(!h.status.is_error());
    }

    #[test]
    fn starting_beyond_grace_is_a_fault() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        eval(&mut t, Some(report(PlaybackState::Starting)), 0, t0);
        let h = eval(
            &mut t,
            Some(report(PlaybackState::Starting)),
            0,
            t0 + ms(10_500),
        );
        assert_eq!(
            h.status,
            HealthStatus::Unhealthy(UnhealthyCause::StartTimeout)
        );
        assert!(h.status.is_error());
    }

    #[test]
    fn failed_and_stopped_states_never_count_as_healthy() {
        let t0 = Instant::now();
        for (state, cause) in [
            (PlaybackState::Failed, UnhealthyCause::PlaybackFailed),
            (PlaybackState::Stopped, UnhealthyCause::PlaybackStopped),
        ] {
            let mut t = HealthTracker::new(HealthPolicy::default());
            let h = eval(&mut t, Some(report(state)), 0, t0);
            assert_eq!(h.status, HealthStatus::Unhealthy(cause), "{state:?}");
            assert!(h.status.is_error() && !h.status.is_healthy());
        }
    }

    /// Process alive (OS says so) but IPC unavailable: probes keep failing.
    #[test]
    fn process_alive_but_ipc_unavailable_is_unhealthy_after_threshold() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        let mut alive = report(PlaybackState::Playing);
        alive.process_alive = Signal::Yes;

        for i in 0..2u64 {
            t.observe_probe(&probe(false, false), t0 + ms(i * 2000));
            let h = eval(&mut t, Some(alive.clone()), 0, t0 + ms(i * 2000));
            assert_eq!(
                h.status,
                HealthStatus::Healthy,
                "below threshold after {i} failures"
            );
        }
        t.observe_probe(&probe(false, false), t0 + ms(4000));
        let h = eval(&mut t, Some(alive), 0, t0 + ms(4000));
        assert_eq!(h.process_alive, Signal::Yes, "process is alive");
        assert_eq!(h.ipc_responsive, Signal::No, "but IPC is not");
        assert_eq!(
            h.status,
            HealthStatus::Unhealthy(UnhealthyCause::IpcUnresponsive)
        );
    }

    #[test]
    fn failed_commands_are_evidence_of_dead_ipc() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        let h = eval(&mut t, Some(report(PlaybackState::Playing)), 2, t0);
        assert_eq!(
            h.status,
            HealthStatus::Unhealthy(UnhealthyCause::IpcUnresponsive)
        );
    }

    #[test]
    fn one_good_probe_clears_ipc_failures() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        for i in 0..3u64 {
            t.observe_probe(&probe(false, false), t0 + ms(i));
        }
        assert!(eval(&mut t, Some(report(PlaybackState::Playing)), 0, t0)
            .status
            .is_error());
        t.observe_probe(&probe(true, true), t0 + ms(10));
        assert!(
            eval(&mut t, Some(report(PlaybackState::Playing)), 0, t0 + ms(10))
                .status
                .is_healthy()
        );
    }

    #[test]
    fn dead_process_is_unhealthy_regardless_of_reported_state() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        let mut r = report(PlaybackState::Playing); // stale: state never updates on death
        r.process_alive = Signal::No;
        let h = eval(&mut t, Some(r), 0, t0);
        assert_eq!(
            h.status,
            HealthStatus::Unhealthy(UnhealthyCause::ProcessDead)
        );
    }

    #[test]
    fn video_output_missing_after_grace_is_a_fault_but_not_before() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        t.observe_probe(&probe(true, false), t0); // IPC fine, no video yet
        assert!(eval(
            &mut t,
            Some(report(PlaybackState::Playing)),
            0,
            t0 + ms(500)
        )
        .status
        .is_healthy());
        t.observe_probe(&probe(true, false), t0 + ms(11_000));
        let h = eval(
            &mut t,
            Some(report(PlaybackState::Playing)),
            0,
            t0 + ms(11_000),
        );
        assert_eq!(
            h.status,
            HealthStatus::Unhealthy(UnhealthyCause::VideoNotInitialized)
        );
    }

    #[test]
    fn missing_player_is_an_error_and_recovery_budget_exhausts_then_resets() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());

        let h = eval(&mut t, None, 0, t0);
        assert_eq!(h.status, HealthStatus::Missing);
        assert!(!h.process_exists && h.status.is_error());

        // Still within the 45 s budget.
        assert_eq!(
            eval(&mut t, None, 0, t0 + ms(44_000)).status,
            HealthStatus::Missing
        );
        // Budget spent.
        let h = eval(&mut t, None, 0, t0 + ms(45_000));
        assert_eq!(h.status, HealthStatus::RecoveryExhausted);
        assert!(matches!(h.recovery, RecoveryState::Exhausted { .. }));

        // The player comes back and works: exhaustion is cleared.
        t.observe_probe(&probe(true, true), t0 + ms(50_000));
        let h = eval(
            &mut t,
            Some(report(PlaybackState::Playing)),
            0,
            t0 + ms(50_000),
        );
        assert_eq!(h.status, HealthStatus::Healthy);

        // A later outage gets a fresh budget rather than inheriting the old clock.
        let h = eval(&mut t, None, 0, t0 + ms(60_000));
        assert_eq!(h.status, HealthStatus::Missing);
        assert_eq!(
            eval(&mut t, None, 0, t0 + ms(100_000)).status,
            HealthStatus::Missing
        );
        assert_eq!(
            eval(&mut t, None, 0, t0 + ms(105_000)).status,
            HealthStatus::RecoveryExhausted
        );
    }

    #[test]
    fn host_reported_recovery_state_is_authoritative() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        let mut r = report(PlaybackState::Playing);
        r.process_alive = Signal::No;
        r.recovery = Some(RecoveryState::InProgress { attempt: 2 });
        assert_eq!(
            eval(&mut t, Some(r.clone()), 0, t0).status,
            HealthStatus::Recovering
        );

        r.recovery = Some(RecoveryState::Exhausted { attempts: 5 });
        let h = eval(&mut t, Some(r), 0, t0 + ms(1));
        assert_eq!(h.status, HealthStatus::RecoveryExhausted);
    }

    #[test]
    fn new_process_resets_old_probe_failures() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        let mut old = report(PlaybackState::Playing);
        old.uptime = Some(Duration::from_secs(300));
        for i in 0..3u64 {
            t.observe_probe(&probe(false, false), t0 + ms(i));
        }
        assert!(eval(&mut t, Some(old), 0, t0).status.is_error());

        // Same device, uptime restarted: the replacement process starts with a clean slate.
        let mut fresh = report(PlaybackState::Playing);
        fresh.uptime = Some(Duration::from_secs(1));
        assert!(eval(&mut t, Some(fresh), 0, t0 + ms(100))
            .status
            .is_healthy());
    }

    #[test]
    fn unassigned_devices_are_not_tracked() {
        let t0 = Instant::now();
        let mut t = HealthTracker::new(HealthPolicy::default());
        let out = t.evaluate(&[], &[report(PlaybackState::Failed)], |_| 0, t0);
        assert!(out.is_empty());
    }

    #[test]
    fn probe_scheduler_backs_off_on_failure_and_resets_on_success() {
        let t0 = Instant::now();
        let mut s = ProbeScheduler::new(ms(2000), ms(30_000));
        assert!(s.is_due(t0));
        s.record_round(t0, false);
        assert_eq!(s.interval(), ms(4000));
        assert!(!s.is_due(t0 + ms(3999)));
        assert!(s.is_due(t0 + ms(4000)));
        for i in 1..10u64 {
            s.record_round(t0 + ms(i), false);
        }
        assert_eq!(s.interval(), ms(30_000), "capped");
        s.record_round(t0 + ms(100), true);
        assert_eq!(s.interval(), ms(2000), "reset");
    }
}
