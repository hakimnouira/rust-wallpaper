//! Dynamic monitor lifecycle.
//!
//! * [`MonitorRegistry`] knows which monitors are connected *now* and which wallpaper each
//!   should show. Monitors are identified by a stable [`MonitorId`] (hardware identity), not by
//!   the `\\.\DISPLAYn` name, which Windows may renumber when topology changes.
//! * **User assignments** are preferences: they are persisted through an [`AssignmentStore`] and
//!   survive disconnects, so a monitor that returns gets its wallpaper back.
//! * **Default assignments** are runtime-only. They are derived for monitors without a user
//!   assignment and are dropped on disconnect. They are never persisted, so a default can never
//!   be mistaken for, or overwrite, something the user chose.
//! * [`MonitorLifecycle`] reconciles the registry with the real wallpaper host through the
//!   [`WallpaperHost`] trait (implemented for the Win32 manager, faked in tests).
//!
//! Default policy for a monitor with no user assignment (documented, deterministic):
//! * the **primary** monitor uses the configured fallback wallpaper, if any;
//! * every **other** monitor clones the primary monitor's effective assignment, or the fallback
//!   if the primary has none;
//! * a secondary monitor's personal choice is never mirrored onto another monitor;
//! * with no fallback and nothing to clone, the monitor stays unassigned: it shows the plain
//!   background and is not an error.

use crate::health::PlayerReport;
use crate::runtime::PlaybackSink;
use crate::{HardwareAcceleration, PlaybackConfig};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------------------
// Identity and assignments
// ---------------------------------------------------------------------------------------

/// Stable identity of a physical monitor connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MonitorId(String);

impl MonitorId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Identity from the monitor's hardware device-interface path (the `DeviceID` Windows gives
    /// for `EnumDisplayDevices(..., EDD_GET_DEVICE_INTERFACE_NAME)`), e.g.
    /// `\\?\DISPLAY#GSM5B09#5&2b1c7a8&0&UID4357#{e6f07b5f-...}`. It names the physical monitor on
    /// its port and, unlike `\\.\DISPLAYn`, does not change when other monitors come and go.
    /// Blank input falls back to [`MonitorId::from_device_name`].
    pub fn from_interface_path(path: &str, device_name: &str) -> Self {
        let path = path.trim();
        if path.is_empty() {
            Self::from_device_name(device_name)
        } else {
            Self(format!("hw:{}", path.to_ascii_lowercase()))
        }
    }

    /// Weak identity from the Windows device name; used only when no hardware identity exists.
    pub fn from_device_name(device_name: &str) -> Self {
        Self(format!("dev:{}", device_name.to_ascii_lowercase()))
    }
}

impl std::fmt::Display for MonitorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One connected monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorDescriptor {
    pub id: MonitorId,
    /// Current Windows device name, e.g. `\\.\DISPLAY2`.
    pub device_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

/// A wallpaper and how to play it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub video_path: PathBuf,
    pub config: PlaybackConfig,
}

impl Assignment {
    pub fn new(video_path: impl Into<PathBuf>) -> Self {
        Self {
            video_path: video_path.into(),
            config: PlaybackConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentSource {
    /// Chosen by the user; persisted.
    User,
    /// Derived by policy; runtime-only.
    Default,
}

// ---------------------------------------------------------------------------------------
// Persistent preference store
// ---------------------------------------------------------------------------------------

#[derive(Debug, Error)]
#[error("assignment store error: {0}")]
pub struct StoreError(pub String);

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError(e.to_string())
    }
}

/// Persistence for user assignments.
pub trait AssignmentStore: Send {
    fn load_all(&self) -> Result<Vec<(MonitorId, Assignment)>, StoreError>;
    fn upsert(&mut self, id: &MonitorId, assignment: &Assignment) -> Result<(), StoreError>;
    fn delete(&mut self, id: &MonitorId) -> Result<(), StoreError>;
}

/// Non-persistent store, also used by tests. `fail_writes` simulates a broken disk.
#[derive(Debug, Default)]
pub struct InMemoryAssignmentStore {
    map: HashMap<MonitorId, Assignment>,
    pub fail_writes: bool,
}

impl AssignmentStore for InMemoryAssignmentStore {
    fn load_all(&self) -> Result<Vec<(MonitorId, Assignment)>, StoreError> {
        Ok(self
            .map
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    fn upsert(&mut self, id: &MonitorId, assignment: &Assignment) -> Result<(), StoreError> {
        if self.fail_writes {
            return Err(StoreError("simulated write failure".into()));
        }
        self.map.insert(id.clone(), assignment.clone());
        Ok(())
    }

    fn delete(&mut self, id: &MonitorId) -> Result<(), StoreError> {
        if self.fail_writes {
            return Err(StoreError("simulated write failure".into()));
        }
        self.map.remove(id);
        Ok(())
    }
}

fn hw_to_str(hw: HardwareAcceleration) -> &'static str {
    match hw {
        HardwareAcceleration::Auto => "auto",
        HardwareAcceleration::D3d11va => "d3d11va",
        HardwareAcceleration::Dxva2 => "dxva2",
        HardwareAcceleration::Disabled => "disabled",
    }
}

fn hw_from_str(s: &str) -> HardwareAcceleration {
    match s {
        "d3d11va" => HardwareAcceleration::D3d11va,
        "dxva2" => HardwareAcceleration::Dxva2,
        "disabled" => HardwareAcceleration::Disabled,
        _ => HardwareAcceleration::Auto,
    }
}

/// SQLite-backed store. Uses its own database file, so it can never interfere with the wallpaper
/// library's schema migrations.
pub struct SqliteAssignmentStore {
    conn: rusqlite::Connection,
}

impl SqliteAssignmentStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StoreError> {
        let conn = rusqlite::Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(2))?;
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self, StoreError> {
        Self::init(rusqlite::Connection::open_in_memory()?)
    }

    fn init(conn: rusqlite::Connection) -> Result<Self, StoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS monitor_assignments (
                monitor_id    TEXT PRIMARY KEY NOT NULL,
                video_path    TEXT NOT NULL,
                loop_playback INTEGER NOT NULL,
                mute          INTEGER NOT NULL,
                volume        INTEGER NOT NULL,
                hwdec         TEXT NOT NULL
            );",
        )?;
        Ok(Self { conn })
    }
}

