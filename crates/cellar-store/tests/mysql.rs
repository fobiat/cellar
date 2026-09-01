//! Integration tests against a real MySQL or MariaDB.
//!
//! Skipped, loudly, unless `CELLAR_TEST_DATABASE_URL` is set, so a checkout with
//! no database still runs `cargo test` green. CI sets it against a service
//! container; locally:
//!
//! ```sh
//! docker run -d --rm --name cellar-test-db \
//!   -e MARIADB_ROOT_PASSWORD=cellartest -e MARIADB_DATABASE=cellar \
//!   -p 33061:3306 mariadb:11
//! export CELLAR_TEST_DATABASE_URL='mysql://root:cellartest@127.0.0.1:33061/cellar'
//! ```
//!
//! The point of these is the SQL itself. The bridge's HTTP contract is tested
//! against an in-memory backend in `cellar-server`; nothing there would catch a
//! migration that does not apply or an upsert that does not upsert.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cellar_store::{document, ops};
use sqlx::MySqlPool;

/// Serialises the whole file.
///
/// Every test here drops and recreates the schema, and `cargo test` runs them on
/// several threads, so without this they tear the tables out from under each
/// other and fail with "table already exists" rather than with anything true
/// about the code.
static SCHEMA: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

/// Connect and start from a clean schema, or return `None` to skip.
///
/// The returned guard is held for the test's lifetime; dropping it early would
/// let the next test start dropping tables mid-run.
async fn database() -> Option<(MySqlPool, tokio::sync::MutexGuard<'static, ()>)> {
    let url = std::env::var("CELLAR_TEST_DATABASE_URL").ok()?;
    let guard = SCHEMA
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let pool = cellar_store::connect(&url, 4).await.expect("connect");

    // Each run starts from nothing: a leftover row from a previous run turning a
    // test green is worse than no test.
    for table in [
        "aj_document_revision",
        "aj_document",
        "srv_command",
        "srv_event",
        "srv_player_session",
        "srv_player",
        "srv_session",
        "_sqlx_migrations",
    ] {
        let _ = sqlx::query(&format!("DROP TABLE IF EXISTS {table}"))
            .execute(&pool)
            .await;
    }

    cellar_store::migrate(&pool).await.expect("migrate");
    Some((pool, guard))
}

macro_rules! pool_or_skip {
    () => {
        match database().await {
            // `_guard` stays alive to the end of the test, which is what keeps
            // the schema still underneath it.
            Some((pool, _guard)) => (pool, _guard),
            None => {
                eprintln!("skipping: CELLAR_TEST_DATABASE_URL is not set");
                return;
            }
        }
    };
}

#[tokio::test]
async fn the_migration_applies_to_an_empty_database() {
    let (pool, _guard) = pool_or_skip!();
    cellar_store::ping(&pool).await.unwrap();

    // Running it twice must be a no-op, because `migrate_on_start` does exactly
    // that on every restart.
    cellar_store::migrate(&pool).await.unwrap();
}

#[tokio::test]
async fn a_document_round_trips_and_keeps_its_history() {
    let (pool, _guard) = pool_or_skip!();
    let key = "characters/76561198000000000.json";

    assert!(document::get(&pool, "s", key).await.unwrap().is_none());
    assert!(!document::exists(&pool, "s", key).await.unwrap());

    let first = serde_json::json!({ "Version": 3, "Balance": 8000 });
    let outcome = document::put(&pool, "s", key, &first, Some("gamemode"), None)
        .await
        .unwrap();
    assert!(outcome.created);
    assert_eq!(outcome.revision, 1);

    let second = serde_json::json!({ "Version": 3, "Balance": 7500 });
    let outcome = document::put(&pool, "s", key, &second, Some("gamemode"), None)
        .await
        .unwrap();
    assert!(!outcome.created);
    assert_eq!(outcome.revision, 2, "a write bumps the revision");

    let stored = document::get(&pool, "s", key).await.unwrap().unwrap();
    assert_eq!(stored.body, second);
    assert_eq!(stored.revision, 2);

    // Both writes are recoverable, which is the whole point of the history.
    let history = document::revisions(&pool, "s", key, 10).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].revision, 2);
    assert_eq!(history[1].body, first);
}

