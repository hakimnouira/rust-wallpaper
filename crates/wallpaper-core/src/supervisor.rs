//! Composition of the runtime state machine, player health tracking and monitor lifecycle.
//!
//! [`RuntimeSupervisor`] contains no new decision logic. It wires the existing pieces in the one
//! order that is correct, so the daemon and the tests run exactly the same orchestration:
//!
//! 1. `sync_topology`   monitors added/removed/changed -> assignments follow
//! 2. (host recovery runs: crashed or missing players are relaunched, *playing*)
//! 3. `update_health`   reports + probe results -> verdicts -> `RuntimeError`
//! 4. `reconcile`       desired vs actual -> pause/resume (re-asserts suspension after recovery)

use crate::health::{
    HealthPolicy, HealthStatus, HealthTracker, PlayerHealth, ProbeResult, ProbeScheduler,
};
use crate::monitors::{
    MonitorDescriptor, MonitorLifecycle, ScopedSink, TopologyReport, WallpaperHost,
};
use crate::runtime::{PlayerCommand, RuntimeState};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tracing::warn;

pub struct RuntimeSupervisor {
    pub runtime: RuntimeState,
    pub lifecycle: MonitorLifecycle,
    health: HealthTracker,
    probes: ProbeScheduler,
    latest: Vec<PlayerHealth>,
}

impl RuntimeSupervisor {
    pub fn new(runtime: RuntimeState, lifecycle: MonitorLifecycle, policy: HealthPolicy) -> Self {
        Self {
            runtime,
            lifecycle,
            health: HealthTracker::new(policy),
            probes: ProbeScheduler::new(Duration::from_secs(2), Duration::from_secs(30)),
            latest: Vec::new(),
        }
    }

    /// Monitors added/removed/changed: assignments are synchronized with the new topology.
    pub fn sync_topology<H: WallpaperHost>(
        &mut self,
        current: Vec<MonitorDescriptor>,
        host: &mut H,
        now: Instant,
    ) -> TopologyReport {
        let report = self.lifecycle.sync(current, host, now);
        for gone in &report.diff.removed {
            self.health.forget(&gone.device_name);
            self.runtime.reset_command_history(&gone.device_name);
        }
        // A freshly started wallpaper is a new process: old failure evidence does not apply.
        for device in &report.started {
            self.runtime.reset_command_history(device);
        }
        report
    }

    /// Applies an explicit user choice (persisted) and starts it on the monitor now.
    pub fn assign_user<H: WallpaperHost>(
        &mut self,
        device: &str,
        assignment: crate::monitors::Assignment,
        host: &mut H,
        now: Instant,
    ) -> Result<(), String> {
        let result = self.lifecycle.assign_user(device, assignment, host, now);
        self.runtime.reset_command_history(device);
        result
    }

    /// True when a (potentially blocking) IPC probe round should run now.
    pub fn probe_due(&self, now: Instant) -> bool {
        self.probes.is_due(now)
    }

    /// Records one probe round. Failures stretch the interval until the next round.
    pub fn record_probes(&mut self, results: &[ProbeResult], now: Instant) {
        for r in results {
            self.health.observe_probe(r, now);
            if r.ipc_responded {
                // The player answered: failed commands from before are stale evidence.
                self.runtime.reset_command_history(&r.device);
            }
        }
        let all_ok = results.iter().all(|r| r.ipc_responded);
        self.probes.record_round(now, all_ok);
    }

    /// Re-evaluates player health and applies it to the runtime state. Also rearms players whose
    /// recovery is exhausted, at a slow bounded rate. Returns the verdicts.
    pub fn update_health<H: WallpaperHost>(
        &mut self,
        host: &mut H,
        now: Instant,
    ) -> &[PlayerHealth] {
        let wanted = self.lifecycle.wanted_devices();
        let reports = host.player_reports();
        let runtime = &self.runtime;
        self.latest = self
            .health
            .evaluate(&wanted, &reports, |d| runtime.command_failures(d), now);

        // A player that vanished and returned is a different process.
        for device in self.health.take_new_processes() {
            self.runtime.reset_command_history(&device);
            self.probes.request_now();
        }
        self.runtime.sync_player_health(&self.latest);

        let healthy: Vec<String> = self
            .latest
            .iter()
            .filter(|h| h.status.is_healthy())
            .map(|h| h.device.clone())
            .collect();
        self.lifecycle.note_healthy(&healthy);

        let exhausted: Vec<String> = self
            .latest
            .iter()
            .filter(|h| h.status == HealthStatus::RecoveryExhausted)
            .map(|h| h.device.clone())
            .collect();
        if !exhausted.is_empty() {
            for device in self.lifecycle.rearm_exhausted(&exhausted, host, now) {
                warn!(device = %device, "Recovery was exhausted; wallpaper restarted manually");
                self.runtime.reset_command_history(&device);
                self.probes.request_now();
            }
        }
        &self.latest
    }

