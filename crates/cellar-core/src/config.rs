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
    #[serde(default)]
    pub mariadb: MariaDbConfig,
    #[serde(default)]
    pub backup: BackupConfig,
    #[serde(default)]
    pub release: ReleaseConfig,
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
    pub program_check: bool,
    pub program_check_interval_minutes: u64,
    pub program_release_url: String,
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
            program_check: true,
            program_check_interval_minutes: 60,
            program_release_url: "https://api.github.com/repos/fobiat/cellar/releases/latest"
                .to_owned(),
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

    /// The local `.sbproj` handed to `+game` when `game` is unset.
    ///
    /// `+game`, not `+project`: `+project` loads metadata and idles at the bare
    /// console without ever booting a map.
    #[serde(default)]
    pub project: PathBuf,

    /// A published package ident such as `fobiat.applejackrp`.
    #[serde(default)]
    pub game: Option<String>,

    /// The map ident appended after a published `+game` ident.
    #[serde(default)]
    pub map: Option<String>,

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
    #[serde(default, skip_serializing, skip_deserializing)]
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

/// The engine's suffix for a package loaded from a local `.sbproj`.
pub const LOCAL_PROJECT_SUFFIX: &str = "#local";

impl ServerConfig {
    /// True when this profile runs a published package rather than a local
    /// `.sbproj`.
    pub fn is_published(&self) -> bool {
        self.game
            .as_deref()
            .is_some_and(|game| !game.trim().is_empty())
    }

    /// Why `data_dir` cannot be the directory the game reads, if it cannot.
    ///
    /// The engine appends `#local` to the ident for a local `.sbproj` and names
    /// the data directory after the ident, so a profile carrying the other
    /// mode's leaf still starts: `hosting.json` goes where nothing reads it, and
    /// neither mode can see the other's characters, permissions or features.
    /// Every AppleJackRP profile shipped with the `#local` leaf, published ones
    /// included, until 2026-08-28.
    pub fn data_dir_mode_mismatch(&self) -> Option<String> {
        let leaf = self.data_dir.as_ref()?.file_name()?.to_str()?;

        match (self.is_published(), leaf.ends_with(LOCAL_PROJECT_SUFFIX)) {
            (true, true) => Some(format!(
                "'{leaf}' is the local-project directory, but server.game runs the published \
                 package. Drop the '{LOCAL_PROJECT_SUFFIX}' suffix, or the game will never read \
                 the hosting.json written here."
            )),
            (false, false) => Some(format!(
                "'{leaf}' is the published package directory, but this profile runs a local \
                 .sbproj. Append '{LOCAL_PROJECT_SUFFIX}', or the game will never read the \
                 hosting.json written here."
            )),
            _ => None,
        }
    }
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
    #[serde(skip_serializing, skip_deserializing)]
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
    #[serde(skip_serializing, skip_deserializing)]
    pub url: Option<Secret>,
    /// Optional path to a one-line external MySQL or MariaDB URL. The file is
    /// read only when `CELLAR_DATABASE_URL` is absent and is never serialized.
    pub url_file: Option<PathBuf>,
    pub max_connections: u32,
    /// Who owns the tables in this connection. Game-owned is the safe default:
    /// Cellar can inspect and query the schema, but never invents game tables.
    pub schema_owner: DatabaseSchemaOwner,
    /// Legacy opt-in for Cellar's operational migrations. It is ignored for a
    /// game-owned database and remains only for compatible v0.1 configs.
    pub migrate_on_start: bool,
    /// Keep this many days of `srv_event` rows. Zero keeps everything.
    pub event_retention_days: u32,
}

