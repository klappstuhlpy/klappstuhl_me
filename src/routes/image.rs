//! Routes for image upload, viewing, deletion, and raw serving.

use crate::error::{ApiError, InternalError};
use crate::flash::{FlashMessage, Flasher, Flashes};
use crate::models::{Account, ImageEntry, ImageFile, ResolvedImageData};
use crate::{database::is_unique_constraint_violation, AppState, filters};
use askama::Template;
use axum::extract::multipart::Field;
use axum::extract::Multipart;
use axum::http::{header, StatusCode};
use axum::routing::{delete, get, post};
use axum::{
    extract::{Path, State},
    response::{IntoResponse, Redirect, Response},
    Json, Router,
};
use base64::engine::general_purpose;
use base64::Engine;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use crate::filters::canonical_url;
use crate::headers::Referrer;
use crate::ratelimit::RateLimit;
use crate::utils::get_new_image_id;

// ---------------------------------------------------------------------------
// Allowed MIME types
// ---------------------------------------------------------------------------

const ALLOWED_EXTENSIONS: &[&str] = &["apng", "png", "jpg", "jpeg", "gif", "avif"];

fn is_allowed_extension(ext: &str) -> bool {
    ALLOWED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

// ---------------------------------------------------------------------------
// Upload helpers
// ---------------------------------------------------------------------------

/// A single validated file ready for insertion into the database.
struct ValidatedFile {
    /// The randomly generated image ID (no extension).
    id: String,
    /// The original file extension (lowercase).
    ext: String,
    /// Raw image bytes.
    bytes: Bytes,
    /// MIME type detected from the byte content.
    mimetype: String,
}

async fn validate_field(field: Field<'_>) -> anyhow::Result<ValidatedFile> {
    let filename = field
        .file_name()
        .map(sanitise_file_name::sanitise)
        .ok_or_else(|| anyhow::anyhow!("missing filename"))?;

    let path = std::path::Path::new(&filename);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .ok_or_else(|| anyhow::anyhow!("missing extension"))?;

    if !is_allowed_extension(&ext) {
        anyhow::bail!("unsupported file extension: {ext}");
    }

    let bytes = field.bytes().await?;
    if bytes.is_empty() {
        anyhow::bail!("empty file");
    }

    let mimetype = tree_magic::from_u8(&bytes);

    Ok(ValidatedFile {
        id: get_new_image_id(),
        ext,
        bytes,
        mimetype,
    })
}

async fn collect_fields(mut multipart: Multipart) -> (Vec<ValidatedFile>, usize) {
    let mut files = Vec::new();
    let mut skipped = 0usize;

    while let Ok(Some(field)) = multipart.next_field().await {
        match validate_field(field).await {
            Ok(f) => files.push(f),
            Err(e) => {
                tracing::debug!(error = %e, "skipped upload field");
                skipped += 1;
            }
        }
    }

    (files, skipped)
}

// ---------------------------------------------------------------------------
// Public upload/delete functions (shared by web and API routes)
// ---------------------------------------------------------------------------

/// The result of an upload operation.
#[derive(Debug, Serialize, ToSchema)]
pub struct UploadResult {
    /// Number of files that failed due to a database or validation error.
    pub errors: usize,
    /// Total number of files that were attempted.
    pub total: usize,
    /// Number of files skipped due to unsupported type or missing filename.
    pub skipped: usize,
    /// Canonical URLs of the successfully uploaded files.
    pub links: Vec<String>,
}

impl UploadResult {
    pub fn is_success(&self) -> bool {
        self.total > 0 && self.errors == 0 && self.skipped == 0
    }

    pub fn is_error(&self) -> bool {
        self.total > 0 && self.total == self.errors
    }

    pub fn successful(&self) -> usize {
        self.total.saturating_sub(self.errors)
    }
}

/// The result of a delete operation.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteResult {
    /// The ID of the deleted image.
    pub file: String,
    /// Whether the operation failed.
    pub failed: bool,
}

impl DeleteResult {
    #[allow(dead_code)]
    pub fn is_success(&self) -> bool {
        !self.failed
    }

