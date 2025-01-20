use crate::error::{ApiError, ApiErrorCode, InternalError};
use crate::flash::Flashes;
use crate::models::{Account, ImageEntry, ResolvedImageData};
use crate::{audit, AppState, filters};
use anyhow::bail;
use askama::Template;
use axum::extract::multipart::Field;
use axum::extract::Multipart;
use axum::routing::{get};
use axum::{
    extract::{Path, State},
    response::{IntoResponse, Redirect, Response},
    Router,
};
use base64::engine::general_purpose;
use base64::Engine;
use bytes::Bytes;
use serde::Serialize;
use std::fs::{create_dir_all, read, remove_file};
use std::io::Write;
use std::path::PathBuf;
use askama::filters::format;
use tokio::task::JoinSet;
use utoipa::ToSchema;
use crate::audit::FileOperation;
use crate::filters::canonical_url;
use crate::utils::get_new_image_id;

#[derive(Debug, Clone)]
struct ProcessedFile {
    path: PathBuf,
    identifier: String,
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

async fn verify_file(file_name: PathBuf, field: Field<'_>) -> anyhow::Result<ProcessedFile> {
    match file_name.extension().and_then(|ext| ext.to_str()) {
        Some("apng" | "png" | "jpg" | "jpeg" | "gif" | "avif") => {
            let ext = file_name.extension().and_then(|ext| ext.to_str()).unwrap().to_string();
            let identifier: String = get_new_image_id();
            let bytes = field.bytes().await?;
            let path = PathBuf::from(format!("temp/{}", file_name.to_string_lossy()));

            Ok(ProcessedFile {
                path,
                identifier,
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
        file: FileOperation::placeholder(),
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
            }
            _ => errored += 1,
        }
    }

    let successful = total > 0 && errored == 0 && processed.skipped == 0;
    if successful && errored != total {
        for file in files.clone() {
            links.push(canonical_url(format!("/gallery/{}.{}", file.identifier.clone(), file.ext.clone()))?.to_string());
            let image_data = read(&file.path)?;
            let mimetype = tree_magic::from_u8(&file.bytes);

            // remove the temp file
            match remove_file(&file.path) {
                _ => (),
            };

            let image_data_owned = image_data.clone(); // Ensure ownership
            let err = state
                .database()
                .execute(
                    "INSERT INTO images (id, mimetype, uploader_id, image_data) VALUES (?, ?, ?, ?)",
                    (
                        file.identifier.clone(),
                        mimetype.to_string(),
                        account.id.to_string(),
                        image_data_owned
                    ),
                )
                .await;

            data.file = FileOperation {
                name: file.identifier.clone(),
                failed: false,
            };

            if let Err(e) = err {
                tracing::error!(error=%e, "Could not insert image");
                data.file.failed = true;
            }
            state.audit(audit::AuditLogEntry::full(data.clone(), file.identifier.clone(), account.id)).await;

            let title = if api {
                format!("[API] Image Upload: {:?} files", files.len())
            } else {
                format!("Image Upload: {:?} files", files.len())
            };

            state.send_alert(
                crate::discord::Alert::success(title)
                    .url(format!("/logs?image_id={:?}", file.identifier.clone()))
                    .account(account.clone())
                    .field("ID", file.identifier.clone())
                    .field("Failed", data.file.failed)
            );
        }

        state.cached_images().invalidate().await;
    }

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
    state.audit(audit::AuditLogEntry::full(data, id.clone(), account.id)).await;

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

#[derive(Template)]
#[template(path = "image.html")]
struct EntryTemplate {
    account: Option<Account>,
    entry: ImageEntry,
    data: ResolvedImageData,
    flashes: Flashes,
}

async fn get_entry(
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

    Ok(EntryTemplate {
        account,
        entry,
        data,
        flashes,
    }
    .into_response())
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/gallery/:id", get(get_entry))
}
