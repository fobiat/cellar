//! The bridge's document table.
//!
//! Whole-document reads and writes, keyed by `(scope, doc_key)`, with every
//! write also appended to a revision history. No query language: the gamemode's
//! own interface has five operations and deliberately no sixth, and matching it
//! keeps the seam honest.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{MySqlPool, Row};

use crate::StoreError;

/// A stored document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub key: String,
    pub body: serde_json::Value,
    pub revision: u64,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Option<String>,
}

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOutcome {
    pub revision: u64,
    pub created: bool,
    /// True when the caller named a revision that was not the current one.
    ///
    /// Recorded, never enforced. The shipped `HostedDocumentStore` documents
    /// that it never returns `Rejected` because the concurrency question is
    /// still open upstream, so answering 409 would turn a recoverable write into
    /// a lost one at a client with no code to retry it.
    pub would_conflict: bool,
}

/// Read a JSON column back into a value.
///
/// MySQL 8 hands a `JSON` column over the wire as text; MariaDB implements
/// `JSON` as `LONGTEXT` with a binary collation and hands it over as a BLOB, so
/// asking for a `String` fails there with a type mismatch. Reading bytes works
/// on both, and this is the one place that has to know it.
fn decode_json(row: &sqlx::mysql::MySqlRow, column: &str) -> Result<serde_json::Value, StoreError> {
    let bytes: Vec<u8> = row.try_get(column)?;
    serde_json::from_slice(&bytes).map_err(StoreError::Corrupt)
}

/// Read one document.
pub async fn get(pool: &MySqlPool, scope: &str, key: &str) -> Result<Option<Document>, StoreError> {
    let row = sqlx::query(
        "SELECT body, revision, updated_at, updated_by
         FROM aj_document WHERE scope = ? AND doc_key = ?",
    )
    .bind(scope)
    .bind(key)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let body = decode_json(&row, "body")?;

    Ok(Some(Document {
        key: key.to_owned(),
        body,
        revision: row.try_get::<u64, _>("revision")?,
        updated_at: row.try_get("updated_at")?,
        updated_by: row.try_get("updated_by")?,
    }))
}

/// Whether a document exists, without reading it.
pub async fn exists(pool: &MySqlPool, scope: &str, key: &str) -> Result<bool, StoreError> {
    let row = sqlx::query("SELECT 1 FROM aj_document WHERE scope = ? AND doc_key = ?")
        .bind(scope)
        .bind(key)
        .fetch_optional(pool)
        .await?;

    Ok(row.is_some())
}

/// Write a document whole, bumping its revision and appending to the history.
///
/// `expected_revision` is compared and reported, not enforced. See
/// [`WriteOutcome::would_conflict`].
pub async fn put(
    pool: &MySqlPool,
    scope: &str,
    key: &str,
    body: &serde_json::Value,
    written_by: Option<&str>,
    expected_revision: Option<u64>,
) -> Result<WriteOutcome, StoreError> {
    let text = serde_json::to_string(body).map_err(StoreError::Corrupt)?;

    // One transaction: a body written without its history row would leave the
    // audit trail with a hole exactly where somebody is looking for it.
    let mut tx = pool.begin().await?;

    let current: Option<u64> =
        sqlx::query("SELECT revision FROM aj_document WHERE scope = ? AND doc_key = ? FOR UPDATE")
            .bind(scope)
            .bind(key)
            .fetch_optional(&mut *tx)
            .await?
            .map(|row| row.try_get("revision"))
            .transpose()?;

    let created = current.is_none();
    let next = current.unwrap_or(0) + 1;
    let would_conflict = match (expected_revision, current) {
        (Some(expected), Some(actual)) => expected != actual,
        (Some(_), None) => true,
        (None, _) => false,
    };

    sqlx::query(
        "INSERT INTO aj_document (scope, doc_key, body, revision, updated_by)
         VALUES (?, ?, ?, ?, ?)
         ON DUPLICATE KEY UPDATE
           body = VALUES(body), revision = VALUES(revision), updated_by = VALUES(updated_by)",
    )
    .bind(scope)
    .bind(key)
    .bind(&text)
    .bind(next)
    .bind(written_by)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO aj_document_revision (scope, doc_key, revision, body, written_by)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(scope)
    .bind(key)
    .bind(next)
    .bind(&text)
    .bind(written_by)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(WriteOutcome {
        revision: next,
        created,
        would_conflict,
    })
}

/// Delete a document. Not part of the gamemode's interface; the admin UI uses it.
pub async fn delete(pool: &MySqlPool, scope: &str, key: &str) -> Result<bool, StoreError> {
    let result = sqlx::query("DELETE FROM aj_document WHERE scope = ? AND doc_key = ?")
        .bind(scope)
        .bind(key)
        .execute(pool)
        .await?;

    Ok(result.rows_affected() > 0)
}

/// A row in the document browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSummary {
    pub key: String,
    pub revision: u64,
    pub bytes: u64,
    pub updated_at: DateTime<Utc>,
}

/// List documents, newest first, optionally filtered by key prefix.
pub async fn list(
    pool: &MySqlPool,
    scope: &str,
    prefix: Option<&str>,
    limit: u32,
) -> Result<Vec<DocumentSummary>, StoreError> {
    let pattern = format!("{}%", prefix.unwrap_or(""));

    let rows = sqlx::query(
        "SELECT doc_key, revision, updated_at, OCTET_LENGTH(body) AS bytes
         FROM aj_document
         WHERE scope = ? AND doc_key LIKE ?
         ORDER BY updated_at DESC
         LIMIT ?",
    )
    .bind(scope)
    .bind(pattern)
    .bind(limit.clamp(1, 1000))
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(DocumentSummary {
                key: row.try_get("doc_key")?,
                revision: row.try_get::<u64, _>("revision")?,
                bytes: row.try_get::<i64, _>("bytes")? as u64,
                updated_at: row.try_get("updated_at")?,
            })
        })
        .collect()
}

/// One historical revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    pub revision: u64,
    pub body: serde_json::Value,
    pub written_at: DateTime<Utc>,
    pub written_by: Option<String>,
}

/// Read a document's history, newest first.
pub async fn revisions(
    pool: &MySqlPool,
    scope: &str,
    key: &str,
    limit: u32,
) -> Result<Vec<Revision>, StoreError> {
    let rows = sqlx::query(
        "SELECT revision, body, written_at, written_by
         FROM aj_document_revision
         WHERE scope = ? AND doc_key = ?
         ORDER BY revision DESC
         LIMIT ?",
    )
    .bind(scope)
    .bind(key)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(Revision {
                revision: row.try_get::<u64, _>("revision")?,
                body: decode_json(&row, "body")?,
                written_at: row.try_get("written_at")?,
                written_by: row.try_get("written_by")?,
            })
        })
        .collect()
}
