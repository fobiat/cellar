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
    about = "A dedicated server runner and manager for s&box / AppleJackRP",
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

    /// Read and write bridge documents from the command line.
    Doc {
        #[command(subcommand)]
        action: DocAction,
    },

    /// Update Cellar itself from the published releases.
    SelfUpdate {
        /// Report what is available without installing it.
        #[arg(long)]
        check: bool,
    },

    /// Hash an operator password for the web UI.
    HashPassword,
}

#[derive(Subcommand)]
enum DbAction {
    /// Apply pending migrations.
    Migrate,
    /// Show the schema and row counts.
    Status,
    /// Delete events older than the configured retention.
    Prune,
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
        Command::Doc { action } => commands::doc(&cli.config, action).await,
        Command::SelfUpdate { check } => commands::self_update(check).await,
        Command::HashPassword => commands::hash_password(),
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
