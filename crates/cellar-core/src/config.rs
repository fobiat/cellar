//! `cellar.toml`, and the environment that overrides parts of it.
//!
//! One file drives a Windows-native run and a Wine run: the platform decides how
//! the executable is launched, not the config. Secrets never live in the file;
//! they come from the environment and are held in [`Secret`], which cannot print
//! itself.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::grammar::DEFAULT_READY_PATTERN;
use crate::lifecycle::{BackoffPolicy, RestartPolicy};
use crate::secret::Secret;

/// Everything Cellar needs to run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub supervisor: SupervisorConfig,
    #[serde(default)]
    pub bridge: BridgeConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub notify: NotifyConfig,
    #[serde(default)]
    pub update: UpdateConfig,
}

/// What Cellar is allowed to do about a new version.
///
/// The default is [`UpdatePolicy::Notify`] rather than `Apply`: taking an update
/// is a decision an operator opts into, not one they discover after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum UpdatePolicy {
    /// Do not even check.
    Off,
    /// Check and report.
    #[default]
    Notify,
    /// Check, and take it when it is safe to.
    Apply,
}

/// Version checking and the hands-off updater.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UpdateConfig {
    pub policy: UpdatePolicy,
    /// Minutes between checks. Frequent enough for a game server, infrequent
    /// enough not to hammer a git remote.
    pub check_interval_minutes: u64,
    /// Ask the git remote for its tip. Off keeps every probe local and offline.
    pub check_remote: bool,
    /// Refuse to apply while anybody is connected. The single most important
    /// setting here: an updater that restarts a server with people on it is
    /// worse than no updater.
    pub only_when_empty: bool,
    /// Local-time hours between which an update may be applied. Equal values
    /// mean any time; a wrapping pair (22 to 4) is supported and is when a
    /// roleplay server is actually empty.
    pub window_start_hour: u8,
    pub window_end_hour: u8,
    pub update_gamemode: bool,
    pub update_engine: bool,
    /// Steam's install directory, for reading the build id and for updating.
    pub steam_dir: Option<PathBuf>,
    pub steamcmd: Option<PathBuf>,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            policy: UpdatePolicy::default(),
            check_interval_minutes: 60,
            check_remote: true,
            only_when_empty: true,
            window_start_hour: 0,
            window_end_hour: 0,
            update_gamemode: true,
            update_engine: false,
            steam_dir: None,
            steamcmd: None,
        }
    }
}

/// The child process.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Path to `sbox-server.exe`. A Windows path on Windows, a path inside the
    /// Wine prefix's view on Linux.
    pub executable: PathBuf,

    /// The `.sbproj` handed to `+game`.
    ///
    /// `+game`, not `+project`: `+project` loads metadata and idles at the bare
    /// console without ever booting a map.
    pub project: PathBuf,

    /// Launcher. `Native` on Windows, `Wine` on Linux.
    #[serde(default)]
    pub launcher: Launcher,

    /// Working directory for the child. The engine writes `logs/` relative to
    /// its own base directory, so this decides where the log file lands.
    #[serde(default)]
    pub working_dir: Option<PathBuf>,

    /// Where `logs/sbox-server.log` actually is, when it is not under
    /// `working_dir`. The engine honours `FACEPUNCH_ENGINE` for this.
    #[serde(default)]
    pub log_file: Option<PathBuf>,

    #[serde(default = "default_hostname")]
    pub hostname: String,

    /// Steam Game Server Login Token. Read from `CELLAR_GSLT` at load.
    #[serde(default, skip_serializing)]
    pub gslt: Option<Secret>,

    /// Expose the server's real address instead of routing through Steam's
    /// relay. Off by default, matching the existing deployment.
    #[serde(default)]
    pub direct_connect: bool,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_query_port")]
    pub query_port: u16,

    /// Log line that means "serving". See `grammar::DEFAULT_READY_PATTERN`.
    #[serde(default = "default_ready_pattern")]
    pub ready_pattern: String,

    /// Extra launch arguments, appended verbatim.
    #[serde(default)]
    pub extra_args: Vec<String>,

    /// The engine's data directory, where `hosting.json` is written.
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
}

/// How the executable gets launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Launcher {
    /// Run the executable directly. Windows.
    Native,
    /// Run it under Wine. Linux, which is the only option there: Facepunch's
    /// Linux dedicated server is roadmapped, not shipped.
    Wine,
}

