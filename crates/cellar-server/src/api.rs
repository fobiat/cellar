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
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

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
        .route("/api/control/{action}", post(control))
        .route("/api/players", get(players))
        .route("/api/docs", get(documents))
        .route("/api/docs/{*key}", get(document).delete(delete_document))
        .route("/api/db/tables", get(db_tables))
        .route("/api/db/info", get(db_info))
        .route("/api/db/table/{table}", get(db_browse))
        .route("/api/db/query", post(db_query))
        .route("/api/settings", get(settings).post(set_setting))
        .route("/api/settings/export", get(export_settings))
        .route("/api/versions", get(versions))
        .route("/api/changelog", get(changelog))
        .route("/api/v1/status", get(external_status))
        .route("/api/v1/logs", get(external_logs))
        .route("/api/v1/resources", get(external_resources))
        .route("/api/v1/addresses", get(external_addresses))
}

async fn external_status(State(state): State<Arc<AppState>>, _: ExternalApi) -> Response {
    status(
        State(state),
        Operator {
            name: "api".to_owned(),
        },
    )
    .await
}

async fn external_logs(
    State(state): State<Arc<AppState>>,
    _: ExternalApi,
    query: Query<LogsQuery>,
) -> Response {
    logs(
        State(state),
        Operator {
            name: "api".to_owned(),
        },
        query,
    )
    .await
}

