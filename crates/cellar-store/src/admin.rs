//! The database browser behind the web UI's Records tab.
//!
//! A small, read-only phpMyAdmin: list tables, describe one, page through rows,
//! and run a query. Enough to answer "what is actually in there" without
//! shelling into a pod or exposing a second service.
//!
//! Read-only is enforced twice, and neither is decorative. [`is_read_only`] is a
//! statement-shape check applied before anything reaches the server, and the
//! deployment is expected to give the browser its own `SELECT`-only MySQL grant.
//! The first stops a mistake; only the second stops an attacker, and the code
//! says so rather than implying the parser is a security boundary.

use serde::{Deserialize, Serialize};
use sqlx::{Column, MySqlPool, Row, TypeInfo, ValueRef};

use crate::StoreError;

/// Largest number of rows any browse or query returns.
pub const MAX_ROWS: u32 = 500;

/// A table, as the sidebar lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSummary {
    pub name: String,
    pub rows: u64,
    pub bytes: u64,
    pub comment: Option<String>,
}

/// Facts about the live database, without assuming a gamemode schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseInfo {
    pub connected: bool,
    pub database: Option<String>,
    pub server_version: Option<String>,
    pub table_count: u64,
    pub bytes: u64,
    pub schema_owner: String,
}

/// Read connection metadata from the server and information schema.
pub async fn info(pool: &MySqlPool) -> Result<DatabaseInfo, StoreError> {
    let connection = sqlx::query("SELECT DATABASE() AS database_name, VERSION() AS server_version")
        .fetch_one(pool)
        .await?;
    let totals = sqlx::query(
        "SELECT COUNT(*) AS table_count, COALESCE(SUM(DATA_LENGTH + INDEX_LENGTH), 0) AS bytes
         FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE'",
    )
    .fetch_one(pool)
    .await?;

    Ok(DatabaseInfo {
        connected: true,
        database: connection.try_get("database_name")?,
        server_version: connection.try_get("server_version")?,
        // MySQL returns COUNT and SUM as signed BIGINT metadata even when the
        // values cannot be negative. Decode that wire type explicitly.
        table_count: totals.try_get::<i64, _>("table_count")?.max(0) as u64,
        bytes: totals.try_get::<i64, _>("bytes")?.max(0) as u64,
        schema_owner: "unknown".to_owned(),
    })
}

/// A column, as the schema view shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnSummary {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub key: Option<String>,
    pub default: Option<String>,
}

/// A result set, shaped for a table view.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResultSet {
    pub columns: Vec<String>,
    /// Every value rendered as text or null, because a browser table shows text
    /// and guessing a JSON type per column invites a wrong one.
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
}

/// Every table in the current schema.
pub async fn tables(pool: &MySqlPool) -> Result<Vec<TableSummary>, StoreError> {
    let rows = sqlx::query(
        "SELECT TABLE_NAME, TABLE_ROWS, DATA_LENGTH + INDEX_LENGTH AS BYTES, TABLE_COMMENT
         FROM information_schema.TABLES
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE'
         ORDER BY TABLE_NAME",
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let comment: Option<String> = row.try_get("TABLE_COMMENT").ok().flatten();
            Ok(TableSummary {
                name: row.try_get("TABLE_NAME")?,
                // TABLE_ROWS is an estimate for InnoDB. Shown as one.
                rows: row.try_get::<Option<u64>, _>("TABLE_ROWS")?.unwrap_or(0),
                bytes: row.try_get::<Option<u64>, _>("BYTES")?.unwrap_or(0),
                comment: comment.filter(|c| !c.is_empty()),
            })
        })
        .collect()
}

/// One table's columns.
pub async fn columns(pool: &MySqlPool, table: &str) -> Result<Vec<ColumnSummary>, StoreError> {
    let rows = sqlx::query(
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, COLUMN_DEFAULT
         FROM information_schema.COLUMNS
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ?
         ORDER BY ORDINAL_POSITION",
    )
    .bind(table)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let key: String = row.try_get("COLUMN_KEY")?;
            Ok(ColumnSummary {
                name: row.try_get("COLUMN_NAME")?,
                data_type: row.try_get("COLUMN_TYPE")?,
                nullable: row.try_get::<String, _>("IS_NULLABLE")? == "YES",
                key: (!key.is_empty()).then_some(key),
                default: row.try_get("COLUMN_DEFAULT")?,
            })
        })
        .collect()
}

