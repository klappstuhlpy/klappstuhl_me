//! The streaming `/api/ask` AI endpoint and its admin public-access toggle.
//!
//! `/api/ask` is a thin, server-side proxy in front of the Groq API: the
//! browser POSTs the conversation, we call the model with the key held here,
//! and stream the answer back over SSE. It backs the admin-only "Ask the AI"
//! item in the Spotlight palette. It is rate-limited (see `routes()`),
//! token-capped, and bounded in history length below.

use std::convert::Infallible;

use axum::{
    extract::State,
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::post,
    Json, Router,
};
use futures_util::stream;
use tokio::sync::mpsc;

use crate::{
    ai::{self, AskEvent, ChatTurn},
    models::Account,
    ratelimit::RateLimit,
    AppState,
};

/// Hard caps on what a single request may carry, to bound cost/abuse.
const MAX_TURNS: usize = 16;
const MAX_CHARS_PER_TURN: usize = 4000;

#[derive(serde::Deserialize)]
struct AskRequest {
    #[serde(default)]
    messages: Vec<ChatTurn>,
}

async fn ask(
    State(state): State<AppState>,
    account: Option<Account>,
    Json(req): Json<AskRequest>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    if !state.config().ai.enabled() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    // Token-spend gate: anyone may ask only when public access is on; otherwise
    // it's admins only. 403 tells the client to fall back to local commands.
    let is_admin = account.as_ref().map(|a| a.flags.is_admin()).unwrap_or(false);
    if !is_admin && !ai::public_enabled(&state).await {
        return Err(StatusCode::FORBIDDEN);
    }

    // Sanitise: keep only user/assistant turns, clamp count + length, and
    // require the conversation to end with a user turn.
    let mut turns: Vec<ChatTurn> = req
        .messages
        .into_iter()
        .filter(|t| (t.role == "user" || t.role == "assistant") && !t.content.trim().is_empty())
        .map(|mut t| {
            if t.content.len() > MAX_CHARS_PER_TURN {
                t.content.truncate(MAX_CHARS_PER_TURN);
            }
            t
        })
        .collect();

    if turns.len() > MAX_TURNS {
        turns.drain(..turns.len() - MAX_TURNS);
    }
    if turns.last().map(|t| t.role.as_str()) != Some("user") {
        return Err(StatusCode::BAD_REQUEST);
    }

    let (tx, rx) = mpsc::channel::<AskEvent>(32);
    tokio::spawn(ai::stream_answer(state, turns, tx));

    let body = stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|ev| {
            let data = serde_json::to_string(&ev)
                .unwrap_or_else(|_| r#"{"type":"error","message":"encode failed"}"#.to_string());
            (Ok::<_, Infallible>(Event::default().data(data)), rx)
        })
    });
    Ok(Sse::new(body).keep_alive(KeepAlive::default()))
}

#[derive(serde::Deserialize)]
struct TogglePublic {
    enabled: bool,
}

/// Admin-only: flip whether non-admin callers may spend tokens on `/api/ask`.
/// Persisted to the KV store so it survives restarts and overrides the config
/// default. Returns the new state as JSON.
async fn toggle_public(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<TogglePublic>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    ai::set_public(&state, body.enabled)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    state
        .audit("ai.public.toggle")
        .actor(&account)
        .meta(serde_json::json!({ "enabled": body.enabled }))
        .fire();

    Ok(Json(serde_json::json!({ "public": body.enabled })))
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/admin/ai/public", post(toggle_public)).route(
        "/api/ask",
        // Public-capable + expensive: 6 asks per minute per IP.
        post(ask).route_layer(RateLimit::default().quota(6, 60.0).build()),
    )
}
