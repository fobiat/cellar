//! The web UI's JSON API.
//!
//! Everything here is behind [`crate::session`], because the console it exposes
//! runs at full engine privilege: `ConVarSystem.Run` from the dedicated console
//! is called with `allowProtected: true`, so a caller reaching `/api/exec`
//! reaches `quit`, `kick` and every `applejack_*` command. This is not an
//! observability endpoint with a console bolted on; it is a console.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use cellar_core::lifecycle::RestartPolicy;

use crate::registry::Target;
use crate::session::{ExternalApi, Operator};
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/access", get(access).post(change_access))
        .route("/api/logs", get(logs))
        .route("/api/configs", get(configs))
        .route("/api/configs/activate", post(activate_config))
        .route("/api/release/{action}", post(release))
        .route("/api/exec", post(exec))
        .route("/api/control/kill", post(kill))
        .route("/api/control/{action}", post(control))
        .route("/api/players", get(players))
        .route("/api/docs", get(documents))
        .route("/api/docs/{*key}", get(document).delete(delete_document))
        .route("/api/db/tables", get(db_tables))
        .route("/api/db/info", get(db_info))
        .route("/api/db/table/{table}", get(db_browse))
        .route("/api/db/query", post(db_query))
        .route("/api/instances", get(instances))
        .route("/api/activity", get(activity))
        .route("/api/diagnostics", get(diagnostics))
        .route("/api/jobs", get(jobs))
        .route("/api/jobs/{name}/run", post(run_job))
        .route("/api/db/backups", get(db_backups))
        .route("/api/db/backup", post(db_backup))
        .route("/api/db/restore", post(db_restore))
        .route("/api/settings", get(settings).post(set_setting))
        .route("/api/settings/export", get(export_settings))
        .route("/api/settings/import", post(import_settings))
        .route("/api/versions", get(versions))
        .route("/api/changelog", get(changelog))
        .route("/api/v1/status", get(external_status))
        .route("/api/v1/logs", get(external_logs))
        .route("/api/v1/resources", get(external_resources))
        .route("/api/v1/addresses", get(external_addresses))
        .route("/api/v1/versions", get(external_versions))
        .route("/api/v1/configs", get(external_configs))
        .route("/api/v1/instances", get(external_instances))
        .route("/metrics", get(metrics))
}

async fn metrics(State(state): State<Arc<AppState>>, _: ExternalApi) -> Response {
    let snapshot = match &state.supervisor {
        Some(supervisor) => supervisor.snapshot().await,
        None => None,
    };
    let bridge = state.stats();
    let database_source = database_source(&state);
    let database_up = match &state.pool {
        Some(pool) => cellar_store::admin::info(pool).await.is_ok(),
        None => false,
    };
    let mut output = String::new();
    let scope = prometheus_label(&state.scope);

    metric_header(
        &mut output,
        "cellar_server_state",
        "Current supervised server state",
        "gauge",
    );
    for state_name in [
        "stopped",
        "starting",
        "running",
        "stopping",
        "backoff",
        "crash_looping",
    ] {
        let value = snapshot
            .as_ref()
            .is_some_and(|current| current.state.as_str() == state_name);
        output.push_str(&format!(
            "cellar_server_state{{scope=\"{scope}\",state=\"{state_name}\"}} {}\n",
            u8::from(value)
        ));
    }

    gauge(
        &mut output,
        "cellar_server_players",
        "Connected players",
        snapshot.as_ref().map_or(0, |value| value.players.len()),
        &[("scope", scope.as_str())],
    );
    gauge(
        &mut output,
        "cellar_server_max_players",
        "Configured player limit",
        snapshot.as_ref().map_or(0, |value| value.max_players),
        &[("scope", scope.as_str())],
    );
    gauge(
        &mut output,
        "cellar_server_uptime_seconds",
        "Server uptime in seconds",
        snapshot
            .as_ref()
            .map_or(0, |value| value.uptime_seconds(chrono::Utc::now())),
        &[("scope", scope.as_str())],
    );
    gauge(
        &mut output,
        "cellar_server_restarts_total",
        "Unexpected or requested server restarts",
        snapshot.as_ref().map_or(0, |value| value.restarts),
        &[("scope", scope.as_str())],
    );
    gauge(
        &mut output,
        "cellar_server_unparsed_lines_total",
        "Console lines not recognized by the parser",
        snapshot.as_ref().map_or(0, |value| value.unparsed_lines),
        &[("scope", scope.as_str())],
    );

    if let Some(resources) = snapshot.as_ref().and_then(|value| value.resources) {
        gauge_float(
            &mut output,
            "cellar_process_cpu_percent",
            "Supervised process tree CPU percentage",
            resources.cpu_percent,
            &[("scope", scope.as_str())],
        );
        gauge_float(
            &mut output,
            "cellar_process_cpu_all_cores_percent",
            "Supervised process tree CPU normalized across all logical cores",
            resources.cpu_percent_all_cores,
            &[("scope", scope.as_str())],
        );
        gauge(
            &mut output,
            "cellar_cpu_core_count",
            "Logical host CPU cores used for normalization",
            resources.cpu_core_count,
            &[("scope", scope.as_str())],
        );
        gauge(
            &mut output,
            "cellar_process_memory_bytes",
            "Supervised process tree memory",
            resources.memory_bytes,
            &[("scope", scope.as_str())],
        );
        gauge(
            &mut output,
            "cellar_process_count",
            "Processes in the supervised tree",
            resources.process_count,
            &[("scope", scope.as_str())],
        );
        gauge_float(
            &mut output,
            "cellar_host_cpu_percent",
            "Host CPU percentage",
            resources.host_cpu_percent,
            &[("scope", scope.as_str())],
        );
        gauge_float(
            &mut output,
            "cellar_host_memory_percent",
            "Host memory percentage",
            resources.host_memory_percent,
            &[("scope", scope.as_str())],
        );
        gauge(
            &mut output,
            "cellar_network_receive_bytes_per_second",
            "Host network receive rate",
            resources.network_rx_bytes_per_sec,
            &[("scope", scope.as_str())],
        );
        gauge(
            &mut output,
            "cellar_network_transmit_bytes_per_second",
            "Host network transmit rate",
            resources.network_tx_bytes_per_sec,
            &[("scope", scope.as_str())],
        );
    }

    gauge(
        &mut output,
        "cellar_bridge_reads_total",
        "Bridge document reads",
        bridge.reads,
        &[("scope", scope.as_str())],
    );
    gauge(
        &mut output,
        "cellar_bridge_writes_total",
        "Bridge document writes",
        bridge.writes,
        &[("scope", scope.as_str())],
    );
    gauge(
        &mut output,
        "cellar_bridge_absent_total",
        "Bridge document misses",
        bridge.absent,
        &[("scope", scope.as_str())],
    );
    gauge(
        &mut output,
        "cellar_bridge_refused_total",
        "Bridge requests refused",
        bridge.refused,
        &[("scope", scope.as_str())],
    );
    gauge(
        &mut output,
        "cellar_bridge_healthy",
        "Bridge health",
        u8::from(bridge.healthy),
        &[("scope", scope.as_str())],
    );
    gauge(
        &mut output,
        "cellar_database_connected",
        "Database metadata query health",
        u8::from(database_up),
        &[("scope", scope.as_str())],
    );
    gauge(
        &mut output,
        "cellar_database_source",
        "Configured database source, one for the active source",
        1,
        &[("scope", scope.as_str()), ("source", database_source)],
    );

    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        output,
    )
        .into_response()
}

fn metric_header(output: &mut String, name: &str, help: &str, kind: &str) {
    output.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
}

