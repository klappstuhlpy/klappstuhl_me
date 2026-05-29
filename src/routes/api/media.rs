//! Public image-processing endpoints.
//!
//! - `POST /api/image/{op}` — apply a visual effect (blur, pixelate, deepfry,
//!   invert, grayscale) and return a PNG.
//! - `POST /api/convert` — transcode an image between raster formats
//!   (PNG → WebP, and friends).
//!
//! Both accept the source image either as a multipart `file` upload or as a
//! `url` form field pointing at a public http(s) image. URL fetches are
//! SSRF-guarded (private/reserved addresses are refused, redirects disabled,
//! and the download is size-capped).

use std::net::IpAddr;
use std::time::Duration;

use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::{error::ApiError, headers::ClientIp, models::Scope, AppState};

use super::auth::ApiToken;
use super::utils::RateLimitResponse;

// ─── Shared request body (documentation) ──────────────────────────────────────

/// The image to operate on. Supply exactly one of `file` or `url`.
#[derive(ToSchema)]
#[allow(dead_code)]
struct ImageInput {
    /// The image to process as a binary upload. Optional if `url` is supplied.
    #[schema(format = Binary)]
    file: Option<String>,
    /// A public http(s) URL the server fetches the image from. Optional if
    /// `file` is uploaded.
    url: Option<String>,
}

// ─── Input handling ────────────────────────────────────────────────────────────

/// Reads the source image out of a multipart body: either the `file` upload or
/// (failing that) the `url` field, which is fetched server-side.
async fn read_image_input(mut mp: Multipart) -> Result<Vec<u8>, ApiError> {
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut url: Option<String> = None;

    while let Some(field) = mp.next_field().await.map_err(|e| ApiError::new(e.to_string()))? {
        match field.name().unwrap_or("") {
            "file" => {
                let b = field.bytes().await.map_err(|e| ApiError::new(e.to_string()))?;
                if !b.is_empty() {
                    file_bytes = Some(b.to_vec());
                }
            }
            "url" => {
                let t = field.text().await.map_err(|e| ApiError::new(e.to_string()))?;
                let t = t.trim().to_string();
                if !t.is_empty() {
                    url = Some(t);
                }
            }
            _ => {}
        }
    }

    if let Some(b) = file_bytes {
        return Ok(b);
    }
    if let Some(u) = url {
        return fetch_remote_image(&u).await;
    }
    Err(ApiError::new("provide either a `file` upload or a `url` field"))
}

/// Returns true if `ip` is loopback, private, link-local, or otherwise
/// reserved — i.e. an address an SSRF attacker might target. Used to reject
/// `url` inputs that resolve to internal infrastructure.
fn ip_is_blocked(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.octets()[0] == 0
                // Carrier-grade NAT, 100.64.0.0/10
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 0x40)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // Unique local, fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local, fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .map(|v4| ip_is_blocked(&IpAddr::V4(v4)))
                    .unwrap_or(false)
        }
    }
}

/// Fetches an image from a remote URL with SSRF protections: http(s) only,
/// the resolved address must be public, redirects are disabled, and the body
/// is capped at [`crate::MAX_UPLOAD_SIZE`].
async fn fetch_remote_image(url_str: &str) -> Result<Vec<u8>, ApiError> {
    let url = reqwest::Url::parse(url_str).map_err(|_| ApiError::new("invalid url"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ApiError::new("url must use http or https"));
    }
    let host = url.host_str().ok_or_else(|| ApiError::new("url has no host"))?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err(ApiError::new("refusing to fetch a local address"));
    }

    // Resolve up front and verify every candidate address is public. This
    // defends against hostnames that point at internal infrastructure.
    let port = url.port_or_known_default().unwrap_or(80);
    let mut resolved = false;
    for addr in tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| ApiError::new("could not resolve url host"))?
    {
        resolved = true;
        if ip_is_blocked(&addr.ip()) {
            return Err(ApiError::new("refusing to fetch a private or reserved address"));
        }
    }
    if !resolved {
        return Err(ApiError::new("could not resolve url host"));
    }

    // Dedicated client: no redirects (a redirect could otherwise bounce us to
    // a blocked address after the check above) and a short timeout.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(ApiError::from)?;

    let resp = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "klappstuhl.me image fetcher")
        .send()
        .await
        .map_err(|e| ApiError::new(format!("fetch failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(ApiError::new(format!("remote returned HTTP {}", resp.status().as_u16())));
    }

    use futures_util::StreamExt;
    let cap = crate::MAX_UPLOAD_SIZE as usize;
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::new(format!("fetch failed: {e}")))?;
        if buf.len() + chunk.len() > cap {
            return Err(ApiError::new("remote image exceeds the size limit"));
        }
        buf.extend_from_slice(&chunk);
    }
    if buf.is_empty() {
        return Err(ApiError::new("remote image was empty"));
    }
    Ok(buf)
}

