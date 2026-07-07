//! Public API for the URL shortener.
//!
//! These endpoints expose the same short-link store that powers the `/links`
//! web UI (see [`crate::site::links`]) over the JSON API, gated by the
//! `links:read` / `links:write` scopes. Validation, the per-account free-tier
//! cap, and code generation are shared with the web form so both surfaces behave
//! identically.

use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{
    auth::ApiToken,
    utils::{ApiJson as Json, Page, RateLimitResponse},
};
use crate::{
    error::ApiError,
    headers::ClientIp,
    models::{Account, Scope, ShortLink},
    site::links::{count_links, insert_link, normalize_target, validate_code, InsertError, FREE_LINK_LIMIT},
    utils::get_new_image_id,
    AppState,
};

/// A short link as returned by the API.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiShortLink {
    /// The short code / alias that appears in the URL.
    pub code: String,
    /// The fully-qualified short URL (e.g. `https://r.klappstuhl.me/ab12`).
    pub short_url: String,
    /// The destination the link redirects to.
    pub target_url: String,
    /// How many times the link has been resolved.
    pub clicks: i64,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
}

impl ApiShortLink {
    fn from_link(state: &AppState, link: ShortLink) -> Self {
        let short_url = state.config().short_link_url(&link.code);
        Self {
            short_url,
            code: link.code,
            target_url: link.target_url,
            clicks: link.clicks,
            created_at: link
                .created_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
        }
    }
}

/// Body of a create-link request.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateLinkBody {
    /// The destination URL. A missing scheme defaults to `https://`.
    pub url: String,
    /// An optional custom alias (`[A-Za-z0-9_-]`, ≤64 chars). Omit for a random
    /// code.
    #[serde(default)]
    pub code: Option<String>,
}

async fn account_or_401(state: &AppState, id: i64) -> Result<Account, ApiError> {
    state.get_account(id).await.ok_or_else(ApiError::unauthorized)
}

/// Create a short link
///
/// Creates a short link for the authenticated account. Non-admin accounts are
/// capped at 10 links (delete one to make room).
#[utoipa::path(
    post,
    path = "/links",
    request_body(content = CreateLinkBody, content_type = "application/json"),
    responses(
        (status = 200, description = "The created short link", body = ApiShortLink),
        (status = 400, description = "Invalid URL or alias", body = ApiError),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 403, description = "Missing the links:write scope, or link limit reached", body = ApiError),
        (status = 409, description = "The alias is already taken", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["links:write"])),
    tag = "links"
)]
pub async fn create_link(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Json(body): Json<CreateLinkBody>,
) -> Result<Json<ApiShortLink>, ApiError> {
    auth.require(Scope::LinksWrite)?;
    let account = account_or_401(&state, auth.id).await?;

    if !account.flags.is_admin() && count_links(&state, account.id).await >= FREE_LINK_LIMIT {
        return Err(ApiError::forbidden().with_message(format!(
            "short-link limit reached ({FREE_LINK_LIMIT}); delete one to create another"
        )));
    }

    let target = normalize_target(&body.url).map_err(|e| ApiError::validation("url", e))?;

    let alias = body.code.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let custom_alias = alias.is_some();
    let code = match alias {
        Some(a) => validate_code(a).map_err(|e| ApiError::validation("code", e))?,
        None => get_new_image_id(),
    };

    let final_code = insert_link(&state, code, &target, account.id, custom_alias)
        .await
        .map_err(|e| match e {
            InsertError::Taken => {
                ApiError::new("that alias is already taken").with_code(crate::error::ApiErrorCode::EntryAlreadyExists)
            }
            InsertError::Db => ApiError::new("could not create the short link"),
        })?;

    state
        .audit("link.create")
        .actor(&account)
        .target(final_code.clone())
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "via_api": true }))
        .fire();

    let link = fetch_owned_link(&state, &final_code, account.id)
        .await?
        .ok_or_else(|| ApiError::new("link vanished after creation"))?;
    Ok(Json(ApiShortLink::from_link(&state, link)))
}