fn gauge<T: std::fmt::Display>(
    output: &mut String,
    name: &str,
    help: &str,
    value: T,
    labels: &[(&str, &str)],
) {
    metric_header(output, name, help, "gauge");
    output.push_str(name);
    write_labels(output, labels);
    output.push_str(&format!(" {value}\n"));
}

fn gauge_float(output: &mut String, name: &str, help: &str, value: f32, labels: &[(&str, &str)]) {
    gauge(output, name, help, value, labels);
}

fn write_labels(output: &mut String, labels: &[(&str, &str)]) {
    if labels.is_empty() {
        return;
    }
    output.push('{');
    for (index, (name, value)) in labels.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str(name);
        output.push_str("=\"");
        output.push_str(value);
        output.push('"');
    }
    output.push('}');
}

fn prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

async fn external_status(
    State(state): State<Arc<AppState>>,
    _: ExternalApi,
    target: Target,
) -> Response {
    status(
        State(state),
        Operator {
            name: "api".to_owned(),
        },
        target,
    )
    .await
}

async fn external_logs(
    State(state): State<Arc<AppState>>,
    _: ExternalApi,
    target: Target,
    query: Query<LogsQuery>,
) -> Response {
    logs(
        State(state),
        Operator {
            name: "api".to_owned(),
        },
        target,
        query,
    )
    .await
}

async fn external_resources(
    _: State<Arc<AppState>>,
    _api: ExternalApi,
    target: Target,
) -> Response {
    let snapshot = match &target.handle {
        Some(supervisor) => supervisor.snapshot().await,
        None => None,
    };
    Json(serde_json::json!({
        "resources": snapshot.as_ref().and_then(|value| value.resources),
        "history": snapshot.map(|value| value.resource_history).unwrap_or_default(),
    }))
    .into_response()
}

async fn external_addresses(State(state): State<Arc<AppState>>, _: ExternalApi) -> Response {
    Json(addresses(&state).await).into_response()
}

async fn external_versions(State(state): State<Arc<AppState>>, _: ExternalApi) -> Response {
    versions(
        State(state),
        Operator {
            name: "api".to_owned(),
        },
    )
    .await
}

async fn external_configs(State(state): State<Arc<AppState>>, _: ExternalApi) -> Response {
    let Some(directory) = state.config_directory() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no config file is active");
    };
    let active = state.config_path.lock().ok().and_then(|path| path.clone());
    let running = running_shape(&state);
    let profiles = crate::config_manager::list(&directory, active.as_deref(), Some(&running))
        .await
        .into_iter()
        .map(|profile| {
            // External integrations get the useful profile identity, not the
            // host's local paths. The latter are operational details and can
            // disclose more than a remote dashboard needs.
            serde_json::json!({
                "name": profile.name,
                "mode": profile.mode,
                "active": profile.active,
                "game": profile.game,
                "map": profile.map,
                // The boolean, not the reason: a refusal names host paths and
                // listener addresses, which is exactly what this route trims.
                "switchable": profile.refusal.is_none(),
            })
        })
        .collect::<Vec<_>>();
    Json(serde_json::json!({ "profiles": profiles })).into_response()
}

/// Everything the server is currently set to.
///
/// Asked of the running server every time rather than cached: a feature toggled
/// from the in-game admin panel is exactly the change an operator most wants
/// this screen to be honest about.
/// The settings catalogue for one instance, or why it has none.
///
/// A gamemode that declares no `convar_prefix` has no catalogue Cellar can ask
/// for. Saying that is the whole improvement: before profiles, every such
/// server got `applejack_features` sent to it, which the console rejected, and
/// the tab rendered as empty rather than as unsupported.
fn catalogue_for(target: &Target) -> Option<cellar_core::convar::Catalogue<'_>> {
    target
        .descriptor
        .profile
        .convar_prefix
        .as_deref()
        .map(cellar_core::convar::Catalogue::new)
}

fn no_catalogue() -> Response {
    error(
        StatusCode::NOT_IMPLEMENTED,
        "this gamemode's profile declares no convar_prefix, so Cellar does not know what to ask \
         it for. Add one to [profile] to get the settings catalogue.",
    )
}

async fn settings(_: State<Arc<AppState>>, _operator: Operator, target: Target) -> Response {
    let Some(supervisor) = &target.handle else {
        return error(StatusCode::SERVICE_UNAVAILABLE, unavailable(&target));
    };
    let Some(catalogue) = catalogue_for(&target) else {
        return no_catalogue();
    };

    let features = match supervisor.exec(&catalogue.list_features(), "web").await {
        Ok(reply) => cellar_core::convar::parse_features(&reply),
        Err(why) => return error(StatusCode::BAD_GATEWAY, why),
    };

    let settings = match supervisor.exec(&catalogue.list_settings(), "web").await {
        Ok(reply) => cellar_core::convar::parse_settings(&reply),
        Err(why) => return error(StatusCode::BAD_GATEWAY, why),
    };

    Json(serde_json::json!({ "features": features, "settings": settings })).into_response()
}

#[derive(Deserialize)]
struct SetRequest {
    id: String,
    value: String,
    /// `feature` or `setting`. Sent by the UI, which already knows which table
    /// the row came from, rather than guessed from the id's shape here.
    kind: String,
}

/// Change one feature or setting.
async fn set_setting(
    State(state): State<Arc<AppState>>,
    operator: Operator,
    target: Target,
    Json(request): Json<SetRequest>,
) -> Response {
    let Some(supervisor) = &target.handle else {
        return error(StatusCode::SERVICE_UNAVAILABLE, unavailable(&target));
    };
    let Some(catalogue) = catalogue_for(&target) else {
        return no_catalogue();
    };

    // The id and the value both reach a console that runs at full engine
    // privilege, so neither may carry a second command.
    for field in [&request.id, &request.value] {
        if field.contains(char::is_whitespace) || field.contains(['\n', '\r', ';']) {
            return error(
                StatusCode::BAD_REQUEST,
                "an id and a value are single words",
            );
        }
    }

    let command = match request.kind.as_str() {
        "feature" => catalogue.feature_command(
            &request.id,
            matches!(request.value.as_str(), "on" | "true" | "1"),
        ),
        "setting" => catalogue.setting_command(&request.id, &request.value),
        other => return error(StatusCode::BAD_REQUEST, format!("unknown kind '{other}'")),
    };

    match supervisor.exec(&command, &operator.name).await {
        Ok(reply) => {
            if let Some(pool) = &state.pool
                && let Err(why) = cellar_store::ops::record_command(
                    pool,
                    None,
                    &operator.name,
                    &command,
                    &reply,
                    true,
                )
                .await
            {
                tracing::warn!("could not record the change: {why}");
            }
            Json(serde_json::json!({ "command": command, "reply": reply })).into_response()
        }
        Err(why) => error(StatusCode::BAD_GATEWAY, why),
    }
}

#[derive(Deserialize)]
struct ExportQuery {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    overrides: Option<bool>,
}

/// The configuration as a file, for committing or for applying elsewhere.
async fn export_settings(
    _: State<Arc<AppState>>,
    _operator: Operator,
    target: Target,
    Query(query): Query<ExportQuery>,
) -> Response {
    let Some(supervisor) = &target.handle else {
        return error(StatusCode::SERVICE_UNAVAILABLE, unavailable(&target));
    };
    let Some(catalogue) = catalogue_for(&target) else {
        return no_catalogue();
    };

    let mut snapshot = cellar_core::convar::Snapshot {
        captured_at: Some(chrono::Utc::now().to_rfc3339()),
        hostname: supervisor.snapshot().await.map(|s| s.hostname),
        features: supervisor
            .exec(&catalogue.list_features(), "web")
            .await
            .map(|reply| cellar_core::convar::parse_features(&reply))
            .unwrap_or_default(),
        settings: supervisor
            .exec(&catalogue.list_settings(), "web")
            .await
            .map(|reply| cellar_core::convar::parse_settings(&reply))
            .unwrap_or_default(),
        convars: Vec::new(),
    };

    if query.overrides.unwrap_or(false) {
        snapshot = snapshot.overrides_only();
    }

    let yaml = query.format.as_deref() == Some("yaml");
    let body = if yaml {
        snapshot.to_yaml()
    } else {
        snapshot.to_toml()
    };

    match body {
        Ok(text) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            text,
        )
            .into_response(),
        Err(why) => error(StatusCode::INTERNAL_SERVER_ERROR, why),
    }
}

