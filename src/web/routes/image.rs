//! Routes for image upload, viewing, deletion, and raw serving.

use crate::error::{ApiError, InternalError};
use crate::filters::canonical_url;
use crate::flash::{FlashMessage, Flasher, Flashes};
use crate::headers::{ClientIp, Referrer};
use crate::models::{Account, ImageEntry, ImageFile, ResolvedImageData};
use crate::ratelimit::RateLimit;
use crate::utils::get_new_image_id;
use crate::{database::is_unique_constraint_violation, filters, AppState};
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
use std::net::IpAddr;
use time::OffsetDateTime;
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// Allowed MIME types
// ---------------------------------------------------------------------------

const ALLOWED_EXTENSIONS: &[&str] = &["apng", "png", "jpg", "jpeg", "gif", "avif"];

fn is_allowed_extension(ext: &str) -> bool {
    ALLOWED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

/// Upper bound on a requested time-to-live (365 days). Keeps a typo or a
/// malicious value from pinning storage forever / overflowing the timestamp.
const MAX_TTL_SECONDS: i64 = 365 * 24 * 3600;

/// Optional upload parameters (passed in the query string so the same shared
/// path works for the web form and the API).
#[derive(Debug, Default, Deserialize)]
pub struct UploadParams {
    /// Time-to-live in seconds. When set (and positive), the upload is
    /// auto-deleted by the reaper after this many seconds. Capped at 365 days.
    #[serde(default)]
    pub expires_in: Option<i64>,
}

/// Resolves a requested TTL into an absolute expiry timestamp, clamped to
/// `MAX_TTL_SECONDS`. `None`/zero/negative means "never expires".
fn expiry_from(expires_in: Option<i64>) -> Option<OffsetDateTime> {
    let secs = expires_in.filter(|&s| s > 0)?.min(MAX_TTL_SECONDS);
    Some(OffsetDateTime::now_utc() + time::Duration::seconds(secs))
}

/// Public wrapper over [`expiry_from`] for the API upload handler.
pub fn expiry_from_params(params: &UploadParams) -> Option<OffsetDateTime> {
    expiry_from(params.expires_in)
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

    // Strip EXIF/XMP/text metadata (GPS, camera, etc.) before the bytes are
    // stored or served. Pixel data and animation are preserved.
    let bytes = Bytes::from(crate::metadata::strip(&ext, &bytes));

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
    /// Number of files rejected because a malware scan flagged them.
    #[serde(default)]
    pub infected: usize,
    /// Canonical URLs of the successfully uploaded files.
    pub links: Vec<String>,
}

impl UploadResult {
    pub fn is_success(&self) -> bool {
        self.total > 0 && self.errors == 0 && self.skipped == 0 && self.infected == 0
    }

    pub fn is_error(&self) -> bool {
        self.total > 0 && self.total == self.errors + self.infected
    }

    pub fn successful(&self) -> usize {
        self.total.saturating_sub(self.errors).saturating_sub(self.infected)
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
    client_ip: Option<IpAddr>,
    multipart: Multipart,
    api: bool,
    expires_at: Option<OffsetDateTime>,
) -> Result<UploadResult, ApiError> {
    let (files, skipped) = collect_fields(multipart).await;

    if files.is_empty() {
        return Err(ApiError::new("No valid files were provided."));
    }

    let total = files.len();
    let mut errors = 0usize;
    let mut infected = 0usize;
    let mut links = Vec::with_capacity(total);

    // Only pay the scanning cost when a backend is actually configured.
    let scan_enabled = state.config().clamav_addr.is_some() || state.config().virustotal_api_key.is_some();

    for mut file in files {
        // Malware gate: run the configured scanners (ClamAV + VirusTotal) over
        // the bytes before they ever touch the database. A definite hit on
        // either backend ("infected") is rejected; "unknown"/"clean" pass.
        if scan_enabled {
            let report = crate::scan::scan_bytes(&state, &file.bytes).await;
            if report.verdict == "infected" {
                infected += 1;
                tracing::warn!(
                    sha256 = %report.sha256,
                    virus = ?report.clamav_virus,
                    vt = ?report.vt_status,
                    "rejected infected upload"
                );
                state
                    .audit("image.upload.rejected")
                    .actor(&account)
                    .target(format!("{}.{}", file.id, file.ext))
                    .ip_opt(client_ip)
                    .meta(serde_json::json!({
                        "reason":       "infected",
                        "sha256":       report.sha256,
                        "clamav_virus": report.clamav_virus,
                        "vt_status":    report.vt_status,
                        "vt_positives": report.vt_positives,
                    }))
                    .fire();
                state.send_alert(
                    crate::discord::Alert::error("Blocked Infected Upload")
                        .field("SHA-256", report.sha256)
                        .field("ClamAV", report.clamav_virus.unwrap_or_else(|| "—".into()))
                        .field("VirusTotal", report.vt_status.unwrap_or_else(|| "—".into())),
                );
                continue;
            }
        }

        // Resolve ID conflicts by appending a suffix.
        let insert_result = state
            .database()
            .execute(
                "INSERT INTO images (id, mimetype, uploader_id, image_data, expires_at) VALUES (?, ?, ?, ?, ?)",
                (file.id.clone(), file.mimetype.clone(), account.id, file.bytes.to_vec(), expires_at),
            )
            .await;

        if let Err(ref e) = insert_result {
            if is_unique_constraint_violation(e) {
                // Very unlikely with random IDs, but handle it gracefully.
                file.id = format!("{}-{}", file.id, get_new_image_id());
                let retry = state
                    .database()
                    .execute(
                        "INSERT INTO images (id, mimetype, uploader_id, image_data, expires_at) VALUES (?, ?, ?, ?, ?)",
                        (file.id.clone(), file.mimetype.clone(), account.id, file.bytes.to_vec(), expires_at),
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

    // Audit log so /admin/audit shows who uploaded what, from where, and
    // how it went. Image IDs go in meta (target stays human-readable as a
    // count); for the common case of one upload the ID is enough to
    // round-trip to the /gallery URL.
    state
        .audit("image.upload")
        .actor(&account)
        .target(format!("{total} image{}", if total == 1 { "" } else { "s" }))
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "total":    total,
            "errors":   errors,
            "skipped":  skipped,
            "infected": infected,
            "via_api":  api,
            "links":    links,
        }))
        .fire();

    Ok(UploadResult {
        errors,
        total,
        skipped,
        infected,
        links,
    })
}

/// Deletes a single image by ID.
///
/// This function is shared by the web form handler and the API endpoint.
pub async fn delete_image(
    state: AppState,
    account: Account,
    client_ip: Option<IpAddr>,
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

    state
        .audit("image.delete")
        .actor(&account)
        .target(id.clone())
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "failed":  failed,
            "via_api": api,
        }))
        .fire();

    Ok(DeleteResult { file: id, failed })
}

