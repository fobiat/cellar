//! First-run and re-run setup: install the binaries, initialize the data
//! directory, and create the `cellar` database, user and password.
//!
//! Safe to call more than once. Installing and initializing are skipped when
//! already done (see `release::already_installed` and the data-dir check
//! below), but the database/user/grant step always runs and always resets
//! the password, so `cellar mariadb provision` is also how an operator who
//! lost `CELLAR_DATABASE_URL` recovers it: root has no password (a deliberate
//! property of `--auth-root-authentication-method=normal`, safe because this
//! server never binds anything but loopback), so re-running this can always
//! reach it and issue a fresh password.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cellar_core::config::MariaDbConfig;
use serde::{Deserialize, Serialize};

use crate::{credentials, fetch, install, release};

#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    #[error("mariadb.install_dir is not set")]
    MissingInstallDir,
    #[error("mariadb.data_dir is not set")]
    MissingDataDir,
    #[error("mariadb.sha256 is not set")]
    MissingChecksum,
    #[error(
        "a managed MariaDB is Windows-only: the pinned archive is the winx64 build and every \
         binary here is an .exe. Set mariadb.managed = false and point database.url_file or \
         CELLAR_DATABASE_URL at a MariaDB this host already runs."
    )]
    UnsupportedHost,
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Fetch(#[from] fetch::FetchError),
    #[error(transparent)]
    Install(#[from] install::InstallError),
    #[error("mariadbd did not start accepting connections on port {port} within {seconds}s")]
    StartupTimeout { port: u16, seconds: u64 },
    #[error("{client} exited with an error: {stderr}")]
    ClientFailed {
        client: &'static str,
        stderr: String,
    },
}

/// Non-secret state written after a successful provision, so `status` can
/// report what is there without touching the data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub version: String,
    pub port: u16,
    pub database: String,
    pub username: String,
}

fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(".cellar-provisioned.toml")
}

/// Read what a previous provision left behind, if anything.
///
/// Absent or unreadable is not an error here: an empty or missing data
/// directory is exactly the "not provisioned yet" case `provision` already
/// handles, and a marker that fails to parse is treated the same way rather
/// than blocking a re-provision that would fix it.
pub fn read_marker(data_dir: &Path) -> Option<Marker> {
    let text = std::fs::read_to_string(marker_path(data_dir)).ok()?;
    toml::from_str(&text).ok()
}

fn write_marker(data_dir: &Path, marker: &Marker) -> std::io::Result<()> {
    let text = toml::to_string_pretty(marker).map_err(std::io::Error::other)?;
    std::fs::write(marker_path(data_dir), text)
}

/// The bootstrap SQL, pure and testable without a server.
///
/// `database`/`username` are trusted here: `Config::validate` refuses
/// anything that does not match `[A-Za-z_][A-Za-z0-9_]*` before a config
/// carrying `mariadb.managed = true` is ever loaded, which is what makes
/// interpolating them directly safe. `password` comes only from
/// `credentials::generate_password`, alphanumeric by construction, so it
/// needs no escaping either.
fn bootstrap_sql(database: &str, username: &str, password: &str) -> Vec<String> {
    vec![
        format!("CREATE DATABASE IF NOT EXISTS `{database}`"),
        format!("CREATE USER IF NOT EXISTS '{username}'@'127.0.0.1'"),
        format!("ALTER USER '{username}'@'127.0.0.1' IDENTIFIED BY '{password}'"),
        format!("GRANT ALL PRIVILEGES ON `{database}`.* TO '{username}'@'127.0.0.1'"),
        // So `supervisor.rs` can stop mariadbd cleanly as this same user,
        // rather than keeping a root credential around after provisioning.
        format!("GRANT SHUTDOWN ON *.* TO '{username}'@'127.0.0.1'"),
        "FLUSH PRIVILEGES".to_owned(),
    ]
}