#[derive(Deserialize)]
struct ImportSettingsRequest {
    contents: String,
    #[serde(default)]
    apply: bool,
}

/// Preview or apply a TOML/YAML settings snapshot without writing it into the
/// gamemode checkout. Applying still goes through the live console catalogue.
async fn import_settings(
    _: State<Arc<AppState>>,
    operator: Operator,
    target: Target,
    Json(request): Json<ImportSettingsRequest>,
) -> Response {
    if request.contents.len() > 512 * 1024 {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "settings import is limited to 512 KiB",
        );
    }
    let desired = match cellar_core::convar::Snapshot::parse(&request.contents) {
        Ok(snapshot) => snapshot,
        Err(why) => return error(StatusCode::BAD_REQUEST, why),
    };
    let Some(supervisor) = &target.handle else {
        return error(StatusCode::SERVICE_UNAVAILABLE, unavailable(&target));
    };
    let Some(catalogue) = catalogue_for(&target) else {
        return no_catalogue();
    };

    let current = cellar_core::convar::Snapshot {
        captured_at: None,
        hostname: supervisor
            .snapshot()
            .await
            .map(|snapshot| snapshot.hostname),
        features: supervisor
            .exec(&catalogue.list_features(), "web")
            .await
            .map(|reply| cellar_core::convar::parse_features(&reply))
            .unwrap_or_default(),
        settings: supervisor
            .exec(&catalogue.list_settings(), "web")
            .await
            .map(|reply| cellar_core::convar::parse_settings(&reply))
            .unwrap_or_default(),
        convars: Vec::new(),
    };
    let changes = cellar_core::convar::plan(&catalogue, &current, &desired);
    if !request.apply {
        return Json(serde_json::json!({
            "applied": [],
            "failed": [],
            "changes": changes,
        }))
        .into_response();
    }

    let mut applied = Vec::new();
    let mut failed = Vec::new();
    for change in &changes {
        if let Some(reason) = &change.refused {
            failed.push(serde_json::json!({
                "id": change.id,
                "reason": reason,
            }));
            continue;
        }
        match supervisor.exec(&change.command, &operator.name).await {
            Ok(reply) => {
                let refused = reply.iter().any(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower.contains("refus")
                        || lower.contains("not a valid")
                        || lower.contains("unknown")
                });
                if refused {
                    failed.push(serde_json::json!({
                        "id": change.id,
                        "reply": reply,
                    }));
                } else {
                    applied.push(serde_json::json!({
                        "id": change.id,
                        "command": change.command,
                        "reply": reply,
                    }));
                }
            }
            Err(why) => failed.push(serde_json::json!({
                "id": change.id,
                "reason": why,
            })),
        }
    }

    Json(serde_json::json!({
        "applied": applied,
        "failed": failed,
        "changes": changes,
    }))
    .into_response()
}

/// Installed and available versions, plus what the updater would do about them.
///
/// Probed on request rather than cached: it runs a `git ls-remote` at most, and
/// an operator who opened this tab is asking about *now*.
async fn versions(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let Some(probe) = &state.version_probe else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "version checking is not configured",
        );
    };

    let versions = cellar_update::version::probe(probe).await;

    let players = match &state.supervisor {
        Some(supervisor) => supervisor
            .snapshot()
            .await
            .map(|s| s.players.len())
            .unwrap_or(0),
        None => 0,
    };

    let hour = chrono::Local::now()
        .format("%H")
        .to_string()
        .parse()
        .unwrap_or(0u8);
    let decision = cellar_update::updater::decide(&state.update_config, &versions, players, hour);
    let program_update = state.program_update.read().await.clone();

    Json(serde_json::json!({
        "versions": versions,
        "build_drift": versions.build_drift(),
        "decision": decision,
        "policy": state.update_config.policy,
        "program_update": program_update,
    }))
    .into_response()
}

/// The gamemode's changelog, newest first.
async fn changelog(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let Some(probe) = &state.version_probe else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no project directory is configured",
        );
    };

    Json(cellar_update::read_changelog(&probe.project_dir, 5)).into_response()
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": message.into() }))).into_response()
}

/// Everything the dashboard header needs, in one call.
async fn status(State(state): State<Arc<AppState>>, _: Operator, target: Target) -> Response {
    let snapshot = match &target.handle {
        Some(supervisor) => supervisor.snapshot().await,
        None => None,
    };

    let bridge = state.stats();
    let database = match &state.pool {
        Some(pool) => cellar_store::admin::info(pool).await.is_ok(),
        None => false,
    };
    let mariadb = match &state.mariadb {
        Some(handle) => handle.snapshot().await,
        None => None,
    };
    let active_config = state.config_path.lock().ok().and_then(|path| {
        path.as_ref()
            .and_then(|path| crate::config_manager::load(path).ok())
    });
    let configured_game = target.descriptor.game.clone();
    let configured_map = target.descriptor.map.clone();
    let map_log = match (
        target.descriptor.log_file.as_deref(),
        configured_map.as_deref(),
    ) {
        (Some(path), Some(map)) => tokio::fs::read_to_string(path)
            .await
            .map(|log| log.contains(map) && !log.contains("failed to load map"))
            .unwrap_or(false),
        (None, Some(_)) => false,
        (_, None) => true,
    };
    let spawn_validation = state
        .version_probe
        .as_ref()
        .map(|root| {
            root.project_dir
                .join("Code/Characters/CharacterDirector.cs")
        })
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|source| {
            source.contains("GroundedOrAuthored") && source.contains("Scene.Trace")
        });

    let addresses = addresses(&state).await;
    let anti_cheat = crate::security::inspect(target.descriptor.log_file.as_deref()).await;
    let invite_only = read_access_files(target.descriptor.data_dir.as_deref())
        .await
        .ok()
        .map(|(features, _)| feature_enabled(&features, "admin.inviteonly"));
    let mode = if target.descriptor.game.is_some() {
        "published"
    } else {
        "development"
    };
    let restart_policy = active_config
        .as_ref()
        .map(|config| match config.supervisor.restart {
            RestartPolicy::Never => "never",
            RestartPolicy::Always => "always",
            RestartPolicy::OnFailure => "on_failure",
        })
        .unwrap_or("unknown");

    Json(serde_json::json!({
        "server": snapshot,
        "bridge": bridge,
        "database": database,
        "database_source": database_source(&state),
        "mariadb": mariadb,
        "health": {
            "map": map_log,
            "spawn_validation": spawn_validation,
            "console": target.descriptor.log_file.as_deref().is_some_and(std::path::Path::exists),
        },
        "cellar": {
            "version": env!("CARGO_PKG_VERSION"),
            "commit": option_env!("CELLAR_BUILD_COMMIT").unwrap_or("unknown"),
        },
        "game": configured_game,
        "mode": mode,
        "instance": target.id.to_string(),
        "scope": target.scope,
        "supervisor": {
            "restart_policy": restart_policy,
            "auto_restart_on_crash": matches!(restart_policy, "always" | "on_failure"),
        },
        "addresses": addresses,
        "access": { "invite_only": invite_only },
        "anti_cheat": anti_cheat,
        "web_auth": {
            "bind": state.web_bind,
            "mode": state.web_auth,
            "password_configured": state.web_password_hash.is_some(),
        },
    }))
    .into_response()
}