// ---------------------------------------------------------------------------
// Web route handlers
// ---------------------------------------------------------------------------

async fn upload_file(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    referrer: Option<Referrer>,
    axum::extract::Query(params): axum::extract::Query<UploadParams>,
    account: Account,
    flasher: Flasher,
    multipart: Multipart,
) -> Response {
    let url = referrer.map(|r| r.0).unwrap_or_else(|| "/images".to_string());
    let expires_at = expiry_from(params.expires_in);
    match raw_upload_file(state, account, client_ip, multipart, false, expires_at).await {
        Err(msg) => flasher.add(msg.error.as_ref()).bail(&url),
        Ok(result) => {
            let message = if result.is_success() {
                FlashMessage::success("Upload successful.")
            } else if result.is_error() {
                if result.infected > 0 && result.errors == 0 {
                    FlashMessage::error("Upload blocked: file failed a malware scan.")
                } else {
                    FlashMessage::error("Upload failed.")
                }
            } else {
                let ok = result.successful();
                FlashMessage::warning(format!(
                    "Uploaded {ok} file{}, {} skipped, {} failed, {} blocked by malware scan.",
                    if ok == 1 { "" } else { "s" },
                    result.skipped,
                    result.errors,
                    result.infected,
                ))
            };
            flasher.add(message).bail(&url)
        }
    }
}

#[derive(Deserialize, ToSchema)]
pub struct BulkFilesPayload {
    /// Image IDs to operate on. May include the file extension (e.g. `abc123.png`)
    /// or just the bare ID. Pass an empty list to operate on every image
    /// owned by the authenticated user.
    pub files: Vec<String>,
}

#[derive(Serialize)]
struct BulkFileOperationResponse {
    success: usize,
    failed: usize,
}

