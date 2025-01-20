use axum::extract::{Multipart, Path, State};
use utoipa::ToSchema;

use crate::{error::ApiError, routes::image::{UploadResult, DeleteResult}, AppState};
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
        (status = 403, description = "The user does not have permission to do this", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(
        ("api_key" = [])
    ),
    tag = "images"
)]
pub async fn upload_files(
    State(state): State<AppState>,
    auth: ApiToken,
    multipart: Multipart,
) -> Result<Json<UploadResult>, ApiError> {
    let Some(account) = state.get_account(auth.id).await else {
        return Err(ApiError::unauthorized());
    };

    let result = raw_upload_file(state, account, multipart, true).await?;
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
        ("api_key" = [])
    ),
    tag = "images"
)]
pub async fn delete_image_by_id(
    State(state): State<AppState>,
    auth: ApiToken,
    Path(id): Path<String>,
) -> Result<Json<DeleteResult>, ApiError> {
    let Some(account) = state.get_account(auth.id).await else {
        return Err(ApiError::unauthorized());
    };

    let result = delete_image(state, account, id.clone(), true).await?;
    if result.is_error() {
        return Err(ApiError::new("Delete failed"));
    }

    Ok(Json(result))
}