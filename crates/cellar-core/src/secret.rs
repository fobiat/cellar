//! A string that refuses to print itself.
//!
//! Cellar handles a GSLT, a database password, a Discord webhook URL and an
//! operator password hash. Every one of them passes through a struct that is at
//! some point `Debug`-formatted into a log line or serialised into
//! `status --json`, and the house rule is that a webhook URL is never echoed.
//!
//! Making redaction the type's own behaviour is the only version of that rule
//! which cannot be forgotten at a call site.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A secret. Prints as `***` everywhere; the value comes out only via
/// [`Secret::expose`], which is deliberately awkward to type.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

const REDACTED: &str = "***";

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Read the real value. Every call site is a place to ask "does this end up
    /// in a log?".
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Take the value from an environment variable, if it is set and non-empty.
    pub fn from_env(name: &str) -> Option<Self> {
        match std::env::var(name) {
            Ok(value) if !value.trim().is_empty() => Some(Self::new(value)),
            _ => None,
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// Serialises as `***`, so a secret cannot reach `status --json` or a webhook
/// payload by being a field of something that got serialised.
impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(REDACTED)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self(String::deserialize(d)?))
    }
}

/// Remove a secret from arbitrary text before it is logged.
///
/// The child process writes its own command line and its own errors, and an
/// engine stack trace can contain a connection string. This is the belt to
/// [`Secret`]'s braces.
pub fn redact_all(text: &str, secrets: &[&Secret]) -> String {
    let mut out = text.to_owned();
    for secret in secrets {
        // A very short secret would match everywhere and destroy the line; a
        // real token is never this short, so a misconfigured empty value is
        // skipped rather than turning every log line into asterisks.
        if secret.0.len() >= 8 {
            out = out.replace(&secret.0, REDACTED);
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn never_prints_itself() {
        let secret = Secret::new("hunter2-hunter2-hunter2");
        assert_eq!(format!("{secret}"), "***");
        assert_eq!(format!("{secret:?}"), "***");
        assert_eq!(serde_json::to_string(&secret).unwrap(), "\"***\"");
    }

    #[test]
    fn survives_being_a_field_of_something_serialised() {
        #[derive(Serialize)]
        struct Config {
            name: String,
            token: Secret,
        }

        let json = serde_json::to_string(&Config {
            name: "dev".into(),
            token: Secret::new("A-REAL-LOOKING-GSLT-TOKEN"),
        })
        .unwrap();

        assert!(!json.contains("A-REAL-LOOKING-GSLT-TOKEN"));
        assert!(json.contains("***"));
    }

    #[test]
    fn exposes_only_when_asked() {
        assert_eq!(Secret::new("value").expose(), "value");
    }

    #[test]
    fn redacts_a_leaked_value_out_of_child_output() {
        let token = Secret::new("ABCDEF0123456789");
        let line = format!(
            "entrypoint: starting with +net_game_server_token {}",
            token.expose()
        );
        let clean = redact_all(&line, &[&token]);
        assert!(!clean.contains("ABCDEF0123456789"));
        assert!(clean.ends_with("***"));
    }

    #[test]
    fn a_short_or_empty_secret_does_not_redact_the_world() {
        let empty = Secret::new("");
        let short = Secret::new("ab");
        let line = "a perfectly ordinary log line";
        assert_eq!(redact_all(line, &[&empty, &short]), line);
    }
}