impl AssignmentStore for SqliteAssignmentStore {
    fn load_all(&self) -> Result<Vec<(MonitorId, Assignment)>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT monitor_id, video_path, loop_playback, mute, volume, hwdec
             FROM monitor_assignments ORDER BY monitor_id",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let path: String = row.get(1)?;
            let hw: String = row.get(5)?;
            Ok((
                MonitorId::new(id),
                Assignment {
                    video_path: PathBuf::from(path),
                    config: PlaybackConfig {
                        loop_playback: row.get::<_, i64>(2)? != 0,
                        mute: row.get::<_, i64>(3)? != 0,
                        volume: row.get::<_, i64>(4)?.max(0) as u32,
                        hardware_acceleration: hw_from_str(&hw),
                    },
                },
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn upsert(&mut self, id: &MonitorId, a: &Assignment) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO monitor_assignments
                (monitor_id, video_path, loop_playback, mute, volume, hwdec)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(monitor_id) DO UPDATE SET
                video_path = excluded.video_path,
                loop_playback = excluded.loop_playback,
                mute = excluded.mute,
                volume = excluded.volume,
                hwdec = excluded.hwdec",
            rusqlite::params![
                id.as_str(),
                a.video_path.to_string_lossy(),
                a.config.loop_playback as i64,
                a.config.mute as i64,
                a.config.volume as i64,
                hw_to_str(a.config.hardware_acceleration),
            ],
        )?;
        Ok(())
    }

    fn delete(&mut self, id: &MonitorId) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM monitor_assignments WHERE monitor_id = ?1",
            rusqlite::params![id.as_str()],
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------------------

/// A monitor present both before and after a sync whose properties differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorChange {
    pub old: MonitorDescriptor,
    pub new: MonitorDescriptor,
}

impl MonitorChange {
    pub fn renamed(&self) -> bool {
        self.old.device_name != self.new.device_name
    }

    pub fn geometry_changed(&self) -> bool {
        (self.old.x, self.old.y, self.old.width, self.old.height)
            != (self.new.x, self.new.y, self.new.width, self.new.height)
    }

    pub fn primary_changed(&self) -> bool {
        self.old.is_primary != self.new.is_primary
    }
}

/// What changed between two enumerations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MonitorDiff {
    pub added: Vec<MonitorDescriptor>,
    pub removed: Vec<MonitorDescriptor>,
    pub changed: Vec<MonitorChange>,
}

impl MonitorDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Connected monitors plus their wallpaper assignments.
pub struct MonitorRegistry {
    connected: BTreeMap<MonitorId, MonitorDescriptor>,
    /// Persistent preferences. Kept for monitors that are currently disconnected.
    user: HashMap<MonitorId, Assignment>,
    /// Runtime-only, for connected monitors without a user assignment.
    defaults: HashMap<MonitorId, Assignment>,
    store: Box<dyn AssignmentStore>,
    fallback: Option<Assignment>,
}

impl MonitorRegistry {
    /// Loads persisted user assignments from `store`. A failing store degrades to "no saved
    /// preferences" rather than aborting the daemon.
    pub fn new(store: Box<dyn AssignmentStore>, fallback: Option<Assignment>) -> Self {
        let user = match store.load_all() {
            Ok(rows) => rows.into_iter().collect(),
            Err(e) => {
                warn!(%e, "Could not load saved monitor assignments; starting without them");
                HashMap::new()
            }
        };
        Self {
            connected: BTreeMap::new(),
            user,
            defaults: HashMap::new(),
            store,
            fallback,
        }
    }

    /// Replaces the connected set with `current` and returns what changed.
    ///
    /// Disconnected monitors lose their runtime default but keep their user assignment.
    pub fn sync(&mut self, current: Vec<MonitorDescriptor>) -> MonitorDiff {
        let mut next: BTreeMap<MonitorId, MonitorDescriptor> = BTreeMap::new();
        for mut desc in current {
            // Two monitors reporting the same identity must still be told apart.
            let mut n = 1;
            while next.contains_key(&desc.id) {
                n += 1;
                desc.id = MonitorId::new(format!("{}#{n}", desc.id));
            }
            next.insert(desc.id.clone(), desc);
        }

        let mut diff = MonitorDiff::default();
        for (id, new) in &next {
            match self.connected.get(id) {
                None => diff.added.push(new.clone()),
                Some(old) if old != new => diff.changed.push(MonitorChange {
                    old: old.clone(),
                    new: new.clone(),
                }),
                Some(_) => {}
            }
        }
        for (id, old) in &self.connected {
            if !next.contains_key(id) {
                diff.removed.push(old.clone());
                self.defaults.remove(id);
            }
        }

        self.connected = next;
        self.resolve_defaults();
        diff
    }

    pub fn connected(&self) -> impl Iterator<Item = &MonitorDescriptor> {
        self.connected.values()
    }

    pub fn descriptor_for_device(&self, device: &str) -> Option<&MonitorDescriptor> {
        self.connected.values().find(|m| m.device_name == device)
    }

    pub fn user_assignment(&self, id: &MonitorId) -> Option<&Assignment> {
        self.user.get(id)
    }

    /// The wallpaper this monitor should show, and where that decision came from.
    pub fn desired(&self, id: &MonitorId) -> Option<(&Assignment, AssignmentSource)> {
        self.user
            .get(id)
            .map(|a| (a, AssignmentSource::User))
            .or_else(|| {
                self.defaults
                    .get(id)
                    .map(|a| (a, AssignmentSource::Default))
            })
    }

    /// Records an explicit user choice and persists it. The in-memory assignment is applied even
    /// if persisting fails; the error is returned so the caller can tell the user.
    pub fn assign_user(
        &mut self,
        id: &MonitorId,
        assignment: Assignment,
    ) -> Result<(), StoreError> {
        self.defaults.remove(id);
        self.user.insert(id.clone(), assignment.clone());
        let result = self.store.upsert(id, &assignment);
        self.resolve_defaults();
        result
    }

