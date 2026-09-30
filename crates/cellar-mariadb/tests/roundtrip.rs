//! The claim a backup makes, tested rather than assumed.
//! A backup that has never been restored is a hypothesis. This dumps a real database, destroys what it dumped, restores it and checks the rows came back, which is the only thing that distinguishes a backup from a file.
//! Skipped, loudly, unless `CELLAR_TEST_DATABASE_URL` is set, matching `cellar-store/tests/mysql.rs`. It also needs `mariadb` and `mariadb-dump` on `PATH`, which the sqlx tests do not, because a dump is a separate process and not a query.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cellar_core::config::{BackupConfig, MariaDbConfig};

/// The dump and restore clients are separate processes, so a database Cellar can reach over sqlx is not on its own enough.
fn clients_are_installed() -> bool {
    ["mariadb", "mariadb-dump"]
        .iter()
        .filter(|name| {
            std::process::Command::new(name)
                .arg("--version")
                .output()
                .is_ok()
        })
        .count()
        == 2
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
        "CREATE TABLE cellar_restore_proof (id INT PRIMARY KEY, body VARCHAR(128) CHARACTER SET utf8mb4 NOT NULL, payload VARBINARY(64) NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let text = "must survive: café 漢字 🧪 ' \\ ";
    let binary = vec![0, 0x80, 0xff, b'\\', b'\'', 0x1a];
    sqlx::query("INSERT INTO cellar_restore_proof VALUES (1, ?, ?)")
        .bind(text)
        .bind(&binary)
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

    let content = std::fs::read_to_string(&dump).unwrap();
    assert!(content.contains("SET NAMES utf8mb4"));
    std::fs::write(&dump, format!(
        "SET @cellar_initial_charset = CONCAT(@@character_set_client, ',', @@character_set_connection, ',', @@character_set_results);\n{content}\nCREATE TABLE cellar_restore_charset_proof (body VARCHAR(64));\nINSERT INTO cellar_restore_charset_proof VALUES (@cellar_initial_charset);\n"
    )).unwrap();

    // Both halves of the damage a restore has to undo: rows deleted, and a table gone entirely.
    sqlx::query("DROP TABLE cellar_restore_proof")
        .execute(&pool)
        .await
        .unwrap();

    let restored =
        cellar_mariadb::restore(&dump, &url, &MariaDbConfig::default()).expect("restore");
    assert!(restored.bytes > 0);

    let charset: String = sqlx::query_scalar("SELECT body FROM cellar_restore_charset_proof")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(charset, "utf8mb4,utf8mb4,utf8mb4");
    sqlx::query("DROP TABLE cellar_restore_charset_proof")
        .execute(&pool)
        .await
        .unwrap();

    let body: String = sqlx::query_scalar("SELECT body FROM cellar_restore_proof WHERE id = 1")
        .fetch_one(&pool)
        .await
        .expect("the dropped table and its row came back");
    assert_eq!(body, text);
    let payload: Vec<u8> =
        sqlx::query_scalar("SELECT payload FROM cellar_restore_proof WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(payload, binary);

    let mut wrong_password = reqwest::Url::parse(&url).unwrap();
    wrong_password
        .set_password(Some("incorrect-fixture-password"))
        .unwrap();
    assert!(matches!(
        cellar_mariadb::backup(wrong_password.as_str(), &MariaDbConfig::default(), &backup),
        Err(cellar_mariadb::BackupError::Dump(_))
    ));
    assert!(matches!(
        cellar_mariadb::restore(&dump, wrong_password.as_str(), &MariaDbConfig::default()),
        Err(cellar_mariadb::BackupError::Restore(_))
    ));
    let mut bad_ca = reqwest::Url::parse(&url).unwrap();
    bad_ca.set_query(Some(
        "ssl-mode=verify_identity&ssl-ca=%2Fnonexistent-cellar-fixture-ca.pem",
    ));
    assert!(matches!(
        cellar_mariadb::backup(bad_ca.as_str(), &MariaDbConfig::default(), &backup),
        Err(cellar_mariadb::BackupError::Dump(_))
    ));
    assert!(matches!(
        cellar_mariadb::restore(&dump, bad_ca.as_str(), &MariaDbConfig::default()),
        Err(cellar_mariadb::BackupError::Restore(_))
    ));
    assert_eq!(
        cellar_mariadb::backup::list(directory.path())
            .unwrap()
            .len(),
        1
    );

    sqlx::query("DROP TABLE cellar_restore_proof")
        .execute(&pool)
        .await
        .unwrap();
}