/// Install (if needed), initialize (if needed), and (re-)create the database,
/// user and password. Returns the `mysql://` URL for `CELLAR_DATABASE_URL`.
pub async fn provision(
    config: &MariaDbConfig,
    client: &reqwest::Client,
    existing_password: Option<&str>,
) -> Result<String, ProvisionError> {
    // Before the download, not after: `release::archive_url` serves the winx64
    // zip unconditionally, and every binary this module spawns is named `.exe`.
    // Without this the caller pays a 400MB download, unpacks Windows binaries
    // onto a Linux host, and only then fails on the first exec with a bare
    // "Permission denied (os error 13)" that names nothing.
    if !cfg!(windows) {
        return Err(ProvisionError::UnsupportedHost);
    }

    let install_dir = config
        .install_dir
        .as_ref()
        .ok_or(ProvisionError::MissingInstallDir)?;
    let data_dir = config
        .data_dir
        .as_ref()
        .ok_or(ProvisionError::MissingDataDir)?;

    if release::already_installed(install_dir) {
        tracing::info!(
            "mariadb {} already installed at {}",
            config.version,
            install_dir.display()
        );
    } else {
        let sha256 = config
            .sha256
            .as_deref()
            .ok_or(ProvisionError::MissingChecksum)?;
        let bytes = fetch::download(client, &config.version, sha256).await?;
        install::install(install_dir, &config.version, bytes).await?;
        tracing::info!(
            "installed mariadb {} into {}",
            config.version,
            install_dir.display()
        );
    }

    let bin_dir = install_dir.join("bin");

    // The system schema's own directory is the simplest reliable sign that
    // `mariadb-install-db` has already run here.
    if !data_dir.join("mysql").is_dir() {
        std::fs::create_dir_all(data_dir)?;
        run_install_db(&bin_dir, data_dir).await?;
        tracing::info!("initialized the data directory at {}", data_dir.display());
    }

    let mut server = spawn(&bin_dir, data_dir, config.port)?;
    if let Err(error) = wait_until_accepting(config.port, Duration::from_secs(30)).await {
        let _ = server.kill().await;
        return Err(error);
    }

    let password = existing_password
        .map(str::to_owned)
        .unwrap_or_else(credentials::generate_password);
    let statements = bootstrap_sql(&config.database, &config.username, &password);
    let bootstrap_result = run_root_sql(&bin_dir, config.port, &statements).await;

    // Stop the bootstrap server whether or not the SQL succeeded, so a failed
    // provision never leaves a stray mariadbd holding the port.
    let _ = run_admin_shutdown(&bin_dir, config.port, "root", None).await;
    let _ = server.wait().await;

    bootstrap_result?;

    write_marker(
        data_dir,
        &Marker {
            version: config.version.clone(),
            port: config.port,
            database: config.database.clone(),
            username: config.username.clone(),
        },
    )?;

    Ok(format!(
        "mysql://{}:{password}@127.0.0.1:{}/{}",
        config.username, config.port, config.database
    ))
}

