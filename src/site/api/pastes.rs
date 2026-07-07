//! Public API for the paste host.
//!
//! Create, read, list, and delete hosted text/code pastes, gated by the
//! `pastes:read` / `pastes:write` scopes. The paste bodies are additionally
//! viewable, without auth, at `/p/<id>` (highlighted) and `/p/<id>.txt` (raw) —
//! see [`crate::site::paste`].

use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use utoipa::ToSchema;

use super::{
    auth::ApiToken,
    utils::{ApiJson as Json, Page, RateLimitResponse},
};
use crate::{
    error::ApiError,
    headers::ClientIp,
    models::{Account, Paste, Scope},
    utils::get_new_image_id,
    AppState,
};

/// Maximum paste body size (512 KB).
const MAX_PASTE_BYTES: usize = 512 * 1024;
/// Maximum lifetime of an expiring paste (365 days, matching image uploads).
const MAX_TTL_SECS: i64 = 365 * 24 * 60 * 60;

/// A paste as returned by the API.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiPaste {
    /// The paste's short id.
    pub id: String,
    /// The highlighted landing-page URL (`/p/{id}`).
    pub url: String,
    /// The raw-text URL (`/p/{id}.txt`).
    pub raw_url: String,
    /// The paste body.
    pub content: String,
    /// The language token used for highlighting, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// How many times the paste has been viewed.
    pub views: i64,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
    /// Expiry timestamp (RFC 3339), if the paste auto-deletes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl ApiPaste {
    fn from_paste(state: &AppState, p: Paste) -> Self {
        let rfc3339 = |t: OffsetDateTime| {
            t.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        };
        Self {
            url: state.config().url_to(format!("/p/{}", p.id)),
            raw_url: state.config().url_to(format!("/p/{}.txt", p.id)),
            id: p.id,
            content: p.content,
            language: p.language,
            views: p.views,
            created_at: rfc3339(p.created_at),
            expires_at: p.expires_at.map(rfc3339),
        }
    }
}

/// Body of a create-paste request.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreatePasteBody {
    /// The paste content.
    pub content: String,
    /// Optional language token / extension for syntax highlighting (`rust`,
    /// `py`, `js`, …).
    #[serde(default)]
    pub language: Option<String>,
    /// Optional time-to-live in seconds; the paste is deleted afterwards
    /// (capped at 365 days). Omit for a permanent paste.
    #[serde(default)]
    pub expires_in: Option<i64>,
}

async fn account_or_401(state: &AppState, id: i64) -> Result<Account, ApiError> {
    state.get_account(id).await.ok_or_else(ApiError::unauthorized)
}

/// Create a paste
#[utoipa::path(
    post,
    path = "/pastes",
    request_body(content = CreatePasteBody, content_type = "application/json"),
    responses(
        (status = 200, description = "The created paste", body = ApiPaste),
        (status = 400, description = "Empty or oversized content", body = ApiError),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 403, description = "Missing the pastes:write scope", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["pastes:write"])),
    tag = "pastes"
)]
pub async fn create_paste(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Json(body): Json<CreatePasteBody>,
) -> Result<Json<ApiPaste>, ApiError> {
    auth.require(Scope::PastesWrite)?;
    let account = account_or_401(&state, auth.id).await?;

    if body.content.trim().is_empty() {
        return Err(ApiError::validation("content", "`content` is required"));
    }
    if body.content.len() > MAX_PASTE_BYTES {
        return Err(ApiError::validation("content", "paste too large (512 KB max)"));
    }

    let language = body.language.map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
    let expires_at: Option<OffsetDateTime> = body.expires_in.filter(|s| *s > 0).map(|secs| {
        let secs = secs.min(MAX_TTL_SECS);
        OffsetDateTime::now_utc() + Duration::seconds(secs)
    });

    let id = get_new_image_id();
    state
        .database()
        .execute(
            "INSERT INTO paste (id, account_id, content, language, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            (id.clone(), account.id, body.content, language, expires_at),
        )
        .await
        .map_err(|_| ApiError::new("could not create the paste"))?;

    state
        .audit("paste.create")
        .actor(&account)
        .target(id.clone())
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "via_api": true }))
        .fire();

    let paste = fetch_owned_paste(&state, &id, account.id)
        .await?
        .ok_or_else(|| ApiError::new("paste vanished after creation"))?;
    Ok(Json(ApiPaste::from_paste(&state, paste)))
}

