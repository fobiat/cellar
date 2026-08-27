//! Writing the gamemode's `hosting.json`.
//!
//! `HostingConfigStore.Resolve()` reads this from the engine's data directory
//! and picks the storage provider from it. It refuses a malformed document
//! loudly rather than falling back to local, deliberately, so that a typo cannot
//! quietly send a hosted server's writes to disk.
//!
//! That is a good reason not to hand-write it. Cellar generates it from its own
//! config immediately before launching the child, so the bridge URL the gamemode
//! dials and the address the bridge binds are the same value.

use std::path::{Path, PathBuf};

use cellar_core::config::{BridgeConfig, ServerConfig};
use serde::{Deserialize, Serialize};

/// The document `HostingConfig.cs` deserialises.
///
/// Field names are the C# property names as `System.Text.Json` sees them by
/// default, which is why they are camelCase here and PascalCase there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostingDocument {
    #[serde(rename = "version")]
    pub version: i32,
    #[serde(rename = "provider")]
    pub provider: String,
    #[serde(rename = "bridgeUrl")]
    pub bridge_url: String,
    #[serde(rename = "authAudience")]
    pub auth_audience: String,
    #[serde(rename = "apiKey", skip_serializing_if = "String::is_empty")]
    pub api_key: String,
}

/// `HostingRules.CurrentVersion`. A document from a later version is refused by
/// the gamemode rather than migrated backwards, so this must not run ahead.
pub const CURRENT_VERSION: i32 = 1;

pub const LOCAL_PROVIDER: &str = "local";
pub const HOSTED_PROVIDER: &str = "hosted";

/// The file name, under the engine's data directory beside `features.json`.
pub const DOCUMENT_PATH: &str = "hosting.json";

impl HostingDocument {
    /// Select the bridge.
    pub fn hosted(bridge_url: impl Into<String>, auth_audience: impl Into<String>) -> Self {
        Self {
            version: CURRENT_VERSION,
            provider: HOSTED_PROVIDER.to_owned(),
            bridge_url: bridge_url.into(),
            auth_audience: auth_audience.into(),
            api_key: String::new(),
        }
    }

    /// Select local JSON files, the gamemode's default.
    pub fn local() -> Self {
        Self {
            version: CURRENT_VERSION,
            provider: LOCAL_PROVIDER.to_owned(),
            bridge_url: String::new(),
            auth_audience: String::new(),
            api_key: String::new(),
        }
    }

    /// Refuse a document the gamemode would refuse, before writing it.
    ///
    /// `HostingRules.Resolve` falls back to local and marks the choice
    /// `Refused` for each of these. Falling back is safe but silent, and a
    /// server that quietly stopped using its database is the failure this
    /// check exists to make loud.
    pub fn check(&self) -> Result<(), String> {
        if self.provider == LOCAL_PROVIDER {
            return Ok(());
        }

        if self.provider != HOSTED_PROVIDER {
            return Err(format!(
                "'{}' is not a provider this gamemode has; it reads '{LOCAL_PROVIDER}' and '{HOSTED_PROVIDER}'",
                self.provider
            ));
        }

        let url = self.bridge_url.trim();
        if url.is_empty() {
            return Err("a hosted provider needs a bridgeUrl".to_owned());
        }

        // `IsPlausibleBridgeUrl`: absolute, http or https, with a host.
        let scheme_ok = url.starts_with("http://") || url.starts_with("https://");
        let host = url
            .split_once("://")
            .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""))
            .unwrap_or("");

        if !scheme_ok || host.is_empty() {
            return Err(format!(
                "'{url}' is not a usable bridgeUrl: it must be absolute, http or https, with a host"
            ));
        }

        if self.auth_audience.trim().is_empty() {
            return Err("a hosted provider needs an authAudience".to_owned());
        }

        Ok(())
    }
}

