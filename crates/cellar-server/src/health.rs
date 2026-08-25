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

/// Readiness: the game server is actually serving.
///
/// Returns 503 while starting, backing off or crash-looping, which is what stops
/// a rollout from sending players at a server that has not loaded its map.
async fn readyz(State(state): State<Arc<AppState>>) -> Response {
    let Some(supervisor) = &state.supervisor else {
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
    };

    let Some(snapshot) = supervisor.snapshot().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the supervisor is not answering",
        )
            .into_response();
    };

    if snapshot.state.is_ready() {
        (
            StatusCode::OK,
            format!("running, {} player(s)", snapshot.players.len()),
        )
            .into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("not serving: {}", snapshot.state.as_str()),
        )
            .into_response()
    }
}
