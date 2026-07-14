//! API tokens: minting a scoped key, and the ShareX uploader config built from it.

use crate::{error::ApiError, headers::ClientIp, models::Account, models::Scope, AppState};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct GenerateApiKey {
    new: bool,
    /// Scopes to grant the new token. String values matching `Scope::as_str`
    /// (e.g. "images:read"); unknown values are silently dropped.
    /// Empty array means "legacy / unrestricted" — same as a pre-scopes key.
    #[serde(default)]
    scopes: Vec<String>,
}

#[derive(Serialize)]
pub struct GeneratedApiKey {
    token: String,
}

pub async fn generate_api_key(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Json(payload): Json<GenerateApiKey>,
) -> Result<Json<GeneratedApiKey>, ApiError> {
    if !payload.new {
        state.invalidate_api_keys(account.id).await;
    }
    // Map the string list from the form into typed Scopes, dropping any
    // we don't recognise (a stale client shouldn't be able to inject a
    // spurious permission name into the DB). Privileged scopes (`admin:*`,
    // `images:guild`) are operator/internal-only: silently drop them unless
    // the caller is an admin, so a hand-crafted POST can't self-grant them.
    let is_admin = account.flags.is_admin();
    let scopes: Vec<Scope> = payload
        .scopes
        .iter()
        .filter_map(|s| Scope::from_str(s))
        .filter(|s| is_admin || !s.requires_admin())
        .collect();
    let scopes_str = scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",");
    let token = state.generate_api_key(account.id, &scopes).await?;
    state
        .audit("auth.api_key.generate")
        .actor(&account)
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "regenerated": !payload.new,
            "scopes": scopes_str,
        }))
        .fire();
    Ok(Json(GeneratedApiKey { token }))
}

/// Generates a [ShareX](https://getsharex.com) custom-uploader config
/// (`.sxcu`) pre-filled with the user's API key and this site's upload
/// endpoint. Importing it makes ShareX (and Flameshot/other tools that read
/// the same format) upload screenshots straight to the gallery, copying the
/// returned link to the clipboard.
pub async fn sharex_config(State(state): State<AppState>, account: Account) -> Response {
    let Some(api_key) = state.get_api_key(account.id).await else {
        return (
            StatusCode::BAD_REQUEST,
            "Generate an API key on your account page first, then download this again.",
        )
            .into_response();
    };

    let upload_url = state
        .config()
        .url_to(format!("{}/images/upload", crate::site::api::api_base_path()));
    let config = serde_json::json!({
        "Version": "15.0.0",
        "Name": "klappstuhl.me",
        "DestinationType": "ImageUploader, FileUploader",
        "RequestMethod": "POST",
        "RequestURL": upload_url,
        "Headers": { "Authorization": api_key },
        "Body": "MultipartFormData",
        "FileFormName": "file",
        // UploadResult.links is the array of canonical URLs; take the first.
        "URL": "{json:links[0]}",
        "ErrorMessage": "{json:error}"
    });
    let body = serde_json::to_vec_pretty(&config).unwrap_or_default();

    (
        [
            (axum::http::header::CONTENT_TYPE, "application/json".to_string()),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"klappstuhl.sxcu\"".to_string(),
            ),
        ],
        body,
    )
        .into_response()
}