    pub fn is_error(&self) -> bool {
        self.failed
    }
}

/// Processes a multipart upload and inserts all valid images directly into the database.
///
/// This function is shared by the web form handler and the API endpoint.
pub async fn raw_upload_file(
    state: AppState,
    account: Account,
    multipart: Multipart,
    api: bool,
) -> Result<UploadResult, ApiError> {
    let (files, skipped) = collect_fields(multipart).await;

    if files.is_empty() {
        return Err(ApiError::new("No valid files were provided."));
    }

    let total = files.len();
    let mut errors = 0usize;
    let mut links = Vec::with_capacity(total);

    for mut file in files {
        // Resolve ID conflicts by appending a suffix.
        let insert_result = state
            .database()
            .execute(
                "INSERT INTO images (id, mimetype, uploader_id, image_data) VALUES (?, ?, ?, ?)",
                (file.id.clone(), file.mimetype.clone(), account.id, file.bytes.to_vec()),
            )
            .await;

        if let Err(ref e) = insert_result {
            if is_unique_constraint_violation(e) {
                // Very unlikely with random IDs, but handle it gracefully.
                file.id = format!("{}-{}", file.id, get_new_image_id());
                let retry = state
                    .database()
                    .execute(
                        "INSERT INTO images (id, mimetype, uploader_id, image_data) VALUES (?, ?, ?, ?)",
                        (file.id.clone(), file.mimetype.clone(), account.id, file.bytes.to_vec()),
                    )
                    .await;

                if retry.is_err() {
                    errors += 1;
                    continue;
                }
            } else {
                errors += 1;
                continue;
            }
        }

        let link = canonical_url(format!("/gallery/{}.{}", file.id, file.ext))
            .unwrap_or_default()
            .to_string();
        links.push(link);
    }

    state.invalidate_image_caches().await;

    let title = if api {
        format!("[API] Image Upload: {} files", total)
    } else {
        format!("Image Upload: {} files", total)
    };

    state.send_alert(
        crate::discord::Alert::success(title)
            .account(account)
            .field("Total", total)
            .field("Errors", errors)
            .field("Links", links.join("\n")),
    );

    Ok(UploadResult { errors, total, skipped, links })
}

/// Deletes a single image by ID.
///
/// This function is shared by the web form handler and the API endpoint.
pub async fn delete_image(
    state: AppState,
    account: Account,
    id: String,
    api: bool,
) -> Result<DeleteResult, ApiError> {
    let Some(img) = state.get_image(id.clone()).await else {
        return Err(ApiError::not_found(format!("Image `{id}` was not found")));
    };

    if img.uploader_id.unwrap_or_default() != account.id {
        return Err(ApiError::not_found(format!("Image `{id}` was not found")));
    }

    let result = state
        .database()
        .execute(
            "DELETE FROM images WHERE uploader_id = ? AND id = ?",
            (account.id, id.clone()),
        )
        .await;

    let failed = result.is_err();

    if !failed {
        state.invalidate_image_caches().await;
    }

    let title = if api { "[API] Deleted Image" } else { "Deleted Image" };
    state.send_alert(
        crate::discord::Alert::error(title)
            .account(account)
            .field("ID", id.clone())
            .field("Failed", failed),
    );

    Ok(DeleteResult { file: id, failed })
}

// ---------------------------------------------------------------------------
// Web route handlers
// ---------------------------------------------------------------------------

async fn upload_file(
    State(state): State<AppState>,
    referrer: Option<Referrer>,
    account: Account,
    flasher: Flasher,
    multipart: Multipart,
) -> Response {
    let url = referrer.map(|r| r.0).unwrap_or_else(|| "/images".to_string());
    match raw_upload_file(state, account, multipart, false).await {
        Err(msg) => flasher.add(msg.error.as_ref()).bail(&url),
        Ok(result) => {
            let message = if result.is_success() {
                FlashMessage::success("Upload successful.")
            } else if result.is_error() {
                FlashMessage::error("Upload failed.")
            } else {
                let ok = result.successful();
                FlashMessage::warning(format!(
                    "Uploaded {ok} file{}, {} skipped, {} failed.",
                    if ok == 1 { "" } else { "s" },
                    result.skipped,
                    result.errors,
                ))
            };
            flasher.add(message).bail(&url)
        }
    }
}