/// Ownership of a configured game database.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseSchemaOwner {
    /// The gamemode owns migrations, tables, and data contracts.
    #[default]
    Gamemode,
    /// Cellar's legacy operational schema owns this connection.
    Cellar,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: None,
            url_file: None,
            max_connections: 8,
            schema_owner: DatabaseSchemaOwner::default(),
            migrate_on_start: false,
            event_retention_days: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebConfig {
    pub enabled: bool,
    pub bind: String,
    /// Authentication policy for the operator UI.
    pub auth: WebAuthMode,
    /// Permit plain HTTP between Cellar and a TLS-terminating reverse proxy.
    pub allow_insecure_http: bool,
    /// Mark operator cookies Secure when TLS is terminated before Cellar.
    pub secure_cookies: bool,
    /// Argon2 hash of the operator password, from `CELLAR_WEB_PASSWORD_HASH`.
    #[serde(skip_serializing, skip_deserializing)]
    pub password_hash: Option<Secret>,
}

/// Authentication policy for the web UI.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebAuthMode {
    /// Require a password when one is configured, otherwise allow loopback.
    #[default]
    Auto,
    /// Always require the configured Argon2 password hash.
    Password,
    /// Disable the login gate. Valid only on a loopback bind.
    None,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "127.0.0.1:8081".to_owned(),
            auth: WebAuthMode::default(),
            allow_insecure_http: false,
            secure_cookies: false,
            password_hash: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NotifyConfig {
    pub enabled: bool,
    /// From `CELLAR_DISCORD_WEBHOOK_URL`. Never logged, never serialised.
    #[serde(skip_serializing, skip_deserializing)]
    pub discord_webhook: Option<Secret>,
    /// From `CELLAR_WEBHOOK_URL`. Receives the raw event JSON.
    #[serde(skip_serializing, skip_deserializing)]
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

/// A MariaDB server Cellar downloads, initializes, and supervises itself,
/// rather than only connecting to one hosted elsewhere.
///
/// Deliberately independent of `[database]`: this section only decides
/// whether `cellar run` also spawns and supervises a `mariadbd` process.
/// `database.url` (`CELLAR_DATABASE_URL`) is still the one source of the
/// connection string everything else uses, whether it points at a managed
/// instance or a remote one: `cellar mariadb provision` prints a URL for
/// the operator to set, it does not write one anywhere itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MariaDbConfig {
    /// Download (if needed), initialize (if needed), and supervise a local
    /// instance for the lifetime of `cellar run`.
    pub managed: bool,

    /// Exact version to install, e.g. "11.4.5". Never resolved from
    /// "latest": a server manager that silently tracks a moving target is a
    /// supply-chain surprise, the same reasoning `update.policy` defaults
    /// away from `Apply` for the game itself.
    pub version: String,

    /// sha256 of the official win64 archive for `version`, checked once by
    /// whoever sets `version` against MariaDB's own published hashes. The
    /// download is verified only against this pinned value, never against a
    /// checksum fetched over the same connection as the archive.
    pub sha256: Option<String>,

    /// Where the versioned binaries are unpacked. Required when `managed`.
    pub install_dir: Option<PathBuf>,

    /// Where the data directory lives, independent of `install_dir` so a
    /// version upgrade never touches it. Required when `managed`.
    pub data_dir: Option<PathBuf>,

    /// Loopback port. Not 3306: a machine that already has something bound
    /// there, a stray prior MariaDB or MySQL install, should not collide
    /// with a managed instance silently choosing the same port.
    pub port: u16,

    /// Database and user created during provisioning. Restricted to
    /// `[A-Za-z_][A-Za-z0-9_]*` by `validate()`, since both are interpolated
    /// into the bootstrap `CREATE DATABASE`/`CREATE USER` statements.
    pub database: String,
    pub username: String,

    pub restart: RestartPolicy,
    pub backoff: BackoffPolicy,

    /// How long a graceful stop waits for `mariadb-admin shutdown` to finish
    /// before killing the process outright.
    pub graceful_timeout_seconds: u64,
}

/// Scheduled logical backups of the Cellar database.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BackupConfig {
    pub enabled: bool,
    pub directory: Option<PathBuf>,
    pub interval_hours: u64,
    pub retain: usize,
}

/// Optional project-local commands for building and publishing the game.
///
/// Cellar never invents an s&box editor command. The editor owns the Steam
/// session, so operators provide the exact commands their project supports.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReleaseConfig {
    pub build_command: Vec<String>,
    pub publish_command: Vec<String>,
    pub working_dir: Option<PathBuf>,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            directory: None,
            interval_hours: 24,
            retain: 7,
        }
    }
}

