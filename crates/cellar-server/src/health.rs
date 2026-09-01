//! Liveness and readiness.
//!
//! This exists because the deployment asked for it in a comment:
//!
//! > No readiness/liveness probes: this isn't HTTP ... Revisit once there's a
//! > confirmed way to ask this server "are you actually serving."
//!
//! There is now, and it is not a guess at a network protocol. Cellar owns the
//! process and watches its log, so readiness is "the supervisor saw the
//! readiness line and the process has not exited since", which is a fact rather
//! than an inference from a port being open.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
}

/// Liveness: Cellar itself is answering.
///
/// Deliberately not a check on the game server. A liveness probe that fails
/// during a legitimate restart gets the pod killed mid-restart, which is the
/// classic way to turn a recoverable fault into a crash loop.
async fn healthz() -> Response {
    (StatusCode::OK, "ok").into_response()
}

/// Readiness: every instance that speaks for this process is serving.
///
/// Returns 503 while starting, backing off or crash-looping, which is what stops
/// a rollout from sending players at a server that has not loaded its map.
///
/// **Every `required` instance, never any of them.** A development instance
/// that cannot start on a host with no editor must not fail readiness for a
/// healthy production server, and equally a production server that is down must
/// not be papered over by a development one that is up. `required = false` is
/// how an instance opts out of speaking here at all.
async fn readyz(State(state): State<Arc<AppState>>) -> Response {
    let required: Vec<_> = state
        .instances
        .iter()
        .filter(|entry| entry.required)
        .collect();

    if required.is_empty() {
        // A bridge-only process has no game server to speak for. It is ready
        // when its database is, because that is all it does.
        return match &state.pool {
            Some(pool) => match cellar_store::ping(pool).await {
                Ok(()) => (StatusCode::OK, "bridge ready").into_response(),
                Err(why) => {
                    (StatusCode::SERVICE_UNAVAILABLE, format!("database: {why}")).into_response()
                }
            },
            None => (StatusCode::OK, "ready").into_response(),
        };
    }

    let mut players = 0;
    let mut not_serving = Vec::new();

    for entry in required {
        let Some(supervisor) = &entry.handle else {
            not_serving.push(format!(
                "{}: {}",
                entry.id,
                entry.unavailable.as_deref().unwrap_or("not supervised")
            ));
            continue;
        };

        match supervisor.snapshot().await {
            Some(snapshot) if snapshot.state.is_ready() => players += snapshot.players.len(),
            Some(snapshot) => {
                not_serving.push(format!("{}: {}", entry.id, snapshot.state.as_str()))
            }
            None => not_serving.push(format!("{}: the supervisor is not answering", entry.id)),
        }
    }

    if not_serving.is_empty() {
        (StatusCode::OK, format!("running, {players} player(s)")).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("not serving: {}", not_serving.join("; ")),
        )
            .into_response()
    }
}