    /// Removes a user preference (monitor falls back to the default policy).
    pub fn clear_user(&mut self, id: &MonitorId) -> Result<(), StoreError> {
        self.user.remove(id);
        let result = self.store.delete(id);
        self.resolve_defaults();
        result
    }

    /// Connected monitors ordered primary first, then by device name.
    fn ordered(&self) -> Vec<&MonitorDescriptor> {
        let mut order: Vec<&MonitorDescriptor> = self.connected.values().collect();
        order.sort_by(|a, b| (!a.is_primary, &a.device_name).cmp(&(!b.is_primary, &b.device_name)));
        order
    }

    fn effective(&self, id: &MonitorId) -> Option<&Assignment> {
        self.desired(id).map(|(a, _)| a)
    }

    fn resolve_defaults(&mut self) {
        // A monitor that gained a user assignment no longer needs a default.
        let user_ids: Vec<MonitorId> = self.user.keys().cloned().collect();
        for id in user_ids {
            self.defaults.remove(&id);
        }

        let order: Vec<(MonitorId, bool)> = self
            .ordered()
            .into_iter()
            .map(|m| (m.id.clone(), m.is_primary))
            .collect();
        // The monitor others clone from: the flagged primary, else the first by device name.
        let primary_id: Option<MonitorId> = order
            .iter()
            .find(|(_, primary)| *primary)
            .or_else(|| order.first())
            .map(|(id, _)| id.clone());

        for (id, is_primary) in &order {
            if self.effective(id).is_some() {
                continue;
            }
            let from_primary = if *is_primary {
                None
            } else {
                primary_id.as_ref().and_then(|p| self.effective(p)).cloned()
            };
            if let Some(assignment) = from_primary.or_else(|| self.fallback.clone()) {
                debug!(monitor = %id, video = %assignment.video_path.display(), "Derived default assignment");
                self.defaults.insert(id.clone(), assignment);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Lifecycle: keep the real host in sync with the registry
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// The host has no window for this monitor yet.
    NoWindow,
    Failed(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::NoWindow => f.write_str("no wallpaper window for monitor yet"),
            StartError::Failed(e) => f.write_str(e),
        }
    }
}

/// The real wallpaper host, as the lifecycle layer needs it.
///
/// `player_reports` must not block on player IPC. Starting a wallpaper replaces whatever the
/// host currently runs (or remembers) for that device.
pub trait WallpaperHost: PlaybackSink {
    fn start_wallpaper(&mut self, device: &str, assignment: &Assignment) -> Result<(), StartError>;
    /// Stops and forgets the wallpaper when a monitor has no effective assignment.
    /// The default keeps lightweight test hosts source-compatible.
    fn stop_wallpaper(&mut self, _device: &str) -> Result<(), String> { Ok(()) }
    fn player_reports(&self) -> Vec<PlayerReport>;
}

/// Attempts before giving up on starting a wallpaper until something changes.
pub const MAX_START_ATTEMPTS: u32 = 5;
const START_BACKOFF_BASE: Duration = Duration::from_millis(500);
const REARM_BASE: Duration = Duration::from_secs(30);
const REARM_MAX: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
struct StartRetry {
    desired: Assignment,
    attempts: u32,
    next_at: Instant,
    gave_up: bool,
}

#[derive(Debug, Clone)]
struct Rearm {
    count: u32,
    next_at: Instant,
}

/// Outcome of one lifecycle pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TopologyReport {
    pub diff: MonitorDiff,
    /// Devices a wallpaper was successfully started on.
    pub started: Vec<String>,
    /// Devices whose start failed for good (retry budget spent), with the last error.
    pub start_failures: Vec<(String, String)>,
}

/// Keeps wallpaper assignments synchronized with the real monitor topology.
pub struct MonitorLifecycle {
    registry: MonitorRegistry,
    /// What the host has been told to run per device name. Mirrors the host's own memory of
    /// sources, which survives disconnects, so a returning monitor is restored by the host.
    applied: HashMap<String, Assignment>,
    start_retry: HashMap<String, StartRetry>,
    rearm: HashMap<String, Rearm>,
}

impl MonitorLifecycle {
    pub fn new(registry: MonitorRegistry) -> Self {
        Self {
            registry,
            applied: HashMap::new(),
            start_retry: HashMap::new(),
            rearm: HashMap::new(),
        }
    }

    pub fn registry(&self) -> &MonitorRegistry {
        &self.registry
    }

    pub fn connected_devices(&self) -> Vec<String> {
        self.registry
            .connected()
            .map(|m| m.device_name.clone())
            .collect()
    }

    /// Connected monitors that have a wallpaper the host has been told about. These are the
    /// monitors whose player health matters.
    pub fn assigned_devices(&self) -> Vec<String> {
        self.registry
            .connected()
            .filter(|m| {
                self.registry
                    .desired(&m.id)
                    .is_some_and(|(a, _)| self.applied.get(&m.device_name) == Some(a))
            })
            .map(|m| m.device_name.clone())
            .collect()
    }

    /// Connected monitors that *should* be showing a wallpaper, whether or not starting it has
    /// succeeded yet. A monitor in this set with no healthy player is a fault: this is the set
    /// player health is judged against.
    pub fn wanted_devices(&self) -> Vec<String> {
        self.registry
            .connected()
            .filter(|m| self.registry.desired(&m.id).is_some())
            .map(|m| m.device_name.clone())
            .collect()
    }

    /// Devices whose wallpaper has not been started yet (window pending or retries running).
    pub fn pending_devices(&self) -> Vec<String> {
        let mut v: Vec<String> = self.start_retry.keys().cloned().collect();
        v.sort();
        v
    }