/// List short links
///
/// Lists the authenticated account's short links, newest first, with
/// Discord-style cursor pagination.
#[utoipa::path(
    get,
    path = "/links",
    params(Page),
    responses(
        (status = 200, description = "The account's short links", body = [ApiShortLink]),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 403, description = "Missing the links:read scope", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["links:read"])),
    tag = "links"
)]
pub async fn list_links(
    State(state): State<AppState>,
    Query(page): Query<Page>,
    auth: ApiToken,
) -> Result<Json<Vec<ApiShortLink>>, ApiError> {
    auth.require(Scope::LinksRead)?;
    let account = account_or_401(&state, auth.id).await?;

    let limit = page.effective_limit() as i64;
    // Numbered params (?1 reused) drive keyset pagination over created_at DESC.
    // An unknown/foreign cursor code is forgiven (behaves as "no bound").
    let links: Vec<ShortLink> = state
        .database()
        .all(
            "SELECT * FROM short_link \
             WHERE account_id = ?1 \
               AND (?2 IS NULL \
                    OR NOT EXISTS (SELECT 1 FROM short_link WHERE code = ?2 AND account_id = ?1) \
                    OR created_at < (SELECT created_at FROM short_link WHERE code = ?2 AND account_id = ?1)) \
               AND (?3 IS NULL \
                    OR NOT EXISTS (SELECT 1 FROM short_link WHERE code = ?3 AND account_id = ?1) \
                    OR created_at > (SELECT created_at FROM short_link WHERE code = ?3 AND account_id = ?1)) \
             ORDER BY created_at DESC \
             LIMIT ?4",
            (account.id, page.after.clone(), page.before.clone(), limit),
        )
        .await
        .map_err(|_| ApiError::new("failed to list links"))?;

    Ok(Json(
        links.into_iter().map(|l| ApiShortLink::from_link(&state, l)).collect(),
    ))
}

/// Get a short link
#[utoipa::path(
    get,
    path = "/links/{code}",
    params(("code" = String, Path, description = "The link's short code / alias.")),
    responses(
        (status = 200, description = "The short link", body = ApiShortLink),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 403, description = "Missing the links:read scope", body = ApiError),
        (status = 404, description = "No such link owned by this account", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["links:read"])),
    tag = "links"
)]
pub async fn get_link(
    State(state): State<AppState>,
    Path(code): Path<String>,
    auth: ApiToken,
) -> Result<Json<ApiShortLink>, ApiError> {
    auth.require(Scope::LinksRead)?;
    let account = account_or_401(&state, auth.id).await?;

    let link = fetch_owned_link(&state, &code, account.id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no short link `{code}`")))?;
    Ok(Json(ApiShortLink::from_link(&state, link)))
}

/// Delete a short link
#[utoipa::path(
    delete,
    path = "/links/{code}",
    params(("code" = String, Path, description = "The link's short code / alias.")),
    responses(
        (status = 200, description = "The deleted short link", body = ApiShortLink),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 403, description = "Missing the links:write scope", body = ApiError),
        (status = 404, description = "No such link owned by this account", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["links:write"])),
    tag = "links"
)]
pub async fn delete_link(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Path(code): Path<String>,
    auth: ApiToken,
) -> Result<Json<ApiShortLink>, ApiError> {
    auth.require(Scope::LinksWrite)?;
    let account = account_or_401(&state, auth.id).await?;

    let link = fetch_owned_link(&state, &code, account.id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no short link `{code}`")))?;

    state
        .database()
        .execute("DELETE FROM short_link WHERE id = ?1", [link.id])
        .await
        .map_err(|_| ApiError::new("could not delete the short link"))?;

    state
        .audit("link.delete")
        .actor(&account)
        .target(link.code.clone())
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "via_api": true }))
        .fire();

    Ok(Json(ApiShortLink::from_link(&state, link)))
}

/// Loads a link by code only if the account owns it.
async fn fetch_owned_link(state: &AppState, code: &str, account_id: i64) -> Result<Option<ShortLink>, ApiError> {
    state
        .database()
        .get(
            "SELECT * FROM short_link WHERE code = ?1 AND account_id = ?2",
            (code.to_string(), account_id),
        )
        .await
        .map_err(|_| ApiError::new("failed to load link"))
}