    pub fn health(&self) -> &[PlayerHealth] {
        &self.latest
    }

    /// Makes real players match the desired state, for connected monitors only.
    pub fn reconcile<H: WallpaperHost>(
        &mut self,
        host: &mut H,
        now: Instant,
    ) -> Vec<(String, PlayerCommand)> {
        let devices: HashSet<String> = self.lifecycle.connected_devices().into_iter().collect();
        let mut scoped = ScopedSink {
            inner: host,
            devices: &devices,
        };
        self.runtime.reconcile(&mut scoped, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{PlayerReport, Signal};
    use crate::monitors::{
        Assignment, InMemoryAssignmentStore, MonitorId, MonitorRegistry, StartError,
    };
    use crate::runtime::{ActualPlayback, ForegroundObservation, PlaybackSink, RuntimeEvent};
    use crate::{
        ForegroundKind, PerformanceProfile, PlaybackPolicy, PlaybackState, SuspensionReason,
    };
    use std::collections::HashMap;

    const D1: &str = r"\\.\DISPLAY1";
    const D2: &str = r"\\.\DISPLAY2";

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn mon(id: &str, device: &str, x: i32, primary: bool) -> MonitorDescriptor {
        MonitorDescriptor {
            id: MonitorId::new(id),
            device_name: device.to_string(),
            x,
            y: 0,
            width: 1920,
            height: 1080,
            is_primary: primary,
        }
    }

    #[derive(Clone)]
    struct SimPlayer {
        state: PlaybackState,
        alive: bool,
        ipc_ok: bool,
    }

    /// Simulates the real manager closely enough to expose the bugs that matter:
    /// recovery relaunches players *playing*; a dead player's reported state goes stale; a hung
    /// player's IPC blocks for 1.5 s per command; manager recovery has a retry budget.
    #[derive(Default)]
    struct SimHost {
        windows: HashSet<String>,
        players: HashMap<String, SimPlayer>,
        sources: HashMap<String, Assignment>,
        /// Whether `player_reports` can tell if the process is alive (the host API of the future).
        report_liveness: bool,
        recoveries_left: HashMap<String, u32>,
        max_recoveries: u32,
        pause_calls: Vec<String>,
        resume_calls: Vec<String>,
        blocked: Duration,
        start_calls: usize,
    }

    impl SimHost {
        fn new() -> Self {
            Self {
                report_liveness: true,
                max_recoveries: 5,
                ..Default::default()
            }
        }
        fn connect(&mut self, d: &str) {
            self.windows.insert(d.to_string());
        }
        fn disconnect(&mut self, d: &str) {
            self.windows.remove(d);
            self.players.remove(d);
        }
        fn crash(&mut self, d: &str) {
            if let Some(p) = self.players.get_mut(d) {
                p.alive = false; // state deliberately stays stale, like the real player struct
            }
        }
        /// Manager recovery: relaunch missing or dead players from remembered sources.
        fn recover(&mut self) {
            for d in self.windows.clone() {
                if !self.sources.contains_key(&d) {
                    continue;
                }
                let needs = match self.players.get(&d) {
                    None => true,
                    Some(p) => !p.alive,
                };
                if !needs {
                    continue;
                }
                let left = self
                    .recoveries_left
                    .entry(d.clone())
                    .or_insert(self.max_recoveries);
                if *left == 0 {
                    continue; // budget exhausted: the dead player stays in the map
                }
                *left -= 1;
                self.players.insert(
                    d,
                    SimPlayer {
                        state: PlaybackState::Playing,
                        alive: true,
                        ipc_ok: true,
                    },
                );
            }
        }
        /// `force_recover_explorer`: stop everything, rebuild from remembered sources.
        fn explorer_recover(&mut self) {
            self.players.clear();
            self.recover();
        }
        fn probe(&self, d: &str) -> ProbeResult {
            let ok = self.players.get(d).is_some_and(|p| p.alive && p.ipc_ok);
            ProbeResult {
                device: d.to_string(),
                ipc_responded: ok,
                video_initialized: ok,
            }
        }
        fn state(&self, d: &str) -> Option<PlaybackState> {
            self.players.get(d).map(|p| p.state)
        }
    }

    impl PlaybackSink for SimHost {
        fn player_states(&self) -> Vec<(String, ActualPlayback)> {
            let mut v: Vec<_> = self
                .players
                .iter()
                .map(|(d, p)| {
                    let a = match p.state {
                        PlaybackState::Playing => ActualPlayback::Playing,
                        PlaybackState::Paused => ActualPlayback::Paused,
                        _ => ActualPlayback::Unavailable,
                    };
                    (d.clone(), a)
                })
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        }
        fn pause_player(&mut self, d: &str) {
            self.pause_calls.push(d.to_string());
            if let Some(p) = self.players.get_mut(d) {
                if p.alive && p.ipc_ok {
                    p.state = PlaybackState::Paused;
                } else if p.alive {
                    self.blocked += Duration::from_millis(1500); // hung IPC: full timeout
                }
            }
        }
        fn resume_player(&mut self, d: &str) {
            self.resume_calls.push(d.to_string());
            if let Some(p) = self.players.get_mut(d) {
                if p.alive && p.ipc_ok {
                    p.state = PlaybackState::Playing;
                } else if p.alive {
                    self.blocked += Duration::from_millis(1500);
                }
            }
        }
    }

    impl WallpaperHost for SimHost {
        fn start_wallpaper(&mut self, d: &str, a: &Assignment) -> Result<(), StartError> {
            self.start_calls += 1;
            if !self.windows.contains(d) {
                return Err(StartError::NoWindow);
            }
            self.sources.insert(d.to_string(), a.clone());
            self.recoveries_left.remove(d);
            self.players.insert(
                d.to_string(),
                SimPlayer {
                    state: PlaybackState::Playing,
                    alive: true,
                    ipc_ok: true,
                },
            );
            Ok(())
        }
        fn player_reports(&self) -> Vec<PlayerReport> {
            self.players
                .iter()
                .map(|(d, p)| PlayerReport {
                    device: d.clone(),
                    state: p.state,
                    uptime: None,
                    memory_bytes: 0,
                    process_alive: if self.report_liveness {
                        Signal::from_bool(p.alive)
                    } else {
                        Signal::Unknown
                    },
                    recovery: None,
                })
                .collect()
        }
    }

    fn supervisor(profile: PerformanceProfile) -> RuntimeSupervisor {
        let registry = MonitorRegistry::new(
            Box::new(InMemoryAssignmentStore::default()),
            Some(Assignment::new("fallback.mp4")),
        );
        RuntimeSupervisor::new(
            RuntimeState::new(PlaybackPolicy::for_profile(profile)),
            MonitorLifecycle::new(registry),
            HealthPolicy::default(),
        )
    }

    /// One daemon pass, in the same order as the daemon loop.
    fn step(sup: &mut RuntimeSupervisor, host: &mut SimHost, now: Instant) {
        host.recover();
        sup.update_health(host, now);
        sup.reconcile(host, now);
    }

    fn setup(devices: &[&str]) -> (RuntimeSupervisor, SimHost, Instant) {
        let t = Instant::now();
        let mut host = SimHost::new();
        let mut sup = supervisor(PerformanceProfile::Balanced);
        let mut monitors = Vec::new();
        for (i, d) in devices.iter().enumerate() {
            host.connect(d);
            monitors.push(mon(&format!("hw{i}"), d, (i as i32) * 1920, i == 0));
        }
        sup.sync_topology(monitors, &mut host, t);
        step(&mut sup, &mut host, t);
        (sup, host, t)
    }

    fn monitors_of(devices: &[&str]) -> Vec<MonitorDescriptor> {
        devices
            .iter()
            .enumerate()
            .map(|(i, d)| mon(&format!("hw{i}"), d, (i as i32) * 1920, i == 0))
            .collect()
    }

    fn game(device: &str) -> Vec<ForegroundObservation> {
        vec![ForegroundObservation {
            device: device.to_string(),
            kind: ForegroundKind::BorderlessFullscreen,
            description: "game.exe".into(),
        }]
    }

    // ---- recovery preserves the desired playback state -----------------------------------

    #[test]
    fn recovery_while_user_paused_stays_paused() {
        let (mut sup, mut host, t) = setup(&[D1]);
        sup.runtime.set_user_paused(Some(D1), true);
        step(&mut sup, &mut host, t + ms(10));
        assert_eq!(host.state(D1), Some(PlaybackState::Paused));
        host.resume_calls.clear();

        host.crash(D1);
        step(&mut sup, &mut host, t + ms(300)); // recovery relaunches it Playing, then reconcile
        assert_eq!(
            host.state(D1),
            Some(PlaybackState::Paused),
            "user pause survives recovery"
        );
        assert!(host.resume_calls.is_empty(), "never resumed in between");
        assert!(sup.runtime.suspension().is_user_paused(D1));
    }

    #[test]
    fn recovery_while_fullscreen_active_stays_paused_then_resumes_when_game_closes() {
        let (mut sup, mut host, t) = setup(&[D1, D2]);
        let mons = vec![D1.to_string(), D2.to_string()];
        let mut now = t;
        for _ in 0..8 {
            now += ms(100);
            sup.runtime.observe_foreground(&mons, &game(D1), now);
            step(&mut sup, &mut host, now);
        }
        assert_eq!(host.state(D1), Some(PlaybackState::Paused));
        assert_eq!(host.state(D2), Some(PlaybackState::Playing));

        host.crash(D1);
        now += ms(100);
        sup.runtime.observe_foreground(&mons, &game(D1), now);
        step(&mut sup, &mut host, now);
        assert_eq!(
            host.state(D1),
            Some(PlaybackState::Paused),
            "fullscreen still suspends"
        );
        assert_eq!(
            host.state(D2),
            Some(PlaybackState::Playing),
            "other monitor untouched"
        );

        for _ in 0..20 {
            now += ms(100);
            sup.runtime.observe_foreground(&mons, &[], now);
            step(&mut sup, &mut host, now);
        }
        assert_eq!(
            host.state(D1),
            Some(PlaybackState::Playing),
            "resumes after the game closes"
        );
    }

    #[test]
    fn suspended_system_never_leaves_a_recovered_player_playing() {
        let (mut sup, mut host, t) = setup(&[D1]);
        sup.runtime.apply_event(RuntimeEvent::SystemSuspend, t);
        step(&mut sup, &mut host, t + ms(10));
        assert_eq!(host.state(D1), Some(PlaybackState::Paused));

        host.crash(D1);
        step(&mut sup, &mut host, t + ms(300));
        assert_eq!(host.state(D1), Some(PlaybackState::Paused));

        sup.runtime
            .apply_event(RuntimeEvent::SystemResume, t + ms(5000));
        step(&mut sup, &mut host, t + ms(5010));
        assert_eq!(host.state(D1), Some(PlaybackState::Playing));
    }

    // ---- health -> RuntimeError ------------------------------------------------------------

    #[test]
    fn failed_player_is_runtime_error_not_healthy_and_not_commanded() {
        let (mut sup, mut host, t) = setup(&[D1]);
        host.players.get_mut(D1).unwrap().state = PlaybackState::Failed;
        sup.runtime.set_user_paused(Some(D1), true);
        step(&mut sup, &mut host, t + ms(500));

        assert!(sup
            .runtime
            .suspension()
            .has_reason(D1, SuspensionReason::RuntimeError));
        assert!(!sup.health()[0].status.is_healthy());
        assert!(
            host.pause_calls.is_empty(),
            "a Failed player is not commandable"
        );

        // It recovers: RuntimeError clears and the user's pause is honoured.
        host.players.get_mut(D1).unwrap().state = PlaybackState::Playing;
        step(&mut sup, &mut host, t + ms(1000));
        assert!(!sup
            .runtime
            .suspension()
            .has_reason(D1, SuspensionReason::RuntimeError));
        assert_eq!(host.state(D1), Some(PlaybackState::Paused));
    }

    #[test]
    fn starting_player_is_neither_healthy_nor_an_error_within_grace() {
        let (mut sup, mut host, t) = setup(&[D1]);
        host.players.get_mut(D1).unwrap().state = PlaybackState::Starting;
        step(&mut sup, &mut host, t + ms(500));
        assert_eq!(sup.health()[0].status, HealthStatus::Starting);
        assert!(!sup
            .runtime
            .suspension()
            .has_reason(D1, SuspensionReason::RuntimeError));

        step(&mut sup, &mut host, t + ms(11_000)); // stuck past the grace period
        assert!(sup
            .runtime
            .suspension()
            .has_reason(D1, SuspensionReason::RuntimeError));
    }

    /// Process alive, IPC dead: pause cannot take effect. The failure is detected from the
    /// commands themselves, and the loop-blocking cost stays bounded by the capped backoff.
    #[test]
    fn alive_but_hung_ipc_is_flagged_and_blocking_stays_bounded() {
        let (mut sup, mut host, t) = setup(&[D1]);
        host.players.get_mut(D1).unwrap().ipc_ok = false;
        sup.runtime.set_user_paused(Some(D1), true);

        let mut flagged_at = None;
        let mut attempt_times: Vec<u64> = Vec::new();
        let mut seen = 0;
        for i in 0..400u64 {
            // 40 s in 100 ms ticks: still inside the 45 s recovery budget.
            step(&mut sup, &mut host, t + ms(i * 100));
            if host.pause_calls.len() > seen {
                seen = host.pause_calls.len();
                attempt_times.push(i * 100);
            }
            if flagged_at.is_none()
                && sup
                    .runtime
                    .suspension()
                    .has_reason(D1, SuspensionReason::RuntimeError)
            {
                flagged_at = Some(i * 100);
            }
        }
        assert!(
            flagged_at.is_some_and(|ms| ms <= 6000),
            "flagged quickly: {flagged_at:?}"
        );
        assert_eq!(
            sup.health()[0].process_alive,
            Signal::Yes,
            "the process is alive"
        );
        assert_eq!(
            sup.health()[0].ipc_responsive,
            Signal::No,
            "but its IPC is not"
        );

        let gaps: Vec<u64> = attempt_times.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(
            gaps.windows(2).all(|w| w[1] >= w[0]),
            "retry gaps never shrink: {gaps:?}"
        );
        assert!(
            attempt_times.len() <= 7,
            "attempts in 40 s: {attempt_times:?}"
        );
        assert!(
            host.blocked <= Duration::from_secs(11),
            "blocked {:?} of 40 s",
            host.blocked
        );
    }

    /// Once the recovery budget is spent the wallpaper is restarted, and the replacement is judged
    /// on its own: the user's pause is applied to it promptly, not after the old backoff expires.
    #[test]
    fn hung_ipc_player_is_restarted_after_the_budget_and_user_pause_applies_promptly() {
        let (mut sup, mut host, t) = setup(&[D1]);
        host.players.get_mut(D1).unwrap().ipc_ok = false;
        sup.runtime.set_user_paused(Some(D1), true);

        let mut restarted_at = None;
        for i in 0..800u64 {
            let before = host.start_calls;
            step(&mut sup, &mut host, t + ms(i * 100));
            if host.start_calls > before && restarted_at.is_none() {
                restarted_at = Some(i * 100);
                break;
            }
        }
        let restarted_at = restarted_at.expect("restarted once recovery was exhausted");
        assert!(
            (45_000..=48_000).contains(&restarted_at),
            "restarted at {restarted_at} ms (budget is 45 s)"
        );

        // Within one second of the restart the new process is paused and judged healthy.
        for i in 1..=10u64 {
            step(&mut sup, &mut host, t + ms(restarted_at + i * 100));
        }
        assert_eq!(
            host.state(D1),
            Some(PlaybackState::Paused),
            "user pause applied to replacement"
        );
        assert!(sup.health()[0].status.is_healthy());
        assert!(!sup
            .runtime
            .suspension()
            .has_reason(D1, SuspensionReason::RuntimeError));
        assert_eq!(
            sup.runtime.command_failures(D1),
            0,
            "old process's failures were discarded"
        );
    }

    #[test]
    fn ipc_that_recovers_by_itself_clears_the_error_via_a_probe() {
        let (mut sup, mut host, t) = setup(&[D1]);
        host.players.get_mut(D1).unwrap().ipc_ok = false;
        sup.runtime.set_user_paused(Some(D1), true);
        for i in 0..200u64 {
            step(&mut sup, &mut host, t + ms(i * 100));
        }
        assert!(sup
            .runtime
            .suspension()
            .has_reason(D1, SuspensionReason::RuntimeError));

        // IPC answers again; the next probe round proves it and the pause lands immediately.
        host.players.get_mut(D1).unwrap().ipc_ok = true;
        let now = t + ms(20_100);
        sup.record_probes(&[host.probe(D1)], now);
        step(&mut sup, &mut host, now);
        assert_eq!(host.state(D1), Some(PlaybackState::Paused));
        assert!(!sup
            .runtime
            .suspension()
            .has_reason(D1, SuspensionReason::RuntimeError));
    }

    #[test]
    fn ipc_that_recovers_by_itself_is_also_noticed_by_the_trial_command_without_probes() {
        let (mut sup, mut host, t) = setup(&[D1]);
        host.players.get_mut(D1).unwrap().ipc_ok = false;
        sup.runtime.set_user_paused(Some(D1), true);
        for i in 0..200u64 {
            step(&mut sup, &mut host, t + ms(i * 100));
        }
        host.players.get_mut(D1).unwrap().ipc_ok = true;
        // No probes at all: the capped-backoff trial command (<= 30 s) still lands.
        for i in 0..320u64 {
            step(&mut sup, &mut host, t + ms(20_000 + i * 100));
        }
        assert_eq!(host.state(D1), Some(PlaybackState::Paused));
        assert!(!sup
            .runtime
            .suspension()
            .has_reason(D1, SuspensionReason::RuntimeError));
    }

    /// With no liveness signal from the host (today's manager API), a dead process whose state
    /// is stale `Playing` is still caught, via failed IPC probe rounds.
    #[test]
    fn dead_process_with_stale_state_is_caught_by_probe_rounds_without_a_liveness_signal() {
        let (mut sup, mut host, t) = setup(&[D1]);
        host.report_liveness = false;
        host.max_recoveries = 0; // manager has given up: the dead player stays in the map
        host.recoveries_left.insert(D1.to_string(), 0);
        host.crash(D1);

        step(&mut sup, &mut host, t + ms(100));
        assert_eq!(
            sup.health()[0].status,
            HealthStatus::Healthy,
            "nothing proves otherwise yet"
        );
        assert_eq!(
            sup.health()[0].unverified(),
            vec!["process_alive", "ipc_responsive", "video_initialized"]
        );

        let mut now = t + ms(100);
        let mut rounds = 0;
        while !sup
            .runtime
            .suspension()
            .has_reason(D1, SuspensionReason::RuntimeError)
            && rounds < 10
        {
            now += sup_probe_wait(&sup);
            assert!(sup.probe_due(now));
            sup.record_probes(&[host.probe(D1)], now);
            step(&mut sup, &mut host, now);
            rounds += 1;
        }
        assert_eq!(rounds, 3, "three failed probe rounds flag it");
        assert_eq!(
            sup.health()[0].status,
            HealthStatus::Unhealthy(crate::health::UnhealthyCause::IpcUnresponsive)
        );
    }

    fn sup_probe_wait(_sup: &RuntimeSupervisor) -> Duration {
        Duration::from_secs(60)
    }

    #[test]
    fn probe_rounds_back_off_while_players_fail_and_reset_on_success() {
        let (mut sup, mut host, t) = setup(&[D1]);
        host.players.get_mut(D1).unwrap().ipc_ok = false;
        let mut now = t;
        let mut gaps = Vec::new();
        for _ in 0..6 {
            while !sup.probe_due(now) {
                now += ms(500);
            }
            gaps.push(now);
            sup.record_probes(&[host.probe(D1)], now);
        }
        let deltas: Vec<u64> = gaps
            .windows(2)
            .map(|w| (w[1] - w[0]).as_millis() as u64)
            .collect();
        assert!(
            deltas.windows(2).all(|w| w[1] >= w[0]),
            "non-decreasing: {deltas:?}"
        );
        assert!(
            *deltas.last().unwrap() >= 16_000,
            "grows toward the cap: {deltas:?}"
        );

        host.players.get_mut(D1).unwrap().ipc_ok = true;
        while !sup.probe_due(now) {
            now += ms(500);
        }
        sup.record_probes(&[host.probe(D1)], now);
        assert!(!sup.probe_due(now + ms(1999)));
        assert!(sup.probe_due(now + ms(2000)), "back to the base interval");
    }

    // ---- monitors ----------------------------------------------------------------------

    #[test]
    fn removing_a_monitor_clears_its_runtime_error_and_leaves_others_alone() {
        let (mut sup, mut host, t) = setup(&[D1, D2]);
        host.disconnect(D2);
        host.pause_calls.clear();
        host.resume_calls.clear();
        host.start_calls = 0;

        sup.sync_topology(monitors_of(&[D1]), &mut host, t + ms(500));
        step(&mut sup, &mut host, t + ms(600));

        assert_eq!(host.state(D1), Some(PlaybackState::Playing));
        assert!(host.pause_calls.is_empty() && host.resume_calls.is_empty());
        assert_eq!(host.start_calls, 0, "no restarts for the remaining monitor");
        assert!(sup.runtime.suspension().active_reasons_for(D2).is_empty());
        assert_eq!(
            sup.health().len(),
            1,
            "stale health tracking for D2 is gone"
        );
    }

    #[test]
    fn newly_connected_monitor_is_assigned_and_enters_health_tracking() {
        let (mut sup, mut host, t) = setup(&[D1]);
        host.connect(D2);
        let rep = sup.sync_topology(monitors_of(&[D1, D2]), &mut host, t + ms(500));
        assert_eq!(rep.started, vec![D2.to_string()]);
        step(&mut sup, &mut host, t + ms(600));
        assert_eq!(sup.health().len(), 2);
        assert!(sup.health().iter().all(|h| h.status.is_healthy()));
    }

    #[test]
    fn reconnected_monitor_returns_and_respects_user_pause() {
        let (mut sup, mut host, t) = setup(&[D1, D2]);
        sup.runtime.set_user_paused(Some(D2), true);
        step(&mut sup, &mut host, t + ms(10));

        host.disconnect(D2);
        sup.sync_topology(monitors_of(&[D1]), &mut host, t + ms(500));
        step(&mut sup, &mut host, t + ms(600));

        host.connect(D2);
        sup.sync_topology(monitors_of(&[D1, D2]), &mut host, t + ms(1500));
        step(&mut sup, &mut host, t + ms(1600)); // manager recovery relaunches D2 Playing
        assert_eq!(
            host.state(D2),
            Some(PlaybackState::Paused),
            "UserPaused survived the disconnect"
        );
        assert_eq!(host.state(D1), Some(PlaybackState::Playing));
    }

    // ---- Explorer ------------------------------------------------------------------------

    #[test]
    fn explorer_restart_keeps_assignments_and_repauses_players() {
        let (mut sup, mut host, t) = setup(&[D1, D2]);
        sup.runtime.set_user_paused(Some(D1), true);
        step(&mut sup, &mut host, t + ms(10));
        let sources_before = host.sources.clone();
        let wanted_before = sup.lifecycle.wanted_devices();

        sup.runtime
            .apply_event(RuntimeEvent::ExplorerRestarted, t + ms(100));
        assert!(
            !sup.runtime.take_pending_work(t + ms(500)).recover_explorer,
            "still settling"
        );
        assert!(sup.runtime.take_pending_work(t + ms(1200)).recover_explorer);
        host.explorer_recover();
        step(&mut sup, &mut host, t + ms(1300));

        assert_eq!(host.sources, sources_before, "assignments preserved");
        assert_eq!(sup.lifecycle.wanted_devices(), wanted_before);
        assert_eq!(host.state(D1), Some(PlaybackState::Paused));
        assert_eq!(host.state(D2), Some(PlaybackState::Playing));
    }

    #[test]
    fn repeated_explorer_notifications_cannot_cause_a_recovery_loop() {
        let (mut sup, _host, t) = setup(&[D1]);
        let mut recoveries = 0;
        // A notification every 200 ms for two minutes.
        for i in 0..600u64 {
            let now = t + ms(i * 200);
            sup.runtime
                .apply_event(RuntimeEvent::ExplorerRestarted, now);
            if sup.runtime.take_pending_work(now).recover_explorer {
                recoveries += 1;
            }
        }
        // Continuous bursts never go quiet, so recovery waits for the settle window to elapse
        // since the *last* notification: it must not fire repeatedly while events keep coming.
        assert!(
            recoveries <= 1,
            "recoveries during a continuous burst: {recoveries}"
        );

        // Once the storm ends, exactly one recovery follows.
        let end = t + ms(600 * 200);
        let mut after = 0;
        for i in 1..100u64 {
            if sup
                .runtime
                .take_pending_work(end + ms(i * 100))
                .recover_explorer
            {
                after += 1;
            }
        }
        assert_eq!(recoveries + after, 1, "one recovery for the whole storm");
    }

    #[test]
    fn repeated_topology_notifications_collapse_into_one_refresh() {
        let (mut sup, _host, t) = setup(&[D1]);
        let mut refreshes = 0;
        for i in 0..200u64 {
            let now = t + ms(i * 10);
            sup.runtime
                .apply_event(RuntimeEvent::DisplayTopologyChanged, now);
            if sup.runtime.take_pending_work(now).refresh_topology {
                refreshes += 1;
            }
        }
        assert_eq!(
            refreshes, 0,
            "nothing while notifications are still arriving"
        );
        assert!(
            sup.runtime
                .take_pending_work(t + ms(2000 + 600))
                .refresh_topology
        );
        assert!(!sup.runtime.take_pending_work(t + ms(5000)).refresh_topology);
    }

    // ---- recovery budget -------------------------------------------------------------------

    #[test]
    fn recovery_exhaustion_is_reported_rearmed_slowly_and_reset_when_healthy() {
        let (mut sup, mut host, t) = setup(&[D1]);
        host.max_recoveries = 0;
        host.recoveries_left.insert(D1.to_string(), 0);
        host.crash(D1); // dead, and the manager no longer relaunches it

        let mut now = t;
        step(&mut sup, &mut host, now);
        assert_eq!(
            sup.health()[0].status,
            HealthStatus::Unhealthy(crate::health::UnhealthyCause::ProcessDead)
        );
        assert!(sup
            .runtime
            .suspension()
            .has_reason(D1, SuspensionReason::RuntimeError));

        // Past the 45 s budget the verdict becomes exhausted, and one manual restart is issued.
        let starts_before = host.start_calls;
        now = t + Duration::from_secs(46);
        step(&mut sup, &mut host, now);
        assert_eq!(
            host.start_calls,
            starts_before + 1,
            "manual restart issued once"
        );

        // The restart worked: healthy again, RuntimeError cleared, budgets reset.
        step(&mut sup, &mut host, now + ms(100));
        assert!(sup.health()[0].status.is_healthy());
        assert!(!sup
            .runtime
            .suspension()
            .has_reason(D1, SuspensionReason::RuntimeError));

        // A later crash starts a fresh budget instead of being instantly "exhausted".
        host.recoveries_left.insert(D1.to_string(), 0);
        host.crash(D1);
        let later = now + Duration::from_secs(300);
        step(&mut sup, &mut host, later);
        assert_eq!(
            sup.health()[0].status,
            HealthStatus::Unhealthy(crate::health::UnhealthyCause::ProcessDead)
        );
    }

    #[test]
    fn exhausted_rearm_is_rate_limited() {
        let (mut sup, mut host, t) = setup(&[D1]);
        host.max_recoveries = 0;
        host.recoveries_left.insert(D1.to_string(), 0);
        host.crash(D1);
        step(&mut sup, &mut host, t);
        let base = host.start_calls;

        // The restarted player keeps dying immediately: restarts must back off, not spin.
        for i in 0..600u64 {
            let now = t + Duration::from_secs(46) + ms(i * 1000);
            host.recoveries_left.insert(D1.to_string(), 0);
            host.crash(D1);
            step(&mut sup, &mut host, now);
        }
        let restarts = host.start_calls - base;
        assert!(
            restarts <= 10,
            "10 minutes of constant failure produced {restarts} restarts"
        );
        assert!(restarts >= 2, "but it does keep trying: {restarts}");
    }
}