/// Windows' `mariadb-install-db.exe` is not the Unix `mariadb-install-db`
/// shell script: it is a distinct binary (internally still `mysql_install_db
/// Ver 1.00`, from before the MariaDB/MySQL rename) with a much smaller
/// option set, no `--auth-root-authentication-method`, no `--skip-test-db`,
/// confirmed against the real distribution rather than assumed. `--datadir`
/// is the only thing it needs; the result leaves `root` with an empty
/// password over TCP unless `-p`/`-R`/`-N` say otherwise, which is exactly
/// the bootstrap access `provision` relies on.
async fn run_install_db(bin_dir: &Path, data_dir: &Path) -> Result<(), ProvisionError> {
    let output = tokio::process::Command::new(bin_dir.join("mariadb-install-db.exe"))
        .arg(format!("--datadir={}", data_dir.display()))
        .output()
        .await?;

    if !output.status.success() {
        return Err(ProvisionError::ClientFailed {
            client: "mariadb-install-db",
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(())
}

/// Spawn `mariadbd`, loopback-bound. Shared with `supervisor.rs`, which spawns
/// the same way for the long-running instance `cellar run` supervises.
pub(crate) fn spawn(
    bin_dir: &Path,
    data_dir: &Path,
    port: u16,
) -> Result<tokio::process::Child, std::io::Error> {
    tokio::process::Command::new(bin_dir.join("mariadbd.exe"))
        .arg(format!("--datadir={}", data_dir.display()))
        .arg(format!("--port={port}"))
        .arg("--bind-address=127.0.0.1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

async fn wait_until_accepting(port: u16, timeout: Duration) -> Result<(), ProvisionError> {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return Ok(());
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(ProvisionError::StartupTimeout {
                port,
                seconds: timeout.as_secs(),
            });
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn run_root_sql(
    bin_dir: &Path,
    port: u16,
    statements: &[String],
) -> Result<(), ProvisionError> {
    use tokio::io::AsyncWriteExt;

    let script = statements
        .iter()
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut child = tokio::process::Command::new(bin_dir.join("mariadb.exe"))
        .arg("--host=127.0.0.1")
        .arg(format!("--port={port}"))
        .arg("--protocol=tcp")
        .arg("--user=root")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(script.as_bytes()).await?;
        // Dropping `stdin` here closes the pipe, which is how the client
        // knows there is no more input coming.
    }

    let output = child.wait_with_output().await?;
    if !output.status.success() {
        return Err(ProvisionError::ClientFailed {
            client: "mariadb",
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(())
}

/// `mariadb-admin shutdown`, as either root (bootstrap) or the app user
/// (`supervisor.rs`'s graceful stop). Errors are deliberately swallowed by
/// both callers: a shutdown that fails leaves the process to be killed
/// outright, which is degraded but not unsafe.
pub(crate) async fn run_admin_shutdown(
    bin_dir: &Path,
    port: u16,
    user: &str,
    password: Option<&str>,
) -> std::io::Result<std::process::ExitStatus> {
    let mut command = tokio::process::Command::new(bin_dir.join("mariadb-admin.exe"));
    command
        .arg("--host=127.0.0.1")
        .arg(format!("--port={port}"))
        .arg(format!("--user={user}"));

    if let Some(password) = password {
        command.arg(format!("--password={password}"));
    }

    command.arg("shutdown").status().await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_sql_creates_the_database_user_and_grants() {
        let statements = bootstrap_sql("cellar", "cellar", "s3cret123");
        let joined = statements.join("; ");

        assert!(joined.contains("CREATE DATABASE IF NOT EXISTS `cellar`"));
        assert!(joined.contains("CREATE USER IF NOT EXISTS 'cellar'@'127.0.0.1'"));
        assert!(joined.contains("ALTER USER 'cellar'@'127.0.0.1' IDENTIFIED BY 's3cret123'"));
        assert!(joined.contains("GRANT ALL PRIVILEGES ON `cellar`.* TO 'cellar'@'127.0.0.1'"));
        assert!(joined.contains("GRANT SHUTDOWN ON *.* TO 'cellar'@'127.0.0.1'"));
        assert!(joined.contains("FLUSH PRIVILEGES"));
    }

    #[test]
    fn bootstrap_sql_is_idempotent_by_construction() {
        // `IF NOT EXISTS` / `ALTER` rather than a bare `CREATE USER`, so a
        // re-run against an already-provisioned instance does not fail on
        // the second statement.
        let statements = bootstrap_sql("cellar", "cellar", "x");
        assert!(statements[1].contains("IF NOT EXISTS"));
        assert!(statements[2].starts_with("ALTER USER"));
    }

    #[test]
    fn the_marker_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_marker(dir.path()).is_none());

        let marker = Marker {
            version: "11.4.5".to_owned(),
            port: 33306,
            database: "cellar".to_owned(),
            username: "cellar".to_owned(),
        };
        write_marker(dir.path(), &marker).unwrap();

        assert_eq!(read_marker(dir.path()), Some(marker));
    }

    #[test]
    fn a_malformed_marker_reads_as_absent_rather_than_erroring() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(marker_path(dir.path()), "not valid toml {{{").unwrap();
        assert!(read_marker(dir.path()).is_none());
    }
}
