//! The claim a backup makes, tested rather than assumed.
//!
//! A backup that has never been restored is a hypothesis. This dumps a real
//! database, destroys what it dumped, restores it and checks the rows came
//! back, which is the only thing that distinguishes a backup from a file.
//!
//! Skipped, loudly, unless `CELLAR_TEST_DATABASE_URL` is set, matching
//! `cellar-store/tests/mysql.rs`. It also needs `mariadb` and `mariadb-dump` on
//! `PATH`, which the sqlx tests do not, because a dump is a separate process
//! and not a query.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cellar_core::config::{BackupConfig, MariaDbConfig};

/// The dump and restore clients are separate processes, so a database Cellar
/// can reach over sqlx is not on its own enough.
fn clients_are_installed() -> bool {
    ["mariadb", "mariadb-dump", "mysql", "mysqldump"]
        .iter()
        .filter(|name| {
            std::process::Command::new(name)
                .arg("--version")
                .output()
                .is_ok()
        })
        .count()
        >= 2
}

#[tokio::test]
async fn a_dump_restores_the_rows_it_was_taken_from() {
    let Ok(url) = std::env::var("CELLAR_TEST_DATABASE_URL") else {
        eprintln!("skipping: CELLAR_TEST_DATABASE_URL is not set");
        return;
    };
    if !clients_are_installed() {
        eprintln!("skipping: mariadb and mariadb-dump are not both on PATH");
        return;
    }

    let pool = cellar_store::connect(&url, 2).await.expect("connect");
    sqlx::query("DROP TABLE IF EXISTS cellar_restore_proof")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE cellar_restore_proof (id INT PRIMARY KEY, body VARCHAR(64) NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO cellar_restore_proof VALUES (1, 'must survive')")
        .execute(&pool)
        .await
        .unwrap();

    let directory = tempfile::tempdir().unwrap();
    let backup = BackupConfig {
        directory: Some(directory.path().to_path_buf()),
        retain: 5,
        ..BackupConfig::default()
    };
    let dump = cellar_mariadb::backup(&url, &MariaDbConfig::default(), &backup).expect("dump");

    // Both halves of the damage a restore has to undo: rows deleted, and a
    // table gone entirely.
    sqlx::query("DROP TABLE cellar_restore_proof")
        .execute(&pool)
        .await
        .unwrap();

    let restored =
        cellar_mariadb::restore(&dump, &url, &MariaDbConfig::default()).expect("restore");
    assert!(restored.bytes > 0);

    let body: String = sqlx::query_scalar("SELECT body FROM cellar_restore_proof WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("the dropped table and its row came back");
    assert_eq!(body, "must survive");

    sqlx::query("DROP TABLE cellar_restore_proof")
        .execute(&pool)
        .await
        .unwrap();
}
