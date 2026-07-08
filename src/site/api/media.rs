//! Public image-processing endpoints.
//!
//! - `POST /api/v1/image/{op}` — apply a visual effect (blur, pixelate, deepfry,
//!   invert, grayscale) and return a PNG.
//! - `POST /api/v1/convert` — transcode an image between raster formats
//!   (PNG → WebP, and friends).
//!
//! Both accept the source image either as a multipart `file` upload or as a
//! `url` form field pointing at a public http(s) image. URL fetches are
//! SSRF-guarded (private/reserved addresses are refused, redirects disabled,
//! and the download is size-capped).

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
use super::utils::{fetch_guarded, RateLimitResponse};

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

/// Fetches an image from a remote URL with SSRF protections (see
/// [`fetch_guarded`]): http(s) only, the resolved address must be public,
/// redirects are disabled, and the body is capped at [`crate::MAX_UPLOAD_SIZE`].
async fn fetch_remote_image(url_str: &str) -> Result<Vec<u8>, ApiError> {
    let body = fetch_guarded(url_str, crate::MAX_UPLOAD_SIZE as usize, "klappstuhl.me image fetcher").await?;
    if body.bytes.is_empty() {
        return Err(ApiError::new("remote image was empty"));
    }
    Ok(body.bytes)
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
    path = "/image/{op}",
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
    let account = auth.require_account(&state, Scope::ImagesRead).await?;

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
    path = "/convert",
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
    let account = auth.require_account(&state, Scope::ImagesRead).await?;

    let bytes = read_image_input(multipart).await?;
    let to = params.to.to_ascii_lowercase();
    let quality = params.quality.unwrap_or(85).clamp(1, 100);
    let share = params.share.unwrap_or(false);
    let to_for_task = to.clone();
    let (data, mime, ext) = tokio::task::spawn_blocking(move || {
        decode_image(&bytes).and_then(|img| encode_to(&img, &to_for_task, quality))
    })
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
            (
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"converted.{ext}\""),
            ),
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
    path = "/metadata",
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
    auth.require_account(&state, Scope::ImagesRead).await?;

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

// ─── Color palette extraction ──────────────────────────────────────────────────

/// One extracted palette color.
#[derive(Serialize, ToSchema)]
pub struct PaletteColor {
    /// The color as a `#rrggbb` hex string.
    pub hex: String,
    /// The color as `[r, g, b]`.
    pub rgb: [u8; 3],
    /// The share of sampled pixels this color covers (0..=1).
    pub proportion: f64,
}

/// The extracted palette, most dominant color first.
#[derive(Serialize, ToSchema)]
pub struct PaletteResult {
    /// The dominant colors, ordered by coverage.
    pub colors: Vec<PaletteColor>,
    /// How many pixels were sampled (after downscaling; transparent pixels are
    /// skipped).
    pub pixels_sampled: u64,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PaletteParams {
    /// How many colors to return (1–12, default 6).
    count: Option<usize>,
}

/// Extract a color palette
///
/// Returns the dominant colors of an image, most prominent first — handy for
/// theming, `accent-color` picks, or embed colors.
///
/// Supply the source as a multipart `file` upload or a `url` form field.
#[utoipa::path(
    post,
    path = "/color/palette",
    request_body(
        content = inline(ImageInput),
        content_type = "multipart/form-data",
        description = "The source image, as a `file` upload or a `url` field."
    ),
    params(PaletteParams),
    responses(
        (status = 200, description = "The dominant colors", body = PaletteResult),
        (status = 400, description = "Bad input or undecodable image", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["images:read"])),
    tag = "media"
)]
pub async fn color_palette(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Query(params): Query<PaletteParams>,
    multipart: Multipart,
) -> Result<Json<PaletteResult>, ApiError> {
    let account = auth.require_account(&state, Scope::ImagesRead).await?;

    let count = params.count.unwrap_or(6).clamp(1, 12);
    let bytes = read_image_input(multipart).await?;
    let result = tokio::task::spawn_blocking(move || -> Result<PaletteResult, ApiError> {
        let img = decode_image(&bytes)?;
        Ok(extract_palette(&img, count))
    })
    .await
    .map_err(|_| ApiError::new("palette extraction task failed"))??;

    state
        .audit("api.color.palette")
        .actor(&account)
        .ip_opt(client_ip)
        .fire();

    Ok(Json(result))
}

