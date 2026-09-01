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

/// One thing that happened, from either the audit or the observation table.
///
/// A single row type for both because an operator asking "what happened at
/// 21:04" does not care which table it landed in, and the two are only
/// meaningful next to each other: a crash three seconds after a command is the
/// pair that explains itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub at: DateTime<Utc>,
    /// `command` for the audit table, otherwise the event's own kind.
    pub kind: String,
    /// Who caused it. `operator` for a console command, `server` otherwise.
    pub source: &'static str,
    /// The operator's name for a command, the logger for an event.
    pub actor: Option<String>,
    pub detail: String,
    /// A command's reply, truncated. Absent for events.
    pub reply: Option<String>,
    /// Whether a command succeeded. Absent for events, which have no verdict.
    pub ok: Option<bool>,
    #[serde(with = "cellar_core::event::steam_id_wire::option")]
    pub steam_id: Option<u64>,
    pub session_id: Option<u64>,
    /// Which supervised server. Absent for a row whose session predates scopes
    /// or was written without one.
    pub scope: Option<String>,
}

/// What to include in an activity listing.
#[derive(Debug, Clone, Default)]
pub struct ActivityQuery {
    /// Only this instance's scope. `None` is every scope in the database.
    pub scope: Option<String>,
    /// `operator`, `server`, or both when `None`.
    pub source: Option<String>,
    /// Case-insensitive substring of the detail, the actor or the reply.
    pub text: Option<String>,
    /// How far back. Zero or `None` is everything retained.
    pub days: Option<u32>,
    pub limit: u32,
}

/// The merged audit and observation timeline, newest first.
///
/// Two queries merged in Rust rather than a SQL `UNION`: the two tables share
/// almost no columns, so a union needs six `NULL AS` casts per side, and the
/// version that reads clearly is the one that will still be correct after
/// somebody adds a column. Each side is limited before the merge and the merge
/// is truncated, which is exact because both sides arrive newest-first.
pub async fn activity(
    pool: &MySqlPool,
    query: &ActivityQuery,
) -> Result<Vec<ActivityEntry>, StoreError> {
    let limit = query.limit.clamp(1, 2000);
    let mut entries = Vec::new();

    let wants = |source: &str| {
        query
            .source
            .as_deref()
            .is_none_or(|wanted| wanted.eq_ignore_ascii_case(source))
    };

    if wants("operator") {
        // A LEFT JOIN, not an inner one. `session_id` is nullable on both
        // tables, and an inner join would silently drop every command run
        // while no server was up, which is exactly when an operator is most
        // likely to be looking.
        let rows = sqlx::query(
            "SELECT c.at, c.actor, c.command, c.reply, c.ok, c.session_id, s.scope
             FROM srv_command c
             LEFT JOIN srv_session s ON s.id = c.session_id
             WHERE (? IS NULL OR s.scope = ?)
               AND (? = 0 OR c.at >= DATE_SUB(CURRENT_TIMESTAMP(3), INTERVAL ? DAY))
             ORDER BY c.at DESC, c.id DESC
             LIMIT ?",
        )
        .bind(query.scope.as_deref())
        .bind(query.scope.as_deref())
        .bind(query.days.unwrap_or(0))
        .bind(query.days.unwrap_or(0))
        .bind(limit)
        .fetch_all(pool)
        .await?;

        for row in rows {
            entries.push(ActivityEntry {
                at: row.try_get("at")?,
                kind: "command".to_owned(),
                source: "operator",
                actor: row.try_get("actor")?,
                detail: row.try_get("command")?,
                reply: row.try_get("reply")?,
                ok: row.try_get("ok")?,
                steam_id: None,
                session_id: row.try_get("session_id")?,
                scope: row.try_get("scope")?,
            });
        }
    }

    if wants("server") {
        let rows = sqlx::query(
            "SELECT e.at, e.kind, e.logger, e.steam_id, e.payload, e.session_id, s.scope
             FROM srv_event e
             LEFT JOIN srv_session s ON s.id = e.session_id
             WHERE (? IS NULL OR s.scope = ?)
               AND (? = 0 OR e.at >= DATE_SUB(CURRENT_TIMESTAMP(3), INTERVAL ? DAY))
             ORDER BY e.at DESC, e.id DESC
             LIMIT ?",
        )
        .bind(query.scope.as_deref())
        .bind(query.scope.as_deref())
        .bind(query.days.unwrap_or(0))
        .bind(query.days.unwrap_or(0))
        .bind(limit)
        .fetch_all(pool)
        .await?;

        for row in rows {
            // Read as bytes, not as `String`. MariaDB reports a JSON column as
            // BLOB over the wire, so decoding it straight to `Option<String>`
            // is a hard `ColumnDecode` error rather than a wrong value. It went
            // unnoticed at first because every payload written before today was
            // NULL, which decodes fine either way.
            let payload: Option<Vec<u8>> = row.try_get("payload")?;
            // The recorder writes a plain string into that JSON column, so the
            // stored bytes are `"pid 4 ..."` with the quotes. Unwrap a JSON
            // string back to its text and leave anything else as written, so
            // older rows and any future structured payload still read.
            let detail = payload
                .map(|raw| {
                    let raw = String::from_utf8_lossy(&raw).into_owned();
                    match serde_json::from_str::<serde_json::Value>(&raw) {
                        Ok(serde_json::Value::String(text)) => text,
                        _ => raw,
                    }
                })
                .unwrap_or_default();

            entries.push(ActivityEntry {
                at: row.try_get("at")?,
                kind: row.try_get("kind")?,
                source: "server",
                actor: row.try_get("logger")?,
                detail,
                reply: None,
                ok: None,
                steam_id: row.try_get("steam_id")?,
                session_id: row.try_get("session_id")?,
                scope: row.try_get("scope")?,
            });
        }
    }

    // Filtered here rather than in SQL because the two tables put the operator's
    // words in different columns, and one `LIKE` per column per table is four
    // clauses that have to stay in step with the row type.
    if let Some(needle) = query.text.as_deref().filter(|text| !text.trim().is_empty()) {
        let needle = needle.trim().to_lowercase();
        entries.retain(|entry| {
            entry.detail.to_lowercase().contains(&needle)
                || entry.kind.to_lowercase().contains(&needle)
                || entry
                    .actor
                    .as_deref()
                    .is_some_and(|actor| actor.to_lowercase().contains(&needle))
                || entry
                    .reply
                    .as_deref()
                    .is_some_and(|reply| reply.to_lowercase().contains(&needle))
        });
    }

    entries.sort_by_key(|entry| std::cmp::Reverse(entry.at));
    entries.truncate(limit as usize);
    Ok(entries)
}

/// A player as the roster and the web UI show them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerRecord {
    #[serde(with = "cellar_core::event::steam_id_wire")]
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
