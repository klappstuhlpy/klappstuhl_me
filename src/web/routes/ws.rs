//! Single live-update WebSocket endpoint at `/ws`.
//!
//! Protocol (text frames carrying JSON):
//!
//! Client → server:
//! ```json
//! { "action": "subscribe",   "topics": ["metrics", "audit"] }
//! { "action": "unsubscribe", "topics": ["audit"] }
//! ```
//!
//! Server → client (after a topic event):
//! ```json
//! { "topic": "metrics", "data": { …same payload as the matching HTTP endpoint… } }
//! ```
//!
//! Authentication is the same cookie session used everywhere else —
//! the [`Account`] extractor on the upgrade handler 401s when the
//! client isn't logged in.  Non-admin sessions are accepted but can
//! only subscribe to topics they would otherwise be allowed to read.

use std::collections::HashSet;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use crate::{models::Account, AppState};

/// `subscribe` and `unsubscribe` use the same shape.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
enum ClientMessage {
    Subscribe { topics: Vec<String> },
    Unsubscribe { topics: Vec<String> },
}

/// Topics non-admin clients are allowed to subscribe to.  Empty set means
/// admins-only (which is the current default for every defined topic).
fn topic_allowed(topic: &str, is_admin: bool) -> bool {
    if is_admin {
        return true;
    }
    // Future: add user-scope topics here (e.g. "my-uploads").
    let _ = topic;
    false
}

async fn ws_upgrade(State(state): State<AppState>, account: Account, ws: WebSocketUpgrade) -> Response {
    let is_admin = account.flags.is_admin();
    ws.on_upgrade(move |socket| handle_socket(state, socket, is_admin))
}

async fn handle_socket(state: AppState, socket: WebSocket, is_admin: bool) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.live_subscribe();
    let mut subscriptions: HashSet<String> = HashSet::new();

    // Greet so clients can detect a successful upgrade and switch off
    // their polling fallback without waiting for the first event.
    let _ = sender
        .send(Message::Text(r#"{"topic":"_meta","data":{"hello":true}}"#.into()))
        .await;

    loop {
        tokio::select! {
            // ── Outbound: live events from the broadcast hub ──────
            evt = events.recv() => {
                match evt {
                    Ok(event) => {
                        if !subscriptions.contains(event.topic) {
                            continue;
                        }
                        let body = serde_json::json!({
                            "topic": event.topic,
                            "data": event.data,
                        });
                        if sender.send(Message::Text(body.to_string())).await.is_err() {
                            break; // client gone
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        // Slow consumer dropped n messages — surface so
                        // the client can fall back to polling for a
                        // catch-up snapshot.
                        let body = serde_json::json!({
                            "topic": "_meta",
                            "data": { "lagged": n }
                        });
                        let _ = sender.send(Message::Text(body.to_string())).await;
                    }
                    Err(RecvError::Closed) => break,
                }
            }

            // ── Inbound: subscribe / unsubscribe / pings ──────────
            msg = receiver.next() => {
                let Some(Ok(msg)) = msg else { break };
                match msg {
                    Message::Text(text) => {
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(ClientMessage::Subscribe { topics }) => {
                                for t in topics {
                                    if topic_allowed(&t, is_admin) {
                                        subscriptions.insert(t);
                                    }
                                }
                                let body = serde_json::json!({
                                    "topic": "_meta",
                                    "data": { "subscribed": subscriptions.iter().collect::<Vec<_>>() }
                                });
                                let _ = sender.send(Message::Text(body.to_string())).await;
                            }
                            Ok(ClientMessage::Unsubscribe { topics }) => {
                                for t in topics {
                                    subscriptions.remove(&t);
                                }
                            }
                            Err(_) => {
                                // Malformed payload — keep the socket
                                // alive but ignore.
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        let _ = sender.send(Message::Pong(payload)).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/ws", get(ws_upgrade))
}