// ─── Image codec helpers (CPU-bound; run on a blocking thread) ─────────────────

fn decode_image(bytes: &[u8]) -> Result<DynamicImage, ApiError> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| ApiError::new(format!("could not read image: {e}")))?
        .decode()
        .map_err(|e| ApiError::new(format!("could not decode image: {e}")))
}

fn encode_png(img: &DynamicImage) -> Result<Vec<u8>, ApiError> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| ApiError::new(format!("could not encode image: {e}")))?;
    Ok(buf.into_inner())
}

/// Boosts colour saturation by pushing each channel away from the pixel's luma.
fn saturate(img: &DynamicImage, factor: f32) -> DynamicImage {
    let mut rgb = img.to_rgb8();
    for p in rgb.pixels_mut() {
        let [r, g, b] = p.0;
        let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        let adj = |c: u8| -> u8 { (luma + (c as f32 - luma) * factor).round().clamp(0.0, 255.0) as u8 };
        p.0 = [adj(r), adj(g), adj(b)];
    }
    DynamicImage::ImageRgb8(rgb)
}

/// The classic "deep-fried" look: crank contrast and saturation, then bake in
/// heavy JPEG artifacts. `intensity` (1–100) controls how many passes run.
fn deepfry(mut img: DynamicImage, intensity: f32) -> DynamicImage {
    let passes = ((intensity / 20.0).round() as i32).clamp(1, 6);
    for _ in 0..passes {
        img = img.adjust_contrast(40.0);
        img = saturate(&img, 1.6);
        let mut buf = Vec::new();
        let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
        if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 8)
            .encode_image(&rgb)
            .is_ok()
        {
            if let Ok(decoded) = image::load_from_memory_with_format(&buf, ImageFormat::Jpeg) {
                img = decoded;
            }
        }
    }
    img
}

fn apply_op(op: &str, img: DynamicImage, amount: Option<f32>) -> Result<DynamicImage, ApiError> {
    Ok(match op {
        "blur" => img.blur(amount.unwrap_or(8.0).clamp(0.1, 100.0)),
        "pixelate" => {
            let factor = amount.unwrap_or(16.0).clamp(2.0, 256.0);
            let (w, h) = (img.width().max(1), img.height().max(1));
            let sw = ((w as f32 / factor) as u32).max(1);
            let sh = ((h as f32 / factor) as u32).max(1);
            img.resize_exact(sw, sh, FilterType::Nearest)
                .resize_exact(w, h, FilterType::Nearest)
        }
        "invert" => {
            let mut img = img;
            img.invert();
            img
        }
        "grayscale" => img.grayscale(),
        "deepfry" => deepfry(img, amount.unwrap_or(50.0)),
        other => {
            return Err(ApiError::new(format!(
                "unknown operation `{other}` (try blur, pixelate, deepfry, invert, grayscale)"
            )))
        }
    })
}

fn encode_to(img: &DynamicImage, to: &str, quality: u8) -> Result<(Vec<u8>, &'static str, &'static str), ApiError> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let enc = |buf: &mut std::io::Cursor<Vec<u8>>, fmt: ImageFormat, img: &DynamicImage| {
        img.write_to(buf, fmt)
            .map_err(|e| ApiError::new(format!("could not encode image: {e}")))
    };
    let (mime, ext) = match to {
        "png" => {
            enc(&mut buf, ImageFormat::Png, img)?;
            ("image/png", "png")
        }
        "jpeg" | "jpg" => {
            // JpegEncoder lets us honour the quality knob; it needs RGB input.
            let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
                .encode_image(&rgb)
                .map_err(|e| ApiError::new(format!("could not encode image: {e}")))?;
            ("image/jpeg", "jpg")
        }
        "webp" => {
            // image's WebP encoder is lossless and wants RGB/RGBA input.
            enc(&mut buf, ImageFormat::WebP, &DynamicImage::ImageRgba8(img.to_rgba8()))?;
            ("image/webp", "webp")
        }
        "gif" => {
            enc(&mut buf, ImageFormat::Gif, img)?;
            ("image/gif", "gif")
        }
        "bmp" => {
            enc(&mut buf, ImageFormat::Bmp, img)?;
            ("image/bmp", "bmp")
        }
        "tiff" => {
            enc(&mut buf, ImageFormat::Tiff, img)?;
            ("image/tiff", "tiff")
        }
        other => {
            return Err(ApiError::new(format!(
                "unsupported target format `{other}` (try png, jpeg, webp, gif, bmp, tiff)"
            )))
        }
    };
    Ok((buf.into_inner(), mime, ext))
}

