//! Who is allowed to use the web UI.
//!
//! A single operator password, argon2-hashed, exchanged for a random session
//! token held in a cookie. Not a user system: there is one operator, and
//! pretending otherwise would be building an account model nobody asked for.
//!
//! The password gate is not optional on a reachable address. The config layer
//! refuses to start an exposed web UI without a hash, because the console behind
//! it runs at full engine privilege.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng,
};
use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use rand::Rng;

use cellar_core::config::WebAuthMode;

/// How long a session lasts without being used.
const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);

/// The cookie the token lives in.
pub const COOKIE: &str = "cellar_session";

/// A verified operator, produced by the extractor.
pub struct Operator {
    pub name: String,
}

/// Live sessions.
#[derive(Default)]
pub struct Sessions {
    tokens: Mutex<HashMap<String, (String, Instant)>>,
}

impl Sessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a token for a verified login.
    pub fn create(&self, name: &str) -> String {
        let token: String = {
            let mut rng = rand::thread_rng();
            (0..48)
                .map(|_| {
                    const ALPHABET: &[u8] =
                        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
                    ALPHABET[rng.gen_range(0..ALPHABET.len())] as char
                })
                .collect()
        };

        if let Ok(mut tokens) = self.tokens.lock() {
            // Expired entries are swept here rather than on a timer: the map is
            // small and the only thing that grows it is a login.
            tokens.retain(|_, (_, seen)| seen.elapsed() < SESSION_TTL);
            tokens.insert(token.clone(), (name.to_owned(), Instant::now()));
        }

        token
    }

    /// Look a token up, refreshing its idle timer.
    pub fn resolve(&self, token: &str) -> Option<String> {
        let mut tokens = self.tokens.lock().ok()?;
        let (name, seen) = tokens.get_mut(token)?;

        if seen.elapsed() >= SESSION_TTL {
            let name = name.clone();
            tokens.remove(token);
            let _ = name;
            return None;
        }

        *seen = Instant::now();
        Some(name.clone())
    }

    pub fn destroy(&self, token: &str) {
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.remove(token);
        }
    }
}

/// Hash a password for the config file.
///
/// `cellar hash-password` prints this, so a plaintext password never has to be
/// typed into a file that might be committed.
pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| e.to_string())
}

/// Check a password against a stored hash.
pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };

    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Pull one cookie out of a request.
pub fn cookie_value(parts: &Parts, name: &str) -> Option<String> {
    let header = parts
        .headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?;

    header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_owned())
    })
}

impl<S> FromRequestParts<S> for Operator
where
    S: Send + Sync,
    std::sync::Arc<crate::state::AppState>: axum::extract::FromRef<S>,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        use axum::extract::FromRef;
        let state = std::sync::Arc::<crate::state::AppState>::from_ref(state);

        let requires_password = match state.web_auth {
            WebAuthMode::Password => true,
            WebAuthMode::None => false,
            WebAuthMode::Auto => state.web_password_hash.is_some(),
        };

        if !requires_password {
            return Ok(Operator {
                name: "local".to_owned(),
            });
        }

        let token =
            cookie_value(parts, COOKIE).ok_or((StatusCode::UNAUTHORIZED, "not signed in"))?;

        state
            .sessions
            .resolve(&token)
            .map(|name| Operator { name })
            .ok_or((StatusCode::UNAUTHORIZED, "session expired"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_password_verifies_against_its_own_hash_and_nothing_else() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash));
        assert!(!verify_password("Correct horse battery staple", &hash));
        assert!(!verify_password("", &hash));
    }

    #[test]
    fn two_hashes_of_one_password_differ_because_the_salt_does() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("same", &a));
        assert!(verify_password("same", &b));
    }

    #[test]
    fn a_malformed_hash_verifies_nothing_rather_than_everything() {
        assert!(!verify_password("anything", "not-a-hash"));
        assert!(!verify_password("anything", ""));
    }

    #[test]
    fn a_session_resolves_once_created_and_not_after_it_is_destroyed() {
        let sessions = Sessions::new();
        let token = sessions.create("kyle");

        assert_eq!(sessions.resolve(&token).as_deref(), Some("kyle"));

        sessions.destroy(&token);
        assert!(sessions.resolve(&token).is_none());
    }

    #[test]
    fn an_unknown_token_resolves_to_nobody() {
        let sessions = Sessions::new();
        sessions.create("kyle");
        assert!(sessions.resolve("some-other-token").is_none());
    }

    #[test]
    fn tokens_are_long_and_unique() {
        let sessions = Sessions::new();
        let a = sessions.create("kyle");
        let b = sessions.create("kyle");

        assert_ne!(a, b);
        assert!(a.len() >= 48);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn cookies_are_read_out_of_a_header_with_several() {
        let request = axum::http::Request::builder()
            .header("cookie", "theme=dark; cellar_session=abc123; other=x")
            .body(())
            .unwrap();
        let (parts, ()) = request.into_parts();

        assert_eq!(cookie_value(&parts, COOKIE).as_deref(), Some("abc123"));
        assert_eq!(cookie_value(&parts, "theme").as_deref(), Some("dark"));
        assert!(cookie_value(&parts, "absent").is_none());
    }
}
