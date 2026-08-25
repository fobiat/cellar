//! `cellar run`: the supervising foreground mode.
//!
//! Everything that runs continuously is started here and shut down here, in an
//! order that matters. `hosting.json` is written before the child starts,
//! because the gamemode reads it once at boot; the bridge binds before the child
//! starts, because the child will call it during map load; and the signal
//! handler is installed before any of it, because the interesting failure is a
//! rollout arriving mid-startup.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use cellar_core::config::{AuthMode, Config, UpdatePolicy};
use cellar_core::event::Event;
use cellar_runtime::{Handle, Supervisor};
use cellar_server::auth::Policy;
use cellar_server::state::{AppState, Documents, RateLimiter};

pub async fn run(config_path: &Path, with_tui: bool) -> Result<()> {
    let config =
        Config::load(config_path).with_context(|| format!("reading {}", config_path.display()))?;

    let (supervisor, handle, control) = Supervisor::new(config.clone());

    match supervisor.prepare_hosting() {
        Ok(message) => tracing::info!("hosting.json: {message}"),
        // Not fatal: without it the gamemode keeps its own default, which is
        // local files. Loud, because a bridge nobody is using is the quiet
        // failure this whole feature exists to avoid.
        Err(why) => tracing::error!("hosting.json: {why}"),
    }

    // Started before the database connects, and given a moment to come up:
    // connecting while `mariadbd` is still initializing would just be the
    // first of a string of retries. `database.url` is unchanged either way,
    // see `[mariadb]` in cellar-core::config for why the two stay decoupled.
    let mariadb = start_mariadb(&config).await?;

    let pool = open_database(&config).await?;
    let state = build_state(&config, pool.clone(), handle.clone(), mariadb.clone())?;

    let mut servers = Vec::new();

    if config.bridge.enabled {
        let router = cellar_server::bridge_router(state.clone());
        servers.push(bind(&config.bridge.bind, router, "bridge").await?);
    }

    if config.web.enabled {
        let router = cellar_server::web_router(state.clone());
        servers.push(bind(&config.web.bind, router, "web ui").await?);
    }

    if let Some(notifier) = cellar_notify::Notifier::new(&config.notify, &config.server.hostname) {
        tokio::spawn(notifier.run(handle.subscribe()));
    }

    if let Some(pool) = &pool {
        tokio::spawn(record_events(
            pool.clone(),
            handle.subscribe(),
            config.scope(),
        ));
    }

    if config.update.policy != UpdatePolicy::Off {
        tokio::spawn(watch_for_updates(config.clone(), handle.clone()));
    }

    let supervising = tokio::spawn(supervisor.run(control));

    if with_tui {
        cellar_tui::run(handle.clone()).await?;
        handle.stop().await;
    } else {
        wait_for_shutdown().await;
        tracing::info!("stopping the server gracefully");
        // `quit` through the console, not a signal: the engine installs no
        // SIGTERM handler, and a kill skips the Steam logoff and the convar save.
        handle.stop().await;
    }

    let _ = tokio::time::timeout(std::time::Duration::from_secs(60), supervising).await;

    for server in servers {
        server.abort();
    }

    // Stopped last: nothing above needs the database once it has stopped
    // accepting requests, and stopping it earlier would just make the game
    // server's own shutdown, and any in-flight bridge write, fail instead.
    if let Some(mariadb) = &mariadb {
        tracing::info!("stopping mariadb");
        mariadb.stop().await;
    }

    Ok(())
}

/// Start and wait for the locally-hosted MariaDB, when `[mariadb].managed`.
///
/// Absent otherwise: `database.url` already works unchanged for a remote
/// database, and this is the only thing that differs.
async fn start_mariadb(config: &Config) -> Result<Option<cellar_mariadb::Handle>> {
    if !config.mariadb.managed {
        return Ok(None);
    }

    let url = config.database.url.as_ref().context(
        "mariadb.managed needs CELLAR_DATABASE_URL; run `cellar mariadb provision` to get one",
    )?;

    let password = cellar_mariadb::credentials::password_from_database_url(url.expose())
        .context("could not read a password out of CELLAR_DATABASE_URL")?;

    let (supervisor, handle, control) =
        cellar_mariadb::Supervisor::new(config.mariadb.clone(), password);
    tokio::spawn(supervisor.run(control));

    if !handle.wait_ready(std::time::Duration::from_secs(60)).await {
        anyhow::bail!(
            "mariadb did not start accepting connections within 60s on 127.0.0.1:{}",
            config.mariadb.port
        );
    }

    tracing::info!("mariadb is up on 127.0.0.1:{}", config.mariadb.port);
    Ok(Some(handle))
}

fn build_state(
    config: &Config,
    pool: Option<sqlx::MySqlPool>,
    handle: Handle,
    mariadb: Option<cellar_mariadb::Handle>,
) -> Result<Arc<AppState>> {
    let documents = match &pool {
        Some(pool) => Documents::MySql(pool.clone()),
        // Without a database the bridge has nowhere to put a document. The
        // config layer refuses that combination, so this is only reached with
        // the bridge disabled, where the backing is never touched.
        None => Documents::memory(),
    };

    let auth = Policy::from_config(config.bridge.auth, config.bridge.shared_secret.as_ref())
        .map_err(anyhow::Error::msg)?;

    let mut state = AppState::new(documents, auth, config.scope());
    state.max_body_bytes = config.bridge.max_body_bytes;
    state.rate_limiter = RateLimiter::new(config.bridge.rate_limit_per_minute);
    state.supervisor = Some(handle);
    state.pool = pool;
    state.mariadb = mariadb;
    state.web_password_hash = config.web.password_hash.clone();
    state.update_config = config.update.clone();
    state.version_probe = Some(cellar_update::Probe {
        project_dir: project_dir(config),
        steam_dir: config.update.steam_dir.clone(),
        steamcmd: config.update.steamcmd.clone(),
        check_remote: config.update.check_remote,
    });

    if config.bridge.enabled && config.bridge.auth == AuthMode::Trusted {
        tracing::warn!(
            "the bridge accepts any well-formed bearer token without verifying it, and is trusted \
             because it binds {}. See the auth notes in the README.",
            config.bridge.bind
        );
    }

    Ok(Arc::new(state))
}

