mod admin;
mod auth;
mod code;
mod external;
mod guild_images;
mod images;
mod media;
mod scan;
pub mod utils;

use crate::{models::Account, ratelimit::RateLimit, AppState};
use askama::Template;
use axum::routing::delete;
use axum::{
    extract::State,
    http::{
        header::{AUTHORIZATION, USER_AGENT},
        HeaderValue, Method,
    },
    middleware::map_response,
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use tower_http::cors::{AllowOrigin, CorsLayer};
use utoipa::{
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
    Modify, OpenApi,
};

use crate::error::ApiError;
use crate::filters;
pub use auth::{copy_api_token, ApiToken};
pub use media::serve_media;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Klappstuhl.me",
        description = include_str!("../../../templates/api/api_description.md"),
        version = "1.1.0"
    ),
    paths(
        images::upload_files,
        images::delete_image_by_id,
        images::download_images,
        guild_images::upload_guild_images,
        guild_images::list_guild_images,
        guild_images::delete_guild_image,
        media::manipulate_image,
        media::convert_file,
        media::image_info,
        code::render_code,
        external::screenshot,
        external::markdown_pdf,
        external::transcode,
        scan::scan_file,
        admin::list_updates,
    ),
    components(
        schemas(
            ApiError,
            crate::updates::ImageUpdate,
            crate::updates::UpdateState,
            crate::models::ImageEntry,
            crate::site::image::UploadResult,
            crate::site::image::DeleteResult,
            crate::site::image::BulkFilesPayload,
            guild_images::GuildImageInfo,
            guild_images::GuildImagesResult,
            crate::scan::ScanReport,
            media::ImageInfo,
            media::ShareResult,
            code::CodeImageRequest,
            external::ScreenshotRequest,
            external::MarkdownRequest,
        ),
        responses(utils::RateLimitResponse),
    ),
    modifiers(&RequiredAuthentication),
    tags(
        (name = "images", description = "Endpoints for uploading/deleting and getting images at the server."),
        (name = "media", description = "Image manipulation and format conversion. Accepts a `file` upload or a public image `url`."),
        (name = "render", description = "Render content to images (syntax-highlighted code screenshots, …)."),
        (name = "scan", description = "Scan uploaded files for malware via ClamAV and VirusTotal."),
        (name = "admin", description = "Admin-scoped homelab state (requires admin:read / admin:write).")
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
#[template(path = "api/api.html")]
struct ApiDocumentation {
    api_key: String,
}

/// The single source of truth for the public API version. Bumping this migrates
/// the entire surface at once — the canonical router mount, the `X-API-Version`
/// header, the version-discovery document, and every path in the OpenAPI
/// document / Scalar docs — to the new version. Nothing else hard-codes it.
const API_VERSION: &str = "v1";

/// The versioned base path every documented endpoint is served under, derived
/// solely from [`API_VERSION`] (e.g. `/api/v1`). Handlers declare their
/// `#[utoipa::path]` *relative* to this (e.g. `/scan`); the prefix is applied
/// in exactly one place — [`versioned_openapi`] for the docs and [`routes`] for
/// the router — so a version bump never touches a handler.
pub fn api_base_path() -> String {
    format!("/api/{API_VERSION}")
}

/// Builds the OpenAPI document with the version prefix applied. utoipa's
/// `#[utoipa::path]` only accepts string *literals*, so handlers declare bare
/// relative paths and we prepend [`api_base_path`] to every operation here,
/// keeping [`API_VERSION`] the only place the version is written. `{base}`
/// placeholders in the prose description are expanded the same way.
fn versioned_openapi() -> utoipa::openapi::OpenApi {
    let mut openapi = Schema::openapi();
    let base = api_base_path();

    let versioned = openapi
        .paths
        .paths
        .iter()
        .map(|(path, item)| (format!("{base}{path}"), item.clone()))
        .collect();
    openapi.paths.paths = versioned;

    if let Some(description) = openapi.info.description.as_mut() {
        *description = description.replace("{base}", &base);
    }

    openapi
}

async fn spec() -> Json<utoipa::openapi::OpenApi> {
    Json(versioned_openapi())
}

/// A single advertised API version in the discovery document.
#[derive(Serialize)]
struct VersionInfo {
    /// The version identifier, e.g. `v1`.
    version: &'static str,
    /// Lifecycle status: `stable`, `deprecated`, or `sunset`.
    status: &'static str,
    /// The path prefix requests for this version should target.
    base_path: String,
}

/// Version-discovery document returned by `GET /api`.
#[derive(Serialize)]
struct ApiVersions {
    /// The version new integrations should target.
    current: &'static str,
    /// Every version the server currently accepts requests for.
    versions: Vec<VersionInfo>,
}

/// `GET /api` — lets a client discover which API versions exist and which one
/// to target, without hard-coding the prefix. Derived entirely from
/// [`API_VERSION`], so it stays correct across version bumps.
async fn versions() -> Json<ApiVersions> {
    Json(ApiVersions {
        current: API_VERSION,
        versions: vec![VersionInfo {
            version: API_VERSION,
            status: "stable",
            base_path: api_base_path(),
        }],
    })
}

/// Stamps `X-API-Version` onto every API response so a client can tell which
/// version served it regardless of which path prefix it used.
async fn stamp_version(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert("x-api-version", HeaderValue::from_static(API_VERSION));
    response
}

/// Marks a response as coming from the deprecated unversioned alias. Emits the
/// RFC 8594 `Deprecation` header plus a `Link` pointing at the successor so
/// tooling can surface a migration warning. The alias keeps working; this is a
/// nudge, not an error.
async fn mark_deprecated(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert("deprecation", HeaderValue::from_static("true"));
    // Points at the current successor version, derived from `API_VERSION`.
    if let Ok(link) = HeaderValue::from_str(&format!("<{}>; rel=\"successor-version\"", api_base_path())) {
        headers.insert("link", link);
    }
    response
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
    fn openapi_spec_is_version_prefixed() {
        // The version prefix is applied at runtime (utoipa can't read a const in
        // its path attribute), so assert against the *prefixed* document the
        // docs endpoint actually serves. Building `versioned_openapi()` also
        // catches duplicate operation ids / malformed path specs that compile
        // but panic. Expectations are derived from `api_base_path()`, so this
        // test tracks a version bump automatically.
        let spec = versioned_openapi();
        let paths = &spec.paths.paths;
        let base = api_base_path();
        for suffix in [
            "/scan",
            "/convert",
            "/image/{op}",
            "/metadata",
            "/render/code",
            "/render/screenshot",
            "/render/markdown-pdf",
            "/convert/transcode",
            "/admin/updates",
        ] {
            let expected = format!("{base}{suffix}");
            assert!(paths.contains_key(&expected), "missing {expected} in OpenAPI spec");
        }
    }

    #[test]
    fn base_path_derives_from_version() {
        // The whole surface hangs off this: guard the shape so a bad edit to the
        // const (e.g. a stray slash) is caught here rather than at server start.
        assert_eq!(api_base_path(), format!("/api/{API_VERSION}"));
        assert!(api_base_path().starts_with("/api/"));
    }

    #[test]
    fn router_builds_without_route_conflicts() {
        // matchit panics at registration on conflicting paths (e.g. a static
        // segment overlapping a `:param` on axum 0.7). Building the router
        // here surfaces that as a test failure rather than at server start.
        let _ = routes();
    }
}

/// The documented API surface, with paths declared *relative* to the version
/// prefix (e.g. `/scan`). Returned as a fresh router so it can be mounted twice:
/// canonically under the versioned segment and, for backwards compatibility,
/// bare under `/api` (see [`routes`]).
fn v1() -> Router<AppState> {
    Router::new()
        .route("/images/upload", post(images::upload_files))
        .route("/images/download", post(images::download_images))
        .route("/images/:id", delete(images::delete_image_by_id))
        .route(
            "/guilds/:guild_id/images/upload",
            post(guild_images::upload_guild_images),
        )
        .route("/guilds/:guild_id/images", get(guild_images::list_guild_images))
        .route("/guilds/:guild_id/images/:id", delete(guild_images::delete_guild_image))
        // Internal: the bot provisions a per-guild images:guild key here (bearer =
        // gallery_provision_token). Undocumented on purpose — not part of the
        // public API surface.
        .route(
            "/guilds/:guild_id/provision-key",
            post(guild_images::provision_guild_key),
        )
        .route("/scan", post(scan::scan_file))
        .route("/metadata", post(media::image_info))
        .route("/image/:op", post(media::manipulate_image))
        .route("/convert", post(media::convert_file))
        .route("/render/code", post(code::render_code))
        .route("/render/screenshot", post(external::screenshot))
        .route("/render/markdown-pdf", post(external::markdown_pdf))
        .route("/convert/transcode", post(external::transcode))
        .route("/admin/updates", get(admin::list_updates))
}

pub fn routes() -> Router<AppState> {
    // The deprecated unversioned alias: the same handlers under bare `/api/*`,
    // tagged with `Deprecation`/`Link` headers so existing API keys and tools
    // (ShareX configs, scripts) keep working while clients migrate to the
    // versioned prefix.
    let legacy = v1().layer(map_response(mark_deprecated));

    Router::new()
        .route("/", get(versions))
        .route("/openapi.json", get(spec))
        .route("/docs", get(docs))
        // Canonical, versioned mount — the segment comes from `API_VERSION`, so
        // this becomes `/api/v2/...` etc. with a single const change.
        .nest(&format!("/{API_VERSION}"), v1())
        .merge(legacy)
        // `X-API-Version` on every API response, versioned or aliased alike.
        .route_layer(map_response(stamp_version))
        .route_layer(RateLimit::default().quota(25, 60.0).build())
        .route_layer(
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST, Method::DELETE])
                .allow_credentials(true)
                .allow_origin(AllowOrigin::mirror_request())
                .allow_headers([AUTHORIZATION, USER_AGENT]),
        )
}
