//! Unpacking a downloaded MariaDB archive into its versioned install directory.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use crate::release;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("reading the archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("the archive has no {expected}/ directory; its layout may have changed")]
    UnexpectedLayout { expected: String },
}

/// Unpack `bytes` (the archive for `version`) into `install_dir`, stripping
/// the archive's own top-level directory so `install_dir/bin/mariadbd.exe`
/// is the result.
///
/// Idempotent: does nothing if `install_dir` already holds this version's
/// binaries, so a re-run after an interrupted provision does not re-download
/// or re-extract. Runs the extraction on a blocking thread: it is synchronous
/// file I/O, one-time and multi-second, not worth blocking the async runtime.
pub async fn install(
    install_dir: &Path,
    version: &str,
    bytes: Vec<u8>,
) -> Result<(), InstallError> {
    if release::already_installed(install_dir) {
        tracing::info!("{} already has mariadb {version}", install_dir.display());
        return Ok(());
    }

    let install_dir = install_dir.to_owned();
    let root = release::archive_root(version);

    tokio::task::spawn_blocking(move || extract(&bytes, &root, &install_dir))
        .await
        .map_err(|join_error| InstallError::Io(std::io::Error::other(join_error)))??;

    Ok(())
}

/// Extract into a staging directory beside `install_dir`, then rename into
/// place last. A half-extracted archive must never look installed:
/// `release::already_installed` checks for `install_dir` existing at all.
fn extract(bytes: &[u8], root: &str, install_dir: &Path) -> Result<(), InstallError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;

    let parent = install_dir.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".{}.staging",
        install_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("mariadb")
    ));
    // Clean up a previous interrupted attempt before starting a fresh one.
    let _ = std::fs::remove_dir_all(&staging);

    let mut written = 0u32;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;

        // `mangled_name`, not `name`: it strips `..` and rooted components,
        // so a crafted archive cannot write outside `staging` even though
        // the checksum already verified this one came from the pinned
        // config value.
        let mangled = entry.mangled_name();
        let mut components = mangled.components();
        let under_root = matches!(
            components.next(),
            Some(std::path::Component::Normal(top)) if top.to_str() == Some(root)
        );
        if !under_root {
            continue;
        }

        let relative: PathBuf = components.collect();
        if relative.as_os_str().is_empty() {
            // The root directory entry itself, not a file underneath it.
            continue;
        }

        let target = staging.join(&relative);

        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }

        if let Some(target_parent) = target.parent() {
            std::fs::create_dir_all(target_parent)?;
        }

        let mut out = std::fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
        written += 1;
    }

    if written == 0 {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(InstallError::UnexpectedLayout {
            expected: root.to_owned(),
        });
    }

    let _ = std::fs::remove_dir_all(install_dir);
    std::fs::rename(&staging, install_dir)?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn archive_bytes(root: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer.add_directory(format!("{root}/"), options).unwrap();
            for (name, contents) in files {
                writer
                    .start_file(format!("{root}/{name}"), options)
                    .unwrap();
                std::io::Write::write_all(&mut writer, contents).unwrap();
            }
            writer.finish().unwrap();
        }
        buffer.into_inner()
    }

    #[tokio::test]
    async fn extracting_strips_the_top_level_directory() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path().join("11.4.5");
        let bytes = archive_bytes(
            "mariadb-11.4.5-winx64",
            &[
                ("bin/mariadbd.exe", b"pretend server binary"),
                ("bin/mariadb-install-db.exe", b"pretend installer"),
            ],
        );

        install(&install_dir, "11.4.5", bytes).await.unwrap();

        assert!(release::already_installed(&install_dir));
        assert_eq!(
            std::fs::read(install_dir.join("bin").join("mariadbd.exe")).unwrap(),
            b"pretend server binary"
        );
    }

    #[tokio::test]
    async fn installing_twice_is_a_no_op_the_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path().join("11.4.5");
        let bytes = archive_bytes("mariadb-11.4.5-winx64", &[("bin/mariadbd.exe", b"first")]);

        install(&install_dir, "11.4.5", bytes).await.unwrap();

        // A second call with a *different* payload must not re-extract, or an
        // interrupted-then-retried provision could silently downgrade a
        // partially-initialized install.
        let different = archive_bytes("mariadb-11.4.5-winx64", &[("bin/mariadbd.exe", b"second")]);
        install(&install_dir, "11.4.5", different).await.unwrap();

        assert_eq!(
            std::fs::read(install_dir.join("bin").join("mariadbd.exe")).unwrap(),
            b"first"
        );
    }

    #[tokio::test]
    async fn an_archive_without_the_expected_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path().join("11.4.5");
        let bytes = archive_bytes("some-other-layout", &[("bin/mariadbd.exe", b"x")]);

        let error = install(&install_dir, "11.4.5", bytes).await.unwrap_err();
        assert!(matches!(error, InstallError::UnexpectedLayout { .. }));
        assert!(!install_dir.exists());
    }
}