impl Default for MariaDbConfig {
    fn default() -> Self {
        Self {
            managed: false,
            version: String::new(),
            sha256: None,
            install_dir: None,
            data_dir: None,
            port: 33306,
            database: "cellar".to_owned(),
            username: "cellar".to_owned(),
            restart: RestartPolicy::default(),
            backoff: BackoffPolicy::default(),
            graceful_timeout_seconds: 30,
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
        // Boxed: `toml::de::Error` alone pushes this variant past clippy's
        // `result_large_err` threshold, which would otherwise flag every
        // `Result<_, ConfigError>` in this module, `load` and `validate`
        // included.
        source: Box<toml::de::Error>,
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
            source: Box::new(source),
        })?;

        config.overlay_env();
        config.overlay_database_url_file()?;
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

    fn overlay_database_url_file(&mut self) -> Result<(), ConfigError> {
        if self.database.url.is_some() {
            return Ok(());
        }
        let Some(path) = &self.database.url_file else {
            return Ok(());
        };
        let url = std::fs::read_to_string(path)
            .map_err(|source| ConfigError::Read {
                path: path.clone(),
                source,
            })?
            .trim()
            .to_owned();
        if url.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "database.url_file {} is empty",
                path.display()
            )));
        }
        self.database.url = Some(Secret::new(url));
        Ok(())
    }

    /// Refuse a configuration that would fail later, in a way that is harder to
    /// diagnose than a message at startup.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.server.executable.as_os_str().is_empty() {
            return Err(ConfigError::Invalid("server.executable is required".into()));
        }

        if self.server.project.as_os_str().is_empty()
            && self.server.game.as_deref().is_none_or(str::is_empty)
        {
            return Err(ConfigError::Invalid(
                "server.project or server.game is required".into(),
            ));
        }

        if let Some(map) = self
            .server
            .map
            .as_deref()
            .filter(|map| !map.trim().is_empty())
        {
            if self.server.game.as_deref().is_none_or(str::is_empty) {
                return Err(ConfigError::Invalid(
                    "server.map is only valid with a published server.game ident".into(),
                ));
            }
            if !qualified_ident(map) {
                return Err(ConfigError::Invalid(format!(
                    "server.map '{map}' must use org.package form"
                )));
            }
        }

        if self.backup.enabled {
            if !self.database.enabled || self.database.url.is_none() {
                return Err(ConfigError::Invalid(
                    "backup.enabled needs database.enabled and a database URL".into(),
                ));
            }
            if self.backup.retain == 0 {
                return Err(ConfigError::Invalid(
                    "backup.retain must be at least 1 when backups are enabled".into(),
                ));
            }
        }

        if self.bridge.enabled {
            if !self.database.enabled {
                return Err(ConfigError::Invalid(
                    "bridge.enabled needs database.enabled: the bridge has nowhere to store a document".into(),
                ));
            }

            if self.database.url.is_none() {
                return Err(ConfigError::Invalid(
                    "database.enabled needs CELLAR_DATABASE_URL or database.url_file".into(),
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

        if self.web.enabled
            && self.web.auth == WebAuthMode::Password
            && self.web.password_hash.is_none()
        {
            return Err(ConfigError::Invalid(
                "web.auth = \"password\" needs CELLAR_WEB_PASSWORD_HASH".into(),
            ));
        }

        if self.web.enabled && self.web.auth == WebAuthMode::None && !binds_loopback(&self.web.bind)
        {
            return Err(ConfigError::Invalid(
                "web.auth = \"none\" may only bind to loopback".into(),
            ));
        }

        if self.web.enabled
            && self.web.auth == WebAuthMode::Auto
            && !binds_loopback(&self.web.bind)
            && self.web.password_hash.is_none()
        {
            return Err(ConfigError::Invalid(
                "web.enabled on a non-loopback address needs CELLAR_WEB_PASSWORD_HASH: the web UI \
                 runs console commands at full engine privilege"
                    .into(),
            ));
        }

        if self.web.enabled && !binds_loopback(&self.web.bind) {
            if !self.web.allow_insecure_http {
                return Err(ConfigError::Invalid(
                    "web.allow_insecure_http = true is required for a non-loopback bind: put the UI behind a TLS reverse proxy"
                        .into(),
                ));
            }
            if !self.web.secure_cookies {
                return Err(ConfigError::Invalid(
                    "web.secure_cookies = true is required for a non-loopback bind behind a TLS reverse proxy"
                        .into(),
                ));
            }
        }

        if self.mariadb.managed {
            if self.mariadb.version.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "mariadb.managed needs mariadb.version: never resolved from \"latest\"".into(),
                ));
            }

            let checksum_looks_right = self.mariadb.sha256.as_deref().is_some_and(|sha256| {
                sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit())
            });
            if !checksum_looks_right {
                return Err(ConfigError::Invalid(
                    "mariadb.managed needs mariadb.sha256: a 64-character hex sha256 of the pinned \
                     version's official win64 archive, checked once against MariaDB's published \
                     hashes"
                        .into(),
                ));
            }

            if self.mariadb.install_dir.is_none() {
                return Err(ConfigError::Invalid(
                    "mariadb.managed needs mariadb.install_dir".into(),
                ));
            }

            if self.mariadb.data_dir.is_none() {
                return Err(ConfigError::Invalid(
                    "mariadb.managed needs mariadb.data_dir".into(),
                ));
            }

            if !self.database.enabled {
                return Err(ConfigError::Invalid(
                    "mariadb.managed needs database.enabled: a hosted instance with nothing \
                     configured to use it is not doing anything"
                        .into(),
                ));
            }

            if !is_valid_identifier(&self.mariadb.database) {
                return Err(ConfigError::Invalid(
                    "mariadb.database must match [A-Za-z_][A-Za-z0-9_]*: it is interpolated into \
                     a CREATE DATABASE statement during provisioning"
                        .into(),
                ));
            }

            if !is_valid_identifier(&self.mariadb.username) {
                return Err(ConfigError::Invalid(
                    "mariadb.username must match [A-Za-z_][A-Za-z0-9_]*: it is interpolated into \
                     a CREATE USER statement during provisioning"
                        .into(),
                ));
            }
        }

        Ok(())
    }
}