// ─── Query params ──────────────────────────────────────────────────────────────

#[derive(Deserialize, IntoParams)]
pub(crate) struct ManipulateParams {
    /// Effect strength, interpreted per operation: `blur` → Gaussian sigma
    /// (default 8), `pixelate` → block size in pixels (default 16),
    /// `deepfry` → intensity 1–100 (default 50). Ignored by `invert` and
    /// `grayscale`.
    amount: Option<f32>,
    /// When `true`, store the result and return a JSON `ShareResult` with a
    /// short shareable `/m/:id` link instead of the raw image bytes.
    share: Option<bool>,
}

#[derive(Deserialize, IntoParams)]
pub(crate) struct ConvertParams {
    /// Target format. One of: `png`, `jpeg` (alias `jpg`), `webp`, `gif`,
    /// `bmp`, `tiff`.
    to: String,
    /// JPEG quality, 1–100. Only used when `to=jpeg`. Defaults to 85.
    quality: Option<u8>,
    /// When `true`, store the result and return a JSON `ShareResult` with a
    /// short shareable `/m/:id` link instead of the raw image bytes.
    share: Option<bool>,
}

/// Returned (instead of raw bytes) when an endpoint is called with `share=true`.
#[derive(Serialize, ToSchema)]
pub struct ShareResult {
    /// The short id of the stored result.
    pub id: String,
    /// Absolute URL where the result can be viewed (`/m/:id`).
    pub url: String,
    /// The MIME type of the stored result.
    pub content_type: String,
}

// ─── Handlers ──────────────────────────────────────────────────────────────────

/// Manipulate
///
/// Apply a visual effect to an image and get a PNG back.
///
/// The `{op}` path segment selects the effect:
///
/// - `blur` — Gaussian blur (tune with `amount` = sigma).
/// - `pixelate` — mosaic effect (tune with `amount` = block size in pixels).
/// - `deepfry` — oversaturated, crunchy meme look (tune with `amount` = intensity 1–100).
/// - `invert` — invert all colours.
/// - `grayscale` — desaturate to gray.
///
/// Supply the source as a multipart `file` upload or a `url` form field. The
/// result is returned as `image/png`, or — with `share=true` — as a JSON
/// `ShareResult` carrying a short `/m/:id` link to the stored image.
#[utoipa::path(
    post,
    path = "/api/image/{op}",
    request_body(
        content = inline(ImageInput),
        content_type = "multipart/form-data",
        description = "The source image, as a `file` upload or a `url` field."
    ),
    params(
        ("op" = String, Path, description = "Effect: blur, pixelate, deepfry, invert, or grayscale"),
        ManipulateParams,
    ),
    responses(
        (status = 200, description = "The processed image", content_type = "image/png", body = Vec<u8>),
        (status = 400, description = "Bad input or unknown operation", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(
        ("api_key" = ["images:read"])
    ),
    tag = "media"
)]
pub async fn manipulate_image(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Path(op): Path<String>,
    Query(params): Query<ManipulateParams>,
    multipart: Multipart,
) -> Result<Response, ApiError> {
    auth.require(Scope::ImagesRead)?;
    let Some(account) = state.get_account(auth.id).await else {
        return Err(ApiError::unauthorized());
    };

    let op = op.to_ascii_lowercase();
    if !matches!(op.as_str(), "blur" | "pixelate" | "deepfry" | "invert" | "grayscale") {
        return Err(ApiError::new(format!(
            "unknown operation `{op}` (try blur, pixelate, deepfry, invert, grayscale)"
        )));
    }

    let bytes = read_image_input(multipart).await?;
    let amount = params.amount;
    let share = params.share.unwrap_or(false);
    let op_for_task = op.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, ApiError> {
        let img = decode_image(&bytes)?;
        let img = apply_op(&op_for_task, img, amount)?;
        encode_png(&img)
    })
    .await
    .map_err(|_| ApiError::new("image processing task failed"))??;

    state
        .audit("api.image")
        .actor(&account)
        .target(op)
        .ip_opt(client_ip)
        .fire();

    if share {
        return Ok(Json(share_result(&state, out, "image/png")).into_response());
    }
    Ok(([(header::CONTENT_TYPE, "image/png".to_string())], out).into_response())
}

