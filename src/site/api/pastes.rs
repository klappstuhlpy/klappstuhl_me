//! Public API for the paste host.
//!
//! Create, read, list, edit, fork and delete hosted text/code pastes, gated by
//! the `pastes:read` / `pastes:write` scopes. The same pastes are viewable
//! without auth at `/p/<id>` (highlighted), `/p/<id>.txt` (raw), and manageable
//! in a browser at `/paste` / `/pastes` — see [`crate::site::paste`].
//!
//! **This module owns no rules.** Validation, quotas, secret scanning, encryption
//! and the audit trail all live in [`crate::site::paste::service`], which the web
//! handlers call too — so the API and the browser cannot drift apart on what a
//! legal paste is. What is left here is the HTTP shell: scopes, request/response
//! shapes, and the OpenAPI documentation.

use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use super::{
    auth::ApiToken,
    utils::{ApiJson as Json, Page, RateLimitResponse},
};
use crate::site::paste::crypto;
use crate::site::paste::service::{self, Actor, Creator, EditPaste, NewPaste, PasteError};
use crate::{
    error::ApiError,
    headers::ClientIp,
    models::{Paste, Scope, Visibility},
    AppState,
};

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
    ///
    /// **Omitted for a password-protected paste** unless the request supplies the
    /// password (`?password=…`). An encrypted body is ciphertext; the API will
    /// not hand it over, and will not pretend it is text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// The paste's title, if it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The language token used for highlighting, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// `public`, `unlisted` (the default) or `private`.
    pub visibility: Visibility,
    /// Whether the paste is destroyed the first time it is explicitly revealed.
    pub burn_after_read: bool,
    /// Whether the body is password-encrypted at rest.
    pub encrypted: bool,
    /// Size of the stored body, in bytes.
    pub size_bytes: i64,
    /// The paste this one was forked from, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_of: Option<String>,
    /// How many times the paste has been viewed.
    pub views: i64,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
    /// Last-edit timestamp (RFC 3339), if it has ever been edited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Expiry timestamp (RFC 3339), if the paste auto-deletes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// The one-time edit token for an anonymous paste. Present **only** in the
    /// response that created it — it is stored as a hash and cannot be shown
    /// again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit_token: Option<String>,
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

impl ApiPaste {
    /// Builds the response for a paste whose plaintext the caller may see.
    fn new(state: &AppState, p: Paste, content: Option<String>, edit_token: Option<String>) -> Self {
        Self {
            url: state.config().url_to(format!("/p/{}", p.id)),
            raw_url: state.config().url_to(format!("/p/{}.txt", p.id)),
            encrypted: p.is_encrypted(),
            id: p.id,
            content,
            title: p.title,
            language: p.language,
            visibility: p.visibility,
            burn_after_read: p.burn_after_read,
            size_bytes: p.size_bytes,
            fork_of: p.fork_of,
            views: p.views,
            created_at: rfc3339(p.created_at),
            updated_at: p.updated_at.map(rfc3339),
            expires_at: p.expires_at.map(rfc3339),
            edit_token,
        }
    }

    /// The plaintext body, when the API may return one: never for an encrypted
    /// paste (unless `password` opens it), and never for a burn paste — reading
    /// one is an explicit, destructive act, not something a `GET` does.
    fn body_for(paste: &Paste, password: Option<&str>) -> Option<String> {
        if paste.burn_after_read {
            return None;
        }
        if paste.is_encrypted() {
            let password = password?;
            let salt = paste.enc_salt.as_deref()?;
            let nonce = paste.enc_nonce.as_deref()?;
            return crypto::open(password, salt, nonce, &paste.content).and_then(|b| String::from_utf8(b).ok());
        }
        paste.text().map(str::to_string)
    }

    fn from_paste(state: &AppState, paste: Paste, password: Option<&str>) -> Self {
        let content = Self::body_for(&paste, password);
        Self::new(state, paste, content, None)
    }
}

/// Maps a service refusal onto the API's error shape.
fn api_error(error: PasteError) -> ApiError {
    match error {
        PasteError::NotFound => ApiError::not_found(error.message()),
        PasteError::Empty | PasteError::TooLarge(_) => ApiError::validation("content", error.message()),
        PasteError::TitleTooLong => ApiError::validation("title", error.message()),
        PasteError::BadPassword => ApiError::validation("password", error.message()),
        PasteError::AnonymousDisabled => ApiError::forbidden(),
        other => ApiError::new(other.message()),
    }
}

