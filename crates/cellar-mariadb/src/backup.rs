//! Logical dumps, and putting one back.
//!
//! A backup that has never been restored is a hypothesis, so the two halves
//! live together and share the URL parsing and the executable lookup. Neither
//! ever puts the password in an argument: `MYSQL_PWD` is read by every client
//! in the MariaDB suite and does not appear in `ps`.

use std::fs;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use cellar_core::config::{BackupConfig, MariaDbConfig};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("CELLAR_DATABASE_URL is not a mysql URL")]
    InvalidUrl,
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
    #[error("{0} is {1} bytes, which is too small to be a dump of anything")]
    Truncated(PathBuf, u64),
    #[error("{0} has no end-of-dump marker, so mariadb-dump did not finish writing it")]
    Unfinished(PathBuf),
    #[error("could not copy the dump to {0}: {1}")]
    Copy(PathBuf, String),
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

    dumps.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| right.path.cmp(&left.path))
    });
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
    /* The same read-back `create` does before it counts a dump.
     *
     * It was the header check alone here, so an unfinished dump was refused
     * when it was written and accepted when it was restored. Measured against
     * a dump truncated to 3000 bytes: the restore ran, failed on the sliced
     * INSERT, and reported that the database may be half-applied, which it
     * was. A dump whose last line is not the end marker is a dump nobody
     * should discover the shape of halfway through applying it. */
    verify(dump)?;

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

    // Verified before it is counted as a backup, and before `prune` is allowed
    // to delete an older one to make room for it. A dump that has never been
    // read back is a hypothesis, and the failure this catches is the one that
    // matters: a disk that filled up halfway through writing it, which leaves
    // a plausible file with a plausible size and no end marker.
    if backup.verify {
        verify(&output)?;
    }

    if let Some(elsewhere) = &backup.copy_to {
        copy_off_box(&output, elsewhere)?;
    }

    prune(&directory, backup.retain)?;
    Ok(output)
}

/// Read a dump back far enough to know it is one and that it finished.
///
/// Three questions, cheapest first: is there enough of it, does it start like a
/// dump, and does it end like one. `mariadb-dump` writes
/// `-- Dump completed on ...` as its last line and writes nothing at all if it
/// fails early, so the end marker is the one honest signal that the process
/// that wrote this file ran to completion.
pub fn verify(dump: &Path) -> Result<u64, BackupError> {
    if !dump.exists() {
        return Err(BackupError::NoSuchDump(dump.to_path_buf()));
    }

    let bytes = fs::metadata(dump)?.len();
    // A dump of an empty database is still about a kilobyte of header.
    if bytes < 512 {
        return Err(BackupError::Truncated(dump.to_path_buf(), bytes));
    }
    if !looks_like_a_dump(dump)? {
        return Err(BackupError::NotADump(dump.to_path_buf()));
    }

    let tail_from = bytes.saturating_sub(512);
    let mut file = fs::File::open(dump)?;
    file.seek(std::io::SeekFrom::Start(tail_from))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    if !String::from_utf8_lossy(&tail).contains("Dump completed") {
        return Err(BackupError::Unfinished(dump.to_path_buf()));
    }

    Ok(bytes)
}

/// Put a second copy somewhere that is not this disk.
///
/// A copy, not a move: the local one is what `restore` and the retention
/// window are about. The destination is whatever the operator mounted there, a
/// network share or another volume, because Cellar has no business holding
/// credentials for an object store it cannot verify.
fn copy_off_box(dump: &Path, directory: &Path) -> Result<PathBuf, BackupError> {
    let Some(name) = dump.file_name() else {
        return Err(BackupError::NoSuchDump(dump.to_path_buf()));
    };
    fs::create_dir_all(directory)
        .map_err(|why| BackupError::Copy(directory.to_path_buf(), why.to_string()))?;

    let target = directory.join(name);
    fs::copy(dump, &target).map_err(|why| BackupError::Copy(target.clone(), why.to_string()))?;
    Ok(target)
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
    // The last '@', not the first: a password may contain one, and `sqlx`
    // splits the same way, so a URL the pool accepts is a URL this dumps.
    let (credentials, host_database) = rest.rsplit_once('@').ok_or(BackupError::InvalidUrl)?;
    // No password is a URL sqlx connects with, so refusing it here meant
    // Cellar would read a database happily and refuse to back it up, and say
    // so only when somebody finally needed the dump.
    let (user, password) = credentials.split_once(':').unwrap_or((credentials, ""));
    let (host_port, database) = host_database
        .split_once('/')
        .ok_or(BackupError::InvalidUrl)?;
    let (host, port) = host_port.split_once(':').unwrap_or((host_port, "3306"));
    if user.is_empty() || host.is_empty() || database.is_empty() {
        return Err(BackupError::InvalidUrl);
    }
    Ok((
        percent_decode(user),
        percent_decode(password),
        host.to_owned(),
        port.to_owned(),
        database.to_owned(),
    ))
}