    /// Synchronizes with a fresh enumeration. An empty enumeration is treated as a failed
    /// query, never as "all monitors vanished".
    pub fn sync<H: WallpaperHost>(
        &mut self,
        current: Vec<MonitorDescriptor>,
        host: &mut H,
        now: Instant,
    ) -> TopologyReport {
        let mut report = TopologyReport::default();
        if current.is_empty() {
            self.ensure_all(host, now, &mut report);
            return report;
        }

        report.diff = self.registry.sync(current);
        for m in &report.diff.added {
            info!(device = %m.device_name, id = %m.id, "Monitor connected");
        }
        for m in &report.diff.removed {
            info!(device = %m.device_name, id = %m.id, "Monitor disconnected");
            self.start_retry.remove(&m.device_name);
            self.rearm.remove(&m.device_name);
        }
        for c in &report.diff.changed {
            info!(
                device = %c.new.device_name,
                renamed = c.renamed(),
                geometry = c.geometry_changed(),
                primary = c.primary_changed(),
                "Monitor changed"
            );
            if c.renamed() {
                self.applied.remove(&c.old.device_name);
                self.start_retry.remove(&c.old.device_name);
                self.rearm.remove(&c.old.device_name);
            }
        }
        self.ensure_all(host, now, &mut report);
        report
    }

    /// Retries pending starts without a new enumeration.
    pub fn retry_pending<H: WallpaperHost>(
        &mut self,
        host: &mut H,
        now: Instant,
    ) -> TopologyReport {
        let mut report = TopologyReport::default();
        self.ensure_all(host, now, &mut report);
        report
    }

    /// Applies an explicit user choice to the monitor currently named `device`.
    pub fn assign_user<H: WallpaperHost>(
        &mut self,
        device: &str,
        assignment: Assignment,
        host: &mut H,
        now: Instant,
    ) -> Result<(), String> {
        let id = self
            .registry
            .descriptor_for_device(device)
            .map(|m| m.id.clone())
            .ok_or_else(|| format!("monitor '{device}' is not connected"))?;
        let stored = self.registry.assign_user(&id, assignment);
        let mut report = TopologyReport::default();
        self.ensure_all(host, now, &mut report);
        stored.map_err(|e| format!("applied, but could not be saved: {e}"))
    }

    /// Removes the user's choice for `device`; the default policy takes over.
    pub fn unassign_user<H: WallpaperHost>(
        &mut self,
        device: &str,
        host: &mut H,
        now: Instant,
    ) -> Result<(), String> {
        let id = self
            .registry
            .descriptor_for_device(device)
            .map(|m| m.id.clone())
            .ok_or_else(|| format!("monitor '{device}' is not connected"))?;
        let stored = self.registry.clear_user(&id);
        let mut report = TopologyReport::default();
        self.ensure_all(host, now, &mut report);
        stored.map_err(|e| format!("cleared, but could not be saved: {e}"))
    }

    /// Re-issues the wallpaper for monitors whose automatic recovery is exhausted: a bounded,
    /// slowly backing-off manual intervention. Returns the devices restarted.
    pub fn rearm_exhausted<H: WallpaperHost>(
        &mut self,
        exhausted: &[String],
        host: &mut H,
        now: Instant,
    ) -> Vec<String> {
        let mut restarted = Vec::new();
        for device in exhausted {
            let Some(desc) = self.registry.descriptor_for_device(device) else {
                continue;
            };
            let Some((desired, _)) = self.registry.desired(&desc.id) else {
                continue;
            };
            let desired = desired.clone();

            let state = self.rearm.entry(device.clone()).or_insert(Rearm {
                count: 0,
                next_at: now,
            });
            if now < state.next_at {
                continue;
            }
            state.count += 1;
            state.next_at = now
                + REARM_BASE
                    .saturating_mul(1u32 << state.count.min(5))
                    .min(REARM_MAX);

            match host.start_wallpaper(device, &desired) {
                Ok(()) => {
                    warn!(device = %device, attempt = state.count, "Restarted wallpaper after recovery exhaustion");
                    self.applied.insert(device.clone(), desired);
                    restarted.push(device.clone());
                }
                Err(e) => warn!(device = %device, %e, "Manual restart after exhaustion failed"),
            }
        }
        restarted
    }

    /// Clears manual-restart backoff for devices that are healthy again.
    pub fn note_healthy(&mut self, healthy: &[String]) {
        for device in healthy {
            self.rearm.remove(device);
        }
    }

    fn ensure_all<H: WallpaperHost>(
        &mut self,
        host: &mut H,
        now: Instant,
        report: &mut TopologyReport,
    ) {
        let monitors: Vec<MonitorDescriptor> = self.registry.connected().cloned().collect();
        for desc in monitors {
            self.ensure_applied(&desc, host, now, report);
        }
    }