#[derive(Deserialize)]
struct BulkFilesPayload {
    files: Vec<String>,
}

#[derive(Serialize)]
struct BulkFileOperationResponse {
    success: usize,
    failed: usize,
}

async fn bulk_delete_files(
    State(state): State<AppState>,
    account: Account,
    Json(payload): Json<BulkFilesPayload>,
) -> Result<Json<BulkFileOperationResponse>, ApiError> {
    let mut success = 0usize;
    let mut failed = 0usize;
    let total = payload.files.len();
    let description = crate::utils::join_iter(
        "\n",
        payload.files.iter().map(|x| format!("- {x}")).take(25),
    );

    for file in payload.files {
        // Strip extension to get the bare ID.
        let id = file.split('.').next().unwrap_or(&file).to_string();
        let result = state
            .database()
            .execute(
                "DELETE FROM images WHERE uploader_id = ? AND id = ?",
                (account.id, id.clone()),
            )
            .await;

        if result.is_ok() {
            success += 1;
        } else {
            failed += 1;
        }
    }

    if success == 0 {
        return Err(ApiError::not_found("No files were found to delete"));
    }

    state.invalidate_image_caches().await;

    state.send_alert(
        crate::discord::Alert::error("Deleted Images")
            .description(description)
            .account(account)
            .field("Total", total)
            .field("Failed", failed),
    );

    Ok(Json(BulkFileOperationResponse { success, failed }))
}

// ---------------------------------------------------------------------------
// Image display templates
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "image.html")]
#[allow(dead_code)]
struct ImageTemplate {
    account: Option<Account>,
    entry: ImageEntry,
    data: ResolvedImageData,
    flashes: Flashes,
}

async fn get_image_page(
    State(state): State<AppState>,
    Path(image_id): Path<String>,
    account: Option<Account>,
    flashes: Flashes,
) -> Result<Response, InternalError> {
    // Strip extension if present (e.g. "abc.png" → "abc").
    let id = image_id.split('.').next().unwrap_or(&image_id).to_string();

    let Some(entry) = state.get_image(id).await else {
        return Ok(Redirect::to("/").into_response());
    };

    let data = ResolvedImageData {
        bytes: general_purpose::STANDARD.encode(&entry.image_data),
        content_type: entry.mimetype.clone(),
    };

    Ok(ImageTemplate { account, entry, data, flashes }.into_response())
}

/// Serves raw image bytes with the correct `Content-Type`.
///
/// Used by the image grid to load thumbnails without a full page load.
async fn get_image_raw(
    State(state): State<AppState>,
    Path(image_id): Path<String>,
) -> Result<Response, StatusCode> {
    let id = image_id.split('.').next().unwrap_or(&image_id).to_string();

    let Some(entry) = state.get_image(id).await else {
        return Err(StatusCode::NOT_FOUND);
    };

    let mime = entry.mimetype.clone();
    Ok((
        [(header::CONTENT_TYPE, mime)],
        entry.image_data,
    )
        .into_response())
}

#[derive(Template)]
#[template(path = "images.html")]
struct ImagesTemplate {
    account: Option<Account>,
    files: Vec<ImageFile>,
    flashes: Flashes,
}

async fn get_images_page(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
) -> Result<Response, InternalError> {
    let files: Vec<ImageFile> = state
        .resolve_image_files()
        .await
        .iter()
        .filter(|e| e.uploader_id == Some(account.id))
        .cloned()
        .collect();

    Ok(ImagesTemplate { account: Some(account), files, flashes }.into_response())
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/images", get(get_images_page))
        .route("/gallery/:id", get(get_image_page))
        .route("/gallery/:id/raw", get(get_image_raw))
        .route("/images/bulk", delete(bulk_delete_files))
        .route(
            "/images/bulk",
            post(upload_file).layer(RateLimit::default().build()),
        )
}
