//! `cellar`: a dedicated server runner and manager for s&box.
//!
//! `run` is the supervising foreground mode, used as a container entrypoint. It
//! owns the game process, the bridge, the web UI and the updater, and it is the
//! only mode that starts a server. Everything else either inspects a config or
//! talks to a database.

mod commands;
mod runner;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "cellar",
    version,
    about = "An open-source dedicated server manager for s&box",
    long_about = None
)]
struct Cli {
    /// Path to cellar.toml.
    #[arg(short, long, default_value = "cellar.toml", global = true)]
    config: PathBuf,

    /// Log filter, for example `info` or `cellar_runtime=debug`.
    #[arg(long, env = "CELLAR_LOG", default_value = "info", global = true)]
    log: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Supervise the server in the foreground. The container entrypoint.
    Run {
        /// Also draw the terminal dashboard. Needs a real terminal.
        #[arg(long)]
        tui: bool,
    },

    /// Print what the config resolves to, with every secret redacted.
    Config,

    /// Check the config and the environment without starting anything.
    Doctor,

    /// Show installed and available versions.
    Version {
        #[arg(long)]
        json: bool,
    },

    /// Show the gamemode's changelog.
    Changelog {
        /// How many releases to show.
        #[arg(long, default_value_t = 3)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },

    /// Check for updates, and optionally take them.
    Update {
        /// Report only, whatever the configured policy says.
        #[arg(long)]
        check: bool,
        /// Apply now, ignoring the maintenance window. Still refuses a dirty
        /// checkout, and still refuses a populated server unless `--force`.
        #[arg(long)]
        now: bool,
        /// Apply even with players connected. Says what it is doing first.
        #[arg(long)]
        force: bool,
    },

    /// Database maintenance.
    Db {
        #[command(subcommand)]
        action: DbAction,
    },

    /// Manage the locally-hosted MariaDB, when `[mariadb].managed = true`.
    ///
    /// Separate from `db`: that operates on whatever `database.url` already
    /// points at, local or remote, and works the same either way. This only
    /// makes sense when Cellar is hosting the instance itself.
    Mariadb {
        #[command(subcommand)]
        action: MariadbAction,
    },

    /// Read and write bridge documents from the command line.
    Doc {
        #[command(subcommand)]
        action: DocAction,
    },

    /// Read and write the running server's configuration.
    Settings {
        #[command(subcommand)]
        action: SettingsAction,
    },

    /// Update Cellar itself from the published releases.
    SelfUpdate {
        /// Report what is available without installing it.
        #[arg(long)]
        check: bool,
    },

    /// Hash an operator password for the web UI.
    HashPassword,

    /// Expose Cellar over MCP or call another stdio MCP server.
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
}

#[derive(Subcommand)]
pub enum McpAction {
    /// Run Cellar's MCP server over stdio for an MCP host to launch.
    Serve {
        /// Running Cellar web URL. Read tools use CELLAR_API_TOKEN.
        #[arg(long)]
        url: Option<String>,
    },
    /// List tools advertised by another stdio MCP server.
    Tools {
        /// Executable that hosts the MCP server.
        command: String,
        /// Arguments passed to that executable. Repeat for each argument.
        #[arg(long = "arg")]
        args: Vec<String>,
    },
    /// Call a tool on another stdio MCP server.
    Call {
        /// Executable that hosts the MCP server.
        command: String,
        /// Tool name to call.
        #[arg(long)]
        tool: String,
        /// JSON object passed as tool arguments.
        #[arg(long)]
        input: Option<String>,
        /// Arguments passed to that executable. Repeat for each argument.
        #[arg(long = "arg")]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum DbAction {
    /// Apply pending migrations.
    Migrate,
    /// Show the schema and row counts.
    Status,
    /// Delete events older than the configured retention.
    Prune,
    /// Create a timestamped logical dump and prune old dumps.
    Backup,
}

#[derive(Subcommand)]
enum MariadbAction {
    /// Download (if needed), initialize (if needed), and (re-)create the
    /// database, user and password. Prints `CELLAR_DATABASE_URL` to set in
    /// your environment. Safe to re-run, including to recover a lost
    /// password: each install/init step is skipped once already done, but
    /// the database, user and password are always (re-)applied.
    Provision,
    /// Report install and data-directory state without starting anything.
    ///
    /// Deliberately no `start`/`stop`/`restart` here: `cellar run` is the
    /// only thing that supervises the instance, the same way it is the only
    /// thing that starts the game server.
    Status,
}

#[derive(Subcommand)]
pub enum SettingsAction {
    /// Capture the running server's features, settings and convars.
    Dump {
        /// Write YAML instead of TOML.
        #[arg(long)]
        yaml: bool,
        /// Write to a file instead of standard output.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Only what an operator changed away from the defaults.
        ///
        /// The shape worth committing: a full dump is mostly defaults.
        #[arg(long)]
        overrides: bool,
        /// Include the engine's own convars, discovered with `find`.
        #[arg(long, default_value = "applejack")]
        find: String,
    },

    /// Show what a file would change, without changing it.
    Diff { file: PathBuf },

    /// Apply a file to the running server.
    Apply {
        file: PathBuf,
        /// Print the commands instead of sending them.
        #[arg(long)]
        dry_run: bool,
    },

    /// Set one feature or setting.
    Set { id: String, value: String },
}

#[derive(Subcommand)]
enum DocAction {
    /// List documents.
    Ls {
        #[arg(default_value = "")]
        prefix: String,
    },
    /// Print one document.
    Get { key: String },
    /// Write one document from a file, or from stdin with `-`.
    Put { key: String, file: PathBuf },
    /// Show a document's revision history.
    History { key: String },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&cli.log)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        // MCP stdio reserves stdout for JSON-RPC frames. Keeping all Cellar
        // diagnostics on stderr also makes the command safe for host launchers.
        .with_writer(std::io::stderr)
        .init();

    let result = match cli.command {
        Command::Run { tui } => runner::run(&cli.config, tui).await,
        Command::Config => commands::show_config(&cli.config),
        Command::Doctor => commands::doctor(&cli.config).await,
        Command::Version { json } => commands::version(&cli.config, json).await,
        Command::Changelog { limit, json } => commands::changelog(&cli.config, limit, json),
        Command::Update { check, now, force } => {
            commands::update(&cli.config, check, now, force).await
        }
        Command::Db { action } => commands::db(&cli.config, action).await,
        Command::Mariadb { action } => commands::mariadb(&cli.config, action).await,
        Command::Doc { action } => commands::doc(&cli.config, action).await,
        Command::Settings { action } => commands::settings(&cli.config, action).await,
        Command::SelfUpdate { check } => commands::self_update(check).await,
        Command::HashPassword => commands::hash_password(),
        Command::Mcp { action } => commands::mcp(action).await,
    };

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // To stderr and without a backtrace: the common failures here are
            // configuration mistakes, and a stack trace buries the sentence that
            // says which one.
            eprintln!("cellar: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