    fn ensure_applied<H: WallpaperHost>(
        &mut self,
        desc: &MonitorDescriptor,
        host: &mut H,
        now: Instant,
        report: &mut TopologyReport,
    ) {
        let device = &desc.device_name;
        let Some((desired, _source)) = self.registry.desired(&desc.id) else {
            self.start_retry.remove(device);
            if self.applied.contains_key(device) {
                match host.stop_wallpaper(device) {
                    Ok(()) => { self.applied.remove(device); }
                    Err(error) => warn!(device = %device, %error, "Could not stop wallpaper after assignment was cleared"),
                }
            }
            return;
        };
        let desired = desired.clone();

        if self.applied.get(device) == Some(&desired) {
            self.start_retry.remove(device);
            return;
        }

        let retry = self
            .start_retry
            .entry(device.clone())
            .or_insert(StartRetry {
                desired: desired.clone(),
                attempts: 0,
                next_at: now,
                gave_up: false,
            });
        if retry.desired != desired {
            // The wish changed while we were failing: start over for the new wish.
            *retry = StartRetry {
                desired: desired.clone(),
                attempts: 0,
                next_at: now,
                gave_up: false,
            };
        }
        if retry.gave_up || now < retry.next_at {
            return;
        }

        match host.start_wallpaper(device, &desired) {
            Ok(()) => {
                info!(device = %device, video = %desired.video_path.display(), "Wallpaper started");
                self.applied.insert(device.clone(), desired);
                self.start_retry.remove(device);
                report.started.push(device.clone());
            }
            Err(e) => {
                retry.attempts += 1;
                if retry.attempts >= MAX_START_ATTEMPTS {
                    retry.gave_up = true;
                    warn!(device = %device, attempts = retry.attempts, error = %e, "Giving up starting wallpaper");
                    report.start_failures.push((device.clone(), e.to_string()));
                } else {
                    retry.next_at =
                        now + START_BACKOFF_BASE.saturating_mul(1u32 << retry.attempts.min(6));
                    debug!(device = %device, attempts = retry.attempts, error = %e, "Wallpaper start failed; will retry");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Scoped sink
// ---------------------------------------------------------------------------------------

/// Restricts a [`PlaybackSink`] to a set of devices, so the reconciler never commands players
/// for monitors that are no longer connected.
pub struct ScopedSink<'a, S: PlaybackSink> {
    pub inner: &'a mut S,
    pub devices: &'a std::collections::HashSet<String>,
}

impl<S: PlaybackSink> PlaybackSink for ScopedSink<'_, S> {
    fn player_states(&self) -> Vec<(String, crate::runtime::ActualPlayback)> {
        self.inner
            .player_states()
            .into_iter()
            .filter(|(d, _)| self.devices.contains(d))
            .collect()
    }

    fn pause_player(&mut self, device: &str) {
        if self.devices.contains(device) {
            self.inner.pause_player(device);
        }
    }

    fn resume_player(&mut self, device: &str) {
        if self.devices.contains(device) {
            self.inner.resume_player(device);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ActualPlayback;
    use std::collections::HashSet;

    const D1: &str = r"\\.\DISPLAY1";
    const D2: &str = r"\\.\DISPLAY2";
    const D3: &str = r"\\.\DISPLAY3";

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

    fn a(path: &str) -> Assignment {
        Assignment::new(path)
    }

    fn registry(fallback: Option<Assignment>) -> MonitorRegistry {
        MonitorRegistry::new(Box::new(InMemoryAssignmentStore::default()), fallback)
    }

    // ---- fake host --------------------------------------------------------------------

    /// Models the real manager: windows exist for connected monitors, players are dropped when
    /// a monitor disappears but the per-device *source memory* survives, and recovery restores
    /// players from that memory.
    #[derive(Default)]
    struct FakeHost {
        windows: HashSet<String>,
        players: HashMap<String, ActualPlayback>,
        sources: HashMap<String, Assignment>,
        start_calls: Vec<(String, String)>,
        pause_calls: Vec<String>,
        resume_calls: Vec<String>,
        fail_start: bool,
        stop_calls: Vec<String>,
    }

    impl FakeHost {
        fn connect(&mut self, device: &str) {
            self.windows.insert(device.to_string());
        }
        /// Like the manager's `attach_all_monitors`: drops windows and players, keeps sources.
        fn disconnect(&mut self, device: &str) {
            self.windows.remove(device);
            self.players.remove(device);
        }
        /// Like `check_and_recover_players`: relaunch from remembered sources. New players play.
        fn recover(&mut self) {
            for device in self.windows.clone() {
                if self.sources.contains_key(&device) && !self.players.contains_key(&device) {
                    self.players.insert(device, ActualPlayback::Playing);
                }
            }
        }
    }

    impl PlaybackSink for FakeHost {
        fn player_states(&self) -> Vec<(String, ActualPlayback)> {
            let mut v: Vec<_> = self.players.iter().map(|(d, s)| (d.clone(), *s)).collect();
            v.sort_by(|x, y| x.0.cmp(&y.0));
            v
        }
        fn pause_player(&mut self, device: &str) {
            self.pause_calls.push(device.to_string());
            self.players
                .insert(device.to_string(), ActualPlayback::Paused);
        }
        fn resume_player(&mut self, device: &str) {
            self.resume_calls.push(device.to_string());
            self.players
                .insert(device.to_string(), ActualPlayback::Playing);
        }
    }

    impl WallpaperHost for FakeHost {
        fn start_wallpaper(&mut self, device: &str, a: &Assignment) -> Result<(), StartError> {
            self.start_calls
                .push((device.to_string(), a.video_path.display().to_string()));
            if self.fail_start {
                return Err(StartError::Failed("mpv missing".into()));
            }
            if !self.windows.contains(device) {
                return Err(StartError::NoWindow);
            }
            self.sources.insert(device.to_string(), a.clone());
            self.players
                .insert(device.to_string(), ActualPlayback::Playing);
            Ok(())
        }
        fn stop_wallpaper(&mut self, device: &str) -> Result<(), String> {
            self.stop_calls.push(device.to_string());
            self.players.remove(device);
            self.sources.remove(device);
            Ok(())
        }
        fn player_reports(&self) -> Vec<PlayerReport> {
            Vec::new()
        }
    }

    fn lifecycle(fallback: Option<Assignment>) -> MonitorLifecycle {
        MonitorLifecycle::new(registry(fallback))
    }

    #[test]
    fn monitor_ids_are_stable_across_device_renumbering() {
        let path =
            r"\\?\DISPLAY#GSM5B09#5&2b1c7a8&0&UID4357#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        let before = MonitorId::from_interface_path(path, r"\\.\DISPLAY2");
        let after = MonitorId::from_interface_path(&path.to_ascii_uppercase(), r"\\.\DISPLAY1");
        assert_eq!(
            before, after,
            "same hardware, different device name, case-insensitive"
        );

        let other = MonitorId::from_interface_path(
            r"\\?\DISPLAY#GSM5B09#5&2b1c7a8&0&UID4358#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}",
            r"\\.\DISPLAY2",
        );
        assert_ne!(
            before, other,
            "identical model on another port is a different monitor"
        );
    }

    #[test]
    fn monitor_id_falls_back_to_device_name_when_no_hardware_identity() {
        let id = MonitorId::from_interface_path("  ", r"\\.\DISPLAY3");
        assert_eq!(id, MonitorId::from_device_name(r"\\.\DISPLAY3"));
        assert!(id.as_str().starts_with("dev:"));
    }

    // ---- registry ---------------------------------------------------------------------

    #[test]
    fn diff_reports_added_removed_and_changed() {
        let mut r = registry(None);
        let d = r.sync(vec![mon("A", D1, 0, true)]);
        assert_eq!(d.added.len(), 1);
        assert!(d.removed.is_empty() && d.changed.is_empty());

        let d = r.sync(vec![mon("A", D1, 0, true), mon("B", D2, 1920, false)]);
        assert_eq!(
            d.added.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["B"]
        );

        let mut moved = mon("B", D2, 3840, false);
        moved.width = 2560;
        let d = r.sync(vec![mon("A", D1, 0, true), moved]);
        assert_eq!(d.changed.len(), 1);
        assert!(d.changed[0].geometry_changed() && !d.changed[0].renamed());

        let d = r.sync(vec![mon("A", D1, 0, true)]);
        assert_eq!(
            d.removed.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["B"]
        );

        assert!(
            r.sync(vec![mon("A", D1, 0, true)]).is_empty(),
            "no change, no diff"
        );
    }

    #[test]
    fn identical_ids_are_disambiguated() {
        let mut r = registry(None);
        r.sync(vec![mon("SAME", D1, 0, true), mon("SAME", D2, 1920, false)]);
        let ids: Vec<String> = r.connected().map(|m| m.id.to_string()).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn default_policy_clones_primary_then_falls_back() {
        // No fallback, nothing assigned: stays unassigned.
        let mut r = registry(None);
        r.sync(vec![mon("A", D1, 0, true)]);
        assert!(r.desired(&MonitorId::new("A")).is_none());

        // With a fallback: primary gets it, a new monitor clones the primary.
        let mut r = registry(Some(a("fallback.mp4")));
        r.sync(vec![mon("A", D1, 0, true)]);
        r.assign_user(&MonitorId::new("A"), a("mine.mp4")).unwrap();
        r.sync(vec![mon("A", D1, 0, true), mon("B", D2, 1920, false)]);
        let (b, src) = r.desired(&MonitorId::new("B")).unwrap();
        assert_eq!(
            (b.video_path.to_str().unwrap(), src),
            ("mine.mp4", AssignmentSource::Default)
        );
    }

    #[test]
    fn defaults_never_overwrite_user_assignments() {
        let mut r = registry(Some(a("fallback.mp4")));
        r.assign_user(&MonitorId::new("B"), a("b-choice.mp4"))
            .unwrap();
        r.sync(vec![mon("A", D1, 0, true), mon("B", D2, 1920, false)]);
        assert_eq!(
            r.desired(&MonitorId::new("B"))
                .unwrap()
                .0
                .video_path
                .to_str()
                .unwrap(),
            "b-choice.mp4"
        );
        assert_eq!(
            r.desired(&MonitorId::new("B")).unwrap().1,
            AssignmentSource::User
        );
        // Primary A has no preference: it gets the fallback, not B's choice.
        assert_eq!(
            r.desired(&MonitorId::new("A"))
                .unwrap()
                .0
                .video_path
                .to_str()
                .unwrap(),
            "fallback.mp4"
        );
    }

    #[test]
    fn disconnect_drops_default_but_keeps_user_preference() {
        let mut r = registry(Some(a("fallback.mp4")));
        r.sync(vec![mon("A", D1, 0, true), mon("B", D2, 1920, false)]);
        r.assign_user(&MonitorId::new("B"), a("b.mp4")).unwrap();
        r.sync(vec![mon("A", D1, 0, true)]);
        assert!(r.desired(&MonitorId::new("B")).is_none() || r.connected().count() == 1);
        assert_eq!(
            r.user_assignment(&MonitorId::new("B"))
                .unwrap()
                .video_path
                .to_str(),
            Some("b.mp4")
        );

        // Reconnect: preference is back.
        r.sync(vec![mon("A", D1, 0, true), mon("B", D2, 1920, false)]);
        assert_eq!(
            r.desired(&MonitorId::new("B")).unwrap().1,
            AssignmentSource::User
        );
    }

    #[test]
    fn store_failure_keeps_runtime_assignment_and_reports_error() {
        let store = InMemoryAssignmentStore {
            fail_writes: true,
            ..Default::default()
        };
        let mut r = MonitorRegistry::new(Box::new(store), None);
        let res = r.assign_user(&MonitorId::new("A"), a("x.mp4"));
        assert!(res.is_err());
        assert!(
            r.user_assignment(&MonitorId::new("A")).is_some(),
            "still applied in memory"
        );
    }

    // ---- sqlite store -----------------------------------------------------------------

    #[test]
    fn sqlite_store_round_trips_and_survives_reopen() {
        let path = std::env::temp_dir().join(format!("rw-assign-{}.db", uuid::Uuid::new_v4()));
        {
            let mut s = SqliteAssignmentStore::open(&path).unwrap();
            let mut asg = a(r"C:\videos\my wall.mp4");
            asg.config.volume = 33;
            asg.config.mute = false;
            asg.config.hardware_acceleration = HardwareAcceleration::D3d11va;
            s.upsert(&MonitorId::new("hw:abc"), &asg).unwrap();
            s.upsert(&MonitorId::new("hw:def"), &a("two.mp4")).unwrap();
            s.upsert(&MonitorId::new("hw:def"), &a("two-b.mp4"))
                .unwrap(); // update
        }
        let mut s = SqliteAssignmentStore::open(&path).unwrap();
        let rows = s.load_all().unwrap();
        assert_eq!(rows.len(), 2);
        let abc = rows
            .iter()
            .find(|(id, _)| id.as_str() == "hw:abc")
            .unwrap()
            .1
            .clone();
        assert_eq!(abc.video_path.to_str(), Some(r"C:\videos\my wall.mp4"));
        assert_eq!((abc.config.volume, abc.config.mute), (33, false));
        assert_eq!(
            abc.config.hardware_acceleration,
            HardwareAcceleration::D3d11va
        );
        assert_eq!(
            rows.iter()
                .find(|(id, _)| id.as_str() == "hw:def")
                .unwrap()
                .1
                .video_path
                .to_str(),
            Some("two-b.mp4")
        );
        s.delete(&MonitorId::new("hw:abc")).unwrap();
        assert_eq!(s.load_all().unwrap().len(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn registry_loads_persisted_preferences_on_startup() {
        let mut s = SqliteAssignmentStore::in_memory().unwrap();
        s.upsert(&MonitorId::new("A"), &a("saved.mp4")).unwrap();
        let mut r = MonitorRegistry::new(Box::new(s), Some(a("fallback.mp4")));
        r.sync(vec![mon("A", D1, 0, true)]);
        assert_eq!(
            r.desired(&MonitorId::new("A"))
                .unwrap()
                .0
                .video_path
                .to_str(),
            Some("saved.mp4")
        );
    }

    // ---- lifecycle --------------------------------------------------------------------

    #[test]
    fn startup_assigns_each_monitor_and_tracks_them_as_assigned() {
        let t = Instant::now();
        let mut host = FakeHost::default();
        host.connect(D1);
        host.connect(D2);
        let mut lc = lifecycle(Some(a("fallback.mp4")));
        let rep = lc.sync(
            vec![mon("A", D1, 0, true), mon("B", D2, 1920, false)],
            &mut host,
            t,
        );
        assert_eq!(rep.started.len(), 2);
        assert_eq!(lc.assigned_devices(), vec![D1.to_string(), D2.to_string()]);
    }

    #[test]
    fn clearing_last_assignment_stops_the_wallpaper() {
        let t = Instant::now();
        let mut host = FakeHost::default();
        host.connect(D1);
        let mut lc = lifecycle(None);
        lc.sync(vec![mon("A", D1, 0, true)], &mut host, t);
        lc.assign_user(D1, a("chosen.mp4"), &mut host, t).unwrap();
        assert_eq!(host.players.get(D1), Some(&ActualPlayback::Playing));

        lc.unassign_user(D1, &mut host, t + ms(10)).unwrap();

        assert_eq!(host.stop_calls, vec![D1.to_string()]);
        assert!(!host.players.contains_key(D1));
        assert!(!host.sources.contains_key(D1));
        assert!(lc.wanted_devices().is_empty());
    }

    /// Monitor added: it gets a default, and nothing already running is touched.
    #[test]
    fn monitor_added_gets_default_without_disturbing_existing_monitors() {
        let t = Instant::now();
        let mut host = FakeHost::default();
        host.connect(D1);
        let mut lc = lifecycle(Some(a("fallback.mp4")));
        lc.sync(vec![mon("A", D1, 0, true)], &mut host, t);
        host.start_calls.clear();

        host.connect(D2);
        let rep = lc.sync(
            vec![mon("A", D1, 0, true), mon("B", D2, 1920, false)],
            &mut host,
            t + ms(500),
        );
        assert_eq!(rep.started, vec![D2.to_string()]);
        assert_eq!(
            host.start_calls.len(),
            1,
            "only the new monitor was started"
        );
        assert_eq!(host.start_calls[0].0, D2);
        assert!(lc.assigned_devices().contains(&D2.to_string()));
    }

    /// Monitor removed: others keep playing, stale state is gone from the assigned set.
    #[test]
    fn monitor_removed_does_not_interrupt_other_monitors() {
        let t = Instant::now();
        let mut host = FakeHost::default();
        host.connect(D1);
        host.connect(D2);
        let mut lc = lifecycle(Some(a("fallback.mp4")));
        lc.sync(
            vec![mon("A", D1, 0, true), mon("B", D2, 1920, false)],
            &mut host,
            t,
        );
        host.start_calls.clear();

        host.disconnect(D2);
        let rep = lc.sync(vec![mon("A", D1, 0, true)], &mut host, t + ms(500));
        assert_eq!(rep.diff.removed.len(), 1);
        assert!(host.start_calls.is_empty(), "primary was not restarted");
        assert!(host.pause_calls.is_empty() && host.resume_calls.is_empty());
        assert_eq!(host.players.get(D1), Some(&ActualPlayback::Playing));
        assert_eq!(lc.assigned_devices(), vec![D1.to_string()]);
        assert_eq!(lc.connected_devices(), vec![D1.to_string()]);
    }

    /// Reconnect restores the user's assignment; the host's own recovery brings the player back.
    #[test]
    fn reconnect_restores_user_assignment() {
        let t = Instant::now();
        let mut host = FakeHost::default();
        host.connect(D1);
        host.connect(D2);
        let mut lc = lifecycle(Some(a("fallback.mp4")));
        lc.sync(
            vec![mon("A", D1, 0, true), mon("B", D2, 1920, false)],
            &mut host,
            t,
        );
        lc.assign_user(D2, a("b-choice.mp4"), &mut host, t).unwrap();
        assert_eq!(host.sources[D2].video_path.to_str(), Some("b-choice.mp4"));

        host.disconnect(D2);
        lc.sync(vec![mon("A", D1, 0, true)], &mut host, t + ms(1000));
        host.start_calls.clear();

        host.connect(D2);
        lc.sync(
            vec![mon("A", D1, 0, true), mon("B", D2, 1920, false)],
            &mut host,
            t + ms(2000),
        );
        host.recover();

        assert!(
            host.start_calls.is_empty(),
            "host recovery restored it; no redundant restart"
        );
        assert_eq!(host.sources[D2].video_path.to_str(), Some("b-choice.mp4"));
        assert_eq!(host.players.get(D2), Some(&ActualPlayback::Playing));
        assert!(lc.assigned_devices().contains(&D2.to_string()));
    }

    /// Windows renumbers displays: the monitor that was DISPLAY2 is now DISPLAY1 and vice versa.
    /// The host remembers sources by *name*, so a name-keyed restore would swap wallpapers.
    #[test]
    fn renumbered_devices_get_the_right_wallpaper() {
        let t = Instant::now();
        let mut host = FakeHost::default();
        host.connect(D1);
        host.connect(D2);
        let mut lc = lifecycle(None);
        lc.sync(
            vec![mon("A", D1, 0, true), mon("B", D2, 1920, false)],
            &mut host,
            t,
        );
        lc.assign_user(D1, a("a.mp4"), &mut host, t).unwrap();
        lc.assign_user(D2, a("b.mp4"), &mut host, t).unwrap();

        // Same hardware, swapped device names.
        host.start_calls.clear();
        lc.sync(
            vec![mon("A", D2, 1920, false), mon("B", D1, 0, true)],
            &mut host,
            t + ms(1000),
        );
        assert_eq!(
            host.sources[D1].video_path.to_str(),
            Some("b.mp4"),
            "B is DISPLAY1 now"
        );
        assert_eq!(
            host.sources[D2].video_path.to_str(),
            Some("a.mp4"),
            "A is DISPLAY2 now"
        );
    }

    #[test]
    fn user_assignment_is_never_replaced_by_topology_changes() {
        let t = Instant::now();
        let mut host = FakeHost::default();
        host.connect(D1);
        let mut lc = lifecycle(Some(a("fallback.mp4")));
        lc.sync(vec![mon("A", D1, 0, true)], &mut host, t);
        lc.assign_user(D1, a("mine.mp4"), &mut host, t).unwrap();

        for i in 1..6u64 {
            host.connect(D3);
            lc.sync(
                vec![mon("A", D1, 0, true), mon("C", D3, 1920, false)],
                &mut host,
                t + ms(i * 1000),
            );
            host.disconnect(D3);
            lc.sync(
                vec![mon("A", D1, 0, true)],
                &mut host,
                t + ms(i * 1000 + 500),
            );
        }
        assert_eq!(host.sources[D1].video_path.to_str(), Some("mine.mp4"));
    }

    #[test]
    fn empty_enumeration_is_ignored_not_treated_as_all_disconnected() {
        let t = Instant::now();
        let mut host = FakeHost::default();
        host.connect(D1);
        let mut lc = lifecycle(Some(a("fallback.mp4")));
        lc.sync(vec![mon("A", D1, 0, true)], &mut host, t);
        let rep = lc.sync(vec![], &mut host, t + ms(500));
        assert!(rep.diff.is_empty());
        assert_eq!(lc.assigned_devices(), vec![D1.to_string()]);
    }

    #[test]
    fn geometry_change_does_not_restart_anything() {
        let t = Instant::now();
        let mut host = FakeHost::default();
        host.connect(D1);
        let mut lc = lifecycle(Some(a("fallback.mp4")));
        lc.sync(vec![mon("A", D1, 0, true)], &mut host, t);
        host.start_calls.clear();
        let mut resized = mon("A", D1, 0, true);
        resized.width = 2560;
        let rep = lc.sync(vec![resized], &mut host, t + ms(500));
        assert_eq!(rep.diff.changed.len(), 1);
        assert!(host.start_calls.is_empty());
    }

    #[test]
    fn start_waits_for_the_window_then_succeeds() {
        let t = Instant::now();
        let mut host = FakeHost::default(); // no window yet
        let mut lc = lifecycle(Some(a("fallback.mp4")));
        let rep = lc.sync(vec![mon("A", D1, 0, true)], &mut host, t);
        assert!(rep.started.is_empty());
        assert_eq!(lc.pending_devices(), vec![D1.to_string()]);
        assert!(
            lc.assigned_devices().is_empty(),
            "not assigned until actually started"
        );

        host.connect(D1);
        assert!(
            lc.retry_pending(&mut host, t + ms(100)).started.is_empty(),
            "backoff not elapsed"
        );
        let rep = lc.retry_pending(&mut host, t + ms(1500));
        assert_eq!(rep.started, vec![D1.to_string()]);
        assert!(lc.pending_devices().is_empty());
    }

    #[test]
    fn start_retries_are_bounded() {
        let t = Instant::now();
        let mut host = FakeHost {
            fail_start: true,
            ..Default::default()
        };
        host.connect(D1);
        let mut lc = lifecycle(Some(a("fallback.mp4")));
        lc.sync(vec![mon("A", D1, 0, true)], &mut host, t);
        let mut failures = Vec::new();
        for i in 1..60u64 {
            failures.extend(lc.retry_pending(&mut host, t + ms(i * 5000)).start_failures);
        }
        assert_eq!(
            host.start_calls.len() as u32,
            MAX_START_ATTEMPTS,
            "stops after the budget"
        );
        assert_eq!(failures.len(), 1, "reported exactly once");
    }

    #[test]
    fn changing_the_wish_resets_a_failed_start() {
        let t = Instant::now();
        let mut host = FakeHost {
            fail_start: true,
            ..Default::default()
        };
        host.connect(D1);
        let mut lc = lifecycle(Some(a("fallback.mp4")));
        lc.sync(vec![mon("A", D1, 0, true)], &mut host, t);
        for i in 1..30u64 {
            lc.retry_pending(&mut host, t + ms(i * 5000));
        }
        host.fail_start = false;
        lc.assign_user(D1, a("new.mp4"), &mut host, t + ms(200_000))
            .unwrap();
        assert_eq!(host.sources[D1].video_path.to_str(), Some("new.mp4"));
    }

    #[test]
    fn exhausted_monitors_are_rearmed_slowly_and_reset_when_healthy() {
        let t = Instant::now();
        let mut host = FakeHost::default();
        host.connect(D1);
        let mut lc = lifecycle(Some(a("fallback.mp4")));
        lc.sync(vec![mon("A", D1, 0, true)], &mut host, t);
        host.start_calls.clear();
        let ex = vec![D1.to_string()];

        assert_eq!(
            lc.rearm_exhausted(&ex, &mut host, t).len(),
            1,
            "first rearm is immediate"
        );
        assert!(
            lc.rearm_exhausted(&ex, &mut host, t + ms(1000)).is_empty(),
            "rate limited"
        );
        assert!(
            lc.rearm_exhausted(&ex, &mut host, t + ms(59_000))
                .is_empty(),
            "second wait is 60 s"
        );
        assert_eq!(lc.rearm_exhausted(&ex, &mut host, t + ms(61_000)).len(), 1);
        assert_eq!(host.start_calls.len(), 2);

        lc.note_healthy(&ex);
        assert_eq!(
            lc.rearm_exhausted(&ex, &mut host, t + ms(62_000)).len(),
            1,
            "backoff reset"
        );
    }

    #[test]
    fn scoped_sink_hides_disconnected_monitors() {
        let mut host = FakeHost::default();
        host.players.insert(D1.to_string(), ActualPlayback::Playing);
        host.players.insert(D2.to_string(), ActualPlayback::Playing);
        let only_d1: HashSet<String> = [D1.to_string()].into_iter().collect();
        let mut scoped = ScopedSink {
            inner: &mut host,
            devices: &only_d1,
        };
        assert_eq!(scoped.player_states().len(), 1);
        scoped.pause_player(D2);
        scoped.pause_player(D1);
        assert_eq!(host.pause_calls, vec![D1.to_string()]);
    }
}