/// Body of a create-paste request.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreatePasteBody {
    /// The paste content.
    pub content: String,
    /// Optional title.
    #[serde(default)]
    pub title: Option<String>,
    /// Optional language token / extension for syntax highlighting (`rust`,
    /// `py`, `js`, …).
    #[serde(default)]
    pub language: Option<String>,
    /// `public`, `unlisted` (the default) or `private`. Only `public` pastes are
    /// indexable and listed on your profile; the others are link-only.
    #[serde(default)]
    pub visibility: Option<Visibility>,
    /// Destroy the paste the first time it is explicitly revealed.
    #[serde(default)]
    pub burn_after_read: bool,
    /// Encrypt the body with this password (Argon2id + ChaCha20-Poly1305). The
    /// password is never stored — lose it and the paste is unreadable.
    #[serde(default)]
    pub password: Option<String>,
    /// Optional time-to-live in seconds; the paste is deleted afterwards
    /// (capped at 365 days). Omit for a permanent paste.
    #[serde(default)]
    pub expires_in: Option<i64>,
    /// Publish even though the body trips the secret scanner.
    #[serde(default)]
    pub confirm_secrets: bool,
}

/// Body of an edit-paste request.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdatePasteBody {
    /// The new content.
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub visibility: Option<Visibility>,
    /// New TTL in seconds, from now. Omit to make the paste permanent.
    #[serde(default)]
    pub expires_in: Option<i64>,
    /// Required when the paste is password-protected: the new body is re-sealed
    /// under the same password, and there is no way to do that without it.
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub confirm_secrets: bool,
}

/// The password for reading an encrypted paste.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub struct PasswordQuery {
    /// The paste's password, for a password-protected paste. Without it, the
    /// `content` field is omitted from the response.
    #[serde(default)]
    pub password: Option<String>,
}

/// Create a paste
#[utoipa::path(
    post,
    path = "/pastes",
    request_body(content = CreatePasteBody, content_type = "application/json"),
    responses(
        (status = 200, description = "The created paste", body = ApiPaste),
        (status = 400, description = "Empty, oversized, over quota, or carrying a detected secret", body = ApiError),
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
    let account = auth.require_account(&state, Scope::PastesWrite).await?;

    let password = body.password.clone();
    let created = service::create(
        &state,
        Creator::Account(&account),
        client_ip,
        NewPaste {
            content: body.content,
            title: body.title,
            language: body.language,
            visibility: body.visibility.unwrap_or_default(),
            burn_after_read: body.burn_after_read,
            password: body.password,
            expires_in: body.expires_in,
            fork_of: None,
            confirm_secrets: body.confirm_secrets,
        },
    )
    .await
    .map_err(api_error)?;

    Ok(Json(ApiPaste::from_paste(&state, created.paste, password.as_deref())))
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
    let account = auth.require_account(&state, Scope::PastesRead).await?;

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
        pastes
            .into_iter()
            .map(|p| ApiPaste::from_paste(&state, p, None))
            .collect(),
    ))
}

/// Get a paste
#[utoipa::path(
    get,
    path = "/pastes/{id}",
    params(("id" = String, Path, description = "The paste's id."), PasswordQuery),
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
    Query(query): Query<PasswordQuery>,
    auth: ApiToken,
) -> Result<Json<ApiPaste>, ApiError> {
    let account = auth.require_account(&state, Scope::PastesRead).await?;
    let paste = service::load_for(&state, &id, &Actor::account(&account))
        .await
        .map_err(api_error)?;
    Ok(Json(ApiPaste::from_paste(&state, paste, query.password.as_deref())))
}

/// Edit a paste
#[utoipa::path(
    patch,
    path = "/pastes/{id}",
    params(("id" = String, Path, description = "The paste's id.")),
    request_body(content = UpdatePasteBody, content_type = "application/json"),
    responses(
        (status = 200, description = "The updated paste", body = ApiPaste),
        (status = 400, description = "Empty, oversized, or carrying a detected secret", body = ApiError),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 403, description = "Missing the pastes:write scope", body = ApiError),
        (status = 404, description = "No such paste owned by this account", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["pastes:write"])),
    tag = "pastes"
)]
pub async fn update_paste(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Path(id): Path<String>,
    auth: ApiToken,
    Json(body): Json<UpdatePasteBody>,
) -> Result<Json<ApiPaste>, ApiError> {
    let account = auth.require_account(&state, Scope::PastesWrite).await?;
    let actor = Actor::account(&account);

    let paste = service::load_for(&state, &id, &actor).await.map_err(api_error)?;
    let password = body.password.clone();

    let updated = service::edit(
        &state,
        &paste,
        &actor,
        client_ip,
        EditPaste {
            content: body.content,
            title: body.title,
            language: body.language,
            visibility: body.visibility,
            expires_in: body.expires_in,
            password: body.password,
            confirm_secrets: body.confirm_secrets,
        },
    )
    .await
    .map_err(api_error)?;

    Ok(Json(ApiPaste::from_paste(&state, updated, password.as_deref())))
}