impl Default for Launcher {
    fn default() -> Self {
        if cfg!(windows) {
            Self::Native
        } else {
            Self::Wine
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SupervisorConfig {
    pub restart: RestartPolicy,
    pub backoff: BackoffPolicy,

    /// How long a graceful stop waits after sending `quit` before killing.
    ///
    /// The engine installs no SIGTERM handler, so `quit` typed at the console is
    /// the only clean stop there is, and it has nine shutdown steps to get
    /// through including a Steam logoff. Kubernetes must be given at least this
    /// much `terminationGracePeriodSeconds`.
    pub graceful_timeout_seconds: u64,

    /// How often to sample process CPU and memory.
    pub sample_interval_seconds: u64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            restart: RestartPolicy::default(),
            backoff: BackoffPolicy::default(),
            graceful_timeout_seconds: 30,
            sample_interval_seconds: 2,
        }
    }
}

/// The `/v1/doc/{key}` service the gamemode already has a client for.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BridgeConfig {
    pub enabled: bool,

    /// Address to bind. Loopback by default, which is what makes `trusted` auth
    /// defensible.
    pub bind: String,

    /// The URL written into `hosting.json` for the gamemode to call.
    ///
    /// Must be the final URL. The engine strips `Authorization` on every
    /// redirect hop, so a bridge URL that redirects loses its credentials
    /// silently.
    pub public_url: String,

    /// The `Auth.GetToken` service name the gamemode presents a token for.
    pub auth_audience: String,

    pub auth: AuthMode,

    /// Value for `shared_secret` mode, from `CELLAR_BRIDGE_SECRET`.
    #[serde(skip_serializing)]
    pub shared_secret: Option<Secret>,

    /// Which server's documents these are. One value today; the column exists so
    /// that many-servers-sharing is a migration rather than a rewrite.
    pub scope: String,

    /// Largest document body accepted, in bytes.
    pub max_body_bytes: usize,

    /// Requests per minute per route before the bridge starts refusing. The
    /// caller is a game host, and a compromised host is the thing being limited.
    pub rate_limit_per_minute: u32,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "127.0.0.1:8080".to_owned(),
            public_url: "http://127.0.0.1:8080".to_owned(),
            auth_audience: "applejack-bridge".to_owned(),
            auth: AuthMode::Trusted,
            shared_secret: None,
            scope: "default".to_owned(),
            // The five documents are small; a character profile is a few KB.
            // A megabyte is generous and still bounds a hostile write.
            max_body_bytes: 1024 * 1024,
            rate_limit_per_minute: 600,
        }
    }
}

/// How the bridge decides a request is really from its own game server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Require a well-formed bearer token, record it, do not verify it.
    ///
    /// The trust boundary is the process tree, not the token: the gamemode's own
    /// design guarantees only the game host ever talks to the bridge, and Cellar
    /// is the process that launched that host on a loopback bind. This is
    /// honest about what it does rather than claiming a verification that has no
    /// published endpoint to perform.
    Trusted,
    /// Require a configured shared secret. For a bridge reachable off-box.
    /// Needs a matching gamemode-side change to send it.
    SharedSecret,
    /// Verify the token with Facepunch. Unimplemented: no public introspection
    /// endpoint for `Auth.GetToken` was found. Selecting it is refused at load
    /// rather than silently downgraded.
    Facepunch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DatabaseConfig {
    pub enabled: bool,
    /// From `CELLAR_DATABASE_URL`. Never written to the config file.
    #[serde(skip_serializing)]
    pub url: Option<Secret>,
    pub max_connections: u32,
    /// Run pending migrations at startup.
    pub migrate_on_start: bool,
    /// Keep this many days of `srv_event` rows. Zero keeps everything.
    pub event_retention_days: u32,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: None,
            max_connections: 8,
            migrate_on_start: true,
            event_retention_days: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebConfig {
    pub enabled: bool,
    pub bind: String,
    /// Argon2 hash of the operator password, from `CELLAR_WEB_PASSWORD_HASH`.
    #[serde(skip_serializing)]
    pub password_hash: Option<Secret>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "127.0.0.1:8081".to_owned(),
            password_hash: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NotifyConfig {
    pub enabled: bool,
    /// From `CELLAR_DISCORD_WEBHOOK_URL`. Never logged, never serialised.
    #[serde(skip_serializing)]
    pub discord_webhook: Option<Secret>,
    /// From `CELLAR_WEBHOOK_URL`. Receives the raw event JSON.
    #[serde(skip_serializing)]
    pub generic_webhook: Option<Secret>,
    /// Seconds to gather events before sending, so a join wave is one message.
    pub batch_seconds: u64,
    /// Event kinds to send. Empty means every notable kind.
    pub kinds: Vec<String>,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            discord_webhook: None,
            generic_webhook: None,
            batch_seconds: 5,
            kinds: Vec::new(),
        }
    }
}

