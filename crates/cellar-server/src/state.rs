//! Shared state for every route.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use cellar_core::snapshot::BridgeStats;
use cellar_runtime::Handle;
use sqlx::MySqlPool;
use std::path::Path;

use crate::auth::Policy;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProgramUpdateStatus {
    pub current: String,
    pub latest: Option<String>,
    pub update_available: bool,
    pub release_url: String,
    pub checked_at: Option<String>,
    pub error: Option<String>,
}

pub struct ShutdownSignal {
    requested: AtomicBool,
    notify: tokio::sync::Notify,
}

impl ShutdownSignal {
    pub fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    pub fn request(&self) -> bool {
        if self
            .requested
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.notify.notify_waiters();
        true
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    pub async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_requested() {
                return;
            }
            notified.await;
        }
    }
}

impl Default for ShutdownSignal {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ActiveMaintenance {
    pub name: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CompletedMaintenance {
    pub name: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: chrono::DateTime<chrono::Utc>,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MaintenanceStatus {
    pub active: Option<ActiveMaintenance>,
    pub last: Option<CompletedMaintenance>,
}

struct MaintenanceInner {
    gate: Arc<tokio::sync::Semaphore>,
    bridge_gate: Arc<tokio::sync::RwLock<()>>,
    status: Mutex<MaintenanceStatus>,
}

#[derive(Clone)]
pub struct MaintenanceCoordinator {
    inner: Arc<MaintenanceInner>,
}

impl MaintenanceCoordinator {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MaintenanceInner {
                gate: Arc::new(tokio::sync::Semaphore::new(1)),
                bridge_gate: Arc::new(tokio::sync::RwLock::new(())),
                status: Mutex::new(MaintenanceStatus::default()),
            }),
        }
    }

    pub fn try_start(&self, name: impl Into<String>) -> Result<MaintenanceGuard, MaintenanceBusy> {
        self.start(name.into(), false)
    }

    pub fn try_start_exclusive(
        &self,
        name: impl Into<String>,
    ) -> Result<MaintenanceGuard, MaintenanceBusy> {
        self.start(name.into(), true)
    }

    fn start(
        &self,
        name: String,
        block_bridge_writes: bool,
    ) -> Result<MaintenanceGuard, MaintenanceBusy> {
        let permit = self
            .inner
            .gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| MaintenanceBusy {
                active: self
                    .status()
                    .active
                    .map(|operation| operation.name)
                    .unwrap_or_else(|| "another maintenance operation".to_owned()),
            })?;
        let bridge_permit = if block_bridge_writes {
            Some(
                self.inner
                    .bridge_gate
                    .clone()
                    .try_write_owned()
                    .map_err(|_| MaintenanceBusy {
                        active: "a bridge write in flight".to_owned(),
                    })?,
            )
        } else {
            None
        };
        let active = ActiveMaintenance {
            name,
            started_at: chrono::Utc::now(),
        };
        if let Ok(mut status) = self.inner.status.lock() {
            status.active = Some(active.clone());
        }
        Ok(MaintenanceGuard {
            inner: self.inner.clone(),
            active,
            permit: Some(permit),
            bridge_permit,
            finished: false,
        })
    }

    pub fn try_bridge_write(
        &self,
    ) -> Result<tokio::sync::OwnedRwLockReadGuard<()>, MaintenanceBusy> {
        self.inner
            .bridge_gate
            .clone()
            .try_read_owned()
            .map_err(|_| MaintenanceBusy {
                active: self
                    .status()
                    .active
                    .map(|operation| operation.name)
                    .unwrap_or_else(|| "database restore".to_owned()),
            })
    }

    pub fn status(&self) -> MaintenanceStatus {
        self.inner
            .status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default()
    }
}

impl Default for MaintenanceCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("maintenance is busy with {active}")]
pub struct MaintenanceBusy {
    pub active: String,
}

pub struct MaintenanceGuard {
    inner: Arc<MaintenanceInner>,
    active: ActiveMaintenance,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    bridge_permit: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
    finished: bool,
}

#[derive(Clone, Default)]
struct ActiveRuntime {
    path: Option<PathBuf>,
    config: Option<cellar_core::config::Config>,
    instance_id: Option<cellar_core::config::InstanceId>,
    descriptor: Option<crate::registry::Descriptor>,
}

impl std::fmt::Debug for MaintenanceGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MaintenanceGuard")
            .field("name", &self.active.name)
            .field("finished", &self.finished)
            .finish()
    }
}

impl MaintenanceGuard {
    pub fn succeed(mut self, detail: impl Into<String>) {
        self.finish(true, detail.into());
    }

    pub fn fail(mut self, detail: impl Into<String>) {
        self.finish(false, detail.into());
    }

