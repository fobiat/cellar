//! The `/v1/doc/{key}` service AppleJackRP's `HostedDocumentStore` already has a
//! client for.
//!
//! Every status code here is load-bearing, because the client maps them onto
//! three different behaviours and one of the mappings is a data-loss hazard.
//! From `HostedDocumentProtocol.cs`:
//!
//! ```text
//! GET   404 -> Absent      2xx -> Found       anything else -> Unavailable
//! PUT   2xx -> Written     409 -> Rejected    anything else -> Failed
//! HEAD  404 -> Absent      2xx -> Present     anything else -> Unknown
//! ```
//!
//! **404 is the only code that may mean "absent".** A 500, a 502, a timeout and
//! a parse failure are all "I could not tell you". Mapping any of them to absent
//! is 20_PERSISTENCE.md §4.1's catastrophe arriving over the wire: a player
//! joins, their roster reads as empty, and the empty roster is written back over
//! their character.
//!
//! Two further constraints from the same section. The client gives a read 3
//! seconds and a write 5, and opens a circuit breaker after three failures, so
//! slowness here is indistinguishable from being down. And the engine strips
//! `Authorization` on every redirect hop, so this service must never redirect.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::auth;
use crate::state::AppState;

/// Mount the bridge routes.
///
/// One handler chain for all three methods, because the client uses the same
/// route with `GET`, `PUT` and `HEAD` and they must agree about keys and auth.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/v1/doc/{*key}",
        get(read_document).put(write_document).head(head_document),
    )
}

async fn read_document(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Response {
    if let Err(refusal) = admit(&state, &headers, &key) {
        return *refusal;
    }

    match state.documents.get(&state.scope, &key).await {
        Ok(Some(body)) => {
            state.bridge_read();
            // `ReadFromJsonAsync` on the client, so the content type matters.
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body.to_string(),
            )
                .into_response()
        }
        Ok(None) => {
            state.bridge_absent();
            // The one place 404 is correct.
            StatusCode::NOT_FOUND.into_response()
        }
        Err(why) => {
            state.bridge_failed(&why);
            // Never 404 on a failure. "I could not tell you" is 503.
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn head_document(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Response {
    if let Err(refusal) = admit(&state, &headers, &key) {
        return *refusal;
    }

    match state.documents.exists(&state.scope, &key).await {
        Ok(true) => StatusCode::OK.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(why) => {
            state.bridge_failed(&why);
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn write_document(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(key): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(refusal) = admit(&state, &headers, &key) {
        return *refusal;
    }

    if body.len() > state.max_body_bytes {
        // The caller is a game host, and a compromised host is the thing being
        // limited (§7.2). 413 maps to Failed at the client, which journals the
        // write rather than losing it.
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }

    let document: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    match state
        .documents
        .put(&state.scope, &key, &document, Some("gamemode"))
        .await
    {
        Ok(outcome) => {
            state.bridge_write(outcome.would_conflict);
            // Deliberately 204 and never 409, even when the revision moved
            // underneath this write. `HostedDocumentStore` documents that it
            // never surfaces `Rejected` because the concurrency question is
            // still open upstream, so a 409 would reach a client with no code
            // to retry it: a recoverable write would become a lost one. The
            // conflict is counted and shown instead.
            StatusCode::NO_CONTENT.into_response()
        }
        Err(why) => {
            state.bridge_failed(&why);
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

/// Auth, rate limit and key validation, in that order.
// The error is boxed: a `Response` is large, and every caller returns it
// immediately, so the common path should not carry its width on the stack.
fn admit(state: &AppState, headers: &HeaderMap, key: &str) -> Result<(), Box<Response>> {
    if let Err(status) = auth::check(&state.auth, headers) {
        return Err(Box::new(status.into_response()));
    }

    if !state.rate_limiter.allow() {
        return Err(Box::new(StatusCode::TOO_MANY_REQUESTS.into_response()));
    }

    // The same rules the gamemode applies before it sends, so a key that gets
    // here is one the client should never have produced. Refused rather than
    // sanitised, matching the C# posture: a sanitised key is a different key,
    // and writing to a different key is worse than refusing.
    if let Err(refusal) = cellar_core::doc_key::check(key) {
        return Err(Box::new(
            (StatusCode::BAD_REQUEST, refusal.to_string()).into_response(),
        ));
    }

    Ok(())
}
