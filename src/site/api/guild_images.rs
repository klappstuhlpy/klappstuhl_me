//! Guild-scoped image galleries.
//!
//! **⚠️ Internal — not for public use.** These endpoints exist for Percy's bot
//! and its dashboard, and are documented here only for reference. They are gated
//! by the dedicated [`Scope::GuildImages`] (`images:guild`) scope, which is never
//! granted to a normal personal API key (see [`Scope::requires_admin`]); the
//! keys that use them are minted per guild by [`AppState::ensure_guild_api_key`]
//! and are not issued to end users. Do not build against these.
//!
//! They let a trusted service key (Percy's bot) manage a shared per-Discord-guild
//! image gallery: images uploaded here are tagged with the guild's snowflake so
//! the bot (poll banners) and the dashboard see one coherent set. The caller is
//! responsible for authorising the acting guild (Percy checks command
//! permissions; the dashboard checks the OAuth manage-permission before proxying
//! here).

use axum::extract::{Multipart, Path, Query, State};
use serde::Serialize;
use utoipa::ToSchema;

use super::{
    auth::ApiToken,
    utils::{ApiJson as Json, Page, RateLimitResponse},
};
use crate::{
    error::ApiError,
    filters::canonical_url,
    headers::ClientIp,
    models::Scope,
    site::image::{expiry_from_params, raw_upload_file, DeleteResult, UploadParams, UploadResult},
    AppState,
};

/// A single image in a guild's gallery (metadata only — the bytes are served
/// from the canonical `/gallery` URLs).
#[derive(Debug, Serialize, ToSchema)]
pub struct GuildImageInfo {
    /// The bare image id (no extension).
    pub id: String,
    /// The file extension derived from the mimetype (e.g. `png`).
    pub ext: String,
    /// The mime type of the image.
    #[schema(example = "image/png")]
    pub mimetype: String,
    /// The file size in bytes.
    pub size: i64,
    /// The uploader's original filename, if recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_name: Option<String>,
    /// Upload timestamp, as stored (RFC3339 / SQLite datetime text).
    pub uploaded_at: String,
    /// Canonical landing-page URL (`/gallery/{id}.{ext}`).
    pub url: String,
    /// Raw-bytes URL (`/gallery/raw/{id}.{ext}`).
    pub raw_url: String,
}

/// The listing of a guild's gallery.
#[derive(Debug, Serialize, ToSchema)]
pub struct GuildImagesResult {
    /// The images in the guild's gallery, newest first.
    pub images: Vec<GuildImageInfo>,
    /// The number of images returned.
    pub total: usize,
}

#[derive(ToSchema)]
#[allow(dead_code)]
struct UploadedFiles {
    #[schema(format = Binary)]
    file: Vec<String>,
}

/// Upload to a guild gallery
///
/// > **⚠️ Internal — not for public use.** Documented for reference only; served
/// > to Percy's per-guild service keys, not to public API keys.
///
/// Upload one or more images into a Discord guild's shared gallery. Identical to
/// the personal `/images/upload` endpoint, but every uploaded row is tagged with
/// `guild_id` so it appears in the guild's gallery on the dashboard.
#[utoipa::path(
    post,
    path = "/guilds/{guild_id}/images/upload",
    params(
        ("guild_id" = String, Path, description = "The Discord guild snowflake to upload for."),
        ("expires_in" = Option<i64>, Query, description = "Optional time-to-live in seconds (max 365 days)."),
    ),
    request_body(
        content = inline(UploadedFiles),
        content_type = "multipart/form-data",
        description = "The image files to upload."
    ),
    responses(
        (status = 200, description = "Upload processed", body = UploadResult),
        (status = 400, description = "An error occurred", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 403, description = "The API key is missing the images:guild scope", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["images:guild"])),
    tag = "images"
)]
pub async fn upload_guild_images(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Path(guild_id): Path<String>,
    Query(params): Query<UploadParams>,
    auth: ApiToken,
    multipart: Multipart,
) -> Result<Json<UploadResult>, ApiError> {
    auth.require(Scope::GuildImages)?;
    let Some(account) = state.get_account(auth.id).await else {
        return Err(ApiError::unauthorized());
    };

    let expires_at = expiry_from_params(&params);
    let result = raw_upload_file(state, account, client_ip, multipart, true, expires_at, Some(guild_id)).await?;
    if result.is_error() {
        return Err(ApiError::new("Upload failed"));
    }
    Ok(Json(result))
}

