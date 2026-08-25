//! Updating Cellar itself.
//!
//! Separate from [`crate::updater`], which updates the *game*. This one replaces
//! the running binary, and the two have almost nothing in common except the
//! word.
//!
//! The whole difficulty is on Windows: a running `.exe` cannot be deleted or
//! overwritten, but it **can be renamed**. So the sequence is rename-self-aside,
//! write the new binary into the original path, and leave the old one for the
//! next run to sweep up. On Unix the file can simply be replaced, but the same
//! shape is used on both so there is one code path to reason about.
//!
//! Nothing here downloads over plain HTTP, and nothing installs a binary whose
//! SHA-256 does not match the checksum published beside it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where releases are published.
pub const DEFAULT_RELEASES_URL: &str = "https://api.github.com/repos/fobiat/cellar/releases/latest";

/// The suffix a superseded binary is renamed to.
///
/// Left on disk deliberately: on Windows the old file is still mapped by the
/// running process and cannot be removed until it exits.
pub const OLD_SUFFIX: &str = ".old";

/// What a release offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag: String,
    pub notes: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

impl Release {
    /// The asset for a platform, by the naming convention the release workflow uses.
    pub fn asset_for(&self, target: &str) -> Option<&Asset> {
        self.assets
            .iter()
            .find(|asset| asset.name.contains(target) && !asset.name.ends_with(".sha256"))
    }

    /// The checksum file beside an asset.
    pub fn checksum_for(&self, asset: &Asset) -> Option<&Asset> {
        let wanted = format!("{}.sha256", asset.name);
        self.assets
            .iter()
            .find(|candidate| candidate.name == wanted)
    }
}

/// The target triple this build was compiled for.
pub const fn current_target() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux"
    } else if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        "unknown"
    }
}

/// Whether `available` is a different version from `current`.
///
/// A string comparison after stripping a leading `v`, not a semver parse: the
/// only question is "is this the one I am running", and a build that cannot
/// parse its own tag should not refuse to update because of it.
pub fn is_newer(current: &str, available: &str) -> bool {
    normalise(current) != normalise(available) && !normalise(available).is_empty()
}

fn normalise(version: &str) -> &str {
    version.trim().trim_start_matches('v')
}

/// Parse GitHub's release JSON into what this module needs.
pub fn parse_release(json: &serde_json::Value) -> Option<Release> {
    let tag = json.get("tag_name")?.as_str()?.to_owned();
    let notes = json
        .get("body")
        .and_then(|body| body.as_str())
        .unwrap_or_default()
        .to_owned();

    let assets = json
        .get("assets")?
        .as_array()?
        .iter()
        .filter_map(|asset| {
            Some(Asset {
                name: asset.get("name")?.as_str()?.to_owned(),
                url: asset.get("browser_download_url")?.as_str()?.to_owned(),
                size: asset.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
            })
        })
        .collect();

    Some(Release { tag, notes, assets })
}

#[derive(Debug, thiserror::Error)]
pub enum SelfUpdateError {
    #[error("no release asset for {0}")]
    NoAsset(String),

    #[error("no published checksum for {0}; refusing to install an unverified binary")]
    NoChecksum(String),

    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

/// Verify a downloaded binary against its published checksum.
///
/// A separate, pure function so the check that matters most is a test rather
/// than something entangled with the network.
pub fn verify(bytes: &[u8], checksum_file: &str) -> Result<(), SelfUpdateError> {
    // `sha256sum` format: the hash, whitespace, then the filename.
    let expected = checksum_file
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();

    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(SelfUpdateError::Other(format!(
            "'{expected}' is not a sha256 digest"
        )));
    }

    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(SelfUpdateError::ChecksumMismatch { expected, actual });
    }

    Ok(())
}

/// Replace the binary at `target` with `bytes`.
///
/// Rename-aside rather than overwrite, because on Windows the running image
/// cannot be written to but can be moved. The old file is left behind; see
/// [`sweep`].
pub fn install(target: &Path, bytes: &[u8]) -> Result<PathBuf, SelfUpdateError> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let staged = parent.join(format!(
        ".{}.new",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("cellar")
    ));

    // Written beside the target, never in a temp directory: a cross-filesystem
    // rename is a copy, and a copy is not atomic.
    std::fs::write(&staged, bytes)?;
    copy_permissions(target, &staged)?;

    let retired = PathBuf::from(format!("{}{OLD_SUFFIX}", target.display()));
    let _ = std::fs::remove_file(&retired);

    if target.exists() {
        std::fs::rename(target, &retired)?;
    }

    match std::fs::rename(&staged, target) {
        Ok(()) => Ok(retired),
        Err(error) => {
            // Put the original back rather than leaving nothing installed.
            let _ = std::fs::rename(&retired, target);
            let _ = std::fs::remove_file(&staged);
            Err(error.into())
        }
    }
}

#[cfg(unix)]
fn copy_permissions(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(from)
        .map(|m| m.permissions().mode())
        // A fresh install has nothing to copy from; 0o755 is what an executable
        // wants, and getting this wrong means an update that cannot run.
        .unwrap_or(0o755);

    std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode | 0o111))
}

