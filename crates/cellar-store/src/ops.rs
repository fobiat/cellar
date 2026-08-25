//! Cellar's own record: sessions, players, events, console audit.
//!
//! Separate from the bridge's tables by prefix and by purpose. These are
//! observations, and losing them costs history rather than gameplay, so every
//! write here is best-effort at the call site: an operations insert must never
//! be the reason a server fails to start.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{MySqlPool, Row};

use crate::StoreError;

/// Open a session row when the server process starts.
pub async fn begin_session(
    pool: &MySqlPool,
    scope: &str,
    host: Option<&str>,
    command: &str,
) -> Result<u64, StoreError> {
    let result = sqlx::query("INSERT INTO srv_session (scope, host, command) VALUES (?, ?, ?)")
        .bind(scope)
        .bind(host)
        .bind(command)
        .execute(pool)
        .await?;

    Ok(result.last_insert_id())
}

/// Record that the server reached readiness.
pub async fn mark_ready(pool: &MySqlPool, session_id: u64) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE srv_session SET ready_at = CURRENT_TIMESTAMP(3) WHERE id = ? AND ready_at IS NULL",
    )
    .bind(session_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Close a session row when the process exits.
pub async fn end_session(
    pool: &MySqlPool,
    session_id: u64,
    exit_code: Option<i32>,
    graceful: bool,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE srv_session
         SET ended_at = CURRENT_TIMESTAMP(3), exit_code = ?, graceful = ?
         WHERE id = ?",
    )
    .bind(exit_code)
    .bind(graceful)
    .bind(session_id)
    .execute(pool)
    .await?;

    // A session that ends with players still listed as connected would leave
    // rows open forever, and every "who was on at the time" query would then
    // include them. Close them with the session that owned them.
    sqlx::query(
        "UPDATE srv_player_session
         SET left_at = CURRENT_TIMESTAMP(3), leave_reason = 'server_stopped'
         WHERE session_id = ? AND left_at IS NULL",
    )
    .bind(session_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Record a join, and keep the lifetime player row current.
pub async fn player_joined(
    pool: &MySqlPool,
    session_id: Option<u64>,
    steam_id: u64,
    name: &str,
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO srv_player (steam_id, last_name, sessions)
         VALUES (?, ?, 1)
         ON DUPLICATE KEY UPDATE
           last_name = VALUES(last_name),
           last_seen = CURRENT_TIMESTAMP(3),
           sessions = sessions + 1",
    )
    .bind(steam_id)
    .bind(name)
    .execute(&mut *tx)
    .await?;

    sqlx::query("INSERT INTO srv_player_session (session_id, steam_id, name) VALUES (?, ?, ?)")
        .bind(session_id)
        .bind(steam_id)
        .bind(name)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

/// Close the open session for a player and add its length to their total.
pub async fn player_left(
    pool: &MySqlPool,
    session_id: Option<u64>,
    steam_id: u64,
    reason: &str,
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;

    // Only the most recent open row: a reconnect inside one server session
    // leaves an older row that must not be closed twice.
    let row = sqlx::query(
        "SELECT id, TIMESTAMPDIFF(SECOND, joined_at, CURRENT_TIMESTAMP(3)) AS seconds
         FROM srv_player_session
         WHERE steam_id = ? AND left_at IS NULL AND (session_id <=> ?)
         ORDER BY joined_at DESC
         LIMIT 1",
    )
    .bind(steam_id)
    .bind(session_id)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(row) = row {
        let id: u64 = row.try_get("id")?;
        let seconds: i64 = row.try_get("seconds").unwrap_or(0);

        sqlx::query(
            "UPDATE srv_player_session
             SET left_at = CURRENT_TIMESTAMP(3), leave_reason = ?
             WHERE id = ?",
        )
        .bind(reason)
        .bind(id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE srv_player
             SET total_seconds = total_seconds + ?, last_seen = CURRENT_TIMESTAMP(3)
             WHERE steam_id = ?",
        )
        .bind(seconds.max(0))
        .bind(steam_id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Append an observation.
pub async fn record_event(
    pool: &MySqlPool,
    session_id: Option<u64>,
    kind: &str,
    logger: Option<&str>,
    steam_id: Option<u64>,
    payload: Option<&serde_json::Value>,
) -> Result<(), StoreError> {
    let payload = payload
        .map(serde_json::to_string)
        .transpose()
        .map_err(StoreError::Corrupt)?;

    sqlx::query(
        "INSERT INTO srv_event (session_id, kind, logger, steam_id, payload)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(kind)
    .bind(logger)
    .bind(steam_id)
    .bind(payload)
    .execute(pool)
    .await?;

    Ok(())
}

/// Record a console command and its reply.
pub async fn record_command(
    pool: &MySqlPool,
    session_id: Option<u64>,
    actor: &str,
    command: &str,
    reply: &[String],
    ok: bool,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO srv_command (session_id, actor, command, reply, ok) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(actor)
    .bind(command)
    .bind(reply.join("\n"))
    .bind(ok)
    .execute(pool)
    .await?;

    Ok(())
}

/// A player as the roster and the web UI show them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerRecord {
    pub steam_id: u64,
    pub last_name: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub total_seconds: u64,
    pub sessions: u64,
}

/// Every account ever seen, most recent first.
pub async fn players(pool: &MySqlPool, limit: u32) -> Result<Vec<PlayerRecord>, StoreError> {
    let rows = sqlx::query(
        "SELECT steam_id, last_name, first_seen, last_seen, total_seconds, sessions
         FROM srv_player ORDER BY last_seen DESC LIMIT ?",
    )
    .bind(limit.clamp(1, 1000))
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(PlayerRecord {
                steam_id: row.try_get("steam_id")?,
                last_name: row.try_get("last_name")?,
                first_seen: row.try_get("first_seen")?,
                last_seen: row.try_get("last_seen")?,
                total_seconds: row.try_get("total_seconds")?,
                sessions: row.try_get("sessions")?,
            })
        })
        .collect()
}

/// Delete events older than `days`. Zero keeps everything.
pub async fn prune_events(pool: &MySqlPool, days: u32) -> Result<u64, StoreError> {
    if days == 0 {
        return Ok(0);
    }

    let result = sqlx::query(
        "DELETE FROM srv_event WHERE at < DATE_SUB(CURRENT_TIMESTAMP(3), INTERVAL ? DAY)",
    )
    .bind(days)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}