/// List pastes
#[utoipa::path(
    get,
    path = "/pastes",
    params(Page),
    responses(
        (status = 200, description = "The account's pastes", body = [ApiPaste]),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 403, description = "Missing the pastes:read scope", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["pastes:read"])),
    tag = "pastes"
)]
pub async fn list_pastes(
    State(state): State<AppState>,
    Query(page): Query<Page>,
    auth: ApiToken,
) -> Result<Json<Vec<ApiPaste>>, ApiError> {
    auth.require(Scope::PastesRead)?;
    let account = account_or_401(&state, auth.id).await?;

    let limit = page.effective_limit() as i64;
    let pastes: Vec<Paste> = state
        .database()
        .all(
            "SELECT * FROM paste \
             WHERE account_id = ?1 \
               AND (expires_at IS NULL OR datetime(expires_at) > datetime('now')) \
               AND (?2 IS NULL \
                    OR NOT EXISTS (SELECT 1 FROM paste WHERE id = ?2 AND account_id = ?1) \
                    OR created_at < (SELECT created_at FROM paste WHERE id = ?2 AND account_id = ?1)) \
               AND (?3 IS NULL \
                    OR NOT EXISTS (SELECT 1 FROM paste WHERE id = ?3 AND account_id = ?1) \
                    OR created_at > (SELECT created_at FROM paste WHERE id = ?3 AND account_id = ?1)) \
             ORDER BY created_at DESC \
             LIMIT ?4",
            (account.id, page.after.clone(), page.before.clone(), limit),
        )
        .await
        .map_err(|_| ApiError::new("failed to list pastes"))?;

    Ok(Json(
        pastes.into_iter().map(|p| ApiPaste::from_paste(&state, p)).collect(),
    ))
}

/// Get a paste
#[utoipa::path(
    get,
    path = "/pastes/{id}",
    params(("id" = String, Path, description = "The paste's id.")),
    responses(
        (status = 200, description = "The paste", body = ApiPaste),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 403, description = "Missing the pastes:read scope", body = ApiError),
        (status = 404, description = "No such paste owned by this account", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["pastes:read"])),
    tag = "pastes"
)]
pub async fn get_paste(
    State(state): State<AppState>,
    Path(id): Path<String>,
    auth: ApiToken,
) -> Result<Json<ApiPaste>, ApiError> {
    auth.require(Scope::PastesRead)?;
    let account = account_or_401(&state, auth.id).await?;

    let paste = fetch_owned_paste(&state, &id, account.id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no paste `{id}`")))?;
    Ok(Json(ApiPaste::from_paste(&state, paste)))
}

/// Delete a paste
#[utoipa::path(
    delete,
    path = "/pastes/{id}",
    params(("id" = String, Path, description = "The paste's id.")),
    responses(
        (status = 200, description = "The deleted paste", body = ApiPaste),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 403, description = "Missing the pastes:write scope", body = ApiError),
        (status = 404, description = "No such paste owned by this account", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["pastes:write"])),
    tag = "pastes"
)]
pub async fn delete_paste(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Path(id): Path<String>,
    auth: ApiToken,
) -> Result<Json<ApiPaste>, ApiError> {
    auth.require(Scope::PastesWrite)?;
    let account = account_or_401(&state, auth.id).await?;

    let paste = fetch_owned_paste(&state, &id, account.id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no paste `{id}`")))?;

    state
        .database()
        .execute("DELETE FROM paste WHERE id = ?1", [paste.id.clone()])
        .await
        .map_err(|_| ApiError::new("could not delete the paste"))?;

    state
        .audit("paste.delete")
        .actor(&account)
        .target(paste.id.clone())
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "via_api": true }))
        .fire();

    Ok(Json(ApiPaste::from_paste(&state, paste)))
}

/// Loads a paste by id only if the account owns it and it hasn't expired.
async fn fetch_owned_paste(state: &AppState, id: &str, account_id: i64) -> Result<Option<Paste>, ApiError> {
    state
        .database()
        .get(
            "SELECT * FROM paste WHERE id = ?1 AND account_id = ?2 \
             AND (expires_at IS NULL OR datetime(expires_at) > datetime('now'))",
            (id.to_string(), account_id),
        )
        .await
        .map_err(|_| ApiError::new("failed to load paste"))
}
