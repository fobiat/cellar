//! Generating the password for the database user `provision` creates.
//!
//! Never persisted by this crate: `provision::provision` prints
//! `CELLAR_DATABASE_URL` for the operator to set, the same way
//! `cellar hash-password` prints `CELLAR_WEB_PASSWORD_HASH`. See `[mariadb]`
//! in `cellar-core::config` for why this stays out of any file Cellar writes.

use rand::Rng;
use rand::distributions::Alphanumeric;

/// A password safe to embed in a `mysql://` URL without escaping.
///
/// Alphanumeric only, so it never needs percent-encoding in a connection
/// string and never collides with the `:`/`@`/`/` delimiters that string is
/// parsed by. 32 characters of a 62-symbol alphabet is over 190 bits of
/// entropy, more than enough for a credential that never leaves this machine.
pub fn generate_password() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

/// Pull the password back out of a `mysql://user:password@host/db` URL.
///
/// `provision::provision` never persists the generated password anywhere;
/// `CELLAR_DATABASE_URL` is the one place it lives once printed. This is how
/// `cellar run` recovers it at startup, to authenticate `supervisor.rs`'s
/// graceful shutdown later. Only ever called on a URL this crate generated,
/// so the password is always the plain alphanumeric output of
/// `generate_password`, needing no percent-decoding.
pub fn password_from_database_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://")?.1;
    let before_at = after_scheme.split('@').next()?;
    let (_, password) = before_at.split_once(':')?;
    (!password.is_empty()).then(|| password.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_password_is_recovered_from_a_generated_url() {
        let password = generate_password();
        let url = format!("mysql://cellar:{password}@127.0.0.1:33306/cellar");
        assert_eq!(password_from_database_url(&url), Some(password));
    }

    #[test]
    fn a_url_without_a_password_answers_none() {
        assert_eq!(password_from_database_url("mysql://cellar@host/db"), None);
        assert_eq!(password_from_database_url("not a url"), None);
    }

    #[test]
    fn a_generated_password_is_the_right_length_and_charset() {
        let password = generate_password();
        assert_eq!(password.len(), 32);
        assert!(password.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn two_generated_passwords_are_not_the_same() {
        // Not a security proof, just a smoke test that the RNG is actually
        // being consulted rather than something constant slipping through.
        assert_ne!(generate_password(), generate_password());
    }
}