async fn external_resources(State(state): State<Arc<AppState>>, _: ExternalApi) -> Response {
    let snapshot = match &state.supervisor {
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

/// Everything the server is currently set to.
///
/// Asked of the running server every time rather than cached: a feature toggled
/// from the in-game admin panel is exactly the change an operator most wants
/// this screen to be honest about.
async fn settings(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let Some(supervisor) = &state.supervisor else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no server is being supervised",
        );
    };

    let features = match supervisor.exec("applejack_features", "web").await {
        Ok(reply) => cellar_core::convar::parse_features(&reply),
        Err(why) => return error(StatusCode::BAD_GATEWAY, why),
    };

    let settings = match supervisor.exec("applejack_settings", "web").await {
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
    Json(request): Json<SetRequest>,
) -> Response {
    let Some(supervisor) = &state.supervisor else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no server is being supervised",
        );
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
        "feature" => cellar_core::convar::feature_command(
            &request.id,
            matches!(request.value.as_str(), "on" | "true" | "1"),
        ),
        "setting" => cellar_core::convar::setting_command(&request.id, &request.value),
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
    State(state): State<Arc<AppState>>,
    _: Operator,
    Query(query): Query<ExportQuery>,
) -> Response {
    let Some(supervisor) = &state.supervisor else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no server is being supervised",
        );
    };

    let mut snapshot = cellar_core::convar::Snapshot {
        captured_at: Some(chrono::Utc::now().to_rfc3339()),
        hostname: supervisor.snapshot().await.map(|s| s.hostname),
        features: supervisor
            .exec("applejack_features", "web")
            .await
            .map(|reply| cellar_core::convar::parse_features(&reply))
            .unwrap_or_default(),
        settings: supervisor
            .exec("applejack_settings", "web")
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

    Json(serde_json::json!({
        "versions": versions,
        "decision": decision,
        "policy": state.update_config.policy,
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
async fn status(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let snapshot = match &state.supervisor {
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
    let map_log = match (&state.log_file, &state.configured_map) {
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
    let invite_only = read_access_files(&state)
        .await
        .ok()
        .map(|(features, _)| feature_enabled(&features, "admin.inviteonly"));

    Json(serde_json::json!({
        "server": snapshot,
        "bridge": bridge,
        "database": database,
        "mariadb": mariadb,
        "health": {
            "map": map_log,
            "spawn_validation": spawn_validation,
            "console": state.log_file.as_ref().is_some_and(|path| path.exists()),
        },
        "scope": state.scope,
        "addresses": addresses,
        "access": { "invite_only": invite_only },
    }))
    .into_response()
}

async fn addresses(state: &AppState) -> Vec<serde_json::Value> {
    let tailscale_ip = tailscale_ip().await;
    let mut result = Vec::new();
    if state.web_enabled {
        result.push(address("Cellar web", &state.web_bind, &tailscale_ip, true));
    }
    if state.bridge_enabled {
        result.push(address(
            "Document bridge",
            &state.bridge_bind,
            &tailscale_ip,
            false,
        ));
    }
    let server_bind = format!("0.0.0.0:{}", state.server_port);
    result.push(address(
        "Game server",
        &server_bind,
        &tailscale_ip,
        state.server_direct_connect,
    ));
    let query_bind = format!("0.0.0.0:{}", state.query_port);
    result.push(address(
        "Game query",
        &query_bind,
        &tailscale_ip,
        state.server_direct_connect,
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
    let output = tokio::time::timeout(
        std::time::Duration::from_millis(700),
        tokio::process::Command::new("tailscale")
            .args(["ip", "-4"])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// The AppleJack invite gate and its SteamID64 allowlist.
async fn access(State(state): State<Arc<AppState>>, _: Operator) -> Response {
    let (features, permissions) = match read_access_files(&state).await {
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
    category: Option<String>,
}

/// Scan current and rotated engine logs. The files are the persistent source,
/// so search remains useful after Cellar or the browser restarts.
async fn logs(
    State(state): State<Arc<AppState>>,
    _: Operator,
    Query(query): Query<LogsQuery>,
) -> Response {
    let Some(path) = &state.log_file else {
        return Json(serde_json::json!({
            "lines": [], "matched": 0, "scanned_files": 0, "scanned_lines": 0,
            "persistent": false
        }))
        .into_response();
    };
    let result = crate::logs::search(
        path,
        &crate::logs::Query {
            text: query.q.filter(|value| !value.trim().is_empty()),
            tag: query.tag.filter(|value| !value.trim().is_empty()),
            level: query.level,
            category: query.category.filter(|value| !value.trim().is_empty()),
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
    Json(serde_json::json!({
        "profiles": crate::config_manager::list(&directory, active.as_deref()).await
    }))
    .into_response()
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
    if config.web.bind != state.web_bind
        || config.web.enabled != state.web_enabled
        || config.bridge.bind != state.bridge_bind
        || config.bridge.enabled != state.bridge_enabled
    {
        return error(
            StatusCode::BAD_REQUEST,
            "profiles may change the supervised server, but not Cellar listener bindings",
        );
    }
    let current_log = state.log_file.as_deref();
    if current_log != Some(cellar_runtime::log_file_for(&config.server).as_path()) {
        return error(
            StatusCode::BAD_REQUEST,
            "profiles must use the active server log path",
        );
    }
    let Some(supervisor) = &state.supervisor else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no server is being supervised",
        );
    };
    if let Err(why) = supervisor.switch_config(config).await {
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
    Json(request): Json<AccessRequest>,
) -> Response {
    let result = match request.action.as_str() {
        "allow" | "revoke" => {
            let Some(steam_id) = request.steam_id.as_deref() else {
                return error(StatusCode::BAD_REQUEST, "steam_id is required");
            };
            if !valid_steam_id(steam_id) {
                return error(StatusCode::BAD_REQUEST, "steam_id must be a SteamID64");
            }
            edit_allowlist(&state, steam_id, request.action == "allow", &operator.name).await
        }
        "gate" => {
            let Some(enabled) = request.enabled else {
                return error(StatusCode::BAD_REQUEST, "enabled is required");
            };
            edit_gate(&state, enabled, &operator.name).await
        }
        other => Err(format!("unknown access action '{other}'")),
    };

    match result {
        Ok(()) => access(State(state), operator).await,
        Err(why) => error(StatusCode::BAD_GATEWAY, why),
    }
}

async fn edit_allowlist(
    state: &AppState,
    steam_id: &str,
    allow: bool,
    operator: &str,
) -> Result<(), String> {
    let (_, mut permissions) = read_access_files(state).await?;
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

    write_access_file(state, "permissions.json", &permissions, operator).await
}

async fn edit_gate(state: &AppState, enabled: bool, operator: &str) -> Result<(), String> {
    let (mut features, _) = read_access_files(state).await?;
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

    write_access_file(state, "features.json", &features, operator).await
}

async fn read_access_files(
    state: &AppState,
) -> Result<(serde_json::Value, serde_json::Value), String> {
    let Some(root) = &state.game_data_dir else {
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
    state: &AppState,
    name: &str,
    value: &serde_json::Value,
    operator: &str,
) -> Result<(), String> {
    let Some(root) = &state.game_data_dir else {
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
    Json(request): Json<ExecRequest>,
) -> Response {
    let Some(supervisor) = &state.supervisor else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no server is being supervised",
        );
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

/// Start, stop or restart the server.
async fn control(
    State(state): State<Arc<AppState>>,
    operator: Operator,
    Path(action): Path<String>,
) -> Response {
    let Some(supervisor) = &state.supervisor else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no server is being supervised",
        );
    };

    match action.as_str() {
        "stop" => {
            tracing::info!("{} asked for a graceful stop", operator.name);
            supervisor.stop().await;
            Json(serde_json::json!({ "ok": true, "action": "stop" })).into_response()
        }
        "exit" => {
            tracing::info!(
                "{} asked Cellar to exit after a graceful stop",
                operator.name
            );
            supervisor.stop().await;
            state
                .shutdown_requested
                .store(true, std::sync::atomic::Ordering::Release);
            Json(serde_json::json!({ "ok": true, "action": "exit" })).into_response()
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
    Query(query): Query<ListQuery>,
) -> Response {
    let Some(pool) = &state.pool else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no database is configured");
    };

    match cellar_store::document::list(
        pool,
        &state.scope,
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
    Path(key): Path<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no database is configured");
    };

    if let Err(refusal) = cellar_core::doc_key::check(&key) {
        return error(StatusCode::BAD_REQUEST, refusal.to_string());
    }

    let document = match cellar_store::document::get(pool, &state.scope, &key).await {
        Ok(Some(document)) => document,
        Ok(None) => return error(StatusCode::NOT_FOUND, "no such document"),
        Err(why) => return error(StatusCode::BAD_GATEWAY, why.to_string()),
    };

    let history = cellar_store::document::revisions(pool, &state.scope, &key, 25)
        .await
        .unwrap_or_default();

    Json(serde_json::json!({ "document": document, "revisions": history })).into_response()
}

/// Remove a document. The bridge's own interface has no delete; this is the
/// operator's, and it is audited by the revision history it leaves behind.
async fn delete_document(
    State(state): State<Arc<AppState>>,
    operator: Operator,
    Path(key): Path<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "no database is configured");
    };

    if let Err(refusal) = cellar_core::doc_key::check(&key) {
        return error(StatusCode::BAD_REQUEST, refusal.to_string());
    }

    match cellar_store::document::delete(pool, &state.scope, &key).await {
        Ok(true) => {
            tracing::warn!("{} deleted the document {key}", operator.name);
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
        }))
        .into_response();
    };

    match cellar_store::admin::info(pool).await {
        Ok(mut info) => {
            info.schema_owner = state.database_schema_owner.clone();
            Json(info).into_response()
        }
        Err(why) => error(StatusCode::BAD_GATEWAY, why.to_string()),
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
