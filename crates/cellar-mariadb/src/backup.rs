use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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
    let executable = mariadb
        .install_dir
        .as_ref()
        .map(|path| {
            path.join("bin").join(if cfg!(windows) {
                "mariadb-dump.exe"
            } else {
                "mariadb-dump"
            })
        })
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from("mariadb-dump"));

    let result = Command::new(executable)
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
    let mut files = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("cellar-"))
        .filter_map(|entry| {
            entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .map(|modified| (modified, entry.path()))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(modified, _)| *modified);
    for (_, path) in files.into_iter().rev().skip(retain) {
        fs::remove_file(path)?;
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
mod tests {
    use super::*;

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