async fn addresses(state: &AppState) -> Vec<serde_json::Value> {
    let tailscale_ip = tailscale_ip().await;
    let mut result = Vec::new();
    if let Some(ip) = &tailscale_ip {
        result.push(serde_json::json!({
            "label": "Tailscale IPv4",
            "bind": ip,
            "local_url": null,
            "remote_url": null,
            "state": "available",
        }));
    }
    if state.web_enabled {
        result.push(address("Cellar web", &state.web_bind, &tailscale_ip, true));
    }
    if state.bridge_enabled()
        && let Some(bind) = state.bridge_bind()
    {
        result.push(address("Document bridge", bind, &tailscale_ip, false));
    }
    let server_bind = format!("0.0.0.0:{}", state.server_port().unwrap_or_default());
    result.push(address(
        "Game server",
        &server_bind,
        &tailscale_ip,
        state.server_direct_connect(),
    ));
    let query_bind = format!("0.0.0.0:{}", state.query_port().unwrap_or_default());
    result.push(address(
        "Game query",
        &query_bind,
        &tailscale_ip,
        state.server_direct_connect(),
    ));
    result
}

fn address(
    label: &str,
    bind: &str,
    tailscale_ip: &Option<String>,
    remote: bool,
) -> serde_json::Value {
    let exposed = remote
        && !bind.starts_with("127.")
        && !bind.starts_with("localhost:")
        && !bind.starts_with("[::1]");
    let local_host = if bind.starts_with("0.0.0.0:") {
        bind.replacen("0.0.0.0", "127.0.0.1", 1)
    } else {
        bind.to_owned()
    };
    let port = bind.rsplit_once(':').map(|(_, port)| port).unwrap_or("");
    let local_url = if label == "Game server" || label == "Game query" {
        local_host.to_string()
    } else {
        format!("http://{local_host}")
    };
    let remote_url = exposed
        .then(|| {
            tailscale_ip.as_ref().map(|ip| {
                if label == "Game server" || label == "Game query" {
                    format!("{ip}:{port}")
                } else {
                    format!("http://{ip}:{port}")
                }
            })
        })
        .flatten();
    let state = if exposed && tailscale_ip.is_some() {
        "tailnet-ready"
    } else {
        "local-only"
    };
    serde_json::json!({
        "label": label,
        "bind": bind,
        "local_url": local_url,
        "remote_url": remote_url,
        "state": state,
    })
}

async fn tailscale_ip() -> Option<String> {
    let mut commands = vec!["tailscale".to_owned()];
    if cfg!(windows) {
        commands.extend([
            r"C:\Program Files\Tailscale\tailscale.exe".to_owned(),
            r"C:\Program Files (x86)\Tailscale\tailscale.exe".to_owned(),
        ]);
    }

    for command in commands {
        let output = tokio::time::timeout(
            std::time::Duration::from_millis(700),
            tokio::process::Command::new(command)
                .args(["ip", "-4"])
                .output(),
        )
        .await
        .ok()
        .and_then(Result::ok);
        let Some(output) = output else { continue };
        if output.status.success()
            && let Some(ip) = String::from_utf8(output.stdout).ok().and_then(|value| {
                value
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(str::to_owned)
            })
        {
            return Some(ip);
        }
    }
    None
}

/// The AppleJack invite gate and its SteamID64 allowlist.
async fn access(_: State<Arc<AppState>>, _operator: Operator, target: Target) -> Response {
    let (features, permissions) =
        match read_access_files(target.descriptor.data_dir.as_deref()).await {
            Ok(files) => files,
            Err(why) => return error(StatusCode::BAD_GATEWAY, why),
        };

    Json(serde_json::json!({
        "invite_only": feature_enabled(&features, "admin.inviteonly"),
        "allowlist": allowed_ids(&permissions),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct LogsQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    level: Option<cellar_core::event::Level>,
    #[serde(default)]
    level_min: Option<cellar_core::event::Level>,
    #[serde(default)]
    category: Option<String>,
    /// RFC 3339. Only lines after this, for a browser filling a stream gap.
    #[serde(default)]
    since: Option<chrono::DateTime<chrono::Utc>>,
}

/// Scan current and rotated engine logs. The files are the persistent source,
/// so search remains useful after Cellar or the browser restarts.
async fn logs(
    _: State<Arc<AppState>>,
    _operator: Operator,
    target: Target,
    Query(query): Query<LogsQuery>,
) -> Response {
    let Some(path) = target.descriptor.log_file.as_deref() else {
        return Json(serde_json::json!({
            "lines": [], "matched": 0, "scanned_files": 0, "scanned_lines": 0,
            "persistent": false
        }))
        .into_response();
    };
    let result = crate::logs::search(
        path,
        &target.descriptor.profile,
        &crate::logs::Query {
            text: query.q.filter(|value| !value.trim().is_empty()),
            tag: query.tag.filter(|value| !value.trim().is_empty()),
            level: query.level,
            level_min: query.level_min,
            category: query.category.filter(|value| !value.trim().is_empty()),
            since: query.since,
            limit: query.limit.unwrap_or(250).clamp(1, 5000),
        },
    )
    .await;
    Json(result).into_response()
}

async fn configs(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let Some(directory) = state.config_directory() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no config file is active");
    };
    let active = state.config_path.lock().ok().and_then(|path| path.clone());
    let running = running_shape(&state);
    Json(serde_json::json!({
        "profiles":
            crate::config_manager::list(&directory, active.as_deref(), Some(&running)).await
    }))
    .into_response()
}

/// What the running process is, for the switchability rules.
fn running_shape(state: &AppState) -> crate::config_manager::Running<'_> {
    crate::config_manager::Running {
        web_bind: &state.web_bind,
        web_enabled: state.web_enabled,
        bridge_bind: state.bridge_bind(),
        bridge_enabled: state.bridge_enabled(),
        log_file: state.log_file(),
        instances: state.instances.len(),
        supervised: state.supervisor.is_some(),
    }
}

#[derive(Deserialize)]
struct ActivateConfigRequest {
    name: String,
}

async fn activate_config(
    State(state): State<Arc<AppState>>,
    _: Operator,
    Json(request): Json<ActivateConfigRequest>,
) -> Response {
    let Some(directory) = state.config_directory() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no config file is active");
    };
    let path = match crate::config_manager::resolve(&directory, &request.name) {
        Ok(path) => path,
        Err(why) => return error(StatusCode::BAD_REQUEST, why),
    };
    let config = match crate::config_manager::load(&path) {
        Ok(config) => config,
        Err(why) => return error(StatusCode::BAD_REQUEST, why),
    };
    // The same function `/api/configs` used to disable the button, so the
    // inline reason and the refusal cannot disagree. They were separate rules
    // before, which is how a switch could look available and still fail.
    if let Some(why) = crate::config_manager::switch_refusal(&config, &running_shape(&state)) {
        return error(StatusCode::BAD_REQUEST, why);
    }

    let Some(candidate) = config.primary() else {
        return error(StatusCode::BAD_REQUEST, "that profile declares no server");
    };
    let Some(supervisor) = &state.supervisor else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no server is being supervised",
        );
    };
    if let Err(why) = supervisor.switch_config(candidate).await {
        return error(StatusCode::BAD_GATEWAY, why);
    }
    if let Ok(mut active) = state.config_path.lock() {
        *active = Some(path);
    }
    Json(serde_json::json!({ "active": request.name, "restarting": true })).into_response()
}

