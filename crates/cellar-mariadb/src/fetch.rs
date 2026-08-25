//! Downloading a MariaDB release archive.

use crate::release;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("downloading {0}")]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Release(#[from] release::ReleaseError),
}

/// Download the archive for `version` and verify it against
/// `expected_sha256`, the value pinned in `mariadb.sha256`.
///
/// Never trusts a checksum fetched alongside the archive itself: see
/// `release::verify` for why.
pub async fn download(
    client: &reqwest::Client,
    version: &str,
    expected_sha256: &str,
) -> Result<Vec<u8>, FetchError> {
    let url = release::archive_url(version);
    tracing::info!("downloading {url}");

    let bytes = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    release::verify(&bytes, expected_sha256)?;
    tracing::info!("checksum matches the pinned mariadb.sha256");

    Ok(bytes.to_vec())
}
