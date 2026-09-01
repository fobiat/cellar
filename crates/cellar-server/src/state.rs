//! Shared state for every route.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
///
/// An enum rather than a trait object: the in-memory variant exists so the
/// bridge's HTTP contract can be tested without a database, and two variants do
/// not justify dynamic dispatch.
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
}

/// A fixed-window limiter, per process.
///
/// §7.2 asks for one because the caller is a game host and a compromised host is
/// the thing being limited. A fixed window is coarse and that is fine here: the
/// legitimate caller writes a handful of documents per player event, so any
/// limit that does not interfere with that is doing its job.
pub struct RateLimiter {
    per_minute: u32,
    window: Mutex<(Instant, u32)>,
}

pub struct LoginLimiter {
    per_minute: u32,
    window: Mutex<(Instant, u32)>,
}

impl LoginLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            window: Mutex::new((Instant::now(), 0)),
        }
    }

    pub fn allow(&self) -> bool {
        let Ok(mut window) = self.window.lock() else {
            return false;
        };

        if window.0.elapsed() >= Duration::from_secs(60) {
            *window = (Instant::now(), 0);
        }

        window.1 < self.per_minute
    }

    pub fn record_failure(&self) {
        if let Ok(mut window) = self.window.lock() {
            window.1 = window.1.saturating_add(1);
        }
    }
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
            // A poisoned lock must not become a denial of service against the
            // one client that is allowed to call.
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
    pub auth: Policy,
    pub scope: String,
    pub max_body_bytes: usize,
    pub rate_limiter: RateLimiter,
    pub login_limiter: LoginLimiter,
    /// The supervisor, when one is running. Absent for a bridge-only process.
    pub supervisor: Option<Handle>,
    /// The operations database, when configured.
    pub pool: Option<MySqlPool>,
    /// Displayed in the database panel before an operator runs a query.
    pub database_schema_owner: String,
    /// The locally-hosted MariaDB supervisor, when `[mariadb].managed` is on.
    /// Absent for a remote database, same as `pool` above but one layer up:
    /// this is about who is running the server, not how Cellar talks to it.
    pub mariadb: Option<cellar_mariadb::Handle>,
    /// The credential the dump and restore clients need. sqlx holds a pool, not
    /// a URL, and `mariadb-dump` is a separate process that has to be told
    /// where to connect.
    pub database_url: Option<cellar_core::Secret>,
    pub mariadb_config: cellar_core::config::MariaDbConfig,
    pub backup_config: cellar_core::config::BackupConfig,
    /// Argon2 hash of the web UI password, when the web UI is exposed.
    pub web_password_hash: Option<cellar_core::Secret>,
    /// Explicit web authentication policy.
    pub web_auth: cellar_core::config::WebAuthMode,
    pub web_secure_cookies: bool,
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
    ///
    /// The per-instance fields used to be flat here, about a dozen of them, and
    /// every route read them as if there were one server because there was.
    pub instances: crate::registry::Registry,
    pub config_path: Mutex<Option<PathBuf>>,
    /// Every recurring job this process runs, set once at startup.
    ///
    /// A `OnceLock` rather than a field on `new`, because the jobs need things
    /// this state owns (the program-update status) and so cannot be built
    /// before it. Absent in a process that has no jobs, which is any test and
    /// any deployment with backups, updates and retention all off.
    pub scheduler: std::sync::OnceLock<std::sync::Arc<cellar_runtime::Scheduler>>,
    pub web_bind: String,
    pub web_enabled: bool,
    pub shutdown_requested: std::sync::Arc<AtomicBool>,

    reads: AtomicU64,
    writes: AtomicU64,
    absent: AtomicU64,
    refused: AtomicU64,
    would_conflict: AtomicU64,
    healthy: std::sync::atomic::AtomicBool,
    last_error: Mutex<Option<String>>,
}

impl AppState {
    /// The instance an unqualified request means.
    ///
    /// Every accessor below reads through this. The `Target` extractor replaces
    /// them with the instance the caller actually asked for; until it exists,
    /// one server means the primary and these read exactly what the flat fields
    /// used to hold.
    pub fn primary(&self) -> Option<&crate::registry::Entry> {
        self.instances.primary()
    }

    fn primary_descriptor(&self) -> Option<&crate::registry::Descriptor> {
        self.primary().map(|entry| &entry.descriptor)
    }

    pub fn log_file(&self) -> Option<&std::path::Path> {
        self.primary_descriptor()
            .and_then(|d| d.log_file.as_deref())
    }

    pub fn configured_game(&self) -> Option<&str> {
        self.primary_descriptor().and_then(|d| d.game.as_deref())
    }

    pub fn configured_map(&self) -> Option<&str> {
        self.primary_descriptor().and_then(|d| d.map.as_deref())
    }

    pub fn game_data_dir(&self) -> Option<&std::path::Path> {
        self.primary_descriptor()
            .and_then(|d| d.data_dir.as_deref())
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

    pub fn bridge_bind(&self) -> Option<&str> {
        self.primary_descriptor().map(|d| d.bridge_bind.as_str())
    }

    pub fn bridge_enabled(&self) -> bool {
        self.primary_descriptor().is_some_and(|d| d.bridge_enabled)
    }

    pub fn new(documents: Documents, auth: Policy, scope: impl Into<String>) -> Self {
        Self {
            documents,
            auth,
            scope: scope.into(),
            max_body_bytes: 1024 * 1024,
            rate_limiter: RateLimiter::new(600),
            login_limiter: LoginLimiter::new(10),
            supervisor: None,
            pool: None,
            database_schema_owner: "gamemode".to_owned(),
            mariadb: None,
            database_url: None,
            mariadb_config: Default::default(),
            backup_config: Default::default(),
            web_password_hash: None,
            web_auth: Default::default(),
            web_secure_cookies: false,
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
            config_path: Mutex::new(None),
            scheduler: std::sync::OnceLock::new(),
            web_bind: "127.0.0.1:8081".to_owned(),
            web_enabled: false,
            shutdown_requested: std::sync::Arc::new(AtomicBool::new(false)),
            reads: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            absent: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            would_conflict: AtomicU64::new(0),
            healthy: std::sync::atomic::AtomicBool::new(true),
            last_error: Mutex::new(None),
        }
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
            last_error: self.last_error.lock().ok().and_then(|e| e.clone()),
        }
    }

    pub fn active_config_name(&self) -> Option<String> {
        self.config_path
            .lock()
            .ok()
            .and_then(|path| path.as_ref()?.file_stem()?.to_str().map(str::to_owned))
    }

    pub fn config_directory(&self) -> Option<PathBuf> {
        self.config_path
            .lock()
            .ok()
            .and_then(|path| path.as_ref()?.parent().map(Path::to_path_buf))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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
    fn login_limiter_counts_failures_without_blocking_successful_attempts() {
        let limiter = LoginLimiter::new(2);
        assert!(limiter.allow());
        limiter.record_failure();
        assert!(limiter.allow());
        limiter.record_failure();
        assert!(!limiter.allow());
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
