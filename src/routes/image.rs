use crate::error::{ApiError, ApiErrorCode, InternalError};
use crate::flash::{FlashMessage, Flasher, Flashes};
use crate::models::{Account, ImageEntry, ImageFile, ResolvedImageData};
use crate::{audit, AppState, filters};
use anyhow::bail;
use askama::Template;
use axum::extract::multipart::Field;
use axum::extract::Multipart;
use axum::routing::{delete, get, post};
use axum::{extract::{Path, State}, response::{IntoResponse, Redirect, Response}, Json, Router};
use base64::engine::general_purpose;
use base64::Engine;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::fs::{create_dir_all, read, remove_file};
use std::io::Write;
use std::path::PathBuf;
use tokio::task::JoinSet;
use utoipa::ToSchema;
use crate::audit::FileOperation;
use crate::filters::canonical_url;
use crate::headers::Referrer;
use crate::ratelimit::RateLimit;
use crate::utils::get_new_image_id;

#[derive(Debug, Clone)]
struct ProcessedFile {
    name: String,
    path: PathBuf,
    bytes: Bytes,
    ext: String,
}

impl ProcessedFile {
    fn write_to_disk(self) -> std::io::Result<()> {
        match create_dir_all("temp") {
            Ok(_) => (),
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Failed to create directory",
                ));
            }
        }

        let mut fp = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)?;
        fp.write_all(&self.bytes)?;

        Ok(())
    }
}

struct ProcessedFiles {
    files: Vec<ProcessedFile>,
    skipped: usize,
}

async fn verify_file(file: PathBuf, field: Field<'_>) -> anyhow::Result<ProcessedFile> {
    match file.extension().and_then(|ext| ext.to_str()) {
        Some("apng" | "png" | "jpg" | "jpeg" | "gif" | "avif") => {
            let ext = file.extension().and_then(|ext| ext.to_str()).unwrap().to_string();
            let bytes = field.bytes().await?;
            let path = PathBuf::from(format!("temp/{}", file.to_string_lossy()));
            let name = file.to_string_lossy().split(".").collect::<Vec<&str>>()[0].to_string();

            Ok(ProcessedFile {
                name,
                path,
                bytes,
                ext,
            })
        }
        _ => bail!("invalid file extension"),
    }
}

async fn process_files(mut multipart: Multipart) -> anyhow::Result<ProcessedFiles> {
    let mut files = Vec::new();
    let mut skipped = 0;

    while let Some(field) = multipart.next_field().await? {
        let Some(name) = field.file_name().map(sanitise_file_name::sanitise).map(PathBuf::from) else {
            tracing::debug!("Skipped file due to missing filename");
            skipped += 1;
            continue;
        };

        match verify_file(name, field).await {
            Ok(file) => files.push(file),
            Err(e) => {
                tracing::debug!(error=%e, "Skipped file due to validation issue");
                skipped += 1
            }
        }
    }
    Ok(ProcessedFiles { files, skipped })
}

/// The result of an upload operation.
#[derive(Debug, Serialize, ToSchema)]
pub struct UploadResult {
    /// The number of files that did not succeed due to a filesystem error.
    errors: usize,
    /// The number of files that were processed.
    total: usize,
    /// The number of files that were skipped due to some reason
    skipped: usize,
    /// The links to the newly uploaded files.
    links: Vec<String>,
}

#[allow(dead_code)]
impl UploadResult {
    pub fn is_success(&self) -> bool {
        self.total > 0 && self.errors == 0 && self.skipped == 0
    }

    pub fn is_error(&self) -> bool {
        self.total == self.errors
    }

    pub fn successful(&self) -> usize {
        self.total - self.errors
    }
}

/// The result of an delete operation.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteResult {
    /// The file that was deleted.
    pub file: String,
    /// Whether the delete operation was successful.
    pub failed: bool,
}

#[allow(dead_code)]
impl DeleteResult {
    pub fn is_success(&self) -> bool {
        !self.failed
    }

    pub fn is_error(&self) -> bool {
        self.failed
    }
}