#[tokio::test]
async fn scopes_do_not_see_each_other() {
    let (pool, _guard) = pool_or_skip!();
    let key = "features.json";

    document::put(
        &pool,
        "server-a",
        key,
        &serde_json::json!({"a": true}),
        None,
        None,
    )
    .await
    .unwrap();

    assert!(
        document::get(&pool, "server-b", key)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        document::get(&pool, "server-a", key)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_stale_expected_revision_is_reported_but_still_written() {
    let (pool, _guard) = pool_or_skip!();
    let key = "laws.json";

    document::put(&pool, "s", key, &serde_json::json!({"n": 1}), None, None)
        .await
        .unwrap();
    document::put(&pool, "s", key, &serde_json::json!({"n": 2}), None, None)
        .await
        .unwrap();

    // Revision is 2; the caller thinks it is 1.
    let outcome = document::put(&pool, "s", key, &serde_json::json!({"n": 3}), None, Some(1))
        .await
        .unwrap();

    assert!(outcome.would_conflict, "the conflict is noticed");
    assert_eq!(outcome.revision, 3, "and the write still lands");

    let stored = document::get(&pool, "s", key).await.unwrap().unwrap();
    assert_eq!(stored.body, serde_json::json!({"n": 3}));
}

#[tokio::test]
async fn a_document_key_at_the_column_limit_fits() {
    let (pool, _guard) = pool_or_skip!();

    // 128 characters, `DocumentKeys.MaximumLength`. If the column were narrower
    // this would truncate silently under a non-strict sql_mode.
    let key = format!("{}.json", "a".repeat(123));
    assert_eq!(key.len(), 128);
    assert!(cellar_core::doc_key::is_legal(&key));

    document::put(
        &pool,
        "s",
        &key,
        &serde_json::json!({"ok": true}),
        None,
        None,
    )
    .await
    .unwrap();

    let stored = document::get(&pool, "s", &key).await.unwrap().unwrap();
    assert_eq!(stored.key, key);
}

#[tokio::test]
async fn listing_finds_documents_by_prefix() {
    let (pool, _guard) = pool_or_skip!();

    for id in 1..=3u64 {
        let key = format!("characters/{id}.json");
        document::put(&pool, "s", &key, &serde_json::json!({"id": id}), None, None)
            .await
            .unwrap();
    }
    document::put(
        &pool,
        "s",
        "features.json",
        &serde_json::json!({}),
        None,
        None,
    )
    .await
    .unwrap();

    let all = document::list(&pool, "s", None, 100).await.unwrap();
    assert_eq!(all.len(), 4);

    let characters = document::list(&pool, "s", Some("characters/"), 100)
        .await
        .unwrap();
    assert_eq!(characters.len(), 3);
    assert!(characters.iter().all(|d| d.key.starts_with("characters/")));
    assert!(characters.iter().all(|d| d.bytes > 0));
}

#[tokio::test]
async fn a_player_session_accumulates_playtime() {
    let (pool, _guard) = pool_or_skip!();

    let session = ops::begin_session(&pool, "s", Some("test"), "wine sbox-server.exe")
        .await
        .unwrap();
    ops::mark_ready(&pool, session).await.unwrap();

    ops::player_joined(&pool, Some(session), 76561198000000000, "Kyle")
        .await
        .unwrap();
    ops::player_left(&pool, Some(session), 76561198000000000, "disconnected")
        .await
        .unwrap();

    let players = ops::players(&pool, 10).await.unwrap();
    assert_eq!(players.len(), 1);
    assert_eq!(players[0].steam_id, 76561198000000000);
    assert_eq!(players[0].sessions, 1);

    // A second visit counts as a second session and keeps the newer name.
    ops::player_joined(&pool, Some(session), 76561198000000000, "Kyle (renamed)")
        .await
        .unwrap();
    let players = ops::players(&pool, 10).await.unwrap();
    assert_eq!(players[0].sessions, 2);
    assert_eq!(players[0].last_name, "Kyle (renamed)");
}

/// A session that ends while players are listed must not leave rows open, or
/// every later "who was on" query includes people who left months ago.
#[tokio::test]
async fn ending_a_session_closes_any_player_still_connected() {
    let (pool, _guard) = pool_or_skip!();

    let session = ops::begin_session(&pool, "s", None, "cmd").await.unwrap();
    ops::player_joined(&pool, Some(session), 1, "A")
        .await
        .unwrap();
    ops::player_joined(&pool, Some(session), 2, "B")
        .await
        .unwrap();

    ops::end_session(&pool, session, Some(0), true)
        .await
        .unwrap();

    let open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM srv_player_session WHERE session_id = ? AND left_at IS NULL",
    )
    .bind(session)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(open, 0);
}

#[tokio::test]
async fn events_and_commands_are_recorded_and_prunable() {
    let (pool, _guard) = pool_or_skip!();
    let session = ops::begin_session(&pool, "s", None, "cmd").await.unwrap();

    ops::record_event(
        &pool,
        Some(session),
        "player_joined",
        Some("Identity"),
        Some(76561198000000000),
        Some(&serde_json::json!({ "name": "Kyle" })),
    )
    .await
    .unwrap();

    ops::record_command(
        &pool,
        Some(session),
        "kyle",
        "applejack_features",
        &["ui.menu.admin off".to_owned()],
        true,
    )
    .await
    .unwrap();

    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM srv_event")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(events, 1);

    let commands: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM srv_command")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(commands, 1);

    // Nothing is old enough to prune, and a zero retention keeps everything.
    assert_eq!(ops::prune_events(&pool, 90).await.unwrap(), 0);
    assert_eq!(ops::prune_events(&pool, 0).await.unwrap(), 0);
}

#[tokio::test]
async fn the_admin_browser_sees_the_schema_and_refuses_to_write_to_it() {
    let (pool, _guard) = pool_or_skip!();
    document::put(
        &pool,
        "s",
        "features.json",
        &serde_json::json!({"v": 1}),
        None,
        None,
    )
    .await
    .unwrap();

    let tables = cellar_store::admin::tables(&pool).await.unwrap();
    let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
    for expected in ["aj_document", "srv_session", "srv_player", "srv_event"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }

    let columns = cellar_store::admin::columns(&pool, "aj_document")
        .await
        .unwrap();
    let column_names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    assert!(column_names.contains(&"doc_key"));
    assert!(column_names.contains(&"revision"));

    let rows = cellar_store::admin::browse(&pool, "aj_document", 10, 0)
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert!(rows.columns.contains(&"doc_key".to_owned()));

    // A table that is not in the schema never reaches a query.
    assert!(
        cellar_store::admin::browse(&pool, "mysql.user", 10, 0)
            .await
            .is_err()
    );

    let result = cellar_store::admin::query(&pool, "SELECT COUNT(*) AS n FROM aj_document", 10)
        .await
        .unwrap();
    assert_eq!(result.rows[0][0].as_deref(), Some("1"));

    // Every id and revision in this schema is BIGINT UNSIGNED, which does not
    // decode as i64. Rendering it as its type name instead of its value is the
    // bug this asserts against.
    let result = cellar_store::admin::query(&pool, "SELECT revision FROM aj_document", 10)
        .await
        .unwrap();
    assert_eq!(
        result.rows[0][0].as_deref(),
        Some("1"),
        "an unsigned column renders as a number"
    );

    assert!(
        cellar_store::admin::query(&pool, "DELETE FROM aj_document", 10)
            .await
            .is_err()
    );
}

/// The activity timeline, which is the whole of Phase 3's audit screen.
///
/// Worth a real database rather than a unit test: the merge is two queries with
/// `LEFT JOIN`s and a scope filter, and every way it can be wrong is a way SQL
/// is wrong rather than a way Rust is.
#[tokio::test]
async fn activity_merges_the_audit_and_the_observations_newest_first() {
    let Some((pool, _guard)) = database().await else {
        return;
    };

    let dev = ops::begin_session(&pool, "aj-dev", Some("host"), "sbox-server.exe")
        .await
        .unwrap();
    let published = ops::begin_session(&pool, "aj-pub", Some("host"), "sbox-server.exe")
        .await
        .unwrap();

    ops::record_event(
        &pool,
        Some(dev),
        "server_ready",
        Some("Bootstrap"),
        None,
        None,
    )
    .await
    .unwrap();
    ops::record_command(
        &pool,
        Some(dev),
        "kyle",
        "status",
        &["PLAYERS".to_owned()],
        true,
    )
    .await
    .unwrap();
    ops::record_command(&pool, Some(published), "api", "quit", &[], false)
        .await
        .unwrap();
    // A command run while nothing was supervised. The join must keep it: that
    // is exactly when an operator is most likely to be looking.
    ops::record_command(&pool, None, "kyle", "cellar doctor", &[], true)
        .await
        .unwrap();

    let all = ops::activity(
        &pool,
        &ops::ActivityQuery {
            limit: 100,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(all.len(), 4, "one event and three commands");
    assert!(
        all.windows(2).all(|pair| pair[0].at >= pair[1].at),
        "newest first"
    );
    assert!(
        all.iter().any(|entry| entry.detail == "cellar doctor"),
        "a session-less command must survive the join"
    );

    let scoped = ops::activity(
        &pool,
        &ops::ActivityQuery {
            scope: Some("aj-dev".to_owned()),
            limit: 100,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(scoped.len(), 2, "one event and one command in that scope");
    assert!(
        scoped
            .iter()
            .all(|entry| entry.scope.as_deref() == Some("aj-dev"))
    );

    let operator_only = ops::activity(
        &pool,
        &ops::ActivityQuery {
            source: Some("operator".to_owned()),
            limit: 100,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(operator_only.len(), 3);
    assert!(operator_only.iter().all(|entry| entry.source == "operator"));

    // The outcome is the point of an audit: a refused command has to be
    // distinguishable from one that worked.
    let failed = operator_only
        .iter()
        .find(|entry| entry.detail == "quit")
        .unwrap();
    assert_eq!(failed.ok, Some(false));
    assert_eq!(failed.actor.as_deref(), Some("api"));

    // Text search covers the reply, not only the command: "which command
    // printed that?" is the question an operator actually has.
    let by_reply = ops::activity(
        &pool,
        &ops::ActivityQuery {
            text: Some("players".to_owned()),
            limit: 100,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(by_reply.len(), 1);
    assert_eq!(by_reply[0].detail, "status");
}

/// An event row has to say what happened, not only that something of a kind
/// did.
///
/// Every notable event was stored as a bare kind and a timestamp until
/// 2026-09-01, because the recorder passed `None` for the logger, the account
/// and the payload. Nothing read the table back, so nothing caught it.
#[tokio::test]
async fn an_event_row_carries_who_and_what_not_only_its_kind() {
    use cellar_core::event::{Event, LeaveReason};

    let Some((pool, _guard)) = database().await else {
        return;
    };

    let session = ops::begin_session(&pool, "aj-dev", None, "sbox-server.exe")
        .await
        .unwrap();

    for event in [
        Event::ProcessStarted {
            pid: 4242,
            command: "wine sbox-server.exe".to_owned(),
        },
        Event::PlayerJoined {
            steam_id: 76561198000000123,
            name: "Kyle".to_owned(),
        },
        Event::PlayerLeft {
            steam_id: 76561198000000123,
            name: "Kyle".to_owned(),
            reason: LeaveReason::Kicked {
                reason: "afk".to_owned(),
            },
        },
        Event::ProcessExited {
            code: Some(137),
            graceful: false,
        },
    ] {
        let record = event.record();
        let detail = record.detail.map(serde_json::Value::String);
        ops::record_event(
            &pool,
            Some(session),
            event.kind(),
            record.logger,
            record.steam_id,
            detail.as_ref(),
        )
        .await
        .unwrap();
    }

    let entries = ops::activity(
        &pool,
        &ops::ActivityQuery {
            scope: Some("aj-dev".to_owned()),
            limit: 100,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let find = |kind: &str| {
        entries
            .iter()
            .find(|entry| entry.kind == kind)
            .unwrap_or_else(|| panic!("no {kind} row"))
    };

    // The detail is stored in a JSON column as a JSON string, so this also
    // asserts the quotes do not survive the round trip.
    assert_eq!(
        find("process_started").detail,
        "pid 4242: wine sbox-server.exe"
    );
    assert_eq!(
        find("process_exited").detail,
        "exited with code 137 without being asked to"
    );
    assert_eq!(find("player_joined").detail, "Kyle joined");
    assert_eq!(find("player_left").detail, "Kyle was kicked: afk");

    // The account, so "what did this SteamID do" is answerable at all.
    assert_eq!(find("player_joined").steam_id, Some(76561198000000123));
    assert_eq!(find("player_joined").actor.as_deref(), Some("players"));
    assert_eq!(find("process_started").actor.as_deref(), Some("supervisor"));
}
