mod auth;
mod images;
mod media;
mod scan;
pub mod utils;

use crate::{models::Account, ratelimit::RateLimit, AppState};
use askama::Template;
use axum::{
    extract::State,
    http::{
        header::{AUTHORIZATION, USER_AGENT},
        Method,
    },
    routing::{get, post},
    Json, Router,
};
use axum::routing::delete;
use tower_http::cors::{AllowOrigin, CorsLayer};
use utoipa::{
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
    Modify, OpenApi,
};

use crate::error::ApiError;
use crate::filters;
pub use auth::{copy_api_token, ApiToken};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Klappstuhl.me",
        description = include_str!("../../../templates/api_description.md"),
        version = "beta"
    ),
    paths(
        images::upload_files,
        images::delete_image_by_id,
        images::download_images,
        media::manipulate_image,
        media::convert_file,
        media::image_info,
        scan::scan_file,
    ),
    components(
        schemas(
            ApiError,
            crate::models::ImageEntry,
            crate::routes::image::UploadResult,
            crate::routes::image::DeleteResult,
            crate::routes::image::BulkFilesPayload,
            crate::scan::ScanReport,
            media::ImageInfo,
        ),
        responses(utils::RateLimitResponse),
    ),
    modifiers(&RequiredAuthentication),
    tags(
        (name = "images", description = "Endpoints for uploading/deleting and getting images at the server."),
        (name = "media", description = "Image manipulation and format conversion. Accepts a `file` upload or a public image `url`."),
        (name = "scan", description = "Scan uploaded files for malware via ClamAV and VirusTotal.")
    )
)]
pub struct Schema;

struct RequiredAuthentication;

impl Modify for RequiredAuthentication {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "api_key",
                SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("Authorization"))),
            )
        }
    }
}

#[derive(Template)]
#[template(path = "api.html")]
struct ApiDocumentation {
    api_key: String,
}

async fn spec() -> Json<utoipa::openapi::OpenApi> {
    Json(Schema::openapi())
}

async fn docs(State(state): State<AppState>, account: Option<Account>) -> ApiDocumentation {
    let api_key = if let Some(acc) = &account {
        state.get_api_key(acc.id).await.unwrap_or_default()
    } else {
        String::new()
    };
    ApiDocumentation { api_key }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_spec_builds_with_new_paths() {
        // utoipa assembles the document at runtime; this catches duplicate
        // operation ids or malformed path specs that compile but panic.
        let spec = Schema::openapi();
        let paths = &spec.paths.paths;
        for expected in ["/api/scan", "/api/convert", "/api/image/{op}", "/api/metadata"] {
            assert!(paths.contains_key(expected), "missing {expected} in OpenAPI spec");
        }
    }

    #[test]
    fn router_builds_without_route_conflicts() {
        // matchit panics at registration on conflicting paths (e.g. a static
        // segment overlapping a `:param` on axum 0.7). Building the router
        // here surfaces that as a test failure rather than at server start.
        let _ = routes();
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/openapi.json", get(spec))
        .route("/docs", get(docs))
        .route("/images/upload", post(images::upload_files))
        .route("/images/download", post(images::download_images))
        .route("/images/:id", delete(images::delete_image_by_id))
        .route("/scan", post(scan::scan_file))
        .route("/metadata", post(media::image_info))
        .route("/image/:op", post(media::manipulate_image))
        .route("/convert", post(media::convert_file))
        .route_layer(RateLimit::default().quota(25, 60.0).build())
        .route_layer(
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST])
                .allow_credentials(true)
                .allow_origin(AllowOrigin::mirror_request())
                .allow_headers([AUTHORIZATION, USER_AGENT]),
        )
}