/// Builds a ZIP archive of the requested images for an account.
///
/// Returns the raw bytes of the archive together with the count of files
/// included. Only images owned by `account` are eligible; unknown or
/// foreign IDs are silently skipped. If `requested` is empty, every image
/// owned by the account is included.
pub async fn build_images_zip(
    state: &AppState,
    account: &Account,
    requested: &[String],
) -> Result<(Vec<u8>, usize), ApiError> {
    let now = OffsetDateTime::now_utc();
    let owned: Vec<ImageFile> = state
        .resolve_image_files()
        .await
        .iter()
        .filter(|e| e.uploader_id == Some(account.id))
        .filter(|e| e.expires_at.map(|exp| exp > now).unwrap_or(true))
        .cloned()
        .collect();

    let selected: Vec<ImageFile> = if requested.is_empty() {
        owned
    } else {
        let wanted: std::collections::HashSet<String> = requested
            .iter()
            .map(|s| s.split('.').next().unwrap_or(s).to_string())
            .collect();
        owned
            .into_iter()
            .filter(|f| {
                let bare = f.id.split('.').next().unwrap_or(&f.id);
                wanted.contains(bare)
            })
            .collect()
    };

    if selected.is_empty() {
        return Err(ApiError::not_found("No matching images were found"));
    }

    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(selected.len());
    for f in &selected {
        // The cached ImageFile may carry empty image_data because the
        // cache stores metadata only. Fetch the bytes on demand.
        let data = match state
            .resolve_image_data_for(f.id.split('.').next().unwrap_or(&f.id))
            .await
        {
            Some(d) => d,
            None => continue,
        };
        entries.push((f.id.clone(), data));
    }

    if entries.is_empty() {
        return Err(ApiError::not_found("No matching images were found"));
    }

    let count = entries.len();
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        use std::io::Write;
        let buf = std::io::Cursor::new(Vec::<u8>::with_capacity(1024 * 1024));
        let mut zip = zip::ZipWriter::new(buf);
        // Images are already compressed (PNG/JPG/AVIF/GIF), so store rather
        // than re-deflate. This is faster and keeps archive size honest.
        let opts: zip::write::SimpleFileOptions =
            zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            zip.start_file(name, opts)?;
            zip.write_all(&data)?;
        }
        let cursor = zip.finish()?;
        Ok(cursor.into_inner())
    })
    .await
    .map_err(|_| ApiError::new("Failed to build archive"))?
    .map_err(|_| ApiError::new("Failed to build archive"))?;

    Ok((bytes, count))
}

async fn bulk_delete_files(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Json(payload): Json<BulkFilesPayload>,
) -> Result<Json<BulkFileOperationResponse>, ApiError> {
    let mut success = 0usize;
    let mut failed = 0usize;
    let total = payload.files.len();
    let description = crate::utils::join_iter("\n", payload.files.iter().map(|x| format!("- {x}")).take(25));
    let files_audit: Vec<String> = payload.files.clone(); // for audit meta

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

    state
        .audit("image.bulk_delete")
        .actor(&account)
        .target(format!(
            "{success} of {total} image{}",
            if total == 1 { "" } else { "s" }
        ))
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "total":   total,
            "success": success,
            "failed":  failed,
            "files":   files_audit,
        }))
        .fire();

    Ok(Json(BulkFileOperationResponse { success, failed }))
}

async fn bulk_download_files(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Json(payload): Json<BulkFilesPayload>,
) -> Result<Response, ApiError> {
    let (bytes, count) = build_images_zip(&state, &account, &payload.files).await?;
    let total = payload.files.len();

    state
        .audit("image.bulk_download")
        .actor(&account)
        .target(format!("{count} image{}", if count == 1 { "" } else { "s" }))
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "requested": total,
            "delivered": count,
            "via_api":   false,
        }))
        .fire();

    let filename = format!(
        "klappstuhl-images-{}.zip",
        OffsetDateTime::now_utc().unix_timestamp(),
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Image display templates
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "images/image.html")]
#[allow(dead_code)]
struct ImageTemplate {
    account: Option<Account>,
    entry: ImageEntry,
    data: ResolvedImageData,
    flashes: Flashes,
    /// Absolute URL of this landing page (OpenGraph `og:url`).
    page_url: String,
    /// Absolute URL of the raw image bytes (OpenGraph `og:image`), so link
    /// unfurls in Discord/Slack/Twitter show the picture.
    raw_url: String,
}