/// Fork a paste
///
/// Copies a paste's contents into a new one owned by the caller. The fork is a
/// fresh, independent paste: it inherits neither the original's password nor its
/// burn flag.
#[utoipa::path(
    post,
    path = "/pastes/{id}/fork",
    params(("id" = String, Path, description = "The paste to fork."), PasswordQuery),
    responses(
        (status = 200, description = "The new paste", body = ApiPaste),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 403, description = "Missing the pastes:write scope", body = ApiError),
        (status = 404, description = "No such paste, or its contents are not readable", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["pastes:write"])),
    tag = "pastes"
)]
pub async fn fork_paste(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Path(id): Path<String>,
    Query(query): Query<PasswordQuery>,
    auth: ApiToken,
) -> Result<Json<ApiPaste>, ApiError> {
    let account = auth.require_account(&state, Scope::PastesWrite).await?;

    // Any paste can be forked, not just your own — that is what makes it useful.
    // But forking needs the *plaintext*, so an encrypted source needs its
    // password, and a burn source cannot be forked at all: forking it would be a
    // read that dodges the burn.
    let source = service::load(&state, &id)
        .await
        .ok_or_else(|| ApiError::not_found(format!("no paste `{id}`")))?;

    let plaintext = ApiPaste::body_for(&source, query.password.as_deref())
        .ok_or_else(|| ApiError::not_found(format!("the contents of `{id}` are not readable")))?;

    let created = service::fork(&state, &source, &plaintext, Creator::Account(&account), client_ip)
        .await
        .map_err(api_error)?;

    Ok(Json(ApiPaste::from_paste(&state, created.paste, None)))
}

/// A superseded version of a paste's body.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiRevision {
    /// The revision's id.
    pub id: i64,
    /// The body as it was.
    pub content: String,
    /// The title as it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The language as it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// When this version was superseded (RFC 3339).
    pub created_at: String,
}

/// List a paste's revisions
///
/// Newest first, capped at the last 20 — older ones are pruned by the hourly
/// reaper.
#[utoipa::path(
    get,
    path = "/pastes/{id}/revisions",
    params(("id" = String, Path, description = "The paste's id.")),
    responses(
        (status = 200, description = "The paste's revision history", body = [ApiRevision]),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 403, description = "Missing the pastes:read scope", body = ApiError),
        (status = 404, description = "No such paste owned by this account", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["pastes:read"])),
    tag = "pastes"
)]
pub async fn list_revisions(
    State(state): State<AppState>,
    Path(id): Path<String>,
    auth: ApiToken,
) -> Result<Json<Vec<ApiRevision>>, ApiError> {
    let account = auth.require_account(&state, Scope::PastesRead).await?;
    let paste = service::load_for(&state, &id, &Actor::account(&account))
        .await
        .map_err(api_error)?;

    // An encrypted paste's history is ciphertext, and the API does not hand
    // ciphertext out dressed as text.
    if paste.is_encrypted() {
        return Err(ApiError::not_found(
            "the revisions of an encrypted paste are not readable",
        ));
    }

    let revisions = service::revisions(&state, &paste.id)
        .await
        .into_iter()
        .map(|r| ApiRevision {
            id: r.id,
            content: String::from_utf8(r.content).unwrap_or_default(),
            title: r.title,
            language: r.language,
            created_at: rfc3339(r.created_at),
        })
        .collect();

    Ok(Json(revisions))
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
    let account = auth.require_account(&state, Scope::PastesWrite).await?;
    let actor = Actor::account(&account);

    let paste = service::load_for(&state, &id, &actor).await.map_err(api_error)?;
    service::delete(&state, &paste, &actor, client_ip)
        .await
        .map_err(api_error)?;

    Ok(Json(ApiPaste::from_paste(&state, paste, None)))
}
