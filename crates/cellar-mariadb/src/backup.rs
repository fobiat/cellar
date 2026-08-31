//! Logical dumps, and putting one back.
//!
//! A backup that has never been restored is a hypothesis, so the two halves
//! live together and share the URL parsing and the executable lookup. Neither
//! ever puts the password in an argument: `MYSQL_PWD` is read by every client
//! in the MariaDB suite and does not appear in `ps`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use cellar_core::config::{BackupConfig, MariaDbConfig};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("CELLAR_DATABASE_URL is not a mysql URL")]
    InvalidUrl,
    #[error("database URL has no password")]
    MissingPassword,
    #[error("could not create backup directory: {0}")]
    Directory(#[from] std::io::Error),
    #[error("mariadb-dump failed: {0}")]
    Dump(String),
    #[error("{0} does not exist")]
    NoSuchDump(PathBuf),
    #[error(
        "{0} does not look like a mariadb-dump file: no dump header in the first bytes. \
         Restoring it would run whatever it does contain against the database."
    )]
    NotADump(PathBuf),
    #[error(
        "restore failed and the database may be half-applied, because a dump is a stream of \
         statements and the client stops at the first one that fails: {0}"
    )]
    Restore(String),
}

/// One dump on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dump {
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: SystemTime,
}

/// Every dump in `directory`, newest first.
pub fn list(directory: &Path) -> Result<Vec<Dump>, std::io::Error> {
    let mut dumps: Vec<Dump> = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("cellar-"))
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            Some(Dump {
                path: entry.path(),
                bytes: metadata.len(),
                modified: metadata.modified().ok()?,
            })
        })
        .collect();

    dumps.sort_by_key(|dump| std::cmp::Reverse(dump.modified));
    Ok(dumps)
}

/// Apply a dump back over the database it came from.
///
/// Destructive by construction, and not partially reversible: `mariadb-dump`
/// writes `DROP TABLE IF EXISTS` before each `CREATE TABLE`, so every table the
/// dump carries is replaced. Tables it does not carry are left alone, which
/// means restoring an older dump over a newer schema can leave a table nothing
/// created and nothing dropped. Callers stop the supervised server first: the
/// gamemode writes through the bridge continuously, and a write landing
/// mid-restore lands in a table that is about to be dropped.
pub fn restore(
    dump: &Path,
    database_url: &str,
    mariadb: &MariaDbConfig,
) -> Result<Restored, BackupError> {
    let (user, password, host, port, database) = parse_url(database_url)?;

    if !dump.exists() {
        return Err(BackupError::NoSuchDump(dump.to_path_buf()));
    }
    if !looks_like_a_dump(dump)? {
        return Err(BackupError::NotADump(dump.to_path_buf()));
    }

    let bytes = fs::metadata(dump)?.len();
    let handle = fs::File::open(dump)?;

    let result = Command::new(client(mariadb, "mariadb"))
        .args([
            "--host",
            host.as_str(),
            "--port",
            port.as_str(),
            "--user",
            user.as_str(),
            database.as_str(),
        ])
        .env("MYSQL_PWD", password)
        .stdin(Stdio::from(handle))
        .stdout(Stdio::null())
        .output()?;

    if !result.status.success() {
        return Err(BackupError::Restore(
            String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        ));
    }

    Ok(Restored {
        database,
        bytes,
        from: dump.to_path_buf(),
    })
}

/// What a restore did, for the operator and the audit row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    pub database: String,
    pub bytes: u64,
    pub from: PathBuf,
}

/// Whether the file starts like something `mariadb-dump` wrote.
///
/// Cheap, and it is the difference between refusing a wrong path and piping an
/// arbitrary file into a database as SQL.
fn looks_like_a_dump(path: &Path) -> Result<bool, std::io::Error> {
    let mut head = [0u8; 2048];
    let read = fs::File::open(path)?.read(&mut head)?;
    let head = String::from_utf8_lossy(&head[..read]);
    Ok(head.contains("MySQL dump") || head.contains("MariaDB dump"))
}

/// A client binary from the pinned install, or the one on `PATH`.
fn client(mariadb: &MariaDbConfig, name: &str) -> PathBuf {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };

    mariadb
        .install_dir
        .as_ref()
        .map(|path| path.join("bin").join(&file))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Create a logical dump without putting the database password in arguments or logs.
