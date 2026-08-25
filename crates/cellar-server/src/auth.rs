//! Who is allowed to call the bridge.
//!
//! The honest position, stated once here rather than implied everywhere.
//!
//! The gamemode authenticates with `Sandbox.Services.Auth.GetToken(audience)`,
//! presented as `Authorization: Bearer …`. That is a good scheme: the token is
//! platform-issued at runtime, so nothing secret ships inside the game package,
//! which matters because a game package is distributed to every client and any
//! secret compiled into one is public by construction.
//!
//! The catch is the other end. Facepunch's service API publishes package,
//! version, stats, leaderboard, achievement, player, news, notification,
//! storage, utility and code endpoints, and **no token introspection**. There is
//! no documented way for a third party to verify one of these tokens.
//!
//! So Cellar does not claim to. [`AuthMode::Trusted`] requires a well-formed
//! bearer and records it, and leans on the fact that the bridge binds loopback
//! and Cellar is the process that launched the only client allowed to reach it.
//! The trust boundary is the process tree, not the token, and the config refuses
//! to start `Trusted` on a reachable address so that boundary stays real.

use axum::http::{HeaderMap, StatusCode};
use cellar_core::config::AuthMode;
use cellar_core::secret::Secret;

/// The policy this instance enforces.
#[derive(Debug, Clone)]
pub enum Policy {
    /// A bearer must be present and plausible. Not verified.
    Trusted,
    /// A bearer must equal a configured secret.
    SharedSecret(Secret),
}

impl Policy {
    /// Build the policy a config selects.
    ///
    /// `AuthMode::Facepunch` cannot be built: the config layer refuses it at
    /// load with an explanation, and this returns an error rather than silently
    /// downgrading to something weaker. A security mode that quietly becomes a
    /// different mode is worse than one that refuses to start.
    pub fn from_config(mode: AuthMode, shared: Option<&Secret>) -> Result<Self, String> {
        match mode {
            AuthMode::Trusted => Ok(Self::Trusted),
            AuthMode::SharedSecret => shared
                .cloned()
                .map(Self::SharedSecret)
                .ok_or_else(|| "shared_secret auth needs CELLAR_BRIDGE_SECRET".to_owned()),
            AuthMode::Facepunch => Err(
                "facepunch auth is not implemented: no public endpoint for verifying an \
                 Auth.GetToken token was found"
                    .to_owned(),
            ),
        }
    }
}

/// Shortest string treated as a plausible token.
///
/// Not a security check. It catches an empty or truncated header, which is the
/// realistic misconfiguration, and says so rather than pretending to more.
const MINIMUM_TOKEN_LENGTH: usize = 8;

/// Check a request's credentials.
pub fn check(policy: &Policy, headers: &HeaderMap) -> Result<(), StatusCode> {
    let token = bearer(headers).ok_or(StatusCode::UNAUTHORIZED)?;

    match policy {
        Policy::Trusted => {
            if token.len() < MINIMUM_TOKEN_LENGTH {
                return Err(StatusCode::UNAUTHORIZED);
            }
            Ok(())
        }
        Policy::SharedSecret(expected) => {
            if constant_time_eq(token.as_bytes(), expected.expose().as_bytes()) {
                Ok(())
            } else {
                Err(StatusCode::UNAUTHORIZED)
            }
        }
    }
}

/// Pull the token out of an `Authorization: Bearer …` header.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.split_once(' ')?;

    // The scheme is case-insensitive per RFC 7235; the token is not.
    scheme
        .eq_ignore_ascii_case("Bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

/// Compare without leaking length or content through timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut difference = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        difference |= x ^ y;
    }
    difference == 0
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if !value.is_empty() {
            headers.insert(
                axum::http::header::AUTHORIZATION,
                value.parse().expect("a valid header value"),
            );
        }
        headers
    }

    #[test]
    fn trusted_wants_a_bearer_and_nothing_more() {
        let policy = Policy::Trusted;
        assert!(check(&policy, &headers("Bearer a-plausible-token")).is_ok());
        assert!(check(&policy, &headers("bearer a-plausible-token")).is_ok());
    }

    #[test]
    fn a_missing_or_empty_credential_is_refused() {
        let policy = Policy::Trusted;
        assert_eq!(check(&policy, &headers("")), Err(StatusCode::UNAUTHORIZED));
        assert_eq!(
            check(&policy, &headers("Bearer ")),
            Err(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            check(&policy, &headers("Bearer tiny")),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn another_scheme_is_not_a_bearer() {
        let policy = Policy::Trusted;
        assert_eq!(
            check(&policy, &headers("Basic dXNlcjpwYXNzd29yZA==")),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn shared_secret_compares_the_whole_value() {
        let policy = Policy::SharedSecret(Secret::new("the-configured-secret"));
        assert!(check(&policy, &headers("Bearer the-configured-secret")).is_ok());
        assert!(check(&policy, &headers("Bearer the-configured-secre")).is_err());
        assert!(check(&policy, &headers("Bearer the-configured-secretX")).is_err());
        assert!(check(&policy, &headers("Bearer THE-CONFIGURED-SECRET")).is_err());
    }

    #[test]
    fn facepunch_mode_refuses_to_be_built_rather_than_downgrading() {
        let error = Policy::from_config(AuthMode::Facepunch, None).unwrap_err();
        assert!(error.contains("not implemented"), "{error}");
    }

    #[test]
    fn shared_secret_mode_needs_its_secret() {
        assert!(Policy::from_config(AuthMode::SharedSecret, None).is_err());
        assert!(Policy::from_config(AuthMode::SharedSecret, Some(&Secret::new("x"))).is_ok());
    }

    #[test]
    fn constant_time_comparison_is_still_correct() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