/// Compose the document a bridge config implies.
pub fn document_for(bridge: &BridgeConfig) -> HostingDocument {
    if bridge.enabled {
        let mut document =
            HostingDocument::hosted(bridge.public_url.trim(), bridge.auth_audience.trim());
        document.api_key = match bridge.auth {
            cellar_core::config::AuthMode::Trusted => "cellar-local-trusted".to_owned(),
            cellar_core::config::AuthMode::SharedSecret => bridge
                .shared_secret
                .as_ref()
                .map(|secret| secret.expose().to_owned())
                .unwrap_or_default(),
            cellar_core::config::AuthMode::Facepunch => String::new(),
        };
        document
    } else {
        HostingDocument::local()
    }
}

/// Where `hosting.json` goes.
///
/// The engine's data directory is per package, and Cellar cannot derive it
/// reliably across Wine and Windows, so `server.data_dir` is the answer when it
/// is set. Without it, Cellar does not guess: it says so, and the operator
/// points at the directory that holds `features.json`.
pub fn document_path(server: &ServerConfig) -> Option<PathBuf> {
    server.data_dir.as_ref().map(|dir| dir.join(DOCUMENT_PATH))
}

/// Write the document, creating the directory if needed.
pub fn write(path: &Path, document: &HostingDocument) -> Result<(), std::io::Error> {
    document
        .check()
        .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidInput, why))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(document)?;
    std::fs::write(path, format!("{json}\n"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_hosted_document_has_the_field_names_the_csharp_reads() {
        let json = serde_json::to_string(&HostingDocument::hosted(
            "http://127.0.0.1:8080",
            "applejack-bridge",
        ))
        .unwrap();

        assert!(json.contains("\"version\":1"));
        assert!(json.contains("\"provider\":\"hosted\""));
        assert!(json.contains("\"bridgeUrl\":\"http://127.0.0.1:8080\""));
        assert!(json.contains("\"authAudience\":\"applejack-bridge\""));
    }

    #[test]
    fn a_disabled_bridge_writes_the_local_provider() {
        let document = document_for(&BridgeConfig::default());
        assert_eq!(document.provider, LOCAL_PROVIDER);
        document.check().unwrap();
    }

    #[test]
    fn every_refusal_the_gamemode_would_make_is_made_here_first() {
        let mut document = HostingDocument::hosted("", "aud");
        assert!(document.check().unwrap_err().contains("bridgeUrl"));

        document = HostingDocument::hosted("ftp://host/", "aud");
        assert!(document.check().unwrap_err().contains("http"));

        document = HostingDocument::hosted("http://", "aud");
        assert!(document.check().unwrap_err().contains("host"));

        document = HostingDocument::hosted("http://127.0.0.1:8080", "  ");
        assert!(document.check().unwrap_err().contains("authAudience"));

        document = HostingDocument {
            version: CURRENT_VERSION,
            provider: "postgres".to_owned(),
            bridge_url: "http://127.0.0.1:8080".to_owned(),
            auth_audience: "aud".to_owned(),
            api_key: String::new(),
        };
        assert!(document.check().unwrap_err().contains("postgres"));
    }

    #[test]
    fn the_version_never_runs_ahead_of_what_the_gamemode_reads() {
        assert_eq!(CURRENT_VERSION, 1, "HostingRules.CurrentVersion");
    }

    #[test]
    fn writing_creates_the_directory_and_refuses_a_bad_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join(DOCUMENT_PATH);

        write(
            &path,
            &HostingDocument::hosted("http://127.0.0.1:8080", "aud"),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"provider\": \"hosted\""));

        let bad = HostingDocument::hosted("not-a-url", "aud");
        assert!(write(&path, &bad).is_err());
    }

    #[test]
    fn it_round_trips_through_the_document_shape() {
        let original = HostingDocument::hosted("https://bridge.example.com", "applejack-bridge");
        let json = serde_json::to_string(&original).unwrap();
        let back: HostingDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn a_local_trusted_bridge_writes_a_non_platform_token_for_the_game_host() {
        let bridge = BridgeConfig {
            enabled: true,
            ..BridgeConfig::default()
        };

        let document = document_for(&bridge);

        assert_eq!(document.api_key, "cellar-local-trusted");
    }
}