/// List a guild gallery
///
/// > **⚠️ Internal — not for public use.** Documented for reference only; served
/// > to Percy's per-guild service keys, not to public API keys.
///
/// List the images in a Discord guild's shared gallery, newest first. Expired
/// uploads are omitted. Supports Discord-style cursor pagination via
/// `limit`/`before`/`after` (see the [`Page`] parameters).
#[utoipa::path(
    get,
    path = "/guilds/{guild_id}/images",
    params(
        ("guild_id" = String, Path, description = "The Discord guild snowflake."),
        Page,
    ),
    responses(
        (status = 200, description = "The guild's gallery", body = GuildImagesResult),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 403, description = "The API key is missing the images:guild scope", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["images:guild"])),
    tag = "images"
)]
pub async fn list_guild_images(
    State(state): State<AppState>,
    Path(guild_id): Path<String>,
    Query(page): Query<Page>,
    auth: ApiToken,
) -> Result<Json<GuildImagesResult>, ApiError> {
    auth.require(Scope::GuildImages)?;

    let limit = page.effective_limit();
    let after = page.after.clone();
    let before = page.before.clone();

    let rows = state
        .database()
        .call(
            move |conn| -> rusqlite::Result<Vec<(String, String, i64, Option<String>, String)>> {
                // Keyset pagination over (uploaded_at DESC). The cursors are image
                // ids resolved to their `uploaded_at` via a scalar subquery:
                // `after` walks to older rows, `before` to newer ones. An unknown
                // cursor id (no such row) is forgiven — the `NOT EXISTS` guard
                // makes it behave as "no bound" instead of erroring or returning
                // an empty page.
                let mut stmt = conn.prepare(
                    "SELECT id, mimetype, size, original_name, uploaded_at FROM images \
                     WHERE guild_id = :guild \
                       AND (expires_at IS NULL OR datetime(expires_at) > datetime('now')) \
                       AND (:after IS NULL \
                            OR NOT EXISTS (SELECT 1 FROM images WHERE id = :after) \
                            OR uploaded_at < (SELECT uploaded_at FROM images WHERE id = :after)) \
                       AND (:before IS NULL \
                            OR NOT EXISTS (SELECT 1 FROM images WHERE id = :before) \
                            OR uploaded_at > (SELECT uploaded_at FROM images WHERE id = :before)) \
                     ORDER BY uploaded_at DESC \
                     LIMIT :limit",
                )?;
                let rows = stmt
                    .query_map(
                        rusqlite::named_params! {
                            ":guild": guild_id,
                            ":after": after,
                            ":before": before,
                            ":limit": limit,
                        },
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            },
        )
        .await
        .map_err(|_| ApiError::new("Failed to list gallery"))?;

    let images: Vec<GuildImageInfo> = rows
        .into_iter()
        .map(|(id, mimetype, size, original_name, uploaded_at)| {
            let ext = mimetype.split('/').last().unwrap_or("png").to_string();
            let url = canonical_url(format!("/gallery/{id}.{ext}")).unwrap_or_default();
            let raw_url = canonical_url(format!("/gallery/raw/{id}.{ext}")).unwrap_or_default();
            GuildImageInfo {
                id,
                ext,
                mimetype,
                size,
                original_name,
                uploaded_at,
                url,
                raw_url,
            }
        })
        .collect();

    let total = images.len();
    Ok(Json(GuildImagesResult { images, total }))
}

/// Delete from a guild gallery
///
/// > **⚠️ Internal — not for public use.** Documented for reference only; served
/// > to Percy's per-guild service keys, not to public API keys.
///
/// Delete an image from a Discord guild's shared gallery. Scoped by `guild_id`,
/// so a key may only remove images that belong to the guild it was asked to act
/// for.
#[utoipa::path(
    delete,
    path = "/guilds/{guild_id}/images/{id}",
    params(
        ("guild_id" = String, Path, description = "The Discord guild snowflake."),
        ("id" = String, Path, description = "The image's id (extension optional)."),
    ),
    responses(
        (status = 200, description = "Successfully deleted image", body = DeleteResult),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 403, description = "The API key is missing the images:guild scope", body = ApiError),
        (status = 404, description = "Image not found in this guild's gallery", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["images:guild"])),
    tag = "images"
)]
pub async fn delete_guild_image(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Path((guild_id, id)): Path<(String, String)>,
    auth: ApiToken,
) -> Result<Json<DeleteResult>, ApiError> {
    auth.require(Scope::GuildImages)?;
    let Some(account) = state.get_account(auth.id).await else {
        return Err(ApiError::unauthorized());
    };

    // Accept both `abc123` and `abc123.png`.
    let bare = id.split('.').next().unwrap_or(&id).to_string();

    let affected = state
        .database()
        .execute(
            "DELETE FROM images WHERE guild_id = ? AND id = ?",
            (guild_id.clone(), bare.clone()),
        )
        .await
        .map_err(|_| ApiError::new("Delete failed"))?;

    if affected == 0 {
        return Err(ApiError::not_found(format!(
            "Image `{bare}` was not found in this guild's gallery"
        )));
    }

    state.invalidate_image_caches().await;

    state
        .audit("image.guild.delete")
        .actor(&account)
        .target(bare.clone())
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "guild_id": guild_id, "via_api": true }))
        .fire();

    Ok(Json(DeleteResult {
        file: bare,
        failed: false,
    }))
}