    fn finish(&mut self, ok: bool, detail: String) {
        if let Ok(mut status) = self.inner.status.lock() {
            status.active = None;
            status.last = Some(CompletedMaintenance {
                name: self.active.name.clone(),
                started_at: self.active.started_at.to_owned(),
                finished_at: chrono::Utc::now(),
                ok,
                detail,
            });
        }
        self.finished = true;
        self.bridge_permit.take();
        self.permit.take();
    }
}

impl Drop for MaintenanceGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(
                false,
                "operation ended before reporting a result".to_owned(),
            );
        }
    }
}

impl ProgramUpdateStatus {
    pub fn new(release_url: impl Into<String>) -> Self {
        Self {
            current: env!("CARGO_PKG_VERSION").to_owned(),
            latest: None,
            update_available: false,
            release_url: release_url.into(),
            checked_at: None,
            error: None,
        }
    }
}

/// Where documents live.
/// An enum rather than a trait object: the in-memory variant exists so the bridge's HTTP contract can be tested without a database, and two variants do not justify dynamic dispatch.
#[derive(Clone)]
pub enum Documents {
    MySql(MySqlPool),
    /// Test and dry-run backing. Never selected by config.
    Memory(std::sync::Arc<Mutex<std::collections::HashMap<(String, String), serde_json::Value>>>),
}

impl Documents {
    pub fn memory() -> Self {
        Self::Memory(Default::default())
    }

    pub async fn get(&self, scope: &str, key: &str) -> Result<Option<serde_json::Value>, String> {
        match self {
            Self::MySql(pool) => cellar_store::document::get(pool, scope, key)
                .await
                .map(|found| found.map(|d| d.body))
                .map_err(|e| e.to_string()),
            Self::Memory(map) => {
                let map = map
                    .lock()
                    .map_err(|_| "the memory store lock was poisoned".to_owned())?;
                Ok(map.get(&(scope.to_owned(), key.to_owned())).cloned())
            }
        }
    }

    pub async fn exists(&self, scope: &str, key: &str) -> Result<bool, String> {
        match self {
            Self::MySql(pool) => cellar_store::document::exists(pool, scope, key)
                .await
                .map_err(|e| e.to_string()),
            Self::Memory(map) => {
                let map = map
                    .lock()
                    .map_err(|_| "the memory store lock was poisoned".to_owned())?;
                Ok(map.contains_key(&(scope.to_owned(), key.to_owned())))
            }
        }
    }

    pub async fn put(
        &self,
        scope: &str,
        key: &str,
        body: &serde_json::Value,
        by: Option<&str>,
    ) -> Result<cellar_store::document::WriteOutcome, String> {
        match self {
            Self::MySql(pool) => cellar_store::document::put(pool, scope, key, body, by, None)
                .await
                .map_err(|e| e.to_string()),
            Self::Memory(map) => {
                let mut map = map
                    .lock()
                    .map_err(|_| "the memory store lock was poisoned".to_owned())?;
                let created = map
                    .insert((scope.to_owned(), key.to_owned()), body.clone())
                    .is_none();
                Ok(cellar_store::document::WriteOutcome {
                    revision: 1,
                    created,
                    would_conflict: false,
                })
            }
        }
    }

    pub async fn snapshot(&self, scope: &str) -> Result<Vec<SnapshotDocument>, String> {
        match self {
            Self::MySql(pool) => cellar_store::document::all(pool, scope)
                .await
                .map(|documents| {
                    documents
                        .into_iter()
                        .map(|document| SnapshotDocument {
                            key: document.key,
                            body: document.body,
                        })
                        .collect()
                })
                .map_err(|e| e.to_string()),
            Self::Memory(map) => {
                let map = map
                    .lock()
                    .map_err(|_| "the memory store lock was poisoned".to_owned())?;
                Ok(map
                    .iter()
                    .filter(|((document_scope, _), _)| document_scope == scope)
                    .map(|((_, key), body)| SnapshotDocument {
                        key: key.clone(),
                        body: body.clone(),
                    })
                    .collect())
            }
        }
    }

    pub async fn delete(&self, scope: &str, key: &str) -> Result<bool, String> {
        match self {
            Self::MySql(pool) => cellar_store::document::delete(pool, scope, key)
                .await
                .map_err(|e| e.to_string()),
            Self::Memory(map) => {
                let mut map = map
                    .lock()
                    .map_err(|_| "the memory store lock was poisoned".to_owned())?;
                Ok(map.remove(&(scope.to_owned(), key.to_owned())).is_some())
            }
        }
    }

