//! Session management: revoking a session and renaming its description.
//!
//! Both take the *signed* session token the sessions page rendered, not a raw
//! row id — verifying the signature and that the payload's account id matches
//! the caller is what stops one user touching another's session.

use crate::{headers::ClientIp, models::Account, token::Token, AppState};
use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct InvalidateSessionPayload {
    session_id: String,
}

pub async fn invalidate_session(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Json(payload): Json<InvalidateSessionPayload>,
) -> StatusCode {
    let key = state.config().secret_key;
    match Token::from_signed(&payload.session_id, &key) {
        Some(token) if token.id == account.id => {
            state.invalidate_session(&token.base64()).await;
            state
                .audit("auth.session.invalidate")
                .actor(&account)
                .target(token.base64())
                .ip_opt(client_ip)
                .fire();
            StatusCode::NO_CONTENT
        }
        _ => StatusCode::NOT_FOUND,
    }
}

#[derive(Deserialize)]
pub struct RenameSessionPayload {
    session_id: String,
    description: String,
}

/// Relabels a session so the list stays meaningful ("work laptop", "phone").
/// Purely cosmetic — the description is never used for authentication.
pub async fn rename_session(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Json(payload): Json<RenameSessionPayload>,
) -> StatusCode {
    let description = payload.description.trim();
    if description.is_empty() || description.len() > 100 {
        return StatusCode::BAD_REQUEST;
    }

    let key = state.config().secret_key;
    let Some(token) = Token::from_signed(&payload.session_id, &key) else {
        return StatusCode::NOT_FOUND;
    };
    if token.id != account.id {
        return StatusCode::NOT_FOUND;
    }

    match state
        .database()
        .execute(
            "UPDATE session SET description = ? WHERE id = ? AND account_id = ?",
            (description.to_string(), token.base64(), account.id),
        )
        .await
    {
        Ok(1..) => {
            state
                .audit("auth.session.rename")
                .actor(&account)
                .target(token.base64())
                .ip_opt(client_ip)
                .fire();
            StatusCode::NO_CONTENT
        }
        Ok(_) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
