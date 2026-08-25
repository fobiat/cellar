//! Locating and verifying a MariaDB release.
//!
//! MariaDB does not publish to GitHub, so there is no release API to parse the
//! way `cellar-update`'s `selfupdate` module does. Instead this builds the URL
//! into the MariaDB Foundation's own permanent per-version archive
//! (`archive.mariadb.org`), whose layout is documented and stable across
//! versions, and verifies the download against a checksum pinned in
//! `mariadb.sha256` rather than one fetched over the same connection as the
//! archive. See `[mariadb]` in `cellar-core::config` for why that pin exists.

use std::path::Path;

/// Where the official win64 archive for a version lives.
///
/// `archive.mariadb.org` keeps every released version indefinitely, unlike
/// the front page at mariadb.org/download, which only lists current ones.
pub fn archive_url(version: &str) -> String {
    format!(
        "https://archive.mariadb.org/mariadb-{version}/winx64-packages/mariadb-{version}-winx64.zip"
    )
}

/// The archive's top-level directory once unpacked, e.g. `mariadb-11.4.5-winx64`.
///
/// The official zip does not extract flat; everything sits under one directory
/// matching this name, which `install.rs` needs to know to find `bin/`
/// afterwards.
pub fn archive_root(version: &str) -> String {
    format!("mariadb-{version}-winx64")
}

#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("'{0}' is not a sha256 digest")]
    NotASha256(String),
}

/// Verify a downloaded archive against the checksum pinned in config.
///
/// Unlike `cellar-update::selfupdate::verify`, `expected` is never fetched
/// over the network here: it is the value a human copied from MariaDB's
/// published hashes when they set `mariadb.version`. A mirror serving a bad
/// archive alongside a bad checksum does not help an attacker against this
/// check, because the expected value never came from that mirror.
pub fn verify(bytes: &[u8], expected: &str) -> Result<(), ReleaseError> {
    let expected = expected.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ReleaseError::NotASha256(expected));
    }

    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(ReleaseError::ChecksumMismatch { expected, actual });
    }

    Ok(())
}

/// Whether this version's binaries are already unpacked at `install_dir`.
///
/// Idempotency check for `provision`: skip the download entirely when a
/// previous provision (or a re-run after an interruption) already put the
/// binaries in place.
pub fn already_installed(install_dir: &Path) -> bool {
    install_dir.join("bin").join("mariadbd.exe").is_file()
}

/// SHA-256, without a dependency for it.
///
/// Duplicated from `cellar-update::selfupdate::sha256_hex` rather than taking
/// a dependency on that crate just for a hash function; `cellar-update` is
/// about updating the game, and that is the wrong coupling for a few dozen
/// lines of hashing. See that module for the same reasoning stated in full.
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
    }

    #[test]
    fn a_matching_checksum_verifies() {
        let payload = b"a pretend archive";
        verify(payload, &sha256_hex(payload)).unwrap();
    }

    #[test]
    fn a_tampered_download_is_refused() {
        let expected = sha256_hex(b"the real archive");
        let error = verify(b"a different archive", &expected).unwrap_err();
        assert!(matches!(error, ReleaseError::ChecksumMismatch { .. }));
    }

    #[test]
    fn a_malformed_pin_is_refused_rather_than_ignored() {
        assert!(verify(b"x", "not-a-digest").is_err());
        assert!(verify(b"x", "").is_err());
        assert!(verify(b"x", &"z".repeat(64)).is_err());
    }

    #[test]
    fn the_archive_url_matches_the_documented_layout() {
        assert_eq!(
            archive_url("11.4.5"),
            "https://archive.mariadb.org/mariadb-11.4.5/winx64-packages/mariadb-11.4.5-winx64.zip"
        );
        assert_eq!(archive_root("11.4.5"), "mariadb-11.4.5-winx64");
    }

    #[test]
    fn an_empty_install_dir_is_not_already_installed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!already_installed(dir.path()));

        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin").join("mariadbd.exe"), b"x").unwrap();
        assert!(already_installed(dir.path()));
    }
}