    pub async fn replace_scope(
        &self,
        scope: &str,
        documents: &[SnapshotDocument],
        by: Option<&str>,
    ) -> Result<cellar_store::document::ReplaceOutcome, String> {
        let replacement = documents
            .iter()
            .map(|document| (document.key.clone(), document.body.clone()))
            .collect::<Vec<_>>();
        match self {
            Self::MySql(pool) => {
                cellar_store::document::replace_scope(pool, scope, &replacement, by)
                    .await
                    .map_err(|why| why.to_string())
            }
            Self::Memory(map) => {
                let mut keys = std::collections::HashSet::with_capacity(replacement.len());
                for (key, _) in &replacement {
                    cellar_core::doc_key::check(key).map_err(|why| why.to_string())?;
                    if !keys.insert(key.clone()) {
                        return Err(format!("document key '{key}' appears more than once"));
                    }
                }
                let mut map = map
                    .lock()
                    .map_err(|_| "the memory store lock was poisoned".to_owned())?;
                let removed = map
                    .keys()
                    .filter(|(document_scope, key)| document_scope == scope && !keys.contains(key))
                    .count();
                map.retain(|(document_scope, _), _| document_scope != scope);
                map.extend(
                    replacement
                        .into_iter()
                        .map(|(key, body)| ((scope.to_owned(), key), body)),
                );
                Ok(cellar_store::document::ReplaceOutcome {
                    documents: documents.len(),
                    removed,
                })
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SnapshotDocument {
    pub key: String,
    pub body: serde_json::Value,
}

/// The state one document bridge listener owns.
pub struct BridgeState {
    pub documents: Documents,
    pub auth: Policy,
    pub scope: String,
    pub max_body_bytes: usize,
    pub rate_limiter: RateLimiter,
    reads: AtomicU64,
    writes: AtomicU64,
    absent: AtomicU64,
    refused: AtomicU64,
    would_conflict: AtomicU64,
    healthy: AtomicBool,
    last_error: Mutex<Option<String>>,
    maintenance: RwLock<Option<MaintenanceCoordinator>>,
}

impl BridgeState {
    pub fn new(
        documents: Documents,
        auth: Policy,
        scope: impl Into<String>,
        max_body_bytes: usize,
        rate_limit_per_minute: u32,
    ) -> Self {
        Self {
            documents,
            auth,
            scope: scope.into(),
            max_body_bytes,
            rate_limiter: RateLimiter::new(rate_limit_per_minute),
            reads: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            absent: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            would_conflict: AtomicU64::new(0),
            healthy: AtomicBool::new(true),
            last_error: Mutex::new(None),
            maintenance: RwLock::new(None),
        }
    }

    pub fn set_maintenance(&self, maintenance: MaintenanceCoordinator) {
        if let Ok(mut current) = self.maintenance.write() {
            *current = Some(maintenance);
        }
    }

    pub fn try_document_write(
        &self,
    ) -> Result<Option<tokio::sync::OwnedRwLockReadGuard<()>>, MaintenanceBusy> {
        self.maintenance
            .read()
            .ok()
            .and_then(|maintenance| maintenance.clone())
            .map(|maintenance| maintenance.try_bridge_write())
            .transpose()
    }

    pub fn bridge_read(&self) {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.healthy.store(true, Ordering::Relaxed);
    }

    pub fn bridge_absent(&self) {
        self.absent.fetch_add(1, Ordering::Relaxed);
        self.healthy.store(true, Ordering::Relaxed);
    }

    pub fn bridge_write(&self, would_conflict: bool) {
        self.writes.fetch_add(1, Ordering::Relaxed);
        if would_conflict {
            self.would_conflict.fetch_add(1, Ordering::Relaxed);
        }
        self.healthy.store(true, Ordering::Relaxed);
    }

    pub fn bridge_failed(&self, why: &str) {
        self.refused.fetch_add(1, Ordering::Relaxed);
        self.healthy.store(false, Ordering::Relaxed);
        if let Ok(mut last) = self.last_error.lock() {
            *last = Some(why.to_owned());
        }
    }

    pub fn stats(&self) -> BridgeStats {
        BridgeStats {
            enabled: true,
            healthy: self.healthy.load(Ordering::Relaxed),
            reads: self.reads.load(Ordering::Relaxed),
            writes: self.writes.load(Ordering::Relaxed),
            absent: self.absent.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
            would_conflict: self.would_conflict.load(Ordering::Relaxed),
            last_error: self.last_error.lock().ok().and_then(|error| error.clone()),
        }
    }
}

/// A fixed-window limiter, per process.
/// §7.2 asks for one because the caller is a game host and a compromised host is the thing being limited. A fixed window is coarse and that is fine here: the legitimate caller writes a handful of documents per player event, so any limit that does not interfere with that is doing its job.
pub struct RateLimiter {
    per_minute: u32,
    window: Mutex<(Instant, u32)>,
}

pub struct LoginLimiter {
    per_minute: u32,
    window: Mutex<LoginWindow>,
}

struct LoginWindow {
    started: Instant,
    failures: u32,
    generation: u64,
}

pub(crate) struct LoginReservation<'a> {
    limiter: &'a LoginLimiter,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasswordWorkError {
    Busy,
    TimedOut,
    WorkerFailed,
}

const PASSWORD_WORK_TIMEOUT: Duration = Duration::from_secs(30);
const PASSWORD_WORK_CAPACITY: usize = 2;

pub(crate) struct PasswordWork {
    permits: Arc<tokio::sync::Semaphore>,
    timeout: Duration,
}

impl PasswordWork {
    pub(crate) fn new(capacity: usize) -> Self {
        Self::with_timeout(capacity, PASSWORD_WORK_TIMEOUT)
    }

    pub(crate) fn with_timeout(capacity: usize, timeout: Duration) -> Self {
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(capacity)),
            timeout,
        }
    }

    pub(crate) async fn run<T, F>(&self, operation: F) -> Result<T, PasswordWorkError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| PasswordWorkError::Busy)?;
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        });

        match tokio::time::timeout(self.timeout, task).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(PasswordWorkError::WorkerFailed),
            Err(_) => Err(PasswordWorkError::TimedOut),
        }
    }
}