pub fn create(
    database_url: &str,
    mariadb: &MariaDbConfig,
    backup: &BackupConfig,
) -> Result<PathBuf, BackupError> {
    let (user, password, host, port, database) = parse_url(database_url)?;
    let directory = backup
        .directory
        .clone()
        .or_else(|| mariadb.data_dir.as_ref().map(|path| path.join("backups")))
        .ok_or(BackupError::Directory(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "backup.directory is required when MariaDB data_dir is unset",
        )))?;
    fs::create_dir_all(&directory)?;

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| BackupError::InvalidUrl)?
        .as_secs();
    let output = directory.join(format!("cellar-{stamp}.sql"));

    let result = Command::new(client(mariadb, "mariadb-dump"))
        .args([
            "--single-transaction",
            "--routines",
            "--events",
            "--triggers",
            "--host",
            host.as_str(),
            "--port",
            port.as_str(),
            "--user",
            user.as_str(),
            "--result-file",
            output.to_string_lossy().as_ref(),
            database.as_str(),
        ])
        .env("MYSQL_PWD", password)
        .output()
        .map_err(BackupError::Directory)?;

    if !result.status.success() {
        let _ = fs::remove_file(&output);
        return Err(BackupError::Dump(
            String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        ));
    }

    prune(&directory, backup.retain)?;
    Ok(output)
}

fn prune(directory: &Path, retain: usize) -> Result<(), std::io::Error> {
    if retain == 0 {
        return Ok(());
    }
    for dump in list(directory)?.into_iter().skip(retain) {
        fs::remove_file(dump.path)?;
    }
    Ok(())
}

fn parse_url(url: &str) -> Result<(String, String, String, String, String), BackupError> {
    let rest = url
        .strip_prefix("mysql://")
        .or_else(|| url.strip_prefix("mariadb://"))
        .ok_or(BackupError::InvalidUrl)?;
    let (credentials, host_database) = rest.split_once('@').ok_or(BackupError::InvalidUrl)?;
    let (user, password) = credentials
        .split_once(':')
        .ok_or(BackupError::MissingPassword)?;
    let (host_port, database) = host_database
        .split_once('/')
        .ok_or(BackupError::InvalidUrl)?;
    let (host, port) = host_port.split_once(':').unwrap_or((host_port, "3306"));
    if user.is_empty() || password.is_empty() || host.is_empty() || database.is_empty() {
        return Err(BackupError::InvalidUrl);
    }
    Ok((
        user.to_owned(),
        password.to_owned(),
        host.to_owned(),
        port.to_owned(),
        database.to_owned(),
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const HEADER: &str = "-- MariaDB dump 10.19  Distrib 11.4.5-MariaDB, for Linux (x86_64)\n--\n\
                          -- Host: 127.0.0.1    Database: cellar\n";

    #[test]
    fn a_file_that_is_not_a_dump_is_refused_before_it_reaches_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cellar-1.sql");
        fs::write(&path, "DROP DATABASE cellar;\n").unwrap();

        let error = restore(
            &path,
            "mysql://u:p@127.0.0.1:3306/cellar",
            &MariaDbConfig::default(),
        )
        .unwrap_err();

        assert!(matches!(error, BackupError::NotADump(_)), "{error}");
    }

    #[test]
    fn a_missing_dump_is_named_rather_than_reported_as_a_client_failure() {
        let dir = tempfile::tempdir().unwrap();
        let error = restore(
            &dir.path().join("nothing.sql"),
            "mysql://u:p@127.0.0.1:3306/cellar",
            &MariaDbConfig::default(),
        )
        .unwrap_err();

        assert!(matches!(error, BackupError::NoSuchDump(_)), "{error}");
    }

    #[test]
    fn dumps_are_listed_newest_first_and_that_is_what_prune_keeps() {
        let dir = tempfile::tempdir().unwrap();
        for (index, name) in ["cellar-1.sql", "cellar-2.sql", "cellar-3.sql"]
            .iter()
            .enumerate()
        {
            let path = dir.path().join(name);
            fs::write(&path, HEADER).unwrap();
            // Distinct mtimes, since three writes inside one filesystem tick
            // would otherwise make the ordering arbitrary.
            let when = SystemTime::now() + std::time::Duration::from_secs(index as u64 + 1);
            fs::File::open(&path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(when))
                .unwrap();
        }

        let listed = list(dir.path()).unwrap();
        assert_eq!(listed.len(), 3);
        assert!(listed[0].path.ends_with("cellar-3.sql"), "{listed:?}");

        prune(dir.path(), 2).unwrap();
        let after = list(dir.path()).unwrap();
        assert_eq!(after.len(), 2);
        assert!(after.iter().all(|d| !d.path.ends_with("cellar-1.sql")));
    }

    #[test]
    fn parses_the_provisioned_url_shape() {
        let parsed = match parse_url("mysql://cellar:secret@127.0.0.1:33306/cellar") {
            Ok(parsed) => parsed,
            Err(error) => panic!("test URL should parse: {error}"),
        };
        assert_eq!(parsed.0, "cellar");
        assert_eq!(parsed.2, "127.0.0.1");
        assert_eq!(parsed.3, "33306");
        assert_eq!(parsed.4, "cellar");
    }
}