pub async fn raw_upload_file(
    state: AppState,
    account: Account,
    multipart: Multipart,
    api: bool,
) -> Result<UploadResult, ApiError> {
    let Ok(processed) = process_files(multipart).await else {
        return Err(ApiError::new("Internal error when processing files").with_code(ApiErrorCode::BadRequest));
    };

    if processed.files.is_empty() {
        return Err(ApiError::new("Did not upload any files."));
    }

    let files = processed.files.clone();
    let mut links = Vec::new();

    let mut errored = 0usize;
    let total = processed.files.len();
    let mut data = audit::Upload {
        files: Vec::with_capacity(total),
        api,
    };
    let mut set = JoinSet::new();
    for file in processed.files.into_iter() {
        set.spawn_blocking(move || {
            let name = file.path.file_name().and_then(|x| x.to_str()).unwrap().to_owned();
            let failed = file.write_to_disk().is_err();
            FileOperation { name, failed }
        });
    }

    while let Some(task) = set.join_next().await {
        match task {
            Ok(op) => {
                errored += op.failed as usize;
                data.files.push(op);
            }
            _ => errored += 1,
        }
    }

    let successful = total > 0 && errored == 0 && processed.skipped == 0;
    if successful && errored != total {
        for file in files.clone() {
            let mut file_name = file.name.clone();
            let image_data = read(&file.path)?;
            let mimetype = tree_magic::from_u8(&file.bytes);

            // remove the temp file
            match remove_file(&file.path) {
                _ => (),
            };

            let image_data_owned = image_data.clone(); // Ensure ownership
            let result = state
                .database()
                .execute(
                    "INSERT INTO images (id, mimetype, uploader_id, image_data)
                           VALUES (?, ?, ?, ?)", // Ignore conflicts and don't insert
                    (
                        file.name.clone(),
                        mimetype.to_string(),
                        account.id.to_string(),
                        image_data_owned.clone(),
                    ),
                )
                .await;

            // Check if the insertion was successful
            if let Err(e) = result {
                if e.to_string().contains("UNIQUE constraint failed") {
                    // Handle the conflict case here by generating a new ID
                    file_name = format!("{}-{}", file_name, get_new_image_id());
                    state
                        .database()
                        .execute(
                            "INSERT INTO images (id, mimetype, uploader_id, image_data)
                                   VALUES (?, ?, ?, ?)",
                            (
                                file_name.clone(),
                                mimetype.to_string(),
                                account.id.to_string(),
                                image_data_owned,
                            ),
                        )
                        .await?;
                }
            }

            links.push(canonical_url(format!("/gallery/{}.{}", file_name, file.ext.clone()))?.to_string());
        }

        state.cached_images().invalidate().await;
    }

    let audit_id = state.audit(audit::AuditLogEntry::full(data, account.id)).await;

    let title = if api {
        format!("[API] Image Upload: {:?} files", files.len())
    } else {
        format!("Image Upload: {:?} files", files.len())
    };

    state.send_alert(
        crate::discord::Alert::success(title)
            .url(format!("/logs?id={audit_id}"))
            .account(account.clone())
            .field("Total", total)
            .field("Failed", errored)
            .field("Links", links.join("\n")),
    );

    Ok(UploadResult {
        errors: errored,
        total,
        skipped: processed.skipped,
        links,
    })
}

pub async fn delete_image(
    state: AppState,
    account: Account,
    id: String,
    api: bool,
) -> Result<DeleteResult, ApiError> {
    let Some(img) = state.get_image(id.clone()).await else {
        return Err(ApiError::not_found(format!("Image `{}` was not found", id.clone())));
    };

    if img.uploader_id.unwrap_or_default() != account.id {
        return Err(ApiError::not_found(format!("Image `{}` was not found", id.clone())));
    }

    let _ = state
        .database()
        .execute(
            "DELETE FROM images WHERE account_id = ? AND id = ?",
            (account.id, id.clone()),
        )
        .await;

    state.cached_images().invalidate().await;

    let data = audit::DeleteImage {
        file: FileOperation {
            name: id.clone(),
            failed: false,
        },
        api
    };
    state.audit(audit::AuditLogEntry::full(data, account.id)).await;

    let title = if api {
        "[API] Deleted Image"
    } else {
        "Deleted Image"
    };

    state.send_alert(
        crate::discord::Alert::error(title)
            .url(format!("/logs?image_id={:?}", id.clone()))
            .account(account)
            .field("ID", id.clone())
            .field("Failed", false),
    );

    Ok(DeleteResult {
        file: id.clone(),
        failed: false,
    })
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
    let mut success = 0;
    let mut failed = 0;
    let mut audit_data = audit::DeleteFiles {
        files: Vec::with_capacity(payload.files.len()),
    };
    let total = payload.files.len();
    let description = crate::utils::join_iter("\n", payload.files.iter().map(|x| format!("- {x}")).take(25));
    for file in payload.files {
        let filename = file.clone().split(".").collect::<Vec<&str>>()[0].to_string();
        let result = state
            .database()
            .execute(
                "DELETE FROM images WHERE uploader_id = ? AND id = ?",
                (account.id, filename),
            )
            .await;

        audit_data.add_file(file.clone(), result.is_err());
        match result {
            Ok(_) => success += 1,
            Err(_) => failed += 1,
        }
    }

    if success == 0 {
        return Err(ApiError::not_found("No files were found to delete"));
    } else {
        state.cached_images().invalidate().await;
    }

    let audit_id = state
        .audit(audit::AuditLogEntry::full(audit_data, account.id))
        .await;
    state.send_alert(
        crate::discord::Alert::error("Deleted Images")
            .url(format!("/logs?id={audit_id}"))
            .description(description)
            .account(account)
            .field("Total", total)
            .field("Failed", failed)
    );

    Ok(Json(BulkFileOperationResponse {
        success,
        failed,
    }))
}