/// Run a configured build or publish command for the game checkout.
async fn release(
    State(state): State<Arc<AppState>>,
    operator: Operator,
    Path(action): Path<String>,
) -> Response {
    let Some(probe) = &state.version_probe else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no project directory is configured",
        );
    };
    if !matches!(action.as_str(), "build" | "publish") {
        return error(StatusCode::BAD_REQUEST, "action must be build or publish");
    }

    tracing::warn!(
        "{} started the configured release {action} pipeline",
        operator.name
    );
    let result =
        cellar_update::pipeline::run(&state.release_config, &action, &probe.project_dir).await;
    let code = if result.ok {
        StatusCode::OK
    } else {
        StatusCode::BAD_GATEWAY
    };
    (code, Json(result)).into_response()
}

#[derive(Deserialize)]
struct AccessRequest {
    action: String,
    steam_id: Option<String>,
    enabled: Option<bool>,
}

/// Change one invite gate setting or one allowlist entry.
async fn change_access(
    State(state): State<Arc<AppState>>,
    operator: Operator,
    target: Target,
    Json(request): Json<AccessRequest>,
) -> Response {
    let root = target.descriptor.data_dir.clone();
    let result = match request.action.as_str() {
        "allow" | "revoke" => {
            let Some(steam_id) = request.steam_id.as_deref() else {
                return error(StatusCode::BAD_REQUEST, "steam_id is required");
            };
            if !valid_steam_id(steam_id) {
                return error(StatusCode::BAD_REQUEST, "steam_id must be a SteamID64");
            }
            edit_allowlist(
                root.as_deref(),
                steam_id,
                request.action == "allow",
                &operator.name,
            )
            .await
        }
        "gate" => {
            let Some(enabled) = request.enabled else {
                return error(StatusCode::BAD_REQUEST, "enabled is required");
            };
            edit_gate(root.as_deref(), enabled, &operator.name).await
        }
        other => Err(format!("unknown access action '{other}'")),
    };

    match result {
        Ok(()) => access(State(state), operator, target).await,
        Err(why) => error(StatusCode::BAD_GATEWAY, why),
    }
}

async fn edit_allowlist(
    root: Option<&std::path::Path>,
    steam_id: &str,
    allow: bool,
    operator: &str,
) -> Result<(), String> {
    let (_, mut permissions) = read_access_files(root).await?;
    let grants = permissions
        .as_object_mut()
        .ok_or_else(|| "permissions.json is not an object".to_owned())?
        .entry("Grants")
        .or_insert_with(|| serde_json::json!({}));
    let grants = grants
        .as_object_mut()
        .ok_or_else(|| "permissions.json Grants is not an object".to_owned())?;

    if allow {
        let grant = grants
            .entry(steam_id.to_owned())
            .or_insert_with(|| serde_json::json!({ "Bundles": [], "Permissions": [] }));
        let grant = grant
            .as_object_mut()
            .ok_or_else(|| "the selected grant is not an object".to_owned())?;
        let permissions = grant
            .entry("Permissions")
            .or_insert_with(|| serde_json::json!([]));
        let permissions = permissions
            .as_array_mut()
            .ok_or_else(|| "the selected grant Permissions is not an array".to_owned())?;
        if !permissions
            .iter()
            .any(|value| value.as_str() == Some("admin.connect"))
        {
            permissions.push(serde_json::Value::String("admin.connect".to_owned()));
        }
    } else if let Some(grant) = grants.get_mut(steam_id) {
        let grant = grant
            .as_object_mut()
            .ok_or_else(|| "the selected grant is not an object".to_owned())?;
        if let Some(values) = grant
            .get_mut("Permissions")
            .and_then(|value| value.as_array_mut())
        {
            values.retain(|value| value.as_str() != Some("admin.connect"));
        }
        let empty_permissions = grant
            .get("Permissions")
            .and_then(|value| value.as_array())
            .is_none_or(Vec::is_empty);
        let empty_bundles = grant
            .get("Bundles")
            .and_then(|value| value.as_array())
            .is_none_or(Vec::is_empty);
        if empty_permissions && empty_bundles {
            grants.remove(steam_id);
        }
    }

    write_access_file(root, "permissions.json", &permissions, operator).await
}

async fn edit_gate(
    root: Option<&std::path::Path>,
    enabled: bool,
    operator: &str,
) -> Result<(), String> {
    let (mut features, _) = read_access_files(root).await?;
    let object = features
        .as_object_mut()
        .ok_or_else(|| "features.json is not an object".to_owned())?;
    let values = object
        .entry("Enabled")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| "features.json Enabled is not an array".to_owned())?;
    if enabled {
        if !values
            .iter()
            .any(|value| value.as_str() == Some("admin.inviteonly"))
        {
            values.push(serde_json::Value::String("admin.inviteonly".to_owned()));
        }
    } else {
        values.retain(|value| value.as_str() != Some("admin.inviteonly"));
    }

    write_access_file(root, "features.json", &features, operator).await
}

/// `features.json` and `permissions.json` for one instance.
///
/// Takes the directory rather than reading the primary's. Two instances of one
/// gamemode have different data directories by force: the engine appends
/// `#local` to a local project's ident, so the development and published sides
/// cannot share these files however much they look like they should. Reading
/// the primary's for both is a panel that silently describes the wrong server.
async fn read_access_files(
    root: Option<&std::path::Path>,
) -> Result<(serde_json::Value, serde_json::Value), String> {
    let Some(root) = root else {
        return Ok((serde_json::json!({}), serde_json::json!({})));
    };
    let features = read_json_file(
        &root.join("features.json"),
        serde_json::json!({
            "Version": 1,
            "Enabled": []
        }),
    )
    .await?;
    let permissions = read_json_file(
        &root.join("permissions.json"),
        serde_json::json!({
            "Version": 1,
            "Grants": {}
        }),
    )
    .await?;
    Ok((features, permissions))
}

