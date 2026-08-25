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

use crate::session::Operator;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/exec", post(exec))
        .route("/api/control/{action}", post(control))
        .route("/api/players", get(players))
        .route("/api/docs", get(documents))
        .route("/api/docs/{*key}", get(document).delete(delete_document))
        .route("/api/db/tables", get(db_tables))
        .route("/api/db/table/{table}", get(db_browse))
        .route("/api/db/query", post(db_query))
        .route("/api/settings", get(settings).post(set_setting))
        .route("/api/settings/export", get(export_settings))
        .route("/api/versions", get(versions))
        .route("/api/changelog", get(changelog))
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
        Some(pool) => cellar_store::ping(pool).await.is_ok(),
        None => false,
    };
    let mariadb = match &state.mariadb {
        Some(handle) => handle.snapshot().await,
        None => None,
    };

    Json(serde_json::json!({
        "server": snapshot,
        "bridge": bridge,
        "database": database,
        "mariadb": mariadb,
        "scope": state.scope,
    }))
    .into_response()
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
