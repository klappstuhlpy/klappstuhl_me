use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::{error::ApiError, headers::ClientIp, models::Scope, AppState};

use super::auth::ApiToken;
use super::utils::{ApiJson, RateLimitResponse};

/// Maximum accepted source size (100 KB).
const MAX_CODE_BYTES: usize = 100 * 1024;

#[derive(Deserialize, ToSchema)]
pub struct CodeImageRequest {
    /// The source code to render.
    pub code: String,
    /// Language token or extension (e.g. `rust`, `py`, `js`). Falls back to
    /// plain text when omitted or unknown.
    #[serde(default)]
    pub language: Option<String>,
    /// Theme name (a syntect default such as `base16-ocean.dark`,
    /// `InspiredGitHub`, `Solarized (dark)`). Optional.
    #[serde(default)]
    pub theme: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct CodeQuery {
    #[serde(default)]
    share: Option<bool>,
}

/// Code to image
///
/// Render a syntax-highlighted "code screenshot" (Carbon-style) as an SVG.
///
/// The body is JSON with the `code` and optional `language`/`theme`. The
/// result is returned as `image/svg+xml`, or — with `?share=true` — as JSON
/// `{id, url, content_type}` carrying a short `/m/:id` link to the stored SVG.
#[utoipa::path(
    post,
    path = "/api/render/code",
    request_body = CodeImageRequest,
    responses(
        (status = 200, description = "The rendered SVG", content_type = "image/svg+xml", body = String),
        (status = 400, description = "Missing or oversized code", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(
        ("api_key" = ["images:read"])
    ),
    tag = "render"
)]
pub async fn render_code(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Query(query): Query<CodeQuery>,
    ApiJson(req): ApiJson<CodeImageRequest>,
) -> Result<Response, ApiError> {
    auth.require(Scope::ImagesRead)?;
    let account = state.get_account(auth.id).await.ok_or_else(ApiError::unauthorized)?;

    let code = req.code;
    if code.trim().is_empty() {
        return Err(ApiError::new("`code` is required"));
    }
    if code.len() > MAX_CODE_BYTES {
        return Err(ApiError::new("code too large (100 KB max)"));
    }
    let language = req.language.unwrap_or_default();
    let theme = req.theme.unwrap_or_else(|| crate::codeimage::DEFAULT_THEME.to_string());

    let svg = tokio::task::spawn_blocking(move || crate::codeimage::render_svg(&code, &language, &theme))
        .await
        .map_err(|_| ApiError::new("render task failed"))?
        .map_err(|e| ApiError::new(format!("render failed: {e}")))?;

    state.audit("api.render.code").actor(&account).ip_opt(client_ip).fire();

    if query.share.unwrap_or(false) {
        let id = state.store_media(svg.into_bytes(), "image/svg+xml");
        let url = state.config().url_to(format!("/m/{id}"));
        return Ok(ApiJson(serde_json::json!({
            "id": id,
            "url": url,
            "content_type": "image/svg+xml",
        }))
        .into_response());
    }

    Ok(([(header::CONTENT_TYPE, "image/svg+xml".to_string())], svg).into_response())
}