impl LoginLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            window: Mutex::new(LoginWindow {
                started: Instant::now(),
                failures: 0,
                generation: 0,
            }),
        }
    }

    pub(crate) fn reserve(&self) -> Option<LoginReservation<'_>> {
        let Ok(mut window) = self.window.lock() else {
            return None;
        };

        if window.started.elapsed() >= Duration::from_secs(60) {
            window.started = Instant::now();
            window.failures = 0;
            window.generation = window.generation.wrapping_add(1);
        }

        if window.failures >= self.per_minute {
            return None;
        }
        window.failures = window.failures.saturating_add(1);
        Some(LoginReservation {
            limiter: self,
            generation: window.generation,
        })
    }

    fn refund(&self, generation: u64) {
        if let Ok(mut window) = self.window.lock()
            && window.generation == generation
        {
            window.failures = window.failures.saturating_sub(1);
        }
    }
}

impl LoginReservation<'_> {
    pub(crate) fn succeeded(self) {
        self.limiter.refund(self.generation);
    }

    pub(crate) fn not_started(self) {
        self.limiter.refund(self.generation);
    }

    pub(crate) fn failed(self) {}
}

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            window: Mutex::new((Instant::now(), 0)),
        }
    }

    /// Whether this request may proceed.
    pub fn allow(&self) -> bool {
        if self.per_minute == 0 {
            return true;
        }

        let Ok(mut window) = self.window.lock() else {
            // A poisoned lock must not become a denial of service against the one client that is allowed to call.
            return true;
        };

        if window.0.elapsed() >= Duration::from_secs(60) {
            *window = (Instant::now(), 0);
        }

        window.1 += 1;
        window.1 <= self.per_minute
    }
}

/// Everything the routes share.
pub struct AppState {
    pub documents: Documents,
    pub scope: String,
    pub login_limiter: LoginLimiter,
    /// The supervisor, when one is running. Absent for a bridge-only process.
    pub supervisor: Option<Handle>,
    /// The operations database, when configured.
    pub pool: Option<MySqlPool>,
    /// Displayed in the database panel before an operator runs a query.
    pub database_schema_owner: String,
    /// Explicit opt-in for authenticated direct data and schema control.
    pub database_direct_control: bool,
    /// The locally-hosted MariaDB supervisor, when `[mariadb].managed` is on. Absent for a remote database, same as `pool` above but one layer up: this is about who is running the server, not how Cellar talks to it.
    pub mariadb: Option<cellar_mariadb::Handle>,
    /// The credential the dump and restore clients need. sqlx holds a pool, not a URL, and `mariadb-dump` is a separate process that has to be told where to connect.
    pub database_url: Option<cellar_core::Secret>,
    pub mariadb_config: cellar_core::config::MariaDbConfig,
    pub backup_config: cellar_core::config::BackupConfig,
    pub persistence_config: cellar_core::config::PersistenceConfig,
    /// Argon2 hash of the web UI password, when the web UI is exposed.
    pub web_password_hash: Mutex<Option<cellar_core::Secret>>,
    /// The private file used when the operator completes first-run setup.
    pub web_password_path: Mutex<Option<PathBuf>>,
    /// One setup operation may own password storage at a time.
    pub web_password_setup: Arc<tokio::sync::Semaphore>,
    pub(crate) password_work: PasswordWork,
    /// Explicit web authentication policy.
    pub web_auth: cellar_core::config::WebAuthMode,
    pub web_secure_cookies: bool,
    pub web_tailscale_configured: bool,
    pub web_tailscale_enabled: std::sync::atomic::AtomicBool,
    pub web_tailscale_bind: Mutex<Option<String>>,
    /// Bearer token for read-only machine integrations under `/api/v1`.
    pub external_api_token: Option<cellar_core::Secret>,
    /// Live web sessions.
    pub sessions: crate::session::Sessions,
    /// Where to look for versions, when version checking is configured.
    pub version_probe: Option<cellar_update::Probe>,
    pub update_config: cellar_core::config::UpdateConfig,
    pub program_update: std::sync::Arc<tokio::sync::RwLock<ProgramUpdateStatus>>,
    pub release_config: cellar_core::config::ReleaseConfig,
    /// Every supervised server this process owns.
    /// The per-instance fields used to be flat here, about a dozen of them, and every route read them as if there were one server because there was.
    pub instances: crate::registry::Registry,
    active_runtime: RwLock<ActiveRuntime>,
    /// Every recurring job this process runs, set once at startup.
    /// A `OnceLock` rather than a field on `new`, because the jobs need things this state owns (the program-update status) and so cannot be built before it. Absent in a process that has no jobs, which is any test and any deployment with backups, updates and retention all off.
    pub scheduler: std::sync::OnceLock<std::sync::Arc<cellar_runtime::Scheduler>>,
    pub maintenance: MaintenanceCoordinator,
    pub web_bind: String,
    pub web_enabled: bool,
    pub shutdown_requested: std::sync::Arc<ShutdownSignal>,
    bridges: RwLock<BTreeMap<cellar_core::config::InstanceId, Arc<BridgeState>>>,
    primary_bridge: RwLock<Option<Arc<BridgeState>>>,
}

