//! API endpoints backed by optional external binaries (Chromium, ffmpeg).
//!
//! Each is config-gated: when the required tool isn't installed/configured the
//! handler returns a `500` error with a clear "not available" message rather
//! than failing obscurely.

use axum::extract::{Multipart, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::error::{ApiError, ApiErrorCode};
use crate::{exttools, headers::ClientIp, models::Scope, AppState};

use super::auth::ApiToken;
use super::utils::{ApiJson, RateLimitResponse};

fn unavailable(tool: &str) -> ApiError {
    ApiError::new(format!("{tool} is not available on this server")).with_code(ApiErrorCode::ServerError)
}

fn shared_or_bytes(state: &AppState, bytes: Vec<u8>, content_type: &str, share: bool) -> Response {
    if share {
        let id = state.store_media(bytes, content_type.to_string());
        let url = state.config().url_to(format!("/m/{id}"));
        return ApiJson(serde_json::json!({
            "id": id, "url": url, "content_type": content_type,
        }))
        .into_response();
    }
    ([(header::CONTENT_TYPE, content_type.to_string())], bytes).into_response()
}

// ─── Screenshot ──────────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct ScreenshotRequest {
    /// Public http(s) URL to capture. Private/reserved addresses are refused.
    pub url: String,
    /// Viewport width in pixels (default 1280).
    #[serde(default)]
    pub width: Option<u32>,
    /// Viewport height in pixels (default 800).
    #[serde(default)]
    pub height: Option<u32>,
    /// Emulate dark mode.
    #[serde(default)]
    pub dark_mode: bool,
    /// Emulate a mobile viewport / user-agent.
    #[serde(default)]
    pub mobile: bool,
    /// Capture a tall (approximate full-page) screenshot.
    #[serde(default)]
    pub full_page: bool,
}

#[derive(Deserialize)]
pub(crate) struct ShareQuery {
    #[serde(default)]
    share: Option<bool>,
}

/// Screenshot
///
/// Render a web page to a PNG with headless Chromium. Supports dark mode, a
/// mobile viewport, and approximate full-page capture.
///
/// Requires a Chromium/Chrome binary on the server (`chromium_path` config or
/// on `PATH`); otherwise returns 503.
#[utoipa::path(
    post,
    path = "/render/screenshot",
    request_body = ScreenshotRequest,
    responses(
        (status = 200, description = "PNG screenshot", content_type = "image/png", body = Vec<u8>),
        (status = 400, description = "Invalid or non-public URL", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 500, description = "Chromium not available or render failed", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["images:read"])),
    tag = "render"
)]
pub async fn screenshot(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Query(q): Query<ShareQuery>,
    ApiJson(req): ApiJson<ScreenshotRequest>,
) -> Result<Response, ApiError> {
    auth.require(Scope::ImagesRead)?;
    let account = state.get_account(auth.id).await.ok_or_else(ApiError::unauthorized)?;

    exttools::assert_public_url(&req.url).await.map_err(ApiError::new)?;
    let Some(bin) = exttools::chromium(&state) else {
        return Err(unavailable("Chromium"));
    };

    let opts = exttools::ShotOptions {
        width: req.width.unwrap_or(1280).clamp(64, 3840),
        height: req.height.unwrap_or(800).clamp(64, 8000),
        dark_mode: req.dark_mode,
        mobile: req.mobile,
        full_page: req.full_page,
    };
    let png = exttools::screenshot(&bin, &req.url, &opts)
        .await
        .map_err(|e| ApiError::new(e).with_code(ApiErrorCode::ServerError))?;

    state
        .audit("api.render.screenshot")
        .actor(&account)
        .target(req.url)
        .ip_opt(client_ip)
        .fire();
    Ok(shared_or_bytes(&state, png, "image/png", q.share.unwrap_or(false)))
}

// ─── Markdown → PDF ──────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct MarkdownRequest {
    /// The Markdown source to render.
    pub markdown: String,
}

