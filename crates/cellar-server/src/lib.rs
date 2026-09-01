//! Cellar's HTTP surface.
//!
//! Three things on one server, deliberately separable by config: the bridge the
//! gamemode calls, the health endpoints Kubernetes calls, and the web UI a
//! person calls. The bridge is the one with a contract it does not own; see
//! [`bridge`].

pub mod api;
pub mod auth;
pub mod bridge;
pub mod config_manager;
pub mod health;
pub mod logs;
pub mod registry;
pub mod security;
pub mod session;
pub mod state;
pub mod ui;
pub mod ws;

use std::sync::Arc;

use axum::Router;

pub use state::{AppState, Documents};

/// The bridge and health only. What a `cellar run` with `bridge.enabled` binds.
pub fn bridge_router(state: Arc<AppState>) -> Router {
    Router::new()
        .merge(bridge::routes())
        .merge(health::routes())
        .with_state(state)
}

/// The operator's web UI, its API and its live stream.
///
/// Bound separately from the bridge on purpose. The bridge is called by the game
/// server and wants a loopback bind with no human near it; the web UI is called
/// by a person and may be exposed. Sharing one listener would mean one exposure
/// decision for two very different audiences.
pub fn web_router(state: Arc<AppState>) -> Router {
    Router::new()
        .merge(ui::routes())
        .merge(api::routes())
        .merge(ws::routes())
        .merge(health::routes())
        .with_state(state)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod contract_tests {
    //! The bridge's half of a contract the other half already ships.
    //!
    //! These assert the status codes `HostedDocumentProtocol.cs` maps, and the
    //! expectations are taken from `RuleTests/HostedDocumentProtocolTests.cs`
    //! rather than invented, so the two halves are provably talking about the
    //! same protocol.

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::auth::Policy;

    const TOKEN: &str = "a-plausible-bearer-token";

    fn app() -> (Router, Arc<AppState>) {
        let state = Arc::new(AppState::new(
            Documents::memory(),
            Policy::Trusted,
            "test-scope",
        ));
        (bridge_router(state.clone()), state)
    }

    fn request(method: &str, key: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(format!("/v1/doc/{key}"))
            .header("Authorization", format!("Bearer {TOKEN}"))
    }

    async fn send(app: &Router, request: Request<Body>) -> (StatusCode, String) {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The one mapping that must never move: absent is 404 and only 404.
    #[tokio::test]
    async fn a_document_that_does_not_exist_is_404() {
        let (app, _) = app();
        let (status, _) = send(
            &app,
            request("GET", "characters/76561198000000000.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_written_document_reads_back_byte_for_byte() {
        let (app, _) = app();
        let body = serde_json::json!({ "Version": 3, "Balance": 8000, "Name": "Kyle" });

        let (status, _) = send(
            &app,
            request("PUT", "characters/76561198000000000.json")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
        // `InterpretWrite` treats any 2xx as Written; 204 is what §6.3 specifies.
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, text) = send(
            &app,
            request("GET", "characters/76561198000000000.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            body
        );
    }

    #[tokio::test]
    async fn head_answers_present_and_absent_without_a_body() {
        let (app, _) = app();

        let (status, _) = send(
            &app,
            request("HEAD", "features.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        send(
            &app,
            request("PUT", "features.json")
                .body(Body::from(r#"{"Version":1}"#))
                .unwrap(),
        )
        .await;

        let (status, body) = send(
            &app,
            request("HEAD", "features.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty(), "HEAD carries no body");
    }

    #[tokio::test]
    async fn every_document_the_gamemode_writes_round_trips() {
        let (app, _) = app();

        for key in [
            "characters/76561198000000000.json",
            "features.json",
            "laws.json",
            "permissions.json",
            "doors/minimal.json",
        ] {
            let (status, _) = send(
                &app,
                request("PUT", key)
                    .body(Body::from(r#"{"Version":1}"#))
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "writing {key}");

            let (status, _) = send(&app, request("GET", key).body(Body::empty()).unwrap()).await;
            assert_eq!(status, StatusCode::OK, "reading {key}");
        }
    }

    #[tokio::test]
    async fn no_credential_is_401_and_never_404() {
        let (app, _) = app();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/doc/features.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // 401 maps to Unavailable at the client, which journals the write. A 404
        // here would tell the gamemode the document does not exist.
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_shared_secret_bridge_refuses_the_wrong_token() {
        let state = Arc::new(AppState::new(
            Documents::memory(),
            Policy::SharedSecret(cellar_core::Secret::new("the-real-secret")),
            "test-scope",
        ));
        let app = bridge_router(state);

        let (status, _) = send(
            &app,
            Request::builder()
                .uri("/v1/doc/features.json")
                .header("Authorization", "Bearer not-the-real-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// A key the gamemode would never send. Refused, and refused with something
    /// that is not 404, so it cannot be read as "absent".
    #[tokio::test]
    async fn an_illegal_key_is_refused_but_not_as_absent() {
        let (app, _) = app();

        for key in ["Features.json", "..%2Fsecrets.json", "nul.json"] {
            let (status, _) = send(&app, request("GET", key).body(Body::empty()).unwrap()).await;
            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "{key} must not read as absent"
            );
            assert!(status.is_client_error(), "{key} -> {status}");
        }
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_refused_rather_than_stored() {
        let (app, _) = app();
        let (status, _) = send(
            &app,
            request("PUT", "features.json")
                .body(Body::from("this is not json"))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (status, _) = send(
            &app,
            request("GET", "features.json").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "nothing was stored");
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused_as_a_failure_not_as_a_conflict() {
        let state = Arc::new(AppState::new(Documents::memory(), Policy::Trusted, "s"));
        let app = bridge_router(state);

        let huge = serde_json::json!({ "padding": "x".repeat(2 * 1024 * 1024) });
        let (status, _) = send(
            &app,
            Request::builder()
                .method("PUT")
                .uri("/v1/doc/features.json")
                .header("Authorization", format!("Bearer {TOKEN}"))
                .body(Body::from(huge.to_string()))
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_ne!(status, StatusCode::CONFLICT);
    }

    /// The gamemode's client cannot act on a conflict yet, so the bridge does
    /// not create one.
    #[tokio::test]
    async fn a_second_write_wins_rather_than_conflicting() {
        let (app, _) = app();

        for balance in [100, 200, 300] {
            let (status, _) = send(
                &app,
                request("PUT", "characters/1.json")
                    .body(Body::from(format!(r#"{{"Balance":{balance}}}"#)))
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT);
        }

        let (_, text) = send(
            &app,
            request("GET", "characters/1.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert!(text.contains("300"));
    }

    #[tokio::test]
    async fn the_rate_limiter_refuses_with_429_and_not_with_404() {
        let mut state = AppState::new(Documents::memory(), Policy::Trusted, "s");
        state.rate_limiter = crate::state::RateLimiter::new(2);
        let app = bridge_router(Arc::new(state));

        let mut statuses = Vec::new();
        for _ in 0..4 {
            let (status, _) = send(
                &app,
                request("GET", "features.json").body(Body::empty()).unwrap(),
            )
            .await;
            statuses.push(status);
        }

        assert_eq!(statuses[2], StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(statuses[3], StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn counters_track_what_the_dashboard_shows() {
        let (app, state) = app();

        send(
            &app,
            request("GET", "features.json").body(Body::empty()).unwrap(),
        )
        .await;
        send(
            &app,
            request("PUT", "features.json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
        send(
            &app,
            request("GET", "features.json").body(Body::empty()).unwrap(),
        )
        .await;

        let stats = state.stats();
        assert_eq!(stats.absent, 1);
        assert_eq!(stats.writes, 1);
        assert_eq!(stats.reads, 1);
        assert!(stats.healthy);
    }

    #[tokio::test]
    async fn liveness_answers_even_with_no_server_attached() {
        let (app, _) = app();
        let (status, body) = send(
            &app,
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
    }

    /// One Cellar has to be able to recognise another.
    ///
    /// This is not decoration. `doctor`'s "another Cellar is already bound"
    /// case and the refusal that stops `cellar db restore` running while a
    /// supervised server writes to the database both depend on it, and both
    /// were unreachable until 2026-09-01 because they sniffed this response
    /// for `"cellar"` or `"state"` and the body has always been `ok`.
    #[tokio::test]
    async fn liveness_identifies_itself_as_a_cellar() {
        let (app, _) = app();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response
                .headers()
                .get(cellar_core::HEALTH_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(env!("CARGO_PKG_VERSION")),
        );
    }
}
