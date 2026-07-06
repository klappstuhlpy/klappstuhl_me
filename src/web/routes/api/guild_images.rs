//! Guild-scoped image galleries.
//!
//! These endpoints let a trusted service key (Percy's bot) manage a shared
//! per-Discord-guild image gallery: images uploaded here are tagged with the
//! guild's snowflake so the bot (poll banners) and the dashboard see one
//! coherent set. They are gated by the dedicated [`Scope::GuildImages`]
//! (`images:guild`) scope; the caller is responsible for authorising the acting
//! guild (Percy checks command permissions; the dashboard checks the OAuth
//! manage-permission before proxying here).

use axum::extract::{Multipart, Path, Query, State};
use serde::Serialize;
use utoipa::ToSchema;

use super::{
    auth::ApiToken,
    utils::{ApiJson as Json, RateLimitResponse},
};
use crate::{
    error::ApiError,
    filters::canonical_url,
    headers::ClientIp,
    models::Scope,
    routes::image::{expiry_from_params, raw_upload_file, DeleteResult, UploadParams, UploadResult},
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
/// List the images in a Discord guild's shared gallery, newest first. Expired
/// uploads are omitted.
#[utoipa::path(
    get,
    path = "/guilds/{guild_id}/images",
    params(("guild_id" = String, Path, description = "The Discord guild snowflake.")),
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
    auth: ApiToken,
) -> Result<Json<GuildImagesResult>, ApiError> {
    auth.require(Scope::GuildImages)?;

    let rows = state
        .database()
        .call(
            move |conn| -> rusqlite::Result<Vec<(String, String, i64, Option<String>, String)>> {
                let mut stmt = conn.prepare(
                    "SELECT id, mimetype, size, original_name, uploaded_at FROM images \
                 WHERE guild_id = ? AND (expires_at IS NULL OR datetime(expires_at) > datetime('now')) \
                 ORDER BY uploaded_at DESC",
                )?;
                let rows = stmt
                    .query_map([guild_id], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })?
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
