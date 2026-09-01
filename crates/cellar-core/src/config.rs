//! `cellar.toml`, and the environment that overrides parts of it.
//!
//! One file drives a Windows-native run and a Wine run: the platform decides how
//! the executable is launched, not the config. Secrets never live in the file;
//! they come from the environment and are held in [`Secret`], which cannot print
//! itself.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::grammar::DEFAULT_READY_PATTERN;
use crate::lifecycle::{BackoffPolicy, RestartPolicy};
use crate::secret::Secret;

/// Everything Cellar needs to run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The single-server spelling, desugared into one instance.
    ///
    /// Kept deserializable indefinitely: seven shipped profiles,
    /// `deploy/cellar.toml` and the Kubernetes ConfigMap all use it, and a
    /// config that stops parsing is a server that stops starting. Read it
    /// through [`Config::instances`] rather than directly, or the
    /// `[instances]` spelling is invisible to whatever is reading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerConfig>,

    /// A map keyed by id, not a list: the key is the name, so it cannot be
    /// forgotten or duplicated. `BTreeMap` so every listing is stably ordered.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub instances: BTreeMap<InstanceId, InstanceConfig>,

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

/// The id a legacy `[server]` table desugars to.
pub const DEFAULT_INSTANCE_ID: &str = "default";

/// A stable name for one supervised server.
///
/// Restricted to `[a-z0-9][a-z0-9-]{0,31}`. The same string ends up in a URL
/// query value, a Prometheus label, a tracing span and a `VARCHAR(64)` column,
/// and restricting the charset once at parse time is what makes it safe in all
/// four, the same argument `is_valid_identifier` makes for the SQL bootstrap.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct InstanceId(String);

impl InstanceId {
    pub fn new(value: &str) -> Result<Self, String> {
        let mut bytes = value.bytes();
        let first_is_alphanumeric = bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        let rest_is_safe =
            bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');

        if !first_is_alphanumeric || !rest_is_safe || value.len() > 32 {
            return Err(format!(
                "instance id '{value}' must match [a-z0-9][a-z0-9-]{{0,31}}: it is used as a URL \
                 query value, a metrics label and a database column"
            ));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for InstanceId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::new(&raw).map_err(serde::de::Error::custom)
    }
}

/// One entry under `[instances.<id>]`.
///
/// The server settings are nested under `[instances.<id>.server]` rather than
/// flattened. Flattening would read more like `[server]` and cost
/// `deny_unknown_fields`, which serde cannot apply through a flatten, and a
/// silently ignored `hostnam` in the most important table in the file is a
/// worse trade than three extra characters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceConfig {
    /// Whose data this is. Defaults to the instance id.
    ///
    /// Separate from the id because the id is the routing key and the scope is
    /// the storage key. Collapsing them would move an existing deployment's
    /// documents the moment it named its instance.
    #[serde(default)]
    pub scope: Option<String>,

    /// Declared but not started. How a Windows-only development instance stays
    /// in one config file that also deploys to Linux.
    #[serde(default = "yes")]
    pub enabled: bool,

    /// Whether `/readyz` speaks for this instance.
    ///
    /// The highest-consequence flag here: a development instance that cannot
    /// start on a host with no editor must not fail readiness for a healthy
    /// production server.
    #[serde(default = "yes")]
    pub required: bool,

    pub server: ServerConfig,

    /// Overrides the process-wide `[supervisor]` for this instance only.
    #[serde(default)]
    pub supervisor: Option<SupervisorConfig>,
    #[serde(default)]
    pub bridge: Option<BridgeConfig>,
}

const fn yes() -> bool {
    true
}

/// One instance with every default resolved, which is what the supervisor
/// registry is built from. Produced by [`Config::instances`]; never parsed.
#[derive(Debug, Clone)]
pub struct Instance {
    pub id: InstanceId,
    pub scope: String,
    pub enabled: bool,
    pub required: bool,
    pub server: ServerConfig,
    pub supervisor: SupervisorConfig,
    pub bridge: BridgeConfig,
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

    /// Working directory for the child.
    ///
    /// It does **not** decide where the engine writes. Measured 2026-09-01:
    /// `logs/` and `data/` both follow the executable's own directory, and a
    /// working directory elsewhere moved neither. See `docs/ARCHITECTURE.md`.
    #[serde(default)]
    pub working_dir: Option<PathBuf>,