async fn open_database(config: &Config) -> Result<Option<sqlx::MySqlPool>> {
    if !config.database.enabled {
        return Ok(None);
    }

    let url = config
        .database
        .url
        .as_ref()
        .context("database.enabled needs CELLAR_DATABASE_URL")?;

    let pool = cellar_store::connect(url.expose(), config.database.max_connections)
        .await
        .context("connecting to the database")?;

    if config.database.migrate_on_start {
        cellar_store::migrate(&pool)
            .await
            .context("applying migrations")?;
        tracing::info!("database schema is current");
    }

    Ok(Some(pool))
}

async fn bind(
    address: &str,
    router: axum::Router,
    what: &str,
) -> Result<tokio::task::JoinHandle<()>> {
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("binding the {what} to {address}"))?;

    tracing::info!("{what} listening on {address}");

    Ok(tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router).await {
            tracing::error!("http server stopped: {error}");
        }
    }))
}

/// Mirror the event stream into the operations tables.
///
/// Every write here is best effort. An operations insert must never be the
/// reason a player's join is not handled, so a failure is a warning and the
/// stream carries on.
async fn record_events(
    pool: sqlx::MySqlPool,
    mut events: tokio::sync::broadcast::Receiver<Event>,
    scope: String,
) {
    let mut session: Option<u64> = None;

    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!("the recorder fell behind, {missed} event(s) not stored");
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };

        let result = match &event {
            Event::ProcessStarted { command, .. } => {
                match cellar_store::ops::begin_session(
                    &pool,
                    &scope,
                    hostname().as_deref(),
                    command,
                )
                .await
                {
                    Ok(id) => {
                        session = Some(id);
                        Ok(())
                    }
                    Err(why) => Err(why),
                }
            }
            Event::ServerReady { .. } => match session {
                Some(id) => cellar_store::ops::mark_ready(&pool, id).await,
                None => Ok(()),
            },
            Event::ProcessExited { code, graceful } => match session.take() {
                Some(id) => cellar_store::ops::end_session(&pool, id, *code, *graceful).await,
                None => Ok(()),
            },
            Event::PlayerJoined { steam_id, name } => {
                cellar_store::ops::player_joined(&pool, session, *steam_id, name).await
            }
            Event::PlayerLeft {
                steam_id, reason, ..
            } => {
                let label = cellar_core::snapshot::leave_reason_label(reason);
                cellar_store::ops::player_left(&pool, session, *steam_id, label).await
            }
            _ => Ok(()),
        };

        if let Err(why) = result {
            tracing::warn!("could not record {}: {why}", event.kind());
        }

        if event.is_notable()
            && let Err(why) =
                cellar_store::ops::record_event(&pool, session, event.kind(), None, None, None)
                    .await
        {
            tracing::warn!("could not record an event: {why}");
        }
    }
}

/// Check for updates on a timer, and apply them when the policy and the gates agree.
async fn watch_for_updates(config: Config, handle: Handle) {
    let probe = cellar_update::Probe {
        project_dir: project_dir(&config),
        steam_dir: config.update.steam_dir.clone(),
        steamcmd: config.update.steamcmd.clone(),
        check_remote: config.update.check_remote,
    };

    let mut ticker = tokio::time::interval(cellar_update::updater::interval(&config.update));

    loop {
        ticker.tick().await;

        let versions = cellar_update::version::probe(&probe).await;
        for problem in &versions.problems {
            tracing::debug!("version probe: {problem}");
        }

        let players = handle
            .snapshot()
            .await
            .map(|s| s.players.len())
            .unwrap_or(0);
        let hour = chrono::Local::now()
            .format("%H")
            .to_string()
            .parse()
            .unwrap_or(0u8);

        match cellar_update::updater::decide(&config.update, &versions, players, hour) {
            cellar_update::Decision::UpToDate => {}
            cellar_update::Decision::Available { what } => {
                tracing::info!("update available: {}", what.join(", "));
            }
            cellar_update::Decision::Deferred { what, why } => {
                tracing::info!("update deferred ({why}): {}", what.join(", "));
            }
            cellar_update::Decision::Apply { what } => {
                tracing::warn!("taking update: {}", what.join(", "));

                // Stop before updating: the engine's files are in use while it
                // runs, and Steam cannot replace a running binary.
                handle.stop().await;

                let applied =
                    cellar_update::updater::apply(&config.update, &probe.project_dir).await;
                for step in &applied.steps {
                    if step.ok {
                        tracing::info!("{}: {}", step.name, step.detail);
                    } else {
                        tracing::error!("{} failed: {}", step.name, step.detail);
                    }
                }

                // Restart either way. A half-applied update still needs a
                // running server more than it needs to stay down.
                handle.restart().await;
            }
        }
    }
}

fn project_dir(config: &Config) -> std::path::PathBuf {
    config
        .server
        .project
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME").ok()
}

/// Wait for the signal the platform sends to stop a service.
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut term = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                tracing::error!("could not listen for SIGTERM: {error}");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };

        tokio::select! {
            _ = term.recv() => tracing::info!("SIGTERM"),
            _ = tokio::signal::ctrl_c() => tracing::info!("interrupt"),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("interrupt");
    }
}