impl AppState {
    /// The instance an unqualified request means.
    /// Every accessor below reads through this. The `Target` extractor replaces them with the instance the caller actually asked for; until it exists, one server means the primary and these read exactly what the flat fields used to hold.
    pub fn primary(&self) -> Option<&crate::registry::Entry> {
        self.instances.primary()
    }

    pub fn active_descriptor(&self, entry: &crate::registry::Entry) -> crate::registry::Descriptor {
        self.active_runtime
            .read()
            .ok()
            .and_then(|active| {
                (active.instance_id.as_ref() == Some(&entry.id))
                    .then(|| active.descriptor.clone())
                    .flatten()
            })
            .unwrap_or_else(|| entry.descriptor.clone())
    }

    pub fn active_snapshot(
        &self,
        entry: &crate::registry::Entry,
    ) -> (
        Option<PathBuf>,
        Option<cellar_core::config::Config>,
        crate::registry::Descriptor,
    ) {
        self.active_runtime
            .read()
            .ok()
            .map(|active| {
                let descriptor = if active.instance_id.as_ref() == Some(&entry.id) {
                    active
                        .descriptor
                        .clone()
                        .unwrap_or_else(|| entry.descriptor.clone())
                } else {
                    entry.descriptor.clone()
                };
                (active.path.clone(), active.config.clone(), descriptor)
            })
            .unwrap_or_else(|| (None, None, entry.descriptor.clone()))
    }

    pub fn commit_active_runtime(
        &self,
        path: PathBuf,
        config: &cellar_core::config::Config,
        instance: &cellar_core::config::Instance,
    ) -> Result<(), String> {
        let mut active = self
            .active_runtime
            .write()
            .map_err(|_| "the active runtime lock was poisoned".to_owned())?;
        *active = ActiveRuntime {
            path: Some(path),
            config: Some(config.clone()),
            instance_id: Some(instance.id.clone()),
            descriptor: Some(crate::registry::Descriptor::from_instance(instance)),
        };
        Ok(())
    }

    pub fn active_config_path(&self) -> Option<PathBuf> {
        self.active_runtime
            .read()
            .ok()
            .and_then(|active| active.path.clone())
    }

    fn primary_descriptor(&self) -> Option<crate::registry::Descriptor> {
        self.primary().map(|entry| self.active_descriptor(entry))
    }

    pub fn log_file(&self) -> Option<PathBuf> {
        self.primary_descriptor().and_then(|d| d.log_file)
    }

    pub fn configured_game(&self) -> Option<String> {
        self.primary_descriptor().and_then(|d| d.game)
    }

    pub fn configured_map(&self) -> Option<String> {
        self.primary_descriptor().and_then(|d| d.map)
    }

    pub fn game_data_dir(&self) -> Option<PathBuf> {
        self.primary_descriptor().and_then(|d| d.data_dir)
    }

    pub fn server_port(&self) -> Option<u16> {
        self.primary_descriptor().map(|d| d.port)
    }

    pub fn query_port(&self) -> Option<u16> {
        self.primary_descriptor().map(|d| d.query_port)
    }

    pub fn server_direct_connect(&self) -> bool {
        self.primary_descriptor().is_some_and(|d| d.direct_connect)
    }

    pub fn bridge_bind(&self) -> Option<String> {
        self.primary_descriptor().map(|d| d.bridge_bind)
    }

    pub fn bridge_enabled(&self) -> bool {
        self.primary_descriptor().is_some_and(|d| d.bridge_enabled)
    }