async fn read_json_file(
    path: &std::path::Path,
    fallback: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => {
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(fallback),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

async fn write_access_file(
    root: Option<&std::path::Path>,
    name: &str,
    value: &serde_json::Value,
    operator: &str,
) -> Result<(), String> {
    let Some(root) = root else {
        return Err("the configured server has no game data directory".to_owned());
    };
    let path = root.join(name);
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    tokio::fs::write(&path, format!("{text}\n"))
        .await
        .map_err(|error| format!("{}: {error}", path.display()))?;
    tracing::info!("{operator} updated {}", path.display());
    Ok(())
}

fn valid_steam_id(value: &str) -> bool {
    value.len() == 17
        && value.starts_with("7656119")
        && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn feature_enabled(features: &serde_json::Value, feature: &str) -> bool {
    features
        .get("Enabled")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(feature)))
}

fn allowed_ids(permissions: &serde_json::Value) -> Vec<String> {
    let mut ids = permissions
        .get("Grants")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(|grants| grants.iter())
        .filter_map(|(steam_id, grant)| {
            let has_permission = grant
                .get("Permissions")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|values| {
                    values
                        .iter()
                        .any(|value| value.as_str() == Some("admin.connect"))
                });
            has_permission.then(|| steam_id.clone())
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

#[derive(Deserialize)]
struct ExecRequest {
    command: String,
}

#[derive(Serialize)]
struct ExecResponse {
    command: String,
    reply: Vec<String>,
}

/// Type a command into the server console and return what followed.
async fn exec(
    State(state): State<Arc<AppState>>,
    operator: Operator,
    target: Target,
    Json(request): Json<ExecRequest>,
) -> Response {
    let Some(supervisor) = &target.handle else {
        return error(StatusCode::SERVICE_UNAVAILABLE, unavailable(&target));
    };

    let command = request.command.trim().to_owned();
    if command.is_empty() {
        return error(StatusCode::BAD_REQUEST, "empty command");
    }

    // A newline would let one box submit several commands, which makes the audit
    // row a lie about what was run.
    if command.contains('\n') || command.contains('\r') {
        return error(StatusCode::BAD_REQUEST, "one command at a time");
    }

    match supervisor.exec(&command, &operator.name).await {
        Ok(reply) => {
            if let Some(pool) = &state.pool {
                // Best effort: an audit insert must not fail the command that
                // already ran. It is recorded as a warning instead.
                if let Err(why) = cellar_store::ops::record_command(
                    pool,
                    None,
                    &operator.name,
                    &command,
                    &reply,
                    true,
                )
                .await
                {
                    tracing::warn!("could not record the console command: {why}");
                }
            }
            Json(ExecResponse { command, reply }).into_response()
        }
        Err(why) => error(StatusCode::BAD_GATEWAY, why),
    }
}

/// Kill Cellar and its complete process tree without requiring an instance.
async fn kill(State(_state): State<Arc<AppState>>, operator: Operator) -> Response {
    tracing::warn!(
        "{} requested an emergency kill of Cellar and its process tree",
        operator.name
    );
    cellar_runtime::process::emergency_kill_current_process_tree();
    Json(serde_json::json!({ "ok": true, "action": "kill" })).into_response()
}

/// Start, stop or restart one server.
async fn control(
    State(state): State<Arc<AppState>>,
    operator: Operator,
    target: Target,
    Path(action): Path<String>,
) -> Response {
    // `exit` is process-wide and ignores the target deliberately. Exiting with
    // another instance still running gets that one SIGKILLed by whatever
    // supervises Cellar, which is exactly the shutdown the engine has no
    // handler for.
    if action == "exit" {
        tracing::info!(
            "{} asked Cellar to exit after stopping every instance",
            operator.name
        );
        for entry in state.instances.iter() {
            if let Some(supervisor) = &entry.handle {
                supervisor.stop().await;
            }
        }
        state
            .shutdown_requested
            .store(true, std::sync::atomic::Ordering::Release);
        return Json(serde_json::json!({ "ok": true, "action": "exit" })).into_response();
    }

    let Some(supervisor) = &target.handle else {
        return error(StatusCode::SERVICE_UNAVAILABLE, unavailable(&target));
    };

    match action.as_str() {
        "stop" => {
            tracing::info!("{} asked for a graceful stop", operator.name);
            supervisor.stop().await;
            Json(serde_json::json!({ "ok": true, "action": "stop" })).into_response()
        }
        "restart" => {
            tracing::info!("{} asked for a restart", operator.name);
            supervisor.restart().await;
            Json(serde_json::json!({ "ok": true, "action": "restart" })).into_response()
        }
        other => error(StatusCode::BAD_REQUEST, format!("unknown action '{other}'")),
    }
}

/// Every account the operations tables have ever seen.
async fn players(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let Some(pool) = &state.pool else {
        return Json(serde_json::json!([])).into_response();
    };

    match cellar_store::ops::players(pool, 200).await {
        Ok(players) => Json(players).into_response(),
        Err(why) => error(StatusCode::BAD_GATEWAY, why.to_string()),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    prefix: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u64>,
}

/// The bridge's documents, for the Records tab.
async fn documents(
    State(state): State<Arc<AppState>>,
    _: Operator,
    target: Target,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(pool) = &state.pool else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no database is configured");
    };

    match cellar_store::document::list(
        pool,
        &target.scope,
        query.prefix.as_deref(),
        query.limit.unwrap_or(200),
    )
    .await
    {
        Ok(documents) => Json(documents).into_response(),
        Err(why) => error(StatusCode::BAD_GATEWAY, why.to_string()),
    }
}

/// One document and its revision history.
async fn document(
    State(state): State<Arc<AppState>>,
    _: Operator,
    target: Target,
    Path(key): Path<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no database is configured");
    };

    if let Err(refusal) = cellar_core::doc_key::check(&key) {
        return error(StatusCode::BAD_REQUEST, refusal.to_string());
    }

    let document = match cellar_store::document::get(pool, &target.scope, &key).await {
        Ok(Some(document)) => document,
        Ok(None) => return error(StatusCode::NOT_FOUND, "no such document"),
        Err(why) => return error(StatusCode::BAD_GATEWAY, why.to_string()),
    };

    let history = cellar_store::document::revisions(pool, &target.scope, &key, 25)
        .await
        .unwrap_or_default();

    Json(serde_json::json!({ "document": document, "revisions": history })).into_response()
}

/// Remove a document. The bridge's own interface has no delete; this is the
/// operator's, and it is audited by the revision history it leaves behind.
async fn delete_document(
    State(state): State<Arc<AppState>>,
    operator: Operator,
    target: Target,
    Path(key): Path<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no database is configured");
    };

    if let Err(refusal) = cellar_core::doc_key::check(&key) {
        return error(StatusCode::BAD_REQUEST, refusal.to_string());
    }

    match cellar_store::document::delete(pool, &target.scope, &key).await {
        Ok(true) => {
            tracing::warn!(
                "{} deleted the document {key} in scope {}",
                operator.name,
                target.scope
            );
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Ok(false) => error(StatusCode::NOT_FOUND, "no such document"),
        Err(why) => error(StatusCode::BAD_GATEWAY, why.to_string()),
    }
}

async fn db_tables(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let Some(pool) = &state.pool else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no database is configured");
    };

    match cellar_store::admin::tables(pool).await {
        Ok(tables) => Json(tables).into_response(),
        Err(why) => error(StatusCode::BAD_GATEWAY, why.to_string()),
    }
}

/// Connection and ownership facts for the database panel.
async fn db_info(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let Some(pool) = &state.pool else {
        return Json(serde_json::json!({
            "connected": false,
            "schema_owner": state.database_schema_owner,
            "source": "disabled",
        }))
        .into_response();
    };

    match cellar_store::admin::info(pool).await {
        Ok(info) => Json(serde_json::json!({
            "connected": info.connected,
            "database": info.database,
            "server_version": info.server_version,
            "table_count": info.table_count,
            "bytes": info.bytes,
            "schema_owner": state.database_schema_owner,
            "source": database_source(&state),
        }))
        .into_response(),
        Err(why) => error(StatusCode::BAD_GATEWAY, why.to_string()),
    }
}

/// Write an operator action to the audit table, warning rather than failing.
///
/// The action already happened; refusing to answer because the record could not
/// be written would be a worse outcome than an unrecorded one.
async fn record_action(state: &AppState, operator: &Operator, command: &str, detail: &str) {
    let Some(pool) = &state.pool else { return };
    if let Err(why) = cellar_store::ops::record_command(
        pool,
        None,
        &operator.name,
        command,
        &[detail.to_owned()],
        true,
    )
    .await
    {
        tracing::warn!("could not record '{command}': {why}");
    }
}

/// Where dumps live, matching what `backup::create` decides.
fn backup_directory(state: &AppState) -> Option<std::path::PathBuf> {
    state.backup_config.directory.clone().or_else(|| {
        state
            .mariadb_config
            .data_dir
            .as_ref()
            .map(|path| path.join("backups"))
    })
}

/// The dumps, and the policy that produced them.
///
/// The listing alone cannot answer the two questions somebody opening this
/// asks: is anything taking these, and is the newest one real. The schedule
/// comes from the config, and `verify` is the same read-back `backup::create`
/// does before it counts a dump, which is what separates a file from a backup.
async fn db_backups(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let backup = &state.backup_config;
    let Some(directory) = backup_directory(&state) else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "backup.directory is unset and there is no mariadb.data_dir to derive one from",
        );
    };

    let dumps = cellar_mariadb::backup::list(&directory).unwrap_or_default();
    let free = cellar_runtime::metrics::disk_free(&directory);
    Json(serde_json::json!({
        "directory": directory,
        "database_configured": state.database_url.is_some(),
        "policy": {
            "enabled": backup.enabled,
            "interval_hours": backup.interval_hours,
            "retain": backup.retain,
            "verify": backup.verify,
            "copy_to": backup.copy_to,
            "before_update": backup.before_update,
        },
        "free_bytes": free.as_ref().map(|(bytes, _)| *bytes),
        "mount": free.as_ref().map(|(_, mount)| mount.clone()),
        "dumps": dumps.iter().map(|dump| {
            let verified = cellar_mariadb::backup::verify(&dump.path);
            serde_json::json!({
                "path": dump.path,
                "name": dump.path.file_name().map(|n| n.to_string_lossy()),
                "bytes": dump.bytes,
                "modified": chrono::DateTime::<chrono::Utc>::from(dump.modified),
                "verified": verified.is_ok(),
                "why": verified.err().map(|why| why.to_string()),
            })
        }).collect::<Vec<_>>(),
    }))
    .into_response()
}