fn default_hostname() -> String {
    "AppleJackRP Dev".to_owned()
}

fn default_port() -> u16 {
    27015
}

fn default_query_port() -> u16 {
    27016
}

fn default_ready_pattern() -> String {
    DEFAULT_READY_PATTERN.to_owned()
}

/// Why a config was refused.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("{path} is not valid TOML: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("{0}")]
    Invalid(String),
}

impl Config {
    /// Read a config file and overlay the environment.
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;

        let mut config: Config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })?;

        config.overlay_env();
        config.validate()?;
        Ok(config)
    }

    /// Which server's data this is, for the bridge's `scope` column and the
    /// operations tables. One value today; see `20_PERSISTENCE.md` Q1.
    pub fn scope(&self) -> String {
        self.bridge.scope.clone()
    }

    /// Pull every secret out of the environment.
    ///
    /// Secrets are only ever read here. Nothing else in Cellar reads an
    /// environment variable, so "where does this credential come from" has one
    /// answer.
    pub fn overlay_env(&mut self) {
        self.server.gslt = Secret::from_env("CELLAR_GSLT").or(self.server.gslt.take());
        self.database.url = Secret::from_env("CELLAR_DATABASE_URL").or(self.database.url.take());
        self.bridge.shared_secret =
            Secret::from_env("CELLAR_BRIDGE_SECRET").or(self.bridge.shared_secret.take());
        self.web.password_hash =
            Secret::from_env("CELLAR_WEB_PASSWORD_HASH").or(self.web.password_hash.take());
        self.notify.discord_webhook =
            Secret::from_env("CELLAR_DISCORD_WEBHOOK_URL").or(self.notify.discord_webhook.take());
        self.notify.generic_webhook =
            Secret::from_env("CELLAR_WEBHOOK_URL").or(self.notify.generic_webhook.take());
    }

    /// Refuse a configuration that would fail later, in a way that is harder to
    /// diagnose than a message at startup.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.server.executable.as_os_str().is_empty() {
            return Err(ConfigError::Invalid("server.executable is required".into()));
        }

        if self.server.project.as_os_str().is_empty() {
            return Err(ConfigError::Invalid("server.project is required".into()));
        }

        if self.bridge.enabled {
            if !self.database.enabled {
                return Err(ConfigError::Invalid(
                    "bridge.enabled needs database.enabled: the bridge has nowhere to store a document".into(),
                ));
            }

            if self.database.url.is_none() {
                return Err(ConfigError::Invalid(
                    "database.enabled needs CELLAR_DATABASE_URL in the environment".into(),
                ));
            }

            match self.bridge.auth {
                AuthMode::Facepunch => {
                    return Err(ConfigError::Invalid(
                        "bridge.auth = \"facepunch\" is not implemented: no public endpoint for \
                         verifying an Auth.GetToken token was found. Use \"trusted\" on a loopback \
                         bind, or \"shared_secret\"."
                            .into(),
                    ));
                }
                AuthMode::SharedSecret if self.bridge.shared_secret.is_none() => {
                    return Err(ConfigError::Invalid(
                        "bridge.auth = \"shared_secret\" needs CELLAR_BRIDGE_SECRET".into(),
                    ));
                }
                _ => {}
            }

            // `trusted` leans entirely on nothing else being able to reach the
            // bridge. Saying so at startup is cheaper than discovering it.
            if self.bridge.auth == AuthMode::Trusted && !binds_loopback(&self.bridge.bind) {
                return Err(ConfigError::Invalid(format!(
                    "bridge.auth = \"trusted\" does not verify tokens, so it may only bind \
                     loopback. {} is reachable from elsewhere; use \"shared_secret\" instead.",
                    self.bridge.bind
                )));
            }

            if self.bridge.public_url.trim().is_empty() {
                return Err(ConfigError::Invalid("bridge.public_url is required".into()));
            }

            if self.bridge.auth_audience.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "bridge.auth_audience is required: HostingConfigStore refuses a hosting.json without one".into(),
                ));
            }
        }

        if self.web.enabled && !binds_loopback(&self.web.bind) && self.web.password_hash.is_none() {
            return Err(ConfigError::Invalid(
                "web.enabled on a non-loopback address needs CELLAR_WEB_PASSWORD_HASH: the web UI \
                 runs console commands at full engine privilege"
                    .into(),
            ));
        }

        Ok(())
    }
}

