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
use cellar_core::config::{AuthMode, Config, DatabaseSchemaOwner, UpdatePolicy};
use cellar_core::event::Event;
use cellar_runtime::{Handle, Supervisor};
use cellar_server::auth::Policy;
use cellar_server::state::{AppState, Documents, ProgramUpdateStatus, RateLimiter};

pub async fn run(config_path: &Path, with_tui: bool) -> Result<()> {
    let config =
        Config::load(config_path).with_context(|| format!("reading {}", config_path.display()))?;

    // Started before the database connects, and given a moment to come up:
    // connecting while `mariadbd` is still initializing would just be the
    // first of a string of retries. `database.url` is unchanged either way,
    // see `[mariadb]` in cellar-core::config for why the two stay decoupled.
    let mariadb = start_mariadb(&config).await?;

    let pool = open_database(&config).await?;
    let primary = config
        .primary()
        .context("no server is configured; `cellar doctor` says which table is missing")?;

    // One supervisor per enabled instance. A disabled one still reaches the
    // registry, marked unavailable with its reason, so the dashboard shows a
    // declared server rather than nothing at all.
    let mut supervisors = Vec::new();
    let mut entries: Vec<cellar_server::registry::Entry> = Vec::new();

    for mut instance in config.instances() {
        // The real player ceiling, before a single player connects.
        // `+maxplayers` is not a convar and not a launch switch; the old
        // `entrypoint.sh` passed it for years and it was inert. Reading it here
        // rather than in the supervisor keeps `cellar-runtime` free of a
        // dependency on the update crate, and it is a config-resolution
        // question rather than a process one.
        match cellar_update::project::read(&instance.server.project) {
            Ok(Some(project)) => {
                if let Some(ceiling) = project.max_players {
                    instance.player_ceiling = Some(ceiling);
                }
                if !project.packages.is_empty() {
                    tracing::info!(
                        "instance '{}' resolves {} package(s) from sbox.game at boot: {}",
                        instance.id,
                        project.packages.len(),
                        project.packages.join(", ")
                    );
                }
            }
            Ok(None) => {}
            Err(why) => tracing::warn!("instance '{}': {why}", instance.id),
        }

        let mut entry = cellar_server::registry::Entry::from_instance(&instance);
        if !instance.enabled {
            tracing::info!(
                "instance '{}' is declared but not enabled here",
                instance.id
            );
            entries.push(entry);
            continue;
        }

        // Probed before spawning, because the alternative is what it used to
        // do: back off and retry into a missing binary five times and then
        // report a crash loop, which reads as a broken server rather than a
        // wrong path. `doctor` says the same thing, but nothing makes an
        // operator run it first.
        if let Some(why) = unavailable_reason(&instance) {
            tracing::warn!("instance '{}' will not be started: {why}", instance.id);
            entry.unavailable = Some(why);
            entries.push(entry);
            continue;
        }

        let id = instance.id.clone();
        let (supervisor, handle, control) = Supervisor::new(instance);

        match supervisor.prepare_hosting() {
            Ok(message) => tracing::info!("{id}: hosting.json: {message}"),
            // Not fatal: without it the gamemode keeps its own default, which
            // is local files. Loud, because a bridge nobody is using is the
            // quiet failure this whole feature exists to avoid.
            Err(why) => tracing::error!("{id}: hosting.json: {why}"),
        }

        entry.handle = Some(handle.clone());
        entry.unavailable = None;
        entries.push(entry);
        supervisors.push((id, supervisor, handle, control));
    }

    // The primary is whichever instance actually started, not whichever the
    // config nominates. They differ exactly when the first one could not start,
    // and that is the case where an operator most needs the dashboard up.
    let registry = cellar_server::registry::Registry::new(entries);
    let primary_handle = registry.primary().and_then(|entry| entry.handle.clone());

    if primary_handle.is_none() {
        tracing::error!(
            "no instance started. Cellar is still serving so you can see why; `cellar doctor` \
             says the same thing without starting anything."
        );
    }

    let state = build_state(
        config_path,
        &config,
        registry,
        pool.clone(),
        primary_handle.clone(),
        mariadb.clone(),
    )?;

    let mut servers = Vec::new();

    if config.bridge.enabled {
        let router = cellar_server::bridge_router(state.clone());
        servers.push(bind(&config.bridge.bind, router, "bridge").await?);
    }

    if config.web.enabled {
        let router = cellar_server::web_router(state.clone());
        servers.push(bind(&config.web.bind, router, "web ui").await?);
    }

    // One merged stream, tagged with which server each event came from.
    let merged = fan_in(
        &supervisors
            .iter()
            .map(|(id, _, handle, _)| (id.clone(), handle.clone()))
            .collect::<Vec<_>>(),
    );

    // The merged stream, not the primary's. This used to subscribe to one
    // handle, so on a two-instance deployment a crash on the second server
    // notified nobody, which is exactly the server an unattended deployment
    // hears about last.
    if let Some(notifier) = cellar_notify::Notifier::new(
        &config.notify,
        &primary.server.hostname,
        supervisors.len() > 1,
    ) {
        tokio::spawn(notifier.run(merged.subscribe()));
    }

    if let Some(pool) = &pool {
        let scopes = config
            .instances()
            .into_iter()
            .map(|instance| (instance.id, instance.scope))
            .collect();
        tokio::spawn(record_events(pool.clone(), merged.subscribe(), scopes));
    }

    let scheduler = build_scheduler(&config, &state, pool.clone(), primary_handle.clone());
    if !scheduler.is_empty() {
        let names: Vec<String> = scheduler
            .statuses()
            .into_iter()
            .map(|job| job.name)
            .collect();
        tracing::info!("scheduled jobs: {}", names.join(", "));
        scheduler.start();
    }
    let _ = state.scheduler.set(scheduler);

    let running: Vec<_> = supervisors
        .into_iter()
        .map(|(id, supervisor, handle, control)| {
            (id, handle, tokio::spawn(supervisor.run(control)))
        })
        .collect();

    if with_tui {
        // The TUI is a single-server htop with a command line, and that is a
        // good thing for it to be. It follows the primary.
        let handle = primary_handle
            .clone()
            .context("no instance started, so there is nothing for the TUI to follow")?;
        cellar_tui::run(handle).await?;
    } else {
        wait_for_shutdown(state.shutdown_requested.clone()).await;
        tracing::info!("stopping the server gracefully");
    }

    // `quit` through the console, not a signal: the engine installs no SIGTERM
    // handler, and a kill skips the Steam logoff and the convar save. Shutdown
    // rather than stop, because a stopped server leaves the supervisor resting
    // and still answering, which is what the dashboard wants and not what an
    // exiting process does.
    // One shared budget over every instance, not sixty seconds each. With two
    // servers the sequential version blows the Kubernetes grace period and the
    // pod is SIGKILLed mid-logoff, which is exactly the shutdown the engine has
    // no handler for.
    let budget = std::time::Duration::from_secs(60);
    let _ = tokio::time::timeout(budget, async {
        // Asked concurrently. Each `quit` waits out its own engine's nine
        // shutdown steps, and doing that in sequence is what turns one grace
        // period into N of them.
        let asks: Vec<_> = running
            .iter()
            .map(|(id, handle, _)| {
                let (id, handle) = (id.clone(), handle.clone());
                tokio::spawn(async move {
                    tracing::info!("stopping instance '{id}'");
                    handle.shutdown().await;
                })
            })
            .collect();
        for ask in asks {
            let _ = ask.await;
        }
        for (_, _, task) in running {
            let _ = task.await;
        }
    })
    .await;

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

/// Every recurring job this process runs, in one register.
///
/// These were three separate `tokio::spawn`ed loops that each slept and did a
/// thing, invisible from anywhere but this file, so nothing said when a backup
/// last ran or whether it worked. Worse, `database.event_retention_days` was
/// configured and had **no loop at all**: the setting has done nothing since it
/// was added, and only the manual `cellar db prune` ever acted on it.
///
/// The supervisor's tail tick and the MariaDB supervisor's are deliberately not
/// here. They are a state machine's clock inside a `select!`, with no result to
/// report and no meaning to "run now".
fn build_scheduler(
    config: &Config,
    state: &Arc<AppState>,
    pool: Option<sqlx::MySqlPool>,
    primary: Option<Handle>,
) -> Arc<cellar_runtime::Scheduler> {
    use cellar_runtime::scheduler::Spec;
    let mut scheduler = cellar_runtime::Scheduler::new();

    if config.backup.enabled {
        match config.database.url.clone() {
            Some(url) => {
                let url = url.expose().to_owned();
                let mariadb = config.mariadb.clone();
                let backup = config.backup.clone();
                scheduler.register(
                    Spec {
                        name: "database-backup".to_owned(),
                        description: "Dump the operations database, verify it, and prune old dumps"
                            .to_owned(),
                        interval: std::time::Duration::from_secs(
                            config.backup.interval_hours.max(1) * 3600,
                        ),
                        // Never at startup. A Cellar being restarted in a loop
                        // would otherwise take a dump per restart and prune the
                        // good ones out of the retention window.
                        at_startup: false,
                    },
                    move || {
                        let url = url.clone();
                        let mariadb = mariadb.clone();
                        let backup = backup.clone();
                        // Blocking process work, off the async threads.
                        async move {
                            tokio::task::spawn_blocking(move || {
                                cellar_mariadb::backup(&url, &mariadb, &backup)
                                    .map(|path| format!("wrote {}", path.display()))
                                    .map_err(|why| why.to_string())
                            })
                            .await
                            .unwrap_or_else(|why| Err(why.to_string()))
                        }
                    },
                );
            }
            None => tracing::warn!("database backups enabled but CELLAR_DATABASE_URL is unset"),
        }
    }

    // The job that never existed. `event_retention_days` defaults to 90 and
    // nothing has ever enforced it, so a long-running deployment's `srv_event`
    // and `srv_command` grow without bound.
    if let Some(pool) = pool.clone() {
        let days = config.database.event_retention_days;
        if days > 0 {
            scheduler.register(
                Spec {
                    name: "event-retention".to_owned(),
                    description: format!("Delete recorded events older than {days} days"),
                    interval: std::time::Duration::from_secs(24 * 3600),
                    at_startup: false,
                },
                move || {
                    let pool = pool.clone();
                    async move {
                        cellar_store::ops::prune_events(&pool, days)
                            .await
                            .map(|deleted| format!("deleted {deleted} row(s)"))
                            .map_err(|why| why.to_string())
                    }
                },
            );
        }
    }

    if config.update.policy != UpdatePolicy::Off
        && let Some(handle) = primary
    {
        let config = config.clone();
        scheduler.register(
            Spec {
                name: "game-update-check".to_owned(),
                description: "Check for an update, and take it if the policy and the gates agree"
                    .to_owned(),
                interval: cellar_update::updater::interval(&config.update),
                at_startup: true,
            },
            move || {
                let config = config.clone();
                let handle = handle.clone();
                async move { check_for_updates(&config, &handle).await }
            },
        );
    }

    if config.update.program_check {
        let url = config.update.program_release_url.clone();
        let status = state.program_update.clone();
        scheduler.register(
            Spec {
                name: "program-update-check".to_owned(),
                description: "Check whether a newer Cellar has been released".to_owned(),
                interval: std::time::Duration::from_secs(
                    config.update.program_check_interval_minutes.max(5) * 60,
                ),
                at_startup: true,
            },
            move || {
                let url = url.clone();
                let status = status.clone();
                async move { check_for_program_updates(&url, &status).await }
            },
        );
    }

    Arc::new(scheduler)
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
    config_path: &Path,
    config: &Config,
    instances: cellar_server::registry::Registry,
    pool: Option<sqlx::MySqlPool>,
    handle: Option<Handle>,
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
    state.login_limiter = cellar_server::state::LoginLimiter::new(10);
    state.supervisor = handle;
    state.pool = pool;
    state.database_schema_owner = match config.database.schema_owner {
        DatabaseSchemaOwner::Gamemode => "gamemode".to_owned(),
        DatabaseSchemaOwner::Cellar => "cellar".to_owned(),
    };
    state.mariadb = mariadb;
    state.database_url = config.database.url.clone();
    state.mariadb_config = config.mariadb.clone();
    state.backup_config = config.backup.clone();
    state.web_password_hash = config.web.password_hash.clone();
    state.web_auth = config.web.auth;
    state.web_secure_cookies = config.web.secure_cookies;
    state.external_api_token = cellar_core::Secret::from_env("CELLAR_API_TOKEN");
    state.update_config = config.update.clone();
    state.program_update = Arc::new(tokio::sync::RwLock::new(ProgramUpdateStatus::new(
        config.update.program_release_url.clone(),
    )));
    state.release_config = config.release.clone();
    if let Ok(mut path) = state.config_path.lock() {
        *path = Some(config_path.to_owned());
    }
    state.web_bind = config.web.bind.clone();
    state.web_enabled = config.web.enabled;
    state.instances = instances;
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

    if config.database.migrate_on_start
        && config.database.schema_owner == DatabaseSchemaOwner::Cellar
    {
        cellar_store::migrate(&pool)
            .await
            .context("applying migrations")?;
        tracing::info!("database schema is current");
    } else if config.database.migrate_on_start {
        tracing::warn!(
            "database.migrate_on_start is ignored because schema_owner = gamemode; Cellar never migrates game tables"
        );
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

/// Why an instance cannot be started here, if it cannot.
///
/// Deliberately narrow: only conditions that are certain and cheap to check.
/// Anything that might be true by the time the server actually needs it belongs
/// in `doctor`, not here, because refusing to start a server that would have
/// worked is worse than starting one that fails.
fn unavailable_reason(instance: &cellar_core::config::Instance) -> Option<String> {
    let executable = &instance.server.executable;
    if !executable.exists() {
        return Some(format!("{} does not exist", executable.display()));
    }

    if instance.server.launcher == cellar_core::Launcher::Wine
        && let Some(prefix) = &instance.server.wine_prefix
        && !prefix.exists()
    {
        return Some(format!(
            "the wine prefix {} does not exist",
            prefix.display()
        ));
    }

    None
}

/// Merge every instance's event stream into one, tagged by instance.
///
/// A task per instance rather than a select over N receivers, because the set
/// is fixed at startup and a task is the simpler thing to reason about. Each
/// one handles `Lagged` by continuing: returning would end that task and leave
/// the instance silent on the merged stream while its own channel is perfectly
/// healthy, which is the failure that looks like a dead server and is not one.
fn fan_in(
    instances: &[(cellar_core::config::InstanceId, Handle)],
) -> tokio::sync::broadcast::Sender<cellar_core::event::InstanceEvent> {
    let (merged, _) = tokio::sync::broadcast::channel(1024);

    for (id, handle) in instances {
        let (id, mut events, out) = (id.clone(), handle.subscribe(), merged.clone());
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let _ = out.send(cellar_core::event::InstanceEvent {
                            instance: id.clone(),
                            event,
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(
                            "instance '{id}' fan-in fell behind, {missed} event(s) lost"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    merged
}

/// Which session row is open for which instance.
///
/// One `Option<u64>` used to hold this, which was correct for one server and
/// silently wrong for two: on a merged stream one instance's exit would
/// `take()` the id and close the other instance's session row, leaving the
/// first open forever and the second ended by an event about a different
/// process.
#[derive(Debug, Default)]
struct SessionLedger {
    open: std::collections::HashMap<cellar_core::config::InstanceId, u64>,
}

impl SessionLedger {
    fn begin(&mut self, instance: cellar_core::config::InstanceId, session: u64) {
        self.open.insert(instance, session);
    }

    fn get(&self, instance: &cellar_core::config::InstanceId) -> Option<u64> {
        self.open.get(instance).copied()
    }

    /// Close this instance's session, and only this instance's.
    fn end(&mut self, instance: &cellar_core::config::InstanceId) -> Option<u64> {
        self.open.remove(instance)
    }
}

/// Mirror the event stream into the operations tables.
///
/// Every write here is best effort. An operations insert must never be the
/// reason a player's join is not handled, so a failure is a warning and the
/// stream carries on.
async fn record_events(
    pool: sqlx::MySqlPool,
    mut events: tokio::sync::broadcast::Receiver<cellar_core::event::InstanceEvent>,
    scopes: std::collections::HashMap<cellar_core::config::InstanceId, String>,
) {
    let mut ledger = SessionLedger::default();

    loop {
        let wrapped = match events.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!("the recorder fell behind, {missed} event(s) not stored");
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        let instance = wrapped.instance;
        let event = wrapped.event;
        let session = ledger.get(&instance);
        let Some(scope) = scopes.get(&instance) else {
            tracing::warn!("an event arrived for unknown instance '{instance}'");
            continue;
        };

        let result = match &event {
            Event::ProcessStarted { command, .. } => {
                match cellar_store::ops::begin_session(&pool, scope, hostname().as_deref(), command)
                    .await
                {
                    Ok(id) => {
                        ledger.begin(instance.clone(), id);
                        Ok(())
                    }
                    Err(why) => Err(why),
                }
            }
            Event::ServerReady { .. } => match session {
                Some(id) => cellar_store::ops::mark_ready(&pool, id).await,
                None => Ok(()),
            },
            Event::ProcessExited { code, graceful } => match ledger.end(&instance) {
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

        if event.is_notable() {
            // The logger, the account and a readable detail, not just the kind.
            // Every row used to be a kind and a timestamp because all three were
            // passed as `None`, which nothing noticed while nothing read the
            // table back.
            let record = event.record();
            let detail = record.detail.map(serde_json::Value::String);
            if let Err(why) = cellar_store::ops::record_event(
                &pool,
                session,
                event.kind(),
                record.logger,
                record.steam_id,
                detail.as_ref(),
            )
            .await
            {
                tracing::warn!("could not record an event: {why}");
            }
        }
    }
}

/// One update check. Was a loop with a `tokio::time::interval`; the scheduler
/// owns the timing now, so this returns what happened instead of only logging.
async fn check_for_updates(config: &Config, handle: &Handle) -> Result<String, String> {
    let probe = cellar_update::Probe {
        project_dir: project_dir(config),
        steam_dir: config.update.steam_dir.clone(),
        steamcmd: config.update.steamcmd.clone(),
        check_remote: config.update.check_remote,
    };

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
        cellar_update::Decision::UpToDate => Ok("up to date".to_owned()),
        cellar_update::Decision::Available { what } => {
            let what = what.join(", ");
            tracing::info!("update available: {what}");
            Ok(format!("available, not taken: {what}"))
        }
        cellar_update::Decision::Deferred { what, why } => {
            let what = what.join(", ");
            tracing::info!("update deferred ({why}): {what}");
            Ok(format!("deferred ({why}): {what}"))
        }
        cellar_update::Decision::Apply { what } => {
            let what = what.join(", ");
            tracing::warn!("taking update: {what}");

            // A snapshot first, because this is the one moment a rollback is
            // most likely to be wanted and least likely to have been planned
            // for. Not fatal if it fails: refusing the update would leave a
            // deployment stuck behind on a box with a full backup disk.
            let snapshot = snapshot_before_update(config).await;

            // Stop before updating: the engine's files are in use while it
            // runs, and Steam cannot replace a running binary.
            handle.stop().await;

            let applied = cellar_update::updater::apply(&config.update, &probe.project_dir).await;
            let mut failures = Vec::new();
            for step in &applied.steps {
                if step.ok {
                    tracing::info!("{}: {}", step.name, step.detail);
                } else {
                    tracing::error!("{} failed: {}", step.name, step.detail);
                    failures.push(step.name.clone());
                }
            }

            // Restart either way. A half-applied update still needs a running
            // server more than it needs to stay down.
            handle.restart().await;

            if failures.is_empty() {
                Ok(format!("applied {what}{snapshot}"))
            } else {
                Err(format!(
                    "applied {what}{snapshot}, but these steps failed: {}",
                    failures.join(", ")
                ))
            }
        }
    }
}

/// A dump taken immediately before an update is applied, when one is possible.
///
/// Returns a suffix for the job's own outcome line rather than a `Result`: an
/// update that could not be snapshotted is still an update that should proceed,
/// and the operator needs to know which of the two happened.
async fn snapshot_before_update(config: &Config) -> String {
    if !config.backup.before_update || !config.backup.enabled {
        return String::new();
    }
    let Some(url) = config.database.url.clone() else {
        return String::new();
    };

    let url = url.expose().to_owned();
    let mariadb = config.mariadb.clone();
    let backup = config.backup.clone();
    let taken =
        tokio::task::spawn_blocking(move || cellar_mariadb::backup(&url, &mariadb, &backup)).await;

    match taken {
        Ok(Ok(path)) => {
            tracing::info!("pre-update snapshot written to {}", path.display());
            format!(", after a snapshot to {}", path.display())
        }
        Ok(Err(why)) => {
            tracing::error!("pre-update snapshot failed, taking the update anyway: {why}");
            format!(", with no snapshot ({why})")
        }
        Err(why) => format!(", with no snapshot ({why})"),
    }
}

async fn check_for_program_updates(
    url: &str,
    status: &Arc<tokio::sync::RwLock<ProgramUpdateStatus>>,
) -> Result<String, String> {
    let result = cellar_update::selfupdate::latest_release(url).await;
    let mut state = status.write().await;
    state.checked_at = Some(chrono::Utc::now().to_rfc3339());
    state.error = None;

    match result {
        Ok(release) if cellar_update::selfupdate::is_newer(&state.current, &release.tag) => {
            let changed = state.latest.as_deref() != Some(release.tag.as_str());
            state.latest = Some(release.tag.clone());
            state.update_available = true;
            if changed {
                tracing::info!(
                    "Cellar program update available: {} (run `cellar self-update` to install)",
                    release.tag
                );
            }
            Ok(format!("{} is available", release.tag))
        }
        Ok(release) => {
            let tag = release.tag.clone();
            state.latest = Some(release.tag);
            state.update_available = false;
            Ok(format!("running the latest, {tag}"))
        }
        Err(error) => {
            state.error = Some(error.to_string());
            tracing::warn!("Cellar program update check failed: {error}");
            Err(error.to_string())
        }
    }
}

fn project_dir(config: &Config) -> std::path::PathBuf {
    config
        .primary_server()
        .as_ref()
        .and_then(|server| server.project.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME").ok()
}

/// Wait for the signal the platform sends to stop a service.
async fn wait_for_shutdown(shutdown_requested: std::sync::Arc<std::sync::atomic::AtomicBool>) {
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
            _ = wait_for_api_shutdown(shutdown_requested.clone()) => tracing::info!("API exit"),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("interrupt"),
            _ = wait_for_api_shutdown(shutdown_requested) => tracing::info!("API exit"),
        }
    }
}

async fn wait_for_api_shutdown(requested: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    while !requested.load(std::sync::atomic::Ordering::Acquire) {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use cellar_core::config::InstanceId;

    use super::*;

    fn id(name: &str) -> InstanceId {
        InstanceId::new(name).unwrap()
    }

    /// The interleaving that a single `Option<u64>` got wrong.
    ///
    /// Two servers start, then the first one exits. With one slot, that exit
    /// closed whichever session was stored last, which is the *other* server's,
    /// leaving one row open forever and ending another with an exit code from a
    /// different process. Nothing would have reported it: both writes succeed.
    #[test]
    fn one_instance_exiting_does_not_close_another_instance_session() {
        let mut ledger = SessionLedger::default();

        ledger.begin(id("dev"), 100);
        ledger.begin(id("published"), 200);

        assert_eq!(ledger.end(&id("dev")), Some(100));
        assert_eq!(
            ledger.get(&id("published")),
            Some(200),
            "published's session was closed by dev's exit"
        );
        assert_eq!(ledger.end(&id("published")), Some(200));
    }

    #[test]
    fn a_restart_replaces_only_its_own_session() {
        let mut ledger = SessionLedger::default();
        ledger.begin(id("dev"), 100);
        ledger.begin(id("published"), 200);

        // dev crashes and comes back.
        ledger.end(&id("dev"));
        ledger.begin(id("dev"), 101);

        assert_eq!(ledger.get(&id("dev")), Some(101));
        assert_eq!(ledger.get(&id("published")), Some(200));
    }

    #[test]
    fn an_exit_for_an_instance_that_never_started_closes_nothing() {
        let mut ledger = SessionLedger::default();
        ledger.begin(id("published"), 200);

        assert_eq!(ledger.end(&id("dev")), None);
        assert_eq!(ledger.get(&id("published")), Some(200));
    }

    /// A recurring job that is not in the register is a job nobody can see.
    ///
    /// This file had three `tokio::spawn`ed sleep-and-do loops, none of which
    /// reported when it last ran or whether it worked, and a fourth thing
    /// (`event_retention_days`) that was configured and had no loop at all.
    /// Nothing stops a fifth being added the old way except this test.
    #[test]
    fn no_recurring_work_is_spawned_outside_the_scheduler() {
        const SOURCE: &str = include_str!("runner.rs");

        // Only the product half of the file, so the test does not find itself.
        // `tokio::time::interval` is unambiguous: nothing constructs one except
        // to do a thing repeatedly. The two `loop`s that remain here are
        // event-stream consumers, which have no schedule and no result.
        let product = SOURCE.split("#[cfg(test)]").next().unwrap_or(SOURCE);

        let offenders: Vec<&str> = product
            .lines()
            .map(str::trim)
            .filter(|line| line.contains("tokio::time::interval("))
            .collect();

        assert!(
            offenders.is_empty(),
            "these look like unregistered recurring work: {offenders:?}"
        );

        // And the register is not empty of the ones that were moved into it.
        for name in [
            "database-backup",
            "event-retention",
            "game-update-check",
            "program-update-check",
        ] {
            assert!(
                product.contains(&format!("name: \"{name}\".to_owned()")),
                "the '{name}' job is not registered"
            );
        }
    }
}