async fn db_backup(State(state): State<Arc<AppState>>, operator: Operator) -> Response {
    let Some(url) = &state.database_url else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no database is configured");
    };

    match cellar_mariadb::backup(url.expose(), &state.mariadb_config, &state.backup_config) {
        Ok(path) => {
            record_action(&state, &operator, "db backup", &path.display().to_string()).await;
            Json(serde_json::json!({ "path": path })).into_response()
        }
        Err(why) => error(StatusCode::BAD_GATEWAY, why.to_string()),
    }
}

#[derive(Deserialize)]
struct RestoreRequest {
    /// A dump name from `/api/db/backups`, not a path.
    name: String,
}

/// Replace every table the named dump carries.
///
/// The supervised server is stopped first, and the reply says so. The gamemode
/// writes through the bridge continuously and a write landing mid-restore lands
/// in a table that is about to be dropped, so this is not an optional courtesy.
/// The server is not started again: whoever restored a database should look at
/// it before players reach it.
async fn db_restore(
    State(state): State<Arc<AppState>>,
    operator: Operator,
    Json(request): Json<RestoreRequest>,
) -> Response {
    let Some(url) = &state.database_url else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no database is configured");
    };
    let Some(directory) = backup_directory(&state) else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "backup.directory is unset and there is no mariadb.data_dir to derive one from",
        );
    };

    // A name from the listing, resolved inside the directory. Taking a path
    // would let an operator session read any file on the host into the database
    // as SQL, and the listing is the only set that makes sense anyway.
    let Some(dump) = cellar_mariadb::backup::list(&directory)
        .unwrap_or_default()
        .into_iter()
        .find(|dump| {
            dump.path
                .file_name()
                .is_some_and(|name| name == request.name.as_str())
        })
    else {
        return error(
            StatusCode::NOT_FOUND,
            format!(
                "no dump named '{}' in {}",
                request.name,
                directory.display()
            ),
        );
    };

    let stopped = match &state.supervisor {
        Some(supervisor) => {
            supervisor.stop().await;
            true
        }
        None => false,
    };

    match cellar_mariadb::restore(&dump.path, url.expose(), &state.mariadb_config) {
        Ok(restored) => {
            record_action(
                &state,
                &operator,
                "db restore",
                &format!("{} into {}", restored.from.display(), restored.database),
            )
            .await;
            Json(serde_json::json!({
                "restored": restored.from,
                "database": restored.database,
                "bytes": restored.bytes,
                "server_stopped": stopped,
                "detail": if stopped {
                    "The server was stopped before the restore and has not been started again."
                } else {
                    "No server was running."
                },
            }))
            .into_response()
        }
        Err(why) => error(StatusCode::BAD_GATEWAY, why.to_string()),
    }
}

/// Why an instance cannot be talked to, in the words the config gave.
fn unavailable(target: &Target) -> String {
    target
        .unavailable
        .clone()
        .unwrap_or_else(|| format!("instance '{}' is not being supervised", target.id))
}

/// Every instance this process knows about, running or not.
#[derive(Deserialize)]
struct ActivityQuery {
    #[serde(default)]
    q: Option<String>,
    /// `operator`, `server`, or both when absent.
    #[serde(default)]
    source: Option<String>,
    /// How far back. Absent or 0 is everything the retention policy kept.
    #[serde(default)]
    days: Option<u32>,
    #[serde(default)]
    limit: Option<u32>,
    /// Absent lists every instance's activity, which is the useful default on
    /// a screen whose question is usually "what happened", not "what happened
    /// to this one".
    #[serde(default)]
    instance: Option<String>,
}

/// What has happened: the console audit and the server's own observations, in
/// one timeline.
///
/// No new writes. `record_command` has audited every console command since the
/// console existed and `record_event` has recorded every lifecycle event, and
/// until now nothing read either of them back. The console runs at full engine
/// privilege, so `srv_command` is the only record of who used it, and it was
/// write-only.
async fn activity(
    State(state): State<Arc<AppState>>,
    _: Operator,
    Query(query): Query<ActivityQuery>,
) -> Response {
    let Some(pool) = &state.pool else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no database is configured, so nothing has been recorded to show",
        );
    };

    // Resolved from the registry rather than taken as a scope directly: a
    // caller may not name an arbitrary scope, only an instance this process
    // declares, so the filter cannot be used to read another deployment's rows
    // out of a shared database.
    let scope = match &query.instance {
        Some(id) => match state.instances.get(id) {
            Some(entry) => Some(entry.scope.clone()),
            None => {
                return error(
                    StatusCode::NOT_FOUND,
                    format!(
                        "no instance '{id}'. This config declares: {}",
                        state.instances.ids().join(", ")
                    ),
                );
            }
        },
        None => None,
    };

    let request = cellar_store::ops::ActivityQuery {
        scope,
        source: query
            .source
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_owned()),
        text: query.q.filter(|value| !value.trim().is_empty()),
        days: query.days,
        limit: query.limit.unwrap_or(200).clamp(1, 2000),
    };

    match cellar_store::ops::activity(pool, &request).await {
        Ok(entries) => Json(serde_json::json!({ "entries": entries })).into_response(),
        Err(why) => error(StatusCode::BAD_GATEWAY, why.to_string()),
    }
}