/// Response of the guild-key provisioning endpoint.
#[derive(Debug, Serialize)]
pub struct GuildKeyResponse {
    /// The guild's `images:guild` API key (get-or-created).
    pub token: String,
}

/// Provision (get-or-create) a guild's `images:guild` API key.
///
/// Internal, undocumented endpoint the bot uses so it never needs a personal
/// key: it presents the shared `gallery_provision_token` (matched constant-time)
/// and receives the narrow, per-guild key. The host mints that key under a
/// dedicated non-personal service account and stores the guild → key mapping in
/// `guild_api_key`; repeat calls return the same key until it is revoked. When
/// the feature is unconfigured we answer `401` (indistinguishable from a bad
/// credential) rather than reveal that.
pub async fn provision_guild_key(
    State(state): State<AppState>,
    Path(guild_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<GuildKeyResponse>, ApiError> {
    let Some(expected) = state.config().gallery_provision_token.clone().filter(|t| !t.is_empty()) else {
        return Err(ApiError::unauthorized());
    };
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.strip_prefix("Bearer ").unwrap_or(s).trim())
        .unwrap_or_default();
    if !ct_eq(provided.as_bytes(), expected.as_bytes()) {
        return Err(ApiError::unauthorized());
    }

    let token = state
        .ensure_guild_api_key(&guild_id)
        .await
        .map_err(|_| ApiError::new("failed to provision guild key"))?;
    Ok(Json(GuildKeyResponse { token }))
}

/// Constant-time byte comparison for the provisioning bearer, so a mismatch
/// can't be recovered from response timing. The length check is a negligible
/// side channel for a high-entropy token.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::ct_eq;

    #[test]
    fn ct_eq_matches_exact_and_rejects_everything_else() {
        assert!(ct_eq(b"secret-token", b"secret-token"));
        assert!(!ct_eq(b"secret-token", b"secret-toke")); // shorter
        assert!(!ct_eq(b"secret-token", b"secret-token!")); // longer
        assert!(!ct_eq(b"secret-token", b"Secret-token")); // one byte off
        assert!(!ct_eq(b"", b"x"));
        assert!(ct_eq(b"", b"")); // both empty compare equal (caller rejects empty separately)
    }
}
