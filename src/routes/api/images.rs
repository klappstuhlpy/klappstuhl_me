use axum::extract::{Multipart, Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use utoipa::ToSchema;

use crate::{error::ApiError, headers::ClientIp, models::Scope, routes::image::{UploadResult, DeleteResult, BulkFilesPayload, build_images_zip}, AppState};
use crate::routes::image::{delete_image, raw_upload_file};
use super::{
    auth::ApiToken,
    utils::{ApiJson as Json, RateLimitResponse},
};

#[derive(ToSchema)]
struct UploadedFiles {
    #[schema(format = Binary)]
    #[allow(dead_code)]
    file: Vec<String>,
}

/// Upload
///
/// Upload image files.
///
/// Multiple images can be uploaded at a time.
/// The files must be of the following types: `.apng`, `.png`, `.jpg`, `.jpeg`, `.gif`, `.avif`
///
/// The images get a new unique id assigned that is given in the return body.
///
/// You can have multiple `file` fields.
#[utoipa::path(
    post,
    path = "/api/images/upload",
    request_body(
        content = inline(UploadedFiles),
        content_type = "multipart/form-data",
        description = "The files to upload, they must be images."
    ),
    responses(
        (status = 200, description = "Upload processed", body = UploadResult),
        (status = 400, description = "An error occurred", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 403, description = "The API key is missing the images:write scope", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(
        ("api_key" = ["images:write"])
    ),
    tag = "images"
)]
pub async fn upload_files(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    multipart: Multipart,
) -> Result<Json<UploadResult>, ApiError> {
    auth.require(Scope::ImagesWrite)?;
    let Some(account) = state.get_account(auth.id).await else {
        return Err(ApiError::unauthorized());
    };

    let result = raw_upload_file(state, account, client_ip, multipart, true).await?;
    if result.is_error() {
        return Err(ApiError::new("Upload failed"));
    }
    Ok(Json(result))
}

/// Delete
///
/// Delete an image by its id.
/// Note: To delete an image, you must be the uploader of the image.
#[utoipa::path(
    delete,
    path = "/api/images/{id}",
    responses(
        (status = 200, description = "Successfully deleted image", body = DeleteResult),
        (status = 400, description = "Invalid ID given", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 404, description = "Image not found", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    params(
        ("id" = String, Path, description = "The images's ID")
    ),
    security(
        ("api_key" = ["images:write"])
    ),
    tag = "images"
)]
pub async fn delete_image_by_id(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Path(id): Path<String>,
) -> Result<Json<DeleteResult>, ApiError> {
    auth.require(Scope::ImagesWrite)?;
    let Some(account) = state.get_account(auth.id).await else {
        return Err(ApiError::unauthorized());
    };

    let result = delete_image(state, account, client_ip, id.clone(), true).await?;
    if result.is_error() {
        return Err(ApiError::new("Delete failed"));
    }

    Ok(Json(result))
}

/// Download
///
/// Bundle one or more of your images into a ZIP archive.
///
/// The request body is a JSON object with a `files` array of image IDs.
/// Each ID may include the file extension (e.g. `abc123.png`) or be the
/// bare ID; the server strips the extension before lookup. Pass an empty
/// array to receive every image you own.
///
/// Only images owned by the authenticated account are included; unknown
/// or foreign IDs are silently skipped. If no requested image resolves
/// to one you own, the endpoint returns 404.
///
/// The response is a `application/zip` payload with a
/// `Content-Disposition: attachment` header.
#[utoipa::path(
    post,
    path = "/api/images/download",
    request_body(
        content = inline(BulkFilesPayload),
        content_type = "application/json",
        description = "The IDs of the images to bundle. Empty list = all your images."
    ),
    responses(
        (status = 200, description = "ZIP archive of the requested images", content_type = "application/zip", body = Vec<u8>),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 404, description = "No matching images found", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(
        ("api_key" = ["images:read"])
    ),
    tag = "images"
)]
pub async fn download_images(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Json(payload): Json<BulkFilesPayload>,
) -> Result<Response, ApiError> {
    auth.require(Scope::ImagesRead)?;
    let Some(account) = state.get_account(auth.id).await else {
        return Err(ApiError::unauthorized());
    };

    let total = payload.files.len();
    let (bytes, count) = build_images_zip(&state, &account, &payload.files).await?;

    state.audit("image.bulk_download")
        .actor(&account)
        .target(format!("{count} image{}", if count == 1 { "" } else { "s" }))
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "requested": total,
            "delivered": count,
            "via_api":   true,
        }))
        .fire();

    let filename = format!(
        "klappstuhl-images-{}.zip",
        time::OffsetDateTime::now_utc().unix_timestamp(),
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