/// Whether an address is only reachable from this machine.
pub fn binds_loopback(bind: &str) -> bool {
    let host = match bind.rsplit_once(':') {
        Some((host, _)) => host.trim_matches(['[', ']']),
        None => bind,
    };

    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        // A name is not provably loopback, so it is treated as not.
        Err(_) => host.eq_ignore_ascii_case("localhost"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn minimal() -> Config {
        Config {
            server: ServerConfig {
                executable: PathBuf::from("/home/container/sbox/sbox-server.exe"),
                project: PathBuf::from("/home/container/projects/applejackrp/applejackrp.sbproj"),
                launcher: Launcher::Wine,
                working_dir: None,
                log_file: None,
                hostname: default_hostname(),
                gslt: None,
                direct_connect: false,
                port: default_port(),
                query_port: default_query_port(),
                ready_pattern: default_ready_pattern(),
                extra_args: Vec::new(),
                data_dir: None,
            },
            supervisor: SupervisorConfig::default(),
            bridge: BridgeConfig::default(),
            database: DatabaseConfig::default(),
            web: WebConfig::default(),
            notify: NotifyConfig::default(),
            update: UpdateConfig::default(),
        }
    }

    #[test]
    fn a_minimal_config_is_valid() {
        minimal().validate().unwrap();
    }

    #[test]
    fn the_bridge_refuses_to_run_without_somewhere_to_store_a_document() {
        let mut config = minimal();
        config.bridge.enabled = true;
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("database.enabled"), "{error}");
    }

    #[test]
    fn facepunch_auth_is_refused_rather_than_silently_downgraded() {
        let mut config = minimal();
        config.bridge.enabled = true;
        config.database.enabled = true;
        config.database.url = Some(Secret::new("mysql://user:pw@host/db"));
        config.bridge.auth = AuthMode::Facepunch;

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("not implemented"), "{error}");
    }

    #[test]
    fn trusted_auth_may_not_bind_a_reachable_address() {
        let mut config = minimal();
        config.bridge.enabled = true;
        config.database.enabled = true;
        config.database.url = Some(Secret::new("mysql://user:pw@host/db"));
        config.bridge.auth = AuthMode::Trusted;
        config.bridge.bind = "0.0.0.0:8080".to_owned();

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("loopback"), "{error}");

        config.bridge.bind = "127.0.0.1:8080".to_owned();
        config.validate().unwrap();
    }

    #[test]
    fn shared_secret_auth_needs_its_secret() {
        let mut config = minimal();
        config.bridge.enabled = true;
        config.database.enabled = true;
        config.database.url = Some(Secret::new("mysql://user:pw@host/db"));
        config.bridge.auth = AuthMode::SharedSecret;
        config.bridge.bind = "0.0.0.0:8080".to_owned();

        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("CELLAR_BRIDGE_SECRET")
        );

        config.bridge.shared_secret = Some(Secret::new("a-long-enough-secret"));
        config.validate().unwrap();
    }

    #[test]
    fn an_exposed_web_ui_needs_a_password() {
        let mut config = minimal();
        config.web.enabled = true;
        config.web.bind = "0.0.0.0:8081".to_owned();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("PASSWORD_HASH")
        );
    }

    #[test]
    fn loopback_detection_covers_the_forms_that_matter() {
        assert!(binds_loopback("127.0.0.1:8080"));
        assert!(binds_loopback("localhost:8080"));
        assert!(binds_loopback("[::1]:8080"));
        assert!(!binds_loopback("0.0.0.0:8080"));
        assert!(!binds_loopback("10.0.0.5:8080"));
        assert!(!binds_loopback("bridge.applejack.svc:8080"));
    }

    #[test]
    fn a_config_round_trips_without_leaking_a_secret() {
        let mut config = minimal();
        config.server.gslt = Some(Secret::new("A-REAL-LOOKING-GSLT"));
        config.database.url = Some(Secret::new("mysql://user:password@host/db"));

        let text = toml::to_string(&config).unwrap();
        assert!(!text.contains("A-REAL-LOOKING-GSLT"));
        assert!(!text.contains("password@host"));
    }

    #[test]
    fn an_unknown_key_is_refused_rather_than_ignored() {
        let text = r#"
            [server]
            executable = "a.exe"
            project = "a.sbproj"
            tickrate = 50
        "#;
        let error = toml::from_str::<Config>(text).unwrap_err().to_string();
        assert!(error.contains("tickrate"), "{error}");
    }
}