    pub fn new(documents: Documents, auth: Policy, scope: impl Into<String>) -> Self {
        let scope = scope.into();
        let maintenance = MaintenanceCoordinator::new();
        let primary_bridge = Arc::new(BridgeState::new(
            documents.clone(),
            auth,
            scope.clone(),
            1024 * 1024,
            600,
        ));
        primary_bridge.set_maintenance(maintenance.clone());
        Self {
            documents,
            scope,
            login_limiter: LoginLimiter::new(10),
            supervisor: None,
            pool: None,
            database_schema_owner: "gamemode".to_owned(),
            database_direct_control: false,
            mariadb: None,
            database_url: None,
            mariadb_config: Default::default(),
            backup_config: Default::default(),
            persistence_config: Default::default(),
            web_password_hash: Mutex::new(None),
            web_password_path: Mutex::new(None),
            web_password_setup: Arc::new(tokio::sync::Semaphore::new(1)),
            password_work: PasswordWork::new(PASSWORD_WORK_CAPACITY),
            web_auth: Default::default(),
            web_secure_cookies: false,
            web_tailscale_configured: false,
            web_tailscale_enabled: std::sync::atomic::AtomicBool::new(false),
            web_tailscale_bind: Mutex::new(None),
            external_api_token: None,
            sessions: crate::session::Sessions::new(),
            version_probe: None,
            update_config: Default::default(),
            program_update: std::sync::Arc::new(tokio::sync::RwLock::new(
                ProgramUpdateStatus::new(
                    cellar_core::config::UpdateConfig::default().program_release_url,
                ),
            )),
            release_config: Default::default(),
            instances: Default::default(),
            active_runtime: RwLock::new(ActiveRuntime::default()),
            scheduler: std::sync::OnceLock::new(),
            maintenance,
            web_bind: "127.0.0.1:8081".to_owned(),
            web_enabled: false,
            shutdown_requested: std::sync::Arc::new(ShutdownSignal::new()),
            bridges: RwLock::new(BTreeMap::new()),
            primary_bridge: RwLock::new(Some(primary_bridge)),
        }
    }

    pub fn replace_bridges(
        &self,
        primary: Option<&cellar_core::config::InstanceId>,
        bridges: BTreeMap<cellar_core::config::InstanceId, Arc<BridgeState>>,
    ) {
        for bridge in bridges.values() {
            bridge.set_maintenance(self.maintenance.clone());
        }
        let primary_bridge = primary.and_then(|id| bridges.get(id).cloned());
        if let Ok(mut current) = self.bridges.write() {
            *current = bridges;
        }
        if let Ok(mut current) = self.primary_bridge.write() {
            *current = primary_bridge;
        }
    }

    pub fn bridge_for(&self, id: &cellar_core::config::InstanceId) -> Option<Arc<BridgeState>> {
        self.bridges
            .read()
            .ok()
            .and_then(|bridges| bridges.get(id).cloned())
    }

    pub fn bridge_stats_for(&self, id: &cellar_core::config::InstanceId) -> BridgeStats {
        self.bridge_for(id)
            .map_or_else(BridgeStats::default, |bridge| bridge.stats())
    }

    pub fn bridge_read(&self) {
        if let Some(bridge) = self
            .primary_bridge
            .read()
            .ok()
            .and_then(|bridge| bridge.clone())
        {
            bridge.bridge_read();
        }
    }

    pub fn bridge_absent(&self) {
        if let Some(bridge) = self
            .primary_bridge
            .read()
            .ok()
            .and_then(|bridge| bridge.clone())
        {
            bridge.bridge_absent();
        }
    }

    pub fn bridge_write(&self, would_conflict: bool) {
        if let Some(bridge) = self
            .primary_bridge
            .read()
            .ok()
            .and_then(|bridge| bridge.clone())
        {
            bridge.bridge_write(would_conflict);
        }
    }

    pub fn bridge_failed(&self, why: &str) {
        if let Some(bridge) = self
            .primary_bridge
            .read()
            .ok()
            .and_then(|bridge| bridge.clone())
        {
            bridge.bridge_failed(why);
        }
    }

    pub fn stats(&self) -> BridgeStats {
        self.primary_bridge
            .read()
            .ok()
            .and_then(|bridge| bridge.clone())
            .map_or_else(BridgeStats::default, |bridge| bridge.stats())
    }

    pub fn active_config_name(&self) -> Option<String> {
        self.active_config_path()?
            .file_stem()?
            .to_str()
            .map(str::to_owned)
    }

    pub fn config_directory(&self) -> Option<PathBuf> {
        self.active_config_path()?.parent().map(Path::to_path_buf)
    }

    pub fn web_password(&self) -> Option<cellar_core::Secret> {
        self.web_password_hash
            .lock()
            .ok()
            .and_then(|hash| hash.clone())
    }

    pub fn web_tailscale_bind(&self) -> Option<String> {
        self.web_tailscale_bind
            .lock()
            .ok()
            .and_then(|bind| bind.clone())
    }

    pub fn web_tailscale_is_enabled(&self) -> bool {
        self.web_tailscale_enabled.load(Ordering::Relaxed)
    }