/// Dominant-color extraction by histogram binning: downscale, bucket pixels at
/// 4 bits per channel, average each bucket's true colors, then merge buckets
/// that are perceptually close so near-identical shades don't crowd out the
/// rest of the palette. Deterministic (no k-means seeding).
fn extract_palette(img: &DynamicImage, count: usize) -> PaletteResult {
    // Downscale for a bounded, structure-preserving sample.
    let small = img.resize(96, 96, FilterType::Triangle).to_rgba8();

    // 16 levels per channel → 4096 buckets: (count, sum r, sum g, sum b).
    let mut bins: std::collections::HashMap<u16, (u64, u64, u64, u64)> = std::collections::HashMap::new();
    let mut sampled = 0u64;
    for p in small.pixels() {
        let [r, g, b, a] = p.0;
        if a < 128 {
            continue;
        }
        sampled += 1;
        let key = ((r as u16 >> 4) << 8) | ((g as u16 >> 4) << 4) | (b as u16 >> 4);
        let e = bins.entry(key).or_default();
        e.0 += 1;
        e.1 += r as u64;
        e.2 += g as u64;
        e.3 += b as u64;
    }
    if sampled == 0 {
        return PaletteResult {
            colors: Vec::new(),
            pixels_sampled: 0,
        };
    }

    // Bucket → average color, biggest first.
    let mut clusters: Vec<(u64, [u8; 3])> = bins
        .into_values()
        .map(|(n, r, g, b)| (n, [(r / n) as u8, (g / n) as u8, (b / n) as u8]))
        .collect();
    clusters.sort_by_key(|(n, _)| std::cmp::Reverse(*n));

    // Greedy merge: absorb any cluster close to an already-kept color.
    let dist2 = |a: [u8; 3], b: [u8; 3]| -> u32 {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2) as u32;
        d(a[0], b[0]) + d(a[1], b[1]) + d(a[2], b[2])
    };
    let mut kept: Vec<(u64, [u8; 3])> = Vec::new();
    for (n, color) in clusters {
        match kept.iter_mut().find(|(_, k)| dist2(*k, color) < 32 * 32) {
            Some(existing) => existing.0 += n,
            None => kept.push((n, color)),
        }
    }
    kept.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    kept.truncate(count);

    PaletteResult {
        colors: kept
            .into_iter()
            .map(|(n, [r, g, b])| PaletteColor {
                hex: format!("#{r:02x}{g:02x}{b:02x}"),
                rgb: [r, g, b],
                proportion: n as f64 / sampled as f64,
            })
            .collect(),
        pixels_sampled: sampled,
    }
}

/// Stores `bytes` in the shareable-media cache and builds the JSON result
/// with an absolute `/m/:id` URL.
fn share_result(state: &AppState, bytes: Vec<u8>, content_type: &str) -> ShareResult {
    let id = state.store_media(bytes, content_type.to_string());
    let url = state.config().url_to(format!("/m/{id}"));
    ShareResult {
        id,
        url,
        content_type: content_type.to_string(),
    }
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
        DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, ImageFormat::Png)
            .unwrap();
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
    fn palette_finds_dominant_colors() {
        // Three-quarters red, one-quarter blue.
        let mut img = RgbaImage::new(40, 40);
        for (x, _, p) in img.enumerate_pixels_mut() {
            *p = if x < 30 {
                Rgba([220, 40, 30, 255])
            } else {
                Rgba([20, 60, 200, 255])
            };
        }
        let result = extract_palette(&DynamicImage::ImageRgba8(img), 6);
        assert!(result.pixels_sampled > 0);
        assert!(result.colors.len() >= 2);
        // Red dominates and comes first; proportions sum to ~1.
        let first = &result.colors[0];
        assert!(first.rgb[0] > first.rgb[2], "expected red first, got {:?}", first.rgb);
        assert!(first.proportion > 0.5);
        let total: f64 = result.colors.iter().map(|c| c.proportion).sum();
        assert!((0.9..=1.01).contains(&total));
        assert!(first.hex.starts_with('#') && first.hex.len() == 7);
    }

    #[test]
    fn palette_of_fully_transparent_image_is_empty() {
        let img = RgbaImage::new(8, 8); // all pixels default to alpha 0
        let result = extract_palette(&DynamicImage::ImageRgba8(img), 6);
        assert_eq!(result.pixels_sampled, 0);
        assert!(result.colors.is_empty());
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
}