/// Convert
///
/// Transcode an image to a different raster format — for example PNG → WebP.
///
/// Supported targets (`to` query parameter): `png`, `jpeg` (alias `jpg`),
/// `webp`, `gif`, `bmp`, `tiff`. WebP output is lossless; JPEG honours the
/// optional `quality` parameter (1–100, default 85).
///
/// Supply the source as a multipart `file` upload or a `url` form field. The
/// response carries the matching `Content-Type` and a `Content-Disposition`
/// filename, or — with `share=true` — a JSON `ShareResult` with a short
/// `/m/:id` link to the converted image.
#[utoipa::path(
    post,
    path = "/api/convert",
    request_body(
        content = inline(ImageInput),
        content_type = "multipart/form-data",
        description = "The source image, as a `file` upload or a `url` field."
    ),
    params(ConvertParams),
    responses(
        (status = 200, description = "The converted image", body = Vec<u8>),
        (status = 400, description = "Bad input or unsupported target format", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(
        ("api_key" = ["images:read"])
    ),
    tag = "media"
)]
pub async fn convert_file(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Query(params): Query<ConvertParams>,
    multipart: Multipart,
) -> Result<Response, ApiError> {
    auth.require(Scope::ImagesRead)?;
    let Some(account) = state.get_account(auth.id).await else {
        return Err(ApiError::unauthorized());
    };

    let bytes = read_image_input(multipart).await?;
    let to = params.to.to_ascii_lowercase();
    let quality = params.quality.unwrap_or(85).clamp(1, 100);
    let share = params.share.unwrap_or(false);
    let to_for_task = to.clone();
    let (data, mime, ext) = tokio::task::spawn_blocking(move || decode_image(&bytes).and_then(|img| encode_to(&img, &to_for_task, quality)))
        .await
        .map_err(|_| ApiError::new("image conversion task failed"))??;

    state
        .audit("api.convert")
        .actor(&account)
        .target(to)
        .ip_opt(client_ip)
        .fire();

    if share {
        return Ok(Json(share_result(&state, data, mime)).into_response());
    }

    Ok((
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::CONTENT_DISPOSITION, format!("inline; filename=\"converted.{ext}\"")),
        ],
        data,
    )
        .into_response())
}

/// Metadata about a decoded image.
#[derive(Serialize, ToSchema)]
pub struct ImageInfo {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Detected container format (e.g. `png`, `jpeg`, `webp`).
    pub format: String,
    /// Pixel color type (e.g. `Rgb8`, `Rgba8`, `L8`).
    pub color: String,
    /// Size of the supplied bytes.
    pub file_size: usize,
}

fn format_name(fmt: Option<ImageFormat>) -> String {
    match fmt {
        Some(ImageFormat::Png) => "png",
        Some(ImageFormat::Jpeg) => "jpeg",
        Some(ImageFormat::Gif) => "gif",
        Some(ImageFormat::WebP) => "webp",
        Some(ImageFormat::Bmp) => "bmp",
        Some(ImageFormat::Tiff) => "tiff",
        Some(_) => "other",
        None => "unknown",
    }
    .to_owned()
}