fn markdown_to_html_doc(md: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(md, opts);
    let mut body = String::new();
    html::push_html(&mut body, parser);
    format!(
        r##"<!doctype html><html><head><meta charset="utf-8"><style>
body {{ font-family: -apple-system, Segoe UI, Roboto, sans-serif; line-height: 1.6; max-width: 46rem; margin: 2rem auto; padding: 0 1rem; color: #111; }}
pre {{ background: #f4f4f5; padding: 0.8rem; border-radius: 6px; overflow-x: auto; }}
code {{ font-family: ui-monospace, monospace; }}
table {{ border-collapse: collapse; }} th, td {{ border: 1px solid #ddd; padding: 0.3rem 0.6rem; }}
blockquote {{ border-left: 3px solid #ddd; margin: 0; padding-left: 1rem; color: #555; }}
</style></head><body>{body}</body></html>"##
    )
}

/// Markdown to PDF
///
/// Convert Markdown to a PDF document (rendered via headless Chromium).
/// Requires Chromium on the server; otherwise returns 503.
#[utoipa::path(
    post,
    path = "/render/markdown-pdf",
    request_body = MarkdownRequest,
    responses(
        (status = 200, description = "PDF document", content_type = "application/pdf", body = Vec<u8>),
        (status = 400, description = "Empty markdown", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 500, description = "Chromium not available or render failed", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["images:read"])),
    tag = "render"
)]
pub async fn markdown_pdf(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Query(q): Query<ShareQuery>,
    ApiJson(req): ApiJson<MarkdownRequest>,
) -> Result<Response, ApiError> {
    auth.require(Scope::ImagesRead)?;
    let account = state.get_account(auth.id).await.ok_or_else(ApiError::unauthorized)?;
    if req.markdown.trim().is_empty() {
        return Err(ApiError::new("`markdown` is required"));
    }
    let Some(bin) = exttools::chromium(&state) else {
        return Err(unavailable("Chromium"));
    };

    let html = markdown_to_html_doc(&req.markdown);
    let pdf = exttools::html_to_pdf(&bin, &html)
        .await
        .map_err(|e| ApiError::new(e).with_code(ApiErrorCode::ServerError))?;

    state
        .audit("api.render.markdown_pdf")
        .actor(&account)
        .ip_opt(client_ip)
        .fire();
    Ok(shared_or_bytes(
        &state,
        pdf,
        "application/pdf",
        q.share.unwrap_or(false),
    ))
}

// ─── ffmpeg transcode (MOV→MP4, HEIC→JPG) ────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct TranscodeQuery {
    /// Target format: `mp4` (e.g. from MOV) or `jpg` (e.g. from HEIC).
    to: String,
    #[serde(default)]
    share: Option<bool>,
}

#[derive(ToSchema)]
#[allow(dead_code)]
struct TranscodeUpload {
    /// The media file to convert.
    #[schema(format = Binary)]
    file: String,
}

/// Transcode
///
/// Convert media that needs ffmpeg: `to=mp4` (e.g. MOV→MP4, H.264/AAC) or
/// `to=jpg` (e.g. HEIC→JPG). Send the source as a multipart `file`.
///
/// Requires an `ffmpeg` binary on the server; otherwise returns 503.
#[utoipa::path(
    post,
    path = "/convert/transcode",
    params(("to" = String, Query, description = "Target format: mp4 or jpg")),
    request_body(content = inline(TranscodeUpload), content_type = "multipart/form-data"),
    responses(
        (status = 200, description = "The converted file", body = Vec<u8>),
        (status = 400, description = "Missing file or unsupported target", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 500, description = "ffmpeg not available or conversion failed", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["images:read"])),
    tag = "render"
)]
pub async fn transcode(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Query(q): Query<TranscodeQuery>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    auth.require(Scope::ImagesRead)?;
    let account = state.get_account(auth.id).await.ok_or_else(ApiError::unauthorized)?;

    let to = q.to.to_ascii_lowercase();
    let (in_ext, out_ext, out_args, mime): (&str, &str, Vec<&str>, &str) = match to.as_str() {
        "mp4" => (
            "mov",
            "mp4",
            vec!["-c:v", "libx264", "-c:a", "aac", "-movflags", "+faststart"],
            "video/mp4",
        ),
        "jpg" | "jpeg" => ("heic", "jpg", vec!["-q:v", "2"], "image/jpeg"),
        other => return Err(ApiError::new(format!("unsupported target `{other}` (try mp4 or jpg)"))),
    };

    // Read the uploaded file.
    let mut data: Option<Vec<u8>> = None;
    let mut upload_ext: Option<String> = None;
    while let Some(field) = multipart.next_field().await.map_err(|e| ApiError::new(e.to_string()))? {
        if field.name() == Some("file") {
            if let Some(fname) = field.file_name() {
                if let Some(ext) = std::path::Path::new(fname).extension().and_then(|e| e.to_str()) {
                    upload_ext = Some(ext.to_ascii_lowercase());
                }
            }
            let bytes = field.bytes().await.map_err(|e| ApiError::new(e.to_string()))?;
            if !bytes.is_empty() {
                data = Some(bytes.to_vec());
            }
            break;
        }
    }
    let Some(data) = data else {
        return Err(ApiError::new("no `file` field in upload"));
    };

    let Some(bin) = exttools::ffmpeg(&state) else {
        return Err(unavailable("ffmpeg"));
    };
    let in_ext = upload_ext.as_deref().unwrap_or(in_ext);
    let out = exttools::ffmpeg_convert(&bin, &data, in_ext, out_ext, &out_args)
        .await
        .map_err(|e| ApiError::new(e).with_code(ApiErrorCode::ServerError))?;

    state
        .audit("api.convert.transcode")
        .actor(&account)
        .target(to)
        .ip_opt(client_ip)
        .fire();
    Ok(shared_or_bytes(&state, out, mime, q.share.unwrap_or(false)))
}