    pub fn set_web_password(&self, hash: cellar_core::Secret) -> bool {
        let Ok(mut stored) = self.web_password_hash.lock() else {
            return false;
        };
        *stored = Some(hash);
        self.sessions.destroy_all();
        true
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_signal_is_idempotent_and_wakes_waiters() {
        let signal = Arc::new(ShutdownSignal::new());
        let waiting = tokio::spawn({
            let signal = signal.clone();
            async move { signal.wait().await }
        });

        assert!(signal.request());
        assert!(!signal.request());
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("the waiter was notified")
            .unwrap();
    }

    #[test]
    fn maintenance_operations_are_exclusive_and_publish_their_result() {
        let maintenance = MaintenanceCoordinator::new();
        let first = maintenance
            .try_start("database restore")
            .expect("the first operation owns maintenance");

        let busy = maintenance
            .try_start("database backup")
            .expect_err("a conflicting operation is refused");
        assert_eq!(busy.active, "database restore");
        assert_eq!(
            maintenance.status().active.unwrap().name,
            "database restore"
        );

        first.succeed("restored cellar.sql");
        let status = maintenance.status();
        assert!(status.active.is_none());
        let last = status.last.expect("the completed operation is retained");
        assert_eq!(last.name, "database restore");
        assert!(last.ok);
        assert_eq!(last.detail, "restored cellar.sql");

        assert!(maintenance.try_start("database backup").is_ok());
    }

    #[tokio::test]
    async fn concurrent_maintenance_is_refused_until_the_owner_releases_it() {
        let maintenance = MaintenanceCoordinator::new();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let owner = tokio::spawn({
            let maintenance = maintenance.clone();
            let entered = entered.clone();
            let release = release.clone();
            async move {
                let guard = maintenance
                    .try_start("scheduled database backup")
                    .expect("the scheduled task owns maintenance");
                entered.notify_one();
                release.notified().await;
                guard.succeed("backup complete");
            }
        });

        entered.notified().await;
        let busy = maintenance
            .try_start_exclusive("manual database restore")
            .expect_err("the overlapping restore is refused");
        assert_eq!(busy.active, "scheduled database backup");

        release.notify_one();
        owner.await.unwrap();
        let next = maintenance
            .try_start_exclusive("manual database restore")
            .expect("the gate is reusable after completion");
        next.succeed("restore complete");
    }

    #[test]
    fn restore_exclusion_covers_in_flight_bridge_writes() {
        let maintenance = MaintenanceCoordinator::new();
        let write = maintenance.try_bridge_write().unwrap();
        assert!(maintenance.try_start_exclusive("database restore").is_err());
        drop(write);

        let restore = maintenance.try_start_exclusive("database restore").unwrap();
        assert!(maintenance.try_bridge_write().is_err());
        restore.succeed("restored");
        assert!(maintenance.try_bridge_write().is_ok());
    }

    #[test]
    fn the_limiter_allows_up_to_its_budget_then_refuses() {
        let limiter = RateLimiter::new(3);
        assert!(limiter.allow());
        assert!(limiter.allow());
        assert!(limiter.allow());
        assert!(!limiter.allow());
    }

    #[test]
    fn a_zero_budget_means_unlimited_rather_than_blocked() {
        let limiter = RateLimiter::new(0);
        for _ in 0..1000 {
            assert!(limiter.allow());
        }
    }

    #[test]
    fn login_limiter_reserves_in_flight_attempts_before_work_starts() {
        let limiter = LoginLimiter::new(2);
        let first = limiter.reserve().expect("first attempt has capacity");
        let second = limiter.reserve().expect("second attempt has capacity");
        assert!(limiter.reserve().is_none());

        first.succeeded();
        let replacement = limiter.reserve().expect("success refunds its reservation");
        second.failed();
        replacement.failed();
        assert!(limiter.reserve().is_none());
    }

    #[test]
    fn an_abandoned_started_attempt_keeps_its_failure_charge() {
        let limiter = LoginLimiter::new(1);
        {
            let _abandoned = limiter.reserve().expect("attempt has capacity");
        }

        assert!(limiter.reserve().is_none());
    }

    #[test]
    fn a_previous_window_completion_does_not_change_the_current_window() {
        let limiter = LoginLimiter::new(1);
        let previous = limiter.reserve().expect("previous window has capacity");
        limiter.window.lock().unwrap().started = Instant::now() - Duration::from_secs(61);
        let current = limiter.reserve().expect("new window has capacity");

        previous.failed();
        assert!(limiter.reserve().is_none());
        current.succeeded();
        assert!(limiter.reserve().is_some());
    }

    #[tokio::test]
    async fn password_work_runs_on_a_blocking_worker() {
        let runtime_thread = std::thread::current().id();
        let work = PasswordWork::new(1);

        let worker_thread = work
            .run(|| std::thread::current().id())
            .await
            .expect("worker has capacity");

        assert_ne!(worker_thread, runtime_thread);
    }

    #[tokio::test]
    async fn saturated_password_work_is_refused_before_it_starts() {
        let work = PasswordWork::new(0);
        let result = work.run(|| "must not run").await;

        assert_eq!(result, Err(PasswordWorkError::Busy));
    }

    #[tokio::test]
    async fn timed_out_password_work_keeps_a_bounded_response_time() {
        let work = PasswordWork::with_timeout(1, Duration::from_millis(5));
        let result = work
            .run(|| {
                std::thread::sleep(Duration::from_millis(50));
                "too late"
            })
            .await;

        assert_eq!(result, Err(PasswordWorkError::TimedOut));
        assert_eq!(work.run(|| "must wait").await, Err(PasswordWorkError::Busy));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(work.run(|| "ready").await, Ok("ready"));
    }

    #[tokio::test]
    async fn the_memory_backend_round_trips_a_document() {
        let documents = Documents::memory();
        let body = serde_json::json!({ "balance": 8000 });

        assert!(
            documents
                .get("s", "characters/1.json")
                .await
                .unwrap()
                .is_none()
        );
        assert!(!documents.exists("s", "characters/1.json").await.unwrap());

        let outcome = documents
            .put("s", "characters/1.json", &body, None)
            .await
            .unwrap();
        assert!(outcome.created);

        assert_eq!(
            documents.get("s", "characters/1.json").await.unwrap(),
            Some(body)
        );
        assert!(documents.exists("s", "characters/1.json").await.unwrap());
    }

    #[tokio::test]
    async fn the_memory_backend_replaces_exactly_one_scope() {
        let documents = Documents::memory();
        documents
            .put("s", "old.json", &serde_json::json!({"old": true}), None)
            .await
            .unwrap();
        documents
            .put(
                "other",
                "old.json",
                &serde_json::json!({"untouched": true}),
                None,
            )
            .await
            .unwrap();

        documents
            .replace_scope(
                "s",
                &[
                    SnapshotDocument {
                        key: "one.json".to_owned(),
                        body: serde_json::json!({"one": 1}),
                    },
                    SnapshotDocument {
                        key: "two.json".to_owned(),
                        body: serde_json::json!({"two": 2}),
                    },
                ],
                Some("restore-test"),
            )
            .await
            .unwrap();

        assert!(documents.get("s", "old.json").await.unwrap().is_none());
        assert!(documents.get("s", "one.json").await.unwrap().is_some());
        assert!(documents.get("s", "two.json").await.unwrap().is_some());
        assert!(documents.get("other", "old.json").await.unwrap().is_some());
    }

    #[test]
    fn committing_a_profile_updates_path_and_route_metadata_together() {
        let config = cellar_core::config::Config::parse_at(
            r#"
            [server]
            executable = "/srv/sbox/sbox-server"
            project = "/srv/game/game.sbproj"
            game = "fobiat.first"
            map = "first.map"
            data_dir = "/srv/first/data"
            "#,
            std::path::Path::new("/tmp/first.toml"),
        )
        .unwrap();
        let first = config.primary().unwrap();
        let second_config = cellar_core::config::Config::parse_at(
            r#"
            [server]
            executable = "/srv/sbox/sbox-server"
            project = "/srv/game/game.sbproj"
            game = "fobiat.second"
            map = "second.map"
            data_dir = "/srv/second/data"
            "#,
            std::path::Path::new("/tmp/second.toml"),
        )
        .unwrap();
        let second = second_config.primary().unwrap();
        let entry = crate::registry::Entry::from_instance(&first);
        let state = AppState::new(Documents::memory(), Policy::Trusted, "s");

        state
            .commit_active_runtime(PathBuf::from("/tmp/second.toml"), &second_config, &second)
            .unwrap();
        let (path, active_config, descriptor) = state.active_snapshot(&entry);

        assert_eq!(path, Some(PathBuf::from("/tmp/second.toml")));
        assert_eq!(
            active_config
                .and_then(|config| config.primary())
                .and_then(|instance| instance.server.game),
            Some("fobiat.second".to_owned())
        );
        assert_eq!(descriptor.game.as_deref(), Some("fobiat.second"));
        assert_eq!(descriptor.map.as_deref(), Some("second.map"));
        assert_eq!(descriptor.data_dir, Some(PathBuf::from("/srv/second/data")));
    }

    #[test]
    fn a_failure_marks_the_bridge_unhealthy_and_a_success_clears_it() {
        let state = AppState::new(Documents::memory(), Policy::Trusted, "s");
        assert!(state.stats().healthy);

        state.bridge_failed("mysql is gone");
        assert!(!state.stats().healthy);
        assert_eq!(state.stats().last_error.as_deref(), Some("mysql is gone"));

        state.bridge_read();
        assert!(state.stats().healthy);
    }
}