#[cfg(not(unix))]
fn copy_permissions(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Remove a superseded binary left by a previous update.
///
/// Called at startup. On Windows the previous `.old` is only deletable once the
/// process that was running it has exited, which is exactly now.
pub fn sweep(target: &Path) {
    let retired = PathBuf::from(format!("{}{OLD_SUFFIX}", target.display()));
    if retired.exists() {
        let _ = std::fs::remove_file(&retired);
    }
}

/// SHA-256, without a dependency for it.
///
/// Cellar needs exactly one hash, in one place, to check one download. Pulling
/// in a crate for it would be more supply chain than the problem deserves.
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = sha256(data);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut message = data.to_vec();
    let bit_length = (data.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (index, word) in chunk.chunks_exact(4).enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);

        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut out = [0u8; 32];
    for (index, word) in h.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Against the published NIST vectors, because a hash implemented in-tree
    /// that is subtly wrong would accept a tampered binary.
    #[test]
    fn sha256_matches_the_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_handles_a_block_boundary() {
        // 55, 56 and 64 bytes are where the padding logic goes wrong if it can.
        for length in [54usize, 55, 56, 57, 63, 64, 65, 119, 120] {
            let data = vec![b'x'; length];
            assert_eq!(sha256_hex(&data).len(), 64, "length {length}");
        }

        assert_eq!(
            sha256_hex(&[b'a'; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn a_matching_checksum_verifies() {
        let payload = b"a pretend binary";
        let checksum = format!("{}  cellar-x86_64-pc-windows.exe\n", sha256_hex(payload));
        verify(payload, &checksum).unwrap();
    }

    #[test]
    fn a_tampered_download_is_refused() {
        let checksum = format!("{}  cellar.exe\n", sha256_hex(b"the real binary"));
        let error = verify(b"a different binary", &checksum).unwrap_err();
        assert!(matches!(error, SelfUpdateError::ChecksumMismatch { .. }));
    }

    #[test]
    fn a_malformed_checksum_file_is_refused_rather_than_ignored() {
        assert!(verify(b"x", "not-a-digest  file").is_err());
        assert!(verify(b"x", "").is_err());
        // Right length, wrong alphabet.
        assert!(verify(b"x", &"z".repeat(64)).is_err());
    }

    #[test]
    fn version_comparison_ignores_a_leading_v() {
        assert!(!is_newer("0.1.0", "v0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(is_newer("0.1.0", "v0.2.0"));
        assert!(!is_newer("0.1.0", ""));
    }

    #[test]
    fn it_picks_the_asset_for_a_platform_and_its_checksum() {
        let release = Release {
            tag: "v0.2.0".into(),
            notes: String::new(),
            assets: vec![
                Asset {
                    name: "cellar-x86_64-unknown-linux.tar.gz".into(),
                    url: "u1".into(),
                    size: 1,
                },
                Asset {
                    name: "cellar-x86_64-unknown-linux.tar.gz.sha256".into(),
                    url: "u2".into(),
                    size: 1,
                },
                Asset {
                    name: "cellar-x86_64-pc-windows.zip".into(),
                    url: "u3".into(),
                    size: 1,
                },
                Asset {
                    name: "cellar-x86_64-pc-windows.zip.sha256".into(),
                    url: "u4".into(),
                    size: 1,
                },
            ],
        };

        let windows = release.asset_for("x86_64-pc-windows").unwrap();
        assert_eq!(windows.name, "cellar-x86_64-pc-windows.zip");
        assert_eq!(release.checksum_for(windows).unwrap().url, "u4");

        assert!(release.asset_for("powerpc-unknown-haiku").is_none());
    }

    #[test]
    fn it_reads_a_github_release_document() {
        let json = serde_json::json!({
            "tag_name": "v0.2.0",
            "body": "the notes",
            "assets": [
                { "name": "cellar-x86_64-pc-windows.zip",
                  "browser_download_url": "https://example/1", "size": 4096 }
            ]
        });

        let release = parse_release(&json).unwrap();
        assert_eq!(release.tag, "v0.2.0");
        assert_eq!(release.notes, "the notes");
        assert_eq!(release.assets[0].size, 4096);
    }

    /// The Windows dance, exercised on whatever platform the tests run on: the
    /// rename must leave the old binary recoverable and the new one in place.
    #[test]
    fn installing_moves_the_old_binary_aside_and_puts_the_new_one_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("cellar");
        std::fs::write(&target, b"the old binary").unwrap();

        let retired = install(&target, b"the new binary").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"the new binary");
        assert_eq!(std::fs::read(&retired).unwrap(), b"the old binary");

        sweep(&target);
        assert!(!retired.exists(), "the sweep removes it on the next run");
        assert!(target.exists(), "and never removes the live one");
    }

    #[test]
    fn installing_where_nothing_exists_yet_works() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("cellar");

        install(&target, b"a first install").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"a first install");
    }

    #[cfg(unix)]
    #[test]
    fn the_installed_binary_is_executable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("cellar");
        install(&target, b"#!/bin/sh\necho hi\n").unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "an update that cannot be run is not an update"
        );
    }

    #[test]
    fn sweeping_when_there_is_nothing_to_sweep_is_harmless() {
        let dir = tempfile::tempdir().unwrap();
        sweep(&dir.path().join("cellar"));
    }
}
