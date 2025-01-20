mod auth;
mod images;
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
    ),
    components(
        schemas(
            ApiError,
            crate::models::ImageEntry,
            crate::routes::image::UploadResult,
            crate::routes::image::DeleteResult,
        ),
        responses(utils::RateLimitResponse),
    ),
    modifiers(&RequiredAuthentication),
    tags(
        (name = "images", description = "Endpoints for uploading/deleting and getting images at the server.")
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

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/openapi.json", get(spec))
        .route("/docs", get(docs))
        .route("/images/upload", post(images::upload_files))
        .route("/images/{id}", delete(images::delete_image_by_id))
        .route_layer(RateLimit::default().quota(25, 60.0).build())
        .route_layer(
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST])
                .allow_credentials(true)
                .allow_origin(AllowOrigin::mirror_request())
                .allow_headers([AUTHORIZATION, USER_AGENT]),
        )
}