/// Page through a table.
///
/// The table name cannot be a bound parameter, so it is validated against the
/// live schema first and only then interpolated. A name that is not already a
/// table in this database never reaches a query.
pub async fn browse(
    pool: &MySqlPool,
    table: &str,
    limit: u32,
    offset: u64,
) -> Result<ResultSet, StoreError> {
    let known = tables(pool).await?;
    if !known.iter().any(|t| t.name == table) {
        return Err(StoreError::Database(sqlx::Error::Protocol(format!(
            "'{table}' is not a table in this database"
        ))));
    }

    let limit = limit.clamp(1, MAX_ROWS);
    let sql = format!(
        "SELECT * FROM `{}` LIMIT {} OFFSET {}",
        table.replace('`', ""),
        limit + 1,
        offset
    );
    let rows = sqlx::query(&sql).fetch_all(pool).await?;

    Ok(to_result_set(rows, limit))
}

/// Run an operator's query, refusing anything that is not a read.
pub async fn query(pool: &MySqlPool, sql: &str, limit: u32) -> Result<ResultSet, StoreError> {
    if let Err(why) = is_read_only(sql) {
        return Err(StoreError::Database(sqlx::Error::Protocol(why)));
    }

    let limit = limit.clamp(1, MAX_ROWS);
    let rows = sqlx::query(sql).fetch_all(pool).await?;
    Ok(to_result_set(rows, limit))
}

fn to_result_set(rows: Vec<sqlx::mysql::MySqlRow>, limit: u32) -> ResultSet {
    let Some(first) = rows.first() else {
        return ResultSet::default();
    };

    let columns: Vec<String> = first
        .columns()
        .iter()
        .map(|c| c.name().to_owned())
        .collect();

    let truncated = rows.len() as u32 > limit;

    let rendered = rows
        .iter()
        .take(limit as usize)
        .map(|row| (0..columns.len()).map(|i| render_value(row, i)).collect())
        .collect();

    ResultSet {
        columns,
        rows: rendered,
        truncated,
    }
}

/// Render one cell as text, whatever its type.
fn render_value(row: &sqlx::mysql::MySqlRow, index: usize) -> Option<String> {
    let raw = row.try_get_raw(index).ok()?;
    if raw.is_null() {
        return None;
    }

    let type_name = raw.type_info().name().to_ascii_uppercase();

    // Numbers and times have a sensible text form; everything else is read as
    // bytes and shown lossily, which is right for a browser and wrong for
    // anything that needs the value back.
    //
    // Unsigned first. Every id and every revision in this schema is
    // `BIGINT UNSIGNED`, which does not decode as `i64`, so trying signed first
    // renders the most common column in the database as its type name.
    if let Ok(value) = row.try_get::<u64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<i64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<bool, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<f64, _>(index) {
        return Some(value.to_string());
    }
    if let Ok(value) = row.try_get::<chrono::DateTime<chrono::Utc>, _>(index) {
        return Some(value.to_rfc3339());
    }
    if let Ok(value) = row.try_get::<String, _>(index) {
        return Some(value);
    }
    if let Ok(bytes) = row.try_get::<Vec<u8>, _>(index) {
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }

    Some(format!("<{type_name}>"))
}

/// Whether a statement is a read.
///
/// A shape check, not a SQL parser, and not a security boundary. It exists so an
/// operator cannot fat-finger a `DELETE` into the query box. The boundary is the
/// grant the browser's database user holds.
pub fn is_read_only(sql: &str) -> Result<(), String> {
    let stripped = strip_comments(sql);

    // Two statements in one string is how a read turns into a write. Split on
    // semicolons that are actually separators: one inside a string literal is
    // data, and refusing it would reject legitimate queries.
    let statements: Vec<&str> = split_statements(&stripped)
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect();

    let trimmed = match statements.as_slice() {
        [] => return Err("empty query".to_owned()),
        [one] => one.trim(),
        _ => return Err("one statement at a time".to_owned()),
    };

    const READS: [&str; 5] = ["SELECT", "SHOW", "DESCRIBE", "DESC", "EXPLAIN"];
    let head = trimmed
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();

    // A leading `(` is a parenthesised SELECT or a UNION, both reads.
    let head = if head.starts_with('(') {
        "SELECT".to_owned()
    } else {
        head
    };

    if !READS.contains(&head.as_str()) {
        return Err(format!(
            "{head} is not a read; this browser runs SELECT, SHOW, DESCRIBE and EXPLAIN"
        ));
    }

    // `SELECT ... INTO OUTFILE` writes a file on the server.
    let upper = trimmed.to_ascii_uppercase();
    for forbidden in [
        "INTO OUTFILE",
        "INTO DUMPFILE",
        "FOR UPDATE",
        "LOCK IN SHARE MODE",
    ] {
        if upper.contains(forbidden) {
            return Err(format!("{forbidden} is not allowed here"));
        }
    }

    Ok(())
}

/// Split on semicolons that separate statements, ignoring those inside string
/// literals and quoted identifiers.
fn split_statements(sql: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut in_string: Option<char> = None;
    let mut escaped = false;

    for (at, c) in sql.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        match in_string {
            Some(quote) => {
                if c == '\\' {
                    escaped = true;
                } else if c == quote {
                    in_string = None;
                }
            }
            None => match c {
                '\'' | '"' | '`' => in_string = Some(c),
                ';' => {
                    parts.push(&sql[start..at]);
                    start = at + 1;
                }
                _ => {}
            },
        }
    }

    parts.push(&sql[start..]);
    parts
}

/// Remove `--`, `#` and `/* */` comments, so they cannot hide a second statement.
fn strip_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut in_string: Option<char> = None;

    while let Some(c) = chars.next() {
        if let Some(quote) = in_string {
            out.push(c);
            if c == '\\' {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
            } else if c == quote {
                in_string = None;
            }
            continue;
        }

        match c {
            '\'' | '"' | '`' => {
                in_string = Some(c);
                out.push(c);
            }
            '-' if chars.peek() == Some(&'-') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '#' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = '\0';
                for c in chars.by_ref() {
                    if previous == '*' && c == '/' {
                        break;
                    }
                    previous = c;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }

    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_allowed() {
        for sql in [
            "SELECT * FROM aj_document",
            "  select 1  ",
            "SHOW TABLES",
            "DESCRIBE srv_event",
            "EXPLAIN SELECT * FROM srv_player",
            "(SELECT 1) UNION (SELECT 2)",
            "SELECT * FROM aj_document;",
        ] {
            is_read_only(sql).unwrap_or_else(|e| panic!("{sql} should be allowed: {e}"));
        }
    }

    #[test]
    fn writes_are_refused() {
        for sql in [
            "DELETE FROM aj_document",
            "UPDATE srv_player SET total_seconds = 0",
            "DROP TABLE aj_document",
            "INSERT INTO srv_event (kind) VALUES ('x')",
            "TRUNCATE srv_event",
            "GRANT ALL ON *.* TO 'x'@'%'",
            "CALL something()",
        ] {
            assert!(is_read_only(sql).is_err(), "{sql} must be refused");
        }
    }

    #[test]
    fn a_second_statement_is_refused() {
        assert!(is_read_only("SELECT 1; DROP TABLE aj_document").is_err());
    }

    /// The interesting case: a comment hiding the semicolon that separates a
    /// read from a write.
    #[test]
    fn a_write_hidden_behind_a_comment_is_still_found() {
        assert!(is_read_only("SELECT 1 -- \n; DROP TABLE aj_document").is_err());
        assert!(is_read_only("SELECT 1 /* comment */ ; DELETE FROM srv_event").is_err());
        assert!(is_read_only("SELECT 1 # note\n; TRUNCATE srv_event").is_err());
    }

    #[test]
    fn a_semicolon_inside_a_string_is_not_a_second_statement() {
        is_read_only("SELECT * FROM srv_event WHERE kind = 'a;b'").unwrap();
    }

    #[test]
    fn file_writing_selects_are_refused() {
        assert!(is_read_only("SELECT * FROM aj_document INTO OUTFILE '/tmp/x'").is_err());
        assert!(is_read_only("SELECT * FROM aj_document into dumpfile '/tmp/x'").is_err());
    }

    #[test]
    fn locking_reads_are_refused_because_they_hold_rows() {
        assert!(is_read_only("SELECT * FROM aj_document FOR UPDATE").is_err());
    }

    #[test]
    fn an_empty_query_is_refused_by_name() {
        assert!(is_read_only("   ").is_err());
        assert!(is_read_only("-- only a comment").is_err());
    }

    #[test]
    fn comment_stripping_keeps_string_contents_intact() {
        assert_eq!(
            strip_comments("SELECT '-- not a comment'"),
            "SELECT '-- not a comment'"
        );
        assert_eq!(
            strip_comments("SELECT 1 -- tail\nFROM t"),
            "SELECT 1 \nFROM t"
        );
        assert_eq!(strip_comments("SELECT /* mid */ 1"), "SELECT   1");
    }
}