/// Percent-decoding, for the credentials only.
///
/// A password containing `@`, `:` or `/` has to be percent-encoded to be a
/// URL at all, and every driver decodes it before authenticating. Passing the
/// encoded text to `mariadb-dump` authenticates with a different password than
/// the pool used, which reads as "the backup credentials are wrong" and is
/// really "the backup never decoded them".
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let pair = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = pair.and_then(|pair| u8::from_str_radix(pair, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
        for name in ["cellar-1.sql", "cellar-2.sql", "cellar-3.sql"] {
            let path = dir.path().join(name);
            fs::write(&path, HEADER).unwrap();
        }

        let listed = list(dir.path()).unwrap();
        assert_eq!(listed.len(), 3);
        assert!(listed[0].path.ends_with("cellar-3.sql"), "{listed:?}");

        prune(dir.path(), 2).unwrap();
        let after = list(dir.path()).unwrap();
        assert_eq!(after.len(), 2);
        assert!(after.iter().all(|d| !d.path.ends_with("cellar-1.sql")));
    }

    /// The failure verification exists for: a disk that filled up.
    ///
    /// The file has the right name, a plausible size and a correct header. It
    /// is missing only the last line, which is exactly what a truncated write
    /// looks like and exactly what nothing else notices.
    #[test]
    fn a_dump_that_stops_partway_is_refused() {
        let dir = tempfile::tempdir().unwrap();

        let good = dir.path().join("cellar-1.sql");
        fs::write(
            &good,
            format!(
                "-- MariaDB dump 10.19\n{}\n-- Dump completed on 2026-09-01\n",
                "-- padding\n".repeat(80)
            ),
        )
        .unwrap();
        assert!(verify(&good).is_ok());

        let cut = dir.path().join("cellar-2.sql");
        fs::write(
            &cut,
            format!(
                "-- MariaDB dump 10.19\n{}",
                "INSERT INTO x VALUES (1);\n".repeat(40)
            ),
        )
        .unwrap();
        assert!(matches!(verify(&cut), Err(BackupError::Unfinished(_))));

        let tiny = dir.path().join("cellar-3.sql");
        fs::write(&tiny, "-- MariaDB dump\n").unwrap();
        assert!(matches!(verify(&tiny), Err(BackupError::Truncated(_, _))));

        // Not a dump at all. Restoring this would run whatever it holds.
        let other = dir.path().join("cellar-4.sql");
        fs::write(&other, "x".repeat(4096)).unwrap();
        assert!(matches!(verify(&other), Err(BackupError::NotADump(_))));

        // And the same file is refused by `restore`, before any client runs.
        //
        // It was not: `restore` checked the header and nothing else, so an
        // unfinished dump was refused when it was written and accepted when
        // it was put back. Driven against a real MariaDB, the restore ran,
        // died on a sliced INSERT and reported the database as possibly
        // half-applied, which it was.
        let refusal = restore(
            &cut,
            "mysql://root@127.0.0.1:1/x",
            &MariaDbConfig::default(),
        );
        assert!(
            matches!(refusal, Err(BackupError::Unfinished(_))),
            "restore accepted an unfinished dump: {refusal:?}"
        );
    }

    #[test]
    fn the_off_box_copy_keeps_the_name_and_leaves_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let away = tempfile::tempdir().unwrap();

        let dump = dir.path().join("cellar-7.sql");
        fs::write(&dump, "-- MariaDB dump\n").unwrap();

        let copied = copy_off_box(&dump, away.path()).unwrap();
        assert!(copied.ends_with("cellar-7.sql"));
        // A copy, not a move: the local one is what restore and the retention
        // window are about.
        assert!(dump.exists());
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

    /// Whatever the pool connects with, this has to dump.
    ///
    /// It refused a passwordless URL, which `sqlx` accepts, so a local
    /// database Cellar was reading happily could not be backed up and said so
    /// only when a backup ran. And it never decoded the credentials, so a
    /// password containing `@` (which has to be written `%40` for the URL to
    /// parse at all) reached `mariadb-dump` as the encoded text and failed to
    /// authenticate with a password that was correct.
    #[test]
    fn a_url_the_pool_accepts_is_a_url_this_can_dump() {
        let cases = [
            ("mysql://root@127.0.0.1:3399/scratch", "root", "", "scratch"),
            (
                "mysql://cellar:p%40ss%2Fword@db.internal/cellar",
                "cellar",
                "p@ss/word",
                "cellar",
            ),
            (
                "mariadb://a%20b:c%3Ad@127.0.0.1:3306/games",
                "a b",
                "c:d",
                "games",
            ),
        ];

        for (url, user, password, database) in cases {
            let parsed = match parse_url(url) {
                Ok(parsed) => parsed,
                Err(error) => panic!("{url} should parse: {error}"),
            };
            assert_eq!(parsed.0, user, "user of {url}");
            assert_eq!(parsed.1, password, "password of {url}");
            assert_eq!(parsed.4, database, "database of {url}");
        }

        // Still refused: these are not URLs this can act on at all.
        for url in [
            "postgres://cellar:secret@127.0.0.1/cellar",
            "mysql://cellar:secret@127.0.0.1",
            "mysql://:secret@127.0.0.1/cellar",
        ] {
            assert!(parse_url(url).is_err(), "{url} should be refused");
        }
    }
}
