//! The live event stream behind the dashboard.
//!
//! One websocket carrying the supervisor's event stream as JSON. The alternative
//! is polling, and polling a console means either missing lines or asking for
//! the whole buffer every second.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use tokio::sync::broadcast;

use crate::registry::Target;
use crate::session::Operator;
use crate::state::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/events", get(upgrade))
}

/// Authenticated the same way every other route is: the extractor runs before
/// the upgrade, so an unauthenticated caller never reaches the stream.
async fn upgrade(
    upgrade: WebSocketUpgrade,
    _: State<Arc<AppState>>,
    _operator: Operator,
    target: Target,
) -> Response {
    upgrade.on_upgrade(move |socket| pump(socket, target))
}

/// One instance's stream, not the primary's.
///
/// The dashboard's instance strip switches every other panel; a console still
/// streaming the previous server under a header naming the new one is how a
/// command reaches the wrong thing.
async fn pump(mut socket: WebSocket, target: Target) {
    let Some(supervisor) = &target.handle else {
        let _ = socket
            .send(Message::Text(
                serde_json::json!({
                    "kind": "notice",
                    "raw": format!(
                        "instance '{}' is not running: {}",
                        target.id,
                        target
                            .unavailable
                            .as_deref()
                            .unwrap_or("no server is being supervised")
                    ),
                })
                .to_string()
                .into(),
            ))
            .await;
        return;
    };

    let mut events = supervisor.subscribe();

    loop {
        tokio::select! {
            received = events.recv() => match received {
                Ok(event) => {
                    let Ok(json) = serde_json::to_string(&event) else { continue };
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        return;
                    }
                }
                // A slow browser must not stall the supervisor. It is told what
                // it missed and the stream continues, rather than the connection
                // being torn down or the sender being blocked.
                //
                // Its own kind, not `unparsed`. A gap in the console is a fact
                // about the console and the browser has to be able to mark it
                // as one; `unparsed` means the engine said something the
                // grammar did not recognise, which is a different thing that
                // wants a different treatment.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    let notice = serde_json::json!({
                        "kind": "lagged",
                        "missed": missed,
                        "raw": format!("dropped {missed} event(s): this browser fell behind"),
                        "origin": "cellar",
                    });
                    if socket.send(Message::Text(notice.to_string().into())).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },

            // Read the client side so a close frame is noticed promptly rather
            // than on the next event, which on a quiet server could be minutes.
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}