async fn get_image_page(
    State(state): State<AppState>,
    Path(image_id): Path<String>,
    account: Option<Account>,
    flashes: Flashes,
) -> Result<Response, InternalError> {
    // Strip extension if present (e.g. "abc.png" → "abc").
    let id = image_id.split('.').next().unwrap_or(&image_id).to_string();

    let Some(entry) = state.get_image(id.clone()).await else {
        return Ok(Redirect::to("/").into_response());
    };

    // An expired image is treated as gone (the reaper will remove the row;
    // this closes the up-to-an-hour window between expiry and the next sweep).
    if entry.is_expired() {
        return Ok(Redirect::to("/").into_response());
    }

    // Canonical URL is always "/gallery/{id}.{ext}". If the request came in
    // without an extension (or with the wrong one) bounce the user to the
    // canonical form via a 308 so bookmarks / shared links normalize.
    let canonical_ext = entry.ext();
    let provided_ext = image_id.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    if provided_ext.as_deref() != Some(canonical_ext.as_str()) {
        return Ok(Redirect::permanent(&format!("/gallery/{}.{}", id, canonical_ext)).into_response());
    }

    let data = ResolvedImageData {
        bytes: general_purpose::STANDARD.encode(&entry.image_data),
        content_type: entry.mimetype.clone(),
    };

    let page_url = canonical_url(format!("/gallery/{}.{}", id, canonical_ext)).unwrap_or_default();
    let raw_url = canonical_url(format!("/gallery/raw/{}.{}", id, canonical_ext)).unwrap_or_default();

    Ok(ImageTemplate {
        account,
        entry,
        data,
        flashes,
        page_url,
        raw_url,
    }
    .into_response())
}

/// Serves raw image bytes with the correct `Content-Type`.
///
/// Used by the image grid to load thumbnails without a full page load.
/// Mirrors the canonicalization behavior of [`get_image_page`]: requests
/// for `/gallery/raw/abc` or `/gallery/raw/abc.wrong-ext` get a 308
/// redirect to `/gallery/raw/abc.{canonical-ext}`.
async fn get_image_raw(State(state): State<AppState>, Path(image_id): Path<String>) -> Result<Response, StatusCode> {
    let id = image_id.split('.').next().unwrap_or(&image_id).to_string();

    let Some(entry) = state.get_image(id.clone()).await else {
        return Err(StatusCode::NOT_FOUND);
    };

    if entry.is_expired() {
        return Err(StatusCode::NOT_FOUND);
    }

    let canonical_ext = entry.ext();
    let provided_ext = image_id.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    if provided_ext.as_deref() != Some(canonical_ext.as_str()) {
        return Ok(Redirect::permanent(&format!("/gallery/raw/{}.{}", id, canonical_ext)).into_response());
    }

    let mime = entry.mimetype.clone();
    Ok(([(header::CONTENT_TYPE, mime)], entry.image_data).into_response())
}

#[derive(Template)]
#[template(path = "images/images.html")]
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
    let now = OffsetDateTime::now_utc();
    let files: Vec<ImageFile> = state
        .resolve_image_files()
        .await
        .iter()
        .filter(|e| e.uploader_id == Some(account.id))
        .filter(|e| e.expires_at.map(|exp| exp > now).unwrap_or(true))
        .cloned()
        .collect();

    Ok(ImagesTemplate {
        account: Some(account),
        files,
        flashes,
    }
    .into_response())
}

/// Background reaper: deletes expired images hourly and invalidates the image
/// caches when anything was removed. Expiry is also enforced at serve time, so
/// this is the cleanup half — it keeps the database from accumulating dead
/// rows rather than being the sole gate.
pub fn spawn_expiry_reaper(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            let deleted = state
                .database()
                .call(|conn| {
                    // datetime() normalises whatever timestamp format the
                    // driver stored (RFC3339 with `T`/`Z`/offset, or the
                    // SQLite `YYYY-MM-DD HH:MM:SS` form) so the comparison
                    // doesn't depend on the exact serialisation.
                    conn.execute(
                        "DELETE FROM images WHERE expires_at IS NOT NULL \
                         AND datetime(expires_at) <= datetime('now')",
                        [],
                    )
                })
                .await
                .unwrap_or(0);
            if deleted > 0 {
                tracing::info!(count = deleted, "reaped expired images");
                state.invalidate_image_caches().await;
            }
        }
    });
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/images", get(get_images_page))
        .route("/gallery/:id", get(get_image_page))
        .route("/gallery/raw/:id", get(get_image_raw))
        .route("/images/bulk", delete(bulk_delete_files))
        .route("/images/bulk/download", post(bulk_download_files))
        .route("/images/bulk", post(upload_file).layer(RateLimit::default().build()))
}