/// Every preflight check `cellar doctor` runs, plus the ones only a live
/// process can answer.
///
/// The checks are not reimplemented here. They live in `cellar-diagnostics`,
/// which the CLI calls too, because a second copy of a check is a second copy
/// that drifts. What this route adds is the half doctor cannot see: what the
/// supervisors are actually doing, and how many lines the grammar has refused.
async fn diagnostics(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let path = state.config_path.lock().ok().and_then(|held| held.clone());

    // The binds this very process holds. Without them the screen would report
    // its own listener as a conflict, every time, forever.
    let mut owned = vec![state.web_bind.clone()];
    owned.extend(
        state
            .instances
            .iter()
            .filter(|entry| entry.descriptor.bridge_enabled)
            .map(|entry| entry.descriptor.bridge_bind.clone()),
    );

    let preflight = match &path {
        Some(path) => match cellar_core::config::Config::load(path) {
            Ok(config) => cellar_diagnostics::run(&config, &owned).await,
            Err(why) => one_note(
                "config",
                format!("{} could not be re-read: {why}", path.display()),
            ),
        },
        None => one_note(
            "config",
            "this process was not started from a config file, so the preflight checks cannot be \
             re-run"
                .to_owned(),
        ),
    };

    let mut runtime = Vec::new();
    let mut unparsed = Vec::new();
    for entry in state.instances.iter() {
        let id = entry.id.to_string();
        match &entry.handle {
            None => runtime.push(serde_json::json!({
                "label": "supervisor",
                "outcome": "fail",
                "instance": id,
                "detail": entry.unavailable.clone().unwrap_or_else(||
                    "declared but not supervised by this process".to_owned()),
            })),
            Some(handle) => {
                let snapshot = handle.snapshot().await;
                let Some(snapshot) = snapshot else {
                    runtime.push(serde_json::json!({
                        "label": "supervisor",
                        "outcome": "note",
                        "instance": id,
                        "detail": "supervised, but it has not reported a state yet",
                    }));
                    continue;
                };

                runtime.push(serde_json::json!({
                    "label": "supervisor",
                    "outcome": match snapshot.state {
                        cellar_core::lifecycle::State::Running => "ok",
                        cellar_core::lifecycle::State::Stopped
                        | cellar_core::lifecycle::State::Starting
                        | cellar_core::lifecycle::State::Stopping => "note",
                        _ => "fail",
                    },
                    "instance": id,
                    "detail": match snapshot.last_exit {
                        Some(exit) if snapshot.pid.is_none() => format!(
                            "{}, last exit {}",
                            snapshot.state.as_str(),
                            match exit.code {
                                Some(code) if exit.graceful => format!("{code}, asked for"),
                                Some(code) => format!("{code}, unexpected"),
                                None => "on a signal with no code".to_owned(),
                            }
                        ),
                        _ => format!("{}, {} restart(s)", snapshot.state.as_str(), snapshot.restarts),
                    },
                }));

                // The readiness line is the single most consequential string in
                // an instance's config and the only way a wrong one shows up is
                // a server that starts and never becomes ready.
                runtime.push(serde_json::json!({
                    "label": "ready_pattern",
                    "outcome": "note",
                    "instance": id,
                    "detail": entry.descriptor.ready_pattern.clone(),
                }));

                unparsed.push(serde_json::json!({
                    "instance": id,
                    "lines": snapshot.unparsed_lines,
                    "samples": snapshot.unparsed_samples,
                }));
            }
        }
    }

    runtime.push(serde_json::json!({
        "label": "database",
        "outcome": if state.pool.is_some() { "ok" } else { "note" },
        "detail": format!("{} connection", database_source(&state)),
    }));

    Json(serde_json::json!({
        "config_path": path.map(|path| path.display().to_string()),
        "checks": preflight.checks,
        "runtime": runtime,
        "unparsed": unparsed,
    }))
    .into_response()
}

/// A one-entry report, for when the checks could not be run at all.
fn one_note(label: &str, detail: String) -> cellar_diagnostics::Report {
    cellar_diagnostics::Report {
        checks: vec![cellar_diagnostics::Check {
            label: label.to_owned(),
            outcome: cellar_diagnostics::Outcome::Note,
            detail,
            instance: None,
        }],
    }
}

/// What runs on a timer, when it last ran, and whether it worked.
///
/// These were three `tokio::spawn`ed loops in `runner.rs` with no way to see
/// any of them, and a fourth, event retention, that was configured and had no
/// loop at all.
async fn jobs(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let jobs = state
        .scheduler
        .get()
        .map(|scheduler| scheduler.statuses())
        .unwrap_or_default();
    Json(serde_json::json!({ "jobs": jobs })).into_response()
}

/// Run a job now, and push its next automatic run out by a full interval.
///
/// 202, not 200: this nudges the job's own loop rather than running the work
/// inside the request, so a job cannot be running twice at once however many
/// operators press the button, and the answer is "asked", not "done".
async fn run_job(
    State(state): State<Arc<AppState>>,
    operator: Operator,
    Path(name): Path<String>,
) -> Response {
    let Some(scheduler) = state.scheduler.get() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "this process runs no scheduled jobs",
        );
    };

    if !scheduler.run_now(&name) {
        return error(
            StatusCode::NOT_FOUND,
            format!(
                "no job '{name}'. This process runs: {}",
                scheduler
                    .statuses()
                    .iter()
                    .map(|job| job.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }

    tracing::info!("{} asked for job '{name}' to run now", operator.name);
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "asked": name })),
    )
        .into_response()
}

async fn instances(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let primary = state.instances.primary().map(|entry| entry.id.to_string());
    Json(serde_json::json!({
        "primary": primary,
        "instances": state.instances.iter().map(|entry| serde_json::json!({
            "id": entry.id.to_string(),
            "scope": entry.scope,
            "required": entry.required,
            "running": entry.handle.is_some(),
            "unavailable": entry.unavailable,
            "server": entry.descriptor,
            "profile": entry.descriptor.profile,
        })).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// What instances exist, for a machine caller choosing which to address.
///
/// Trimmed the way `/api/v1/configs` is: an id, whether it is running and what
/// it is, without the host's log paths, data directories or bridge addresses.
/// The id is the only field a caller needs, and it is the one field that is
/// already public by design, since it appears in every `?instance=` it sends.
async fn external_instances(State(state): State<Arc<AppState>>, _: ExternalApi) -> Response {
    Json(serde_json::json!({
        "primary": state.instances.primary().map(|entry| entry.id.to_string()),
        "instances": state.instances.iter().map(|entry| serde_json::json!({
            "id": entry.id.to_string(),
            "scope": entry.scope,
            "required": entry.required,
            "running": entry.handle.is_some(),
            "unavailable": entry.unavailable,
            "game": entry.descriptor.game,
            "map": entry.descriptor.map,
            "gamemode": entry.descriptor.profile.name,
            "ready_pattern": entry.descriptor.ready_pattern,
        })).collect::<Vec<_>>(),
    }))
    .into_response()
}

fn database_source(state: &AppState) -> &'static str {
    if state.pool.is_none() {
        "disabled"
    } else if state.mariadb.is_some() {
        "managed"
    } else {
        "external"
    }
}

async fn db_browse(
    State(state): State<Arc<AppState>>,
    _: Operator,
    Path(table): Path<String>,
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(pool) = &state.pool else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no database is configured");
    };

    let columns = cellar_store::admin::columns(pool, &table)
        .await
        .unwrap_or_default();

    match cellar_store::admin::browse(
        pool,
        &table,
        query.limit.unwrap_or(50),
        query.offset.unwrap_or(0),
    )
    .await
    {
        Ok(rows) => Json(serde_json::json!({ "columns": columns, "result": rows })).into_response(),
        Err(why) => error(StatusCode::BAD_REQUEST, why.to_string()),
    }
}

#[derive(Deserialize)]
struct QueryRequest {
    sql: String,
}

/// Run a read-only query.
///
/// The refusal comes back as 400 with the reason, because "why did my query not
/// run" is the question an operator will actually have.
async fn db_query(
    State(state): State<Arc<AppState>>,
    operator: Operator,
    Json(request): Json<QueryRequest>,
) -> Response {
    let Some(pool) = &state.pool else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no database is configured");
    };

    if let Err(why) = cellar_store::admin::is_read_only(&request.sql) {
        return error(StatusCode::BAD_REQUEST, why);
    }

    tracing::info!("{} ran a query", operator.name);

    match cellar_store::admin::query(pool, &request.sql, cellar_store::admin::MAX_ROWS).await {
        Ok(result) => Json(result).into_response(),
        Err(why) => error(StatusCode::BAD_REQUEST, why.to_string()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    /// `/api/control/kill` shares a prefix with `/api/control/{action}`, and
    /// which one wins decides whether the button kills anything: the parameter
    /// route would hand `control` an action it refuses. Stand-in handlers, so
    /// nothing here can kill the test runner.
    #[tokio::test]
    async fn the_static_control_route_beats_the_parameter_one() {
        let router: Router = Router::new()
            .route("/api/control/kill", post(async || "static"))
            .route("/api/control/{action}", post(async || "parameter"));

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/control/kill")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"static");
    }
}