/// Info
///
/// Inspect an image and return its dimensions, format, color type, and byte
/// size — without storing anything.
///
/// Supply the source as a multipart `file` upload or a `url` form field.
#[utoipa::path(
    post,
    path = "/api/metadata",
    request_body(
        content = inline(ImageInput),
        content_type = "multipart/form-data",
        description = "The source image, as a `file` upload or a `url` field."
    ),
    responses(
        (status = 200, description = "Image metadata", body = ImageInfo),
        (status = 400, description = "Bad input or undecodable image", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(
        ("api_key" = ["images:read"])
    ),
    tag = "media"
)]
pub async fn image_info(
    State(state): State<AppState>,
    auth: ApiToken,
    multipart: Multipart,
) -> Result<Json<ImageInfo>, ApiError> {
    auth.require(Scope::ImagesRead)?;
    if state.get_account(auth.id).await.is_none() {
        return Err(ApiError::unauthorized());
    }

    let bytes = read_image_input(multipart).await?;
    let info = tokio::task::spawn_blocking(move || -> Result<ImageInfo, ApiError> {
        let reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|e| ApiError::new(format!("could not read image: {e}")))?;
        let format = format_name(reader.format());
        let img = reader
            .decode()
            .map_err(|e| ApiError::new(format!("could not decode image: {e}")))?;
        Ok(ImageInfo {
            width: img.width(),
            height: img.height(),
            format,
            color: format!("{:?}", img.color()),
            file_size: bytes.len(),
        })
    })
    .await
    .map_err(|_| ApiError::new("image inspection task failed"))??;

    Ok(Json(info))
}

/// Stores `bytes` in the shareable-media cache and builds the JSON result
/// with an absolute `/m/:id` URL.
fn share_result(state: &AppState, bytes: Vec<u8>, content_type: &str) -> ShareResult {
    let id = state.store_media(bytes, content_type.to_string());
    let url = state.config().url_to(format!("/m/{id}"));
    ShareResult { id, url, content_type: content_type.to_string() }
}

/// Serves a previously shared processed image by its short id. Public (no
/// auth) so the `/m/:id` links can be embedded anywhere. Returns 404 once the
/// entry has been evicted from the bounded cache.
pub async fn serve_media(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.get_media(&id) {
        Some(media) => (
            [
                (header::CONTENT_TYPE, media.content_type),
                (header::CACHE_CONTROL, "public, max-age=86400".to_string()),
            ],
            media.bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "media not found or expired").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    /// A small RGBA test image encoded as PNG bytes.
    fn sample_png() -> Vec<u8> {
        let mut img = RgbaImage::new(32, 24);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgba([(x * 8) as u8, (y * 10) as u8, 128, 255]);
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(img).write_to(&mut buf, ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn every_op_decodes_and_reencodes() {
        let png = sample_png();
        for op in ["blur", "pixelate", "deepfry", "invert", "grayscale"] {
            let img = decode_image(&png).expect("decode");
            let out = apply_op(op, img, None).unwrap_or_else(|e| panic!("op {op}: {e:?}"));
            let bytes = encode_png(&out).unwrap_or_else(|e| panic!("encode {op}: {e:?}"));
            // Output must round-trip back through the decoder.
            decode_image(&bytes).unwrap_or_else(|e| panic!("re-decode {op}: {e:?}"));
        }
    }

    #[test]
    fn unknown_op_is_rejected() {
        let img = decode_image(&sample_png()).unwrap();
        assert!(apply_op("explode", img, None).is_err());
    }

    #[test]
    fn converts_to_every_format() {
        let png = sample_png();
        for to in ["png", "jpeg", "jpg", "webp", "gif", "bmp", "tiff"] {
            let img = decode_image(&png).expect("decode");
            let (bytes, mime, ext) = encode_to(&img, to, 85).unwrap_or_else(|e| panic!("to {to}: {e:?}"));
            assert!(!bytes.is_empty(), "{to} produced no bytes");
            assert!(!mime.is_empty() && !ext.is_empty());
            // The transcoded bytes must be a valid image we can read back.
            decode_image(&bytes).unwrap_or_else(|e| panic!("re-decode {to}: {e:?}"));
        }
    }

    #[test]
    fn unsupported_target_is_rejected() {
        let img = decode_image(&sample_png()).unwrap();
        assert!(encode_to(&img, "heic", 85).is_err());
    }

    #[test]
    fn private_addresses_are_blocked() {
        use std::net::IpAddr;
        for ip in ["127.0.0.1", "10.0.0.1", "192.168.1.1", "169.254.1.1", "::1", "fe80::1", "100.64.0.1"] {
            assert!(ip_is_blocked(&ip.parse::<IpAddr>().unwrap()), "{ip} should be blocked");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!ip_is_blocked(&ip.parse::<IpAddr>().unwrap()), "{ip} should be allowed");
        }
    }
}