async fn upload_file(
    State(state): State<AppState>,
    Referrer(url): Referrer,
    account: Account,
    flasher: Flasher,
    multipart: Multipart,
) -> Response {
    let result = match raw_upload_file(state, account, multipart, false).await {
        Ok(result) => result,
        Err(msg) => return flasher.add(msg.error.as_ref()).bail(&url),
    };
    let message = if result.is_success() {
        FlashMessage::success("Upload successful.")
    } else if result.is_error() {
        FlashMessage::error("Upload failed.")
    } else {
        let successful = result.successful();
        FlashMessage::warning(format!(
            "Uploaded {successful} file{}, {} {} skipped and {} failed",
            if successful == 1 { "" } else { "s" },
            result.skipped,
            if result.skipped == 1 { "was" } else { "were" },
            result.errors,
        ))
    };
    flasher.add(message).bail(&url)
}


#[derive(Template)]
#[template(path = "image.html")]
#[allow(dead_code)]
struct ImageTemplate {
    account: Option<Account>,
    entry: ImageEntry,
    data: ResolvedImageData,
    flashes: Flashes,
}

async fn get_image(
    State(state): State<AppState>,
    Path(image_id): Path<String>,
    account: Option<Account>,
    flashes: Flashes,
) -> Result<Response, InternalError> {
    let mut normalized_id: String = image_id.clone();
    let split_id = normalized_id.split(".");
    if split_id.clone().count() > 1 {
        normalized_id = split_id.clone().collect::<Vec<&str>>()[0].to_string();
    }

    let Some(entry) = state.get_image(normalized_id).await else {
        return Ok(Redirect::to("/").into_response());
    };

    let data = ResolvedImageData {
        bytes: general_purpose::STANDARD.encode(&entry.image_data),
        content_type: entry.mimetype.clone(),
    };

    Ok(ImageTemplate {
        account,
        entry,
        data,
        flashes,
    }
    .into_response())
}

#[derive(Template)]
#[template(path = "images.html")]
#[allow(dead_code)]
struct ImagesTemplate {
    account: Option<Account>,
    files: Vec<ImageFile>,
    flashes: Flashes,
}

pub(crate) async fn get_file_images(state: AppState, user_id: i64) -> std::io::Result<Vec<ImageFile>> {
    let entries = state.resolve_images()
        .await
        .iter()
        .filter(|e| e.uploader_id == Option::from(user_id))
        .cloned()
        .collect::<Vec<_>>();

    let mut files = Vec::new();
    for file in entries {
        let entry = file;
        let filename = format!("{}.{}", entry.id, entry.ext());
        let url = format!("/gallery/{filename}");
        files.push(ImageFile {
            url,
            id: filename,
            mimetype: entry.mimetype,
            image_data: entry.image_data.clone(),
            size: entry.image_data.len() as u64,
            uploaded_at: entry.uploaded_at,
        });
    }
    Ok(files)
}

async fn get_images(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
) -> Result<Response, InternalError> {
    let files = get_file_images(state.clone(), account.id).await?;
    Ok(ImagesTemplate {
        account: Some(account),
        files,
        flashes,
    }
        .into_response())
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/images", get(get_images))
        .route("/gallery/:id", get(get_image))
        .route("/images/bulk", delete(bulk_delete_files))
        .route("/images/bulk", post(upload_file).layer(RateLimit::default().build()))
}