/// Whether a name is safe to interpolate into a bootstrap SQL statement.
///
/// `mariadb.database`/`mariadb.username` can't be bind parameters: MariaDB
/// has no placeholder syntax for identifiers in `CREATE DATABASE`/`CREATE
/// USER`. Restricting the charset before the name ever reaches a query is
/// what makes that interpolation safe, the same reasoning `admin.rs`'s table
/// browser uses for validating a table name against `tables()` before
/// interpolating it.
fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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

fn qualified_ident(value: &str) -> bool {
    let mut parts = value.split('.');
    let Some(org) = parts.next() else {
        return false;
    };
    let Some(package) = parts.next() else {
        return false;
    };
    !org.is_empty()
        && !package.is_empty()
        && parts.next().is_none()
        && org
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.')
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
                game: None,
                map: None,
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
            mariadb: MariaDbConfig::default(),
            backup: BackupConfig::default(),
            release: ReleaseConfig::default(),
        }
    }

    fn managed_mariadb() -> MariaDbConfig {
        MariaDbConfig {
            managed: true,
            version: "11.4.5".to_owned(),
            sha256: Some("a".repeat(64)),
            install_dir: Some(PathBuf::from("/var/lib/cellar/mariadb/11.4.5")),
            data_dir: Some(PathBuf::from("/var/lib/cellar/mariadb/data")),
            ..MariaDbConfig::default()
        }
    }

    #[test]
    fn a_minimal_config_is_valid() {
        minimal().validate().unwrap();
    }

    fn with_data_dir(game: Option<&str>, leaf: &str) -> ServerConfig {
        ServerConfig {
            game: game.map(str::to_owned),
            data_dir: Some(PathBuf::from(format!(
                "/home/container/sbox/data/fobiat/{leaf}"
            ))),
            ..minimal().server
        }
    }

    #[test]
    fn a_published_profile_may_not_point_at_the_local_project_directory() {
        let why = with_data_dir(Some("fobiat.applejackrp"), "applejackrp#local")
            .data_dir_mode_mismatch()
            .expect("the published package never reads the #local directory");

        assert!(why.contains("Drop the '#local' suffix"), "{why}");
    }

    #[test]
    fn a_local_project_profile_may_not_point_at_the_published_directory() {
        let why = with_data_dir(None, "applejackrp")
            .data_dir_mode_mismatch()
            .expect("a local .sbproj run only reads the #local directory");

        assert!(why.contains("Append '#local'"), "{why}");
    }

    #[test]
    fn a_matching_data_dir_is_accepted_in_either_mode() {
        assert!(
            with_data_dir(Some("fobiat.applejackrp"), "applejackrp")
                .data_dir_mode_mismatch()
                .is_none()
        );
        assert!(
            with_data_dir(None, "applejackrp#local")
                .data_dir_mode_mismatch()
                .is_none()
        );
    }

    #[test]
    fn an_unset_data_dir_has_no_mode_to_disagree_with() {
        assert!(minimal().server.data_dir_mode_mismatch().is_none());
    }

    /// Every AppleJackRP profile shipped with the `#local` leaf, published ones
    /// included, until 2026-08-28, and the published Facepunch Sandbox profiles
    /// slipped past the first version of this test because it only read
    /// `applejackrp*` files. Reading the files is the only way to catch any of
    /// them: nothing else in the workspace loads them.
    #[test]
    fn the_shipped_profiles_agree_with_their_own_mode() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut profiles = Vec::new();

        for entry in std::fs::read_dir(root.join("configs")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            if !name.ends_with(".toml") {
                continue;
            }
            profiles.push((name, std::fs::read_to_string(&path).unwrap()));
        }

        profiles.push((
            "deploy/cellar.toml".to_owned(),
            std::fs::read_to_string(root.join("deploy/cellar.toml")).unwrap(),
        ));

        // The Kubernetes manifest embeds the same profile as a ConfigMap; pull
        // the indented block back out rather than leaving it unchecked.
        let manifest = std::fs::read_to_string(root.join("deploy/kubernetes.yaml")).unwrap();
        let embedded: String = manifest
            .lines()
            .skip_while(|line| line.trim_end() != "  cellar.toml: |")
            .skip(1)
            .take_while(|line| line.starts_with("    ") || line.trim().is_empty())
            .map(|line| format!("{}\n", line.strip_prefix("    ").unwrap_or(line)))
            .collect();
        assert!(
            embedded.contains("[server]"),
            "no cellar.toml block found in deploy/kubernetes.yaml"
        );
        profiles.push(("deploy/kubernetes.yaml".to_owned(), embedded));

        let checked = profiles.len();
        for (name, text) in profiles {
            let config: Config =
                toml::from_str(&text).unwrap_or_else(|why| panic!("{name}: {why}"));

            assert!(
                config.server.data_dir_mode_mismatch().is_none(),
                "{name}: {}",
                config.server.data_dir_mode_mismatch().unwrap()
            );
        }

        assert!(checked >= 9, "only {checked} profiles read from {root:?}");
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
    fn an_exposed_web_ui_needs_proxy_tls_acknowledgements() {
        let mut config = minimal();
        config.web.enabled = true;
        config.web.bind = "0.0.0.0:8081".to_owned();
        config.web.password_hash = Some(Secret::new("$argon2id$v=19$m=1,t=1,p=1$hash"));

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("allow_insecure_http"), "{error}");

        config.web.allow_insecure_http = true;
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("secure_cookies"), "{error}");

        config.web.secure_cookies = true;
        config.validate().unwrap();
    }

    #[test]
    fn password_web_auth_can_be_required_on_loopback() {
        let mut config = minimal();
        config.web.enabled = true;
        config.web.auth = WebAuthMode::Password;
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("PASSWORD_HASH")
        );

        config.web.password_hash = Some(Secret::new("$argon2id$v=19$m=1,t=1,p=1$hash"));
        config.validate().unwrap();
    }

    #[test]
    fn unauthenticated_web_auth_is_loopback_only() {
        let mut config = minimal();
        config.web.enabled = true;
        config.web.auth = WebAuthMode::None;
        config.web.bind = "0.0.0.0:8081".to_owned();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("loopback")
        );
    }

    #[test]
    fn game_schema_is_the_safe_database_default() {
        assert_eq!(
            DatabaseConfig::default().schema_owner,
            DatabaseSchemaOwner::Gamemode
        );
        assert!(!DatabaseConfig::default().migrate_on_start);
    }

    #[test]
    fn managed_mariadb_needs_a_pinned_version_and_checksum() {
        let mut config = minimal();
        config.mariadb = managed_mariadb();
        config.database.enabled = true;
        config.validate().unwrap();

        let mut missing_version = config.clone();
        missing_version.mariadb.version = String::new();
        assert!(
            missing_version
                .validate()
                .unwrap_err()
                .to_string()
                .contains("mariadb.version")
        );

        let mut missing_checksum = config.clone();
        missing_checksum.mariadb.sha256 = None;
        assert!(
            missing_checksum
                .validate()
                .unwrap_err()
                .to_string()
                .contains("mariadb.sha256")
        );

        let mut short_checksum = config;
        short_checksum.mariadb.sha256 = Some("not-a-real-sha256".to_owned());
        assert!(
            short_checksum
                .validate()
                .unwrap_err()
                .to_string()
                .contains("mariadb.sha256")
        );
    }

    #[test]
    fn managed_mariadb_needs_install_and_data_dirs() {
        let mut config = minimal();
        config.mariadb = managed_mariadb();
        config.database.enabled = true;

        let mut no_install_dir = config.clone();
        no_install_dir.mariadb.install_dir = None;
        assert!(
            no_install_dir
                .validate()
                .unwrap_err()
                .to_string()
                .contains("install_dir")
        );

        let mut no_data_dir = config.clone();
        no_data_dir.mariadb.data_dir = None;
        assert!(
            no_data_dir
                .validate()
                .unwrap_err()
                .to_string()
                .contains("data_dir")
        );

        config.validate().unwrap();
    }

    #[test]
    fn managed_mariadb_needs_database_enabled() {
        let mut config = minimal();
        config.mariadb = managed_mariadb();
        config.database.enabled = false;

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("database.enabled"), "{error}");

        config.database.enabled = true;
        config.validate().unwrap();
    }

    #[test]
    fn mariadb_identifiers_are_restricted_to_a_safe_charset() {
        let mut config = minimal();
        config.mariadb = managed_mariadb();
        config.database.enabled = true;

        for bad in ["1cellar", "cellar-rp", "cellar db", "cellar;drop"] {
            let mut with_bad_database = config.clone();
            with_bad_database.mariadb.database = bad.to_owned();
            assert!(
                with_bad_database.validate().is_err(),
                "{bad:?} should be refused as a database name"
            );

            let mut with_bad_username = config.clone();
            with_bad_username.mariadb.username = bad.to_owned();
            assert!(
                with_bad_username.validate().is_err(),
                "{bad:?} should be refused as a username"
            );
        }

        for good in ["cellar", "cellar_rp", "_cellar", "Cellar2"] {
            config.mariadb.database = good.to_owned();
            config.mariadb.username = good.to_owned();
            config.validate().unwrap();
        }
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
    fn config_secret_fields_are_rejected_and_must_come_from_the_environment() {
        let text = r#"
            [server]
            executable = "a.exe"
            project = "a.sbproj"
            gslt = "a-token-from-a-file"

            [database]
            url = "mysql://user:file-password@host/db"

            [bridge]
            shared_secret = "bridge-file-secret"

            [web]
            password_hash = "$argon2id$v=19$not-a-real-hash"

            [notify]
            discord_webhook = "https://discord.example/webhook/file-secret"
            generic_webhook = "https://example.test/hook/file-secret"
        "#;

        let error = toml::from_str::<Config>(text).unwrap_err().to_string();
        assert!(error.contains("gslt"), "{error}");
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