    /// Where Cellar reads `sbox-server.log` from, when it is somewhere other
    /// than beside the executable.
    ///
    /// A read path, not a write path. Nothing Cellar passes moves the engine's
    /// write path, `FACEPUNCH_ENGINE` included, so pointing this at a file the
    /// engine does not write means readiness silently never fires.
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

/// The same values an empty `[server]` table would deserialize to.
///
/// Written out rather than derived, because several fields carry a serde
/// `default = "..."` and a derived `Default` would disagree with the file
/// format about `hostname`, `port` and `ready_pattern`.
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::new(),
            project: PathBuf::new(),
            game: None,
            map: None,
            launcher: Launcher::default(),
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
        }
    }
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

    /// The directory the game reads its own state from: `features.json`,
    /// `permissions.json` and the `hosting.json` Cellar writes.
    ///
    /// `data_dir` is the operator's answer and wins, because it is already what
    /// `hosting.json` is written to and pointing the two at different places
    /// cannot be right. The fallback reproduces the engine's own layout,
    /// `<exe dir>/data/<ident split on dots>`, which is derivable only for a
    /// published package: a local `.sbproj` run leaves `game` unset.
    pub fn game_data_dir(&self) -> Option<PathBuf> {
        if let Some(configured) = &self.data_dir {
            return Some(configured.clone());
        }
        let root = self.executable.parent()?;
        let mut path = root.join("data");
        for segment in self.game.as_deref()?.split('.') {
            path.push(segment);
        }
        Some(path)
    }

    /// Where the engine writes its log file.
    ///
    /// `Logging.cs` builds `{base}/logs/{processName}.log`, and `{base}` is
    /// `AppContext.BaseDirectory`: the executable's own directory. It reads
    /// `FACEPUNCH_ENGINE` first, but with `EnvironmentVariableTarget.User`,
    /// which is the Windows registry rather than the process environment, and
    /// setting it in either was measured to move nothing. `server.log_file`
    /// overrides Cellar's read path and does not move the engine's write path.
    pub fn engine_log_file(&self) -> PathBuf {
        if let Some(explicit) = &self.log_file {
            return explicit.clone();
        }
        self.executable
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default()
            .join("logs")
            .join("sbox-server.log")
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

    /// How long a server may sit in `Starting` before Cellar says it is not
    /// working. `0` disables the check.
    ///
    /// A server that never becomes ready is otherwise indistinguishable from one
    /// still starting, and both observed causes are permanent: a `ready_pattern`
    /// the gamemode never emits, and an engine that fails to resolve a package
    /// and then idles at the console at 165% CPU instead of exiting. The
    /// deadline expiring never kills or restarts the child, because a wrong
    /// pattern in front of a healthy server is one of the two cases and killing
    /// it would be the wrong answer to it.
    pub start_timeout_seconds: u64,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            restart: RestartPolicy::default(),
            backoff: BackoffPolicy::default(),
            graceful_timeout_seconds: 30,
            sample_interval_seconds: 2,
            // A cold facepunch.sandbox start measured about 120 seconds on the
            // Arch box, and a first run also compiles packages. Ten minutes is
            // far enough above that to be a statement about a permanent
            // condition rather than a slow disk.
            start_timeout_seconds: 600,
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
    /// operations tables. The primary instance's scope.
    pub fn scope(&self) -> String {
        self.primary()
            .map(|instance| instance.scope)
            .unwrap_or_else(|| self.bridge.scope.clone())
    }

    /// Every declared instance, with the process-wide defaults resolved.
    ///
    /// The load-bearing property is in the legacy arm: a `[server]` config
    /// takes its scope from the existing global `bridge.scope`, **not** from
    /// its id. Every shipped profile therefore keeps writing to exactly the
    /// scope it writes to today, so gaining this feature moves no rows and runs
    /// no migration.
    pub fn instances(&self) -> Vec<Instance> {
        if !self.instances.is_empty() {
            return self
                .instances
                .iter()
                .map(|(id, declared)| Instance {
                    id: id.clone(),
                    scope: declared
                        .scope
                        .clone()
                        .unwrap_or_else(|| id.as_str().to_owned()),
                    enabled: declared.enabled,
                    required: declared.required,
                    server: declared.server.clone(),
                    supervisor: declared
                        .supervisor
                        .clone()
                        .unwrap_or_else(|| self.supervisor.clone()),
                    bridge: declared
                        .bridge
                        .clone()
                        .unwrap_or_else(|| self.bridge.clone()),
                })
                .collect();
        }

        let Some(server) = &self.server else {
            return Vec::new();
        };

        vec![Instance {
            // Unwrap-free: the literal is checked by a test below.
            id: InstanceId::new(DEFAULT_INSTANCE_ID)
                .unwrap_or_else(|_| InstanceId("default".to_owned())),
            scope: self.bridge.scope.clone(),
            enabled: true,
            required: true,
            server: server.clone(),
            supervisor: self.supervisor.clone(),
            bridge: self.bridge.clone(),
        }]
    }

    /// The instance an unqualified request means.
    ///
    /// The first enabled one in id order, so a config that disables its
    /// development instance on Linux still has a primary.
    pub fn primary(&self) -> Option<Instance> {
        let instances = self.instances();
        instances
            .iter()
            .find(|instance| instance.enabled)
            .or_else(|| instances.first())
            .cloned()
    }

    /// The primary's server settings, for a caller that has not been taught
    /// about instances yet.
    pub fn primary_server(&self) -> Option<ServerConfig> {
        self.primary().map(|instance| instance.server)
    }

    /// Pull every secret out of the environment.
    ///
    /// Secrets are only ever read here. Nothing else in Cellar reads an
    /// environment variable, so "where does this credential come from" has one
    /// answer.
    pub fn overlay_env(&mut self) {
        // Every instance shares one GSLT today. Steam issues a token per app
        // account rather than per server process, and the engine takes it as a
        // launch argument, so a per-instance override would be a setting with
        // nothing behind it.
        if let Some(gslt) = Secret::from_env("CELLAR_GSLT") {
            if let Some(server) = self.server.as_mut() {
                server.gslt = Some(gslt.clone());
            }
            for declared in self.instances.values_mut() {
                declared.server.gslt = Some(gslt.clone());
            }
        }
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
        if self.server.is_some() && !self.instances.is_empty() {
            return Err(ConfigError::Invalid(
                "a config may use [server] or [instances], not both. Move the [server] table \
                 under [instances.<name>.server] and give it a scope, or the two would disagree \
                 about which server is the one being described."
                    .into(),
            ));
        }

        let instances = self.instances();
        if instances.is_empty() {
            return Err(ConfigError::Invalid(
                "no server is configured: add a [server] table or an [instances.<name>] one".into(),
            ));
        }

        for instance in &instances {
            validate_server(&instance.id, &instance.server)?;
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

        refuse_shared_resources(&self.exclusive_resources())?;

        Ok(())
    }

    /// What each running instance must hold to itself.
    ///
    /// Disabled instances are excluded: one that is declared but not started
    /// holds nothing, and that is exactly how a Windows-only development
    /// instance stays in a config file that also deploys to Linux.
    fn exclusive_resources(&self) -> Vec<Exclusive> {
        self.instances()
            .into_iter()
            .filter(|instance| instance.enabled)
            .map(|instance| Exclusive {
                log_file: instance.server.engine_log_file(),
                data_dir: instance.server.game_data_dir(),
                bridge_bind: instance
                    .bridge
                    .enabled
                    .then_some(instance.bridge.bind)
                    .filter(|bind| !bind.trim().is_empty()),
                direct_port: instance
                    .server
                    .direct_connect
                    .then_some(instance.server.port),
                scope: instance.scope,
                id: instance.id.as_str().to_owned(),
            })
            .collect()
    }
}

/// The resources exactly one instance may hold.
///
/// Sharing any of these is not a degraded configuration, it is two servers
/// writing over each other, and every one of them has a silent failure mode:
/// two tailers parsing both servers' lines, one `hosting.json`, one document
/// scope taking both servers' writes, one socket.
#[derive(Debug)]
struct Exclusive {
    id: String,
    log_file: PathBuf,
    data_dir: Option<PathBuf>,
    scope: String,
    /// `None` when the bridge is disabled, so a disabled bridge cannot collide.
    bridge_bind: Option<String>,
    /// `Some` only under `direct_connect`. Without it the engine reaches
    /// players through Steam's relay and the port is not a shared resource.
    direct_port: Option<u16>,
}

fn refuse_shared_resources(instances: &[Exclusive]) -> Result<(), ConfigError> {
    // The invariant that keeps this safe to add before it is reachable: a
    // config with one instance cannot collide with itself, and every config
    // that exists today has one. Nothing below may fire for them.
    if instances.len() < 2 {
        return Ok(());
    }

    for (index, one) in instances.iter().enumerate() {
        for other in &instances[index + 1..] {
            let clash = |what: &str, value: &str, fix: &str| {
                ConfigError::Invalid(format!(
                    "instances '{}' and '{}' both use {what} {value}. {fix}",
                    one.id, other.id
                ))
            };

            if one.log_file == other.log_file {
                return Err(clash(
                    "the log file",
                    &one.log_file.display().to_string(),
                    "The engine writes one logs/sbox-server.log per install directory, so both \
                     tailers would parse both servers' lines and every player join would be \
                     counted twice. Give each instance its own install directory.",
                ));
            }

            if let (Some(one_dir), Some(other_dir)) = (&one.data_dir, &other.data_dir)
                && one_dir == other_dir
            {
                return Err(clash(
                    "the data directory",
                    &one_dir.display().to_string(),
                    "hosting.json, features.json and permissions.json all live there, so each \
                     instance needs its own server.data_dir.",
                ));
            }

            if one.scope == other.scope {
                return Err(clash(
                    "the document scope",
                    &one.scope,
                    "The scope is the storage key, so one server's document writes would land on \
                     the other's documents. Give each instance its own scope.",
                ));
            }

            if let (Some(one_bind), Some(other_bind)) = (&one.bridge_bind, &other.bridge_bind)
                && one_bind == other_bind
            {
                return Err(clash(
                    "the bridge address",
                    one_bind,
                    "Only one listener may hold an address. Give each instance its own \
                     bridge.bind, or disable the bridge on one of them.",
                ));
            }

            if let (Some(one_port), Some(other_port)) = (one.direct_port, other.direct_port)
                && one_port == other_port
            {
                return Err(clash(
                    "server.port",
                    &one_port.to_string(),
                    "direct_connect publishes the real address, so the port is a real socket and \
                     the second bind would fail.",
                ));
            }
        }
    }

    Ok(())
}

/// The refusals that are about one server rather than about the process.
///
/// Every message names the table it is talking about, because with several
/// instances "server.executable is required" would not say whose.
fn validate_server(id: &InstanceId, server: &ServerConfig) -> Result<(), ConfigError> {
    let where_ = |field: &str| format!("instance '{id}': {field}");

    if server.executable.as_os_str().is_empty() {
        return Err(ConfigError::Invalid(where_(
            "server.executable is required",
        )));
    }

    if server.project.as_os_str().is_empty() && server.game.as_deref().is_none_or(str::is_empty) {
        return Err(ConfigError::Invalid(where_(
            "server.project or server.game is required",
        )));
    }

    if let Some(map) = server.map.as_deref().filter(|map| !map.trim().is_empty()) {
        if server.game.as_deref().is_none_or(str::is_empty) {
            return Err(ConfigError::Invalid(where_(
                "server.map is only valid with a published server.game ident",
            )));
        }
        if !qualified_ident(map) {
            return Err(ConfigError::Invalid(where_(&format!(
                "server.map '{map}' must use org.package form"
            ))));
        }
    }

    Ok(())
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
            instances: BTreeMap::new(),
            server: Some(ServerConfig {
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
            }),
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

    fn minimal_server() -> ServerConfig {
        minimal().server.unwrap_or_default()
    }

    fn with_data_dir(game: Option<&str>, leaf: &str) -> ServerConfig {
        ServerConfig {
            game: game.map(str::to_owned),
            data_dir: Some(PathBuf::from(format!(
                "/home/container/sbox/data/fobiat/{leaf}"
            ))),
            ..minimal().server.unwrap_or_default()
        }
    }

    #[test]
    fn a_local_project_profile_still_finds_the_directory_the_operator_configured() {
        // The whole of the bug: the derived path needs server.game, a local
        // .sbproj run does not set it, and the Access panel then read
        // features.json and permissions.json as their empty fallbacks while the
        // operator had already named the directory.
        let server = with_data_dir(None, "applejackrp#local");

        assert_eq!(
            server.game_data_dir(),
            Some(PathBuf::from(
                "/home/container/sbox/data/fobiat/applejackrp#local"
            ))
        );
    }

    #[test]
    fn an_unset_data_dir_falls_back_to_the_engines_own_layout() {
        let server = ServerConfig {
            game: Some("fobiat.applejackrp".to_owned()),
            data_dir: None,
            ..minimal().server.unwrap_or_default()
        };
        let derived = server.game_data_dir().expect("a published profile derives");

        assert!(derived.ends_with("data/fobiat/applejackrp"), "{derived:?}");
    }

    #[test]
    fn a_local_project_with_no_data_dir_has_nothing_to_derive_from() {
        let server = ServerConfig {
            game: None,
            data_dir: None,
            ..minimal().server.unwrap_or_default()
        };

        assert_eq!(server.game_data_dir(), None);
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
        assert!(minimal_server().data_dir_mode_mismatch().is_none());
    }

    /// Every AppleJackRP profile shipped with the `#local` leaf, published ones
    /// included, until 2026-08-28, and the published Facepunch Sandbox profiles
    /// slipped past the first version of this test because it only read
    /// `applejackrp*` files. Reading the files is the only way to catch any of
    /// them: nothing else in the workspace loads them.
    #[test]
    fn the_shipped_profiles_agree_with_their_own_mode() {
        for (name, config) in shipped_profiles() {
            assert!(
                config
                    .primary_server()
                    .unwrap_or_default()
                    .data_dir_mode_mismatch()
                    .is_none(),
                "{name}: {}",
                config
                    .primary_server()
                    .unwrap_or_default()
                    .data_dir_mode_mismatch()
                    .unwrap()
            );
        }
    }

    /// Every profile that ships, parsed. Reading the files is the only way to
    /// catch a mistake in one: nothing else in the workspace loads them.
    fn shipped_profiles() -> Vec<(String, Config)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut sources = Vec::new();

        for entry in std::fs::read_dir(root.join("configs")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap().to_owned();
            if !name.ends_with(".toml") {
                continue;
            }
            sources.push((name, std::fs::read_to_string(&path).unwrap()));
        }

        sources.push((
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
        sources.push(("deploy/kubernetes.yaml".to_owned(), embedded));

        assert!(sources.len() >= 9, "only {} profiles read", sources.len());

        sources
            .into_iter()
            .map(|(name, text)| {
                let config = toml::from_str(&text).unwrap_or_else(|why| panic!("{name}: {why}"));
                (name, config)
            })
            .collect()
    }

    fn instance(id: &str) -> Exclusive {
        Exclusive {
            id: id.to_owned(),
            log_file: PathBuf::from(format!("/srv/{id}/logs/sbox-server.log")),
            data_dir: Some(PathBuf::from(format!("/srv/{id}/data/fobiat/applejackrp"))),
            scope: id.to_owned(),
            bridge_bind: Some(format!(
                "127.0.0.1:{}",
                8000 + u16::from(id.as_bytes().first().copied().unwrap_or(b'a'))
            )),
            direct_port: None,
        }
    }

    /// The invariant the whole refusal rests on. Every config that can be
    /// written today has one instance, so none of the refusals can reach a
    /// deployment that starts now. Remove this and the collision checks become
    /// a way to break a working server.
    #[test]
    fn a_single_instance_config_never_reaches_a_collision_refusal() {
        assert_eq!(minimal().exclusive_resources().len(), 1);
        refuse_shared_resources(&[instance("a")]).unwrap();
    }

    #[test]
    fn every_shipped_profile_still_resolves_to_exactly_one_instance() {
        for (name, config) in shipped_profiles() {
            let instances = config.instances();
            assert_eq!(
                instances.len(),
                1,
                "{name} resolved to more than one instance"
            );

            // The load-bearing property of the desugaring. A legacy config takes
            // its scope from the global bridge.scope, never from its id, so
            // gaining the instance model moves no rows and runs no migration.
            // If this ever fails, a deployment's documents are about to move.
            assert_eq!(
                instances[0].scope, config.bridge.scope,
                "{name}'s scope moved"
            );
            assert_eq!(instances[0].id.as_str(), DEFAULT_INSTANCE_ID, "{name}");
        }
    }

    /// The two spellings, side by side, at the level the file format works at.
    fn parse(text: &str) -> Result<Config, String> {
        toml::from_str::<Config>(text).map_err(|why| why.to_string())
    }

    const TWO_INSTANCES: &str = r#"
        [instances.dev.server]
        executable = "/srv/dev/sbox-server.exe"
        project = "/srv/dev/applejackrp.sbproj"
        data_dir = "/srv/dev/data/fobiat/applejackrp#local"

        [instances.published.server]
        executable = "/srv/published/sbox-server.exe"
        game = "fobiat.applejackrp"
        data_dir = "/srv/published/data/fobiat/applejackrp"
    "#;

    #[test]
    fn an_instances_config_defaults_each_scope_to_its_own_id() {
        let config = parse(TWO_INSTANCES).unwrap();
        config.validate().unwrap();

        let instances = config.instances();
        assert_eq!(instances.len(), 2);
        // BTreeMap, so the order is the id order rather than the file order.
        assert_eq!(instances[0].id.as_str(), "dev");
        assert_eq!(instances[0].scope, "dev");
        assert_eq!(instances[1].id.as_str(), "published");
        assert_eq!(instances[1].scope, "published");
        assert!(
            instances
                .iter()
                .all(|instance| instance.enabled && instance.required)
        );
    }

    #[test]
    fn an_instance_may_name_a_scope_that_is_not_its_id() {
        // The id is the routing key and the scope is the storage key. An
        // existing deployment names its old scope here so its documents stay
        // where they are while it gains a second instance.
        let config = parse(
            r#"
            [instances.published]
            scope = "applejackrp-local"
            [instances.published.server]
            executable = "/srv/sbox-server.exe"
            game = "fobiat.applejackrp"
        "#,
        )
        .unwrap();

        assert_eq!(config.instances()[0].scope, "applejackrp-local");
    }

    #[test]
    fn a_config_may_not_use_both_spellings() {
        let config = parse(
            r#"
            [server]
            executable = "/srv/sbox-server.exe"
            project = "/srv/a.sbproj"

            [instances.dev.server]
            executable = "/srv/dev/sbox-server.exe"
            project = "/srv/dev/a.sbproj"
        "#,
        )
        .unwrap();

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("[server] or [instances]"), "{error}");
        assert!(error.contains("Move the [server] table"), "{error}");
    }

    #[test]
    fn a_config_with_no_server_at_all_is_refused() {
        let config = parse(
            "[web]
enabled = false
",
        )
        .unwrap();
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("no server is configured"), "{error}");
    }

    #[test]
    fn a_refusal_names_which_instance_it_is_about() {
        let config = parse(
            r#"
            [instances.dev.server]
            executable = "/srv/dev/sbox-server.exe"
        "#,
        )
        .unwrap();

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("instance 'dev'"), "{error}");
        assert!(error.contains("server.project or server.game"), "{error}");
    }

    #[test]
    fn an_instance_id_is_restricted_to_what_is_safe_in_a_url_and_a_label() {
        assert!(InstanceId::new("dev").is_ok());
        assert!(InstanceId::new("published-2").is_ok());
        assert!(InstanceId::new("0").is_ok());

        for bad in [
            "",
            "-dev",
            "Dev",
            "de v",
            "dev/../etc",
            "dev.one",
            &"a".repeat(33),
        ] {
            assert!(InstanceId::new(bad).is_err(), "'{bad}' should be refused");
        }
    }

    #[test]
    fn a_typo_inside_an_instance_is_refused_rather_than_ignored() {
        // The whole reason the server table is nested rather than flattened:
        // serde cannot apply deny_unknown_fields through a flatten, and a
        // silently ignored `hostnam` in this table is a server with the wrong
        // name and no way to find out why.
        let error = parse(
            r#"
            [instances.dev.server]
            executable = "/srv/sbox-server.exe"
            project = "/srv/a.sbproj"
            hostnam = "typo"
        "#,
        )
        .unwrap_err();

        assert!(error.contains("hostnam"), "{error}");
    }

    #[test]
    fn the_collision_refusals_are_reachable_once_a_config_has_two_instances() {
        // Phase 1 wrote these before anything could reach them. This is the
        // moment they start mattering.
        let config = parse(
            r#"
            [instances.dev.server]
            executable = "/srv/one/sbox-server.exe"
            project = "/srv/dev/a.sbproj"

            [instances.published.server]
            executable = "/srv/one/sbox-server.exe"
            game = "fobiat.applejackrp"
        "#,
        )
        .unwrap();

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("the log file"), "{error}");
        assert!(error.contains("counted twice"), "{error}");
    }

    #[test]
    fn a_disabled_instance_holds_nothing_and_so_cannot_collide() {
        // How a Windows-only development instance stays in a config file that
        // also deploys to Linux.
        let config = parse(
            r#"
            [instances.dev]
            enabled = false
            [instances.dev.server]
            executable = "/srv/one/sbox-server.exe"
            project = "/srv/dev/a.sbproj"

            [instances.published.server]
            executable = "/srv/one/sbox-server.exe"
            game = "fobiat.applejackrp"
        "#,
        )
        .unwrap();

        config.validate().unwrap();
        assert_eq!(config.exclusive_resources().len(), 1);
        // Still declared, and still the second entry.
        assert_eq!(config.instances().len(), 2);
    }

    #[test]
    fn the_primary_is_the_first_enabled_instance() {
        let config = parse(
            r#"
            [instances.dev]
            enabled = false
            [instances.dev.server]
            executable = "/srv/dev/sbox-server.exe"
            project = "/srv/dev/a.sbproj"

            [instances.published.server]
            executable = "/srv/published/sbox-server.exe"
            game = "fobiat.applejackrp"
        "#,
        )
        .unwrap();

        assert_eq!(config.primary().unwrap().id.as_str(), "published");
    }

    #[test]
    fn an_instance_inherits_the_process_wide_supervisor_unless_it_overrides_it() {
        let config = parse(
            r#"
            [supervisor]
            start_timeout_seconds = 42

            [instances.dev.server]
            executable = "/srv/dev/sbox-server.exe"
            project = "/srv/dev/a.sbproj"

            [instances.published]
            [instances.published.supervisor]
            start_timeout_seconds = 7
            [instances.published.server]
            executable = "/srv/published/sbox-server.exe"
            game = "fobiat.applejackrp"
        "#,
        )
        .unwrap();

        let instances = config.instances();
        assert_eq!(instances[0].supervisor.start_timeout_seconds, 42);
        assert_eq!(instances[1].supervisor.start_timeout_seconds, 7);
    }

    #[test]
    fn two_instances_may_not_write_to_one_log_file() {
        let mut second = instance("b");
        second.log_file = instance("a").log_file;

        let error = refuse_shared_resources(&[instance("a"), second])
            .unwrap_err()
            .to_string();

        assert!(error.contains("the log file"), "{error}");
        assert!(error.contains("counted twice"), "{error}");
    }

    #[test]
    fn two_instances_may_not_share_a_data_directory() {
        let mut second = instance("b");
        second.data_dir = instance("a").data_dir;

        let error = refuse_shared_resources(&[instance("a"), second])
            .unwrap_err()
            .to_string();

        assert!(error.contains("server.data_dir"), "{error}");
    }

    #[test]
    fn two_instances_may_not_share_a_document_scope() {
        let mut second = instance("b");
        second.scope = instance("a").scope;

        let error = refuse_shared_resources(&[instance("a"), second])
            .unwrap_err()
            .to_string();

        assert!(error.contains("document scope"), "{error}");
    }

    #[test]
    fn two_instances_may_not_bind_one_bridge_address() {
        let mut second = instance("b");
        second.bridge_bind = instance("a").bridge_bind;

        let error = refuse_shared_resources(&[instance("a"), second])
            .unwrap_err()
            .to_string();

        assert!(error.contains("bridge.bind"), "{error}");
    }

    #[test]
    fn a_disabled_bridge_is_not_an_address_to_collide_over() {
        let mut one = instance("a");
        let mut other = instance("b");
        one.bridge_bind = None;
        other.bridge_bind = None;

        refuse_shared_resources(&[one, other]).unwrap();
    }

    #[test]
    fn only_direct_connect_makes_the_game_port_a_shared_resource() {
        let (mut one, mut other) = (instance("a"), instance("b"));
        one.direct_port = None;
        other.direct_port = None;
        refuse_shared_resources(&[one, other]).unwrap();

        let (mut one, mut other) = (instance("a"), instance("b"));
        one.direct_port = Some(27015);
        other.direct_port = Some(27015);
        let error = refuse_shared_resources(&[one, other])
            .unwrap_err()
            .to_string();

        assert!(error.contains("server.port"), "{error}");
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
        if let Some(server) = config.server.as_mut() {
            server.gslt = Some(Secret::new("A-REAL-LOOKING-GSLT"));
        }
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
