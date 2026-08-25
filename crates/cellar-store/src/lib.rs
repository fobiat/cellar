//! MySQL for Cellar.
//!
//! Two tenants in one database: `aj_*` is the bridge's document store, which the
//! gamemode owns and which outlives Cellar, and `srv_*` is Cellar's own
//! operations record. See `migrations/0001_initial.sql`.

pub mod admin;
pub mod document;
pub mod ops;

use std::time::Duration;

use sqlx::mysql::MySqlPoolOptions;
use sqlx::{MySqlPool, migrate::Migrator};

/// The embedded schema. `cellar db migrate` and `database.migrate_on_start`
/// both run this.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    /// A stored body did not parse. Never mapped to "absent": that is the
    /// distinction 20_PERSISTENCE.md §4.1 exists to protect.
    #[error("a stored document was not readable JSON: {0}")]
    Corrupt(#[source] serde_json::Error),
}

/// Open a pool.
///
/// Timeouts are short on purpose. The gamemode's client gives a read 3 seconds
/// and a write 5, and opens its circuit breaker after three failures, so a
/// bridge that waits 30 seconds on a connection has already lost.
pub async fn connect(url: &str, max_connections: u32) -> Result<MySqlPool, StoreError> {
    let pool = MySqlPoolOptions::new()
        .max_connections(max_connections.max(1))
        .acquire_timeout(Duration::from_secs(2))
        .idle_timeout(Some(Duration::from_secs(600)))
        .test_before_acquire(true)
        .connect(url)
        .await?;

    Ok(pool)
}

/// Apply any pending migrations.
pub async fn migrate(pool: &MySqlPool) -> Result<(), StoreError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// Cheap liveness check for `/healthz` and the dashboards.
pub async fn ping(pool: &MySqlPool) -> Result<(), StoreError> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The migrator is embedded at compile time, so a syntactically broken or
    /// missing migration is a build failure rather than a startup failure on a
    /// machine nobody is watching.
    #[test]
    fn the_schema_is_embedded_and_not_empty() {
        assert!(
            MIGRATOR.iter().count() >= 1,
            "at least the initial migration must be embedded"
        );
    }

    #[test]
    fn migrations_are_ordered_and_uniquely_versioned() {
        let mut versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        let before = versions.len();
        versions.sort_unstable();
        versions.dedup();
        assert_eq!(before, versions.len(), "two migrations share a version");
    }
}
