//! QR-code rendering endpoint.
//!
//! Turns arbitrary text/URLs into a QR code as either a scalable SVG (pure,
//! raster-free) or a PNG raster. Lives under the `render` tag alongside the code
//! screenshot renderer and is gated by the same `images:read` scope.

use std::io::Cursor;

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use utoipa::ToSchema;

use super::auth::ApiToken;
use super::utils::{ApiJson, RateLimitResponse};
use crate::{error::ApiError, headers::ClientIp, models::Scope, AppState};

/// Maximum length of the encoded payload. QR codes top out well below this; the
/// cap simply refuses obviously-abusive inputs early.
const MAX_DATA_BYTES: usize = 4096;
/// Clamp bounds for the requested pixel dimension (SVG min-dimension / PNG side).
const MIN_SIZE: u32 = 64;
const MAX_SIZE: u32 = 2048;
const DEFAULT_SIZE: u32 = 512;

/// Output format for the QR code.
#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum QrFormat {
    /// Scalable vector graphic (`image/svg+xml`). The default.
    #[default]
    Svg,
    /// PNG raster (`image/png`).
    Png,
}

/// Error-correction level, trading data density for damage resistance.
#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum QrEcc {
    /// ~7% recovery.
    Low,
    /// ~15% recovery. The default.
    #[default]
    Medium,
    /// ~25% recovery.
    Quartile,
    /// ~30% recovery.
    High,
}

impl QrEcc {
    fn to_level(self) -> qrcode::EcLevel {
        match self {
            QrEcc::Low => qrcode::EcLevel::L,
            QrEcc::Medium => qrcode::EcLevel::M,
            QrEcc::Quartile => qrcode::EcLevel::Q,
            QrEcc::High => qrcode::EcLevel::H,
        }
    }
}

/// Body of a QR render request.
#[derive(Debug, Deserialize, ToSchema)]
pub struct QrRequest {
    /// The text or URL to encode.
    pub data: String,
    /// Target pixel size (clamped to 64..=2048; default 512). For SVG this is
    /// the minimum dimension; for PNG the image side length.
    #[serde(default)]
    pub size: Option<u32>,
    /// Output format: `svg` (default) or `png`.
    #[serde(default)]
    pub format: QrFormat,
    /// Error-correction level: `low`, `medium` (default), `quartile`, `high`.
    #[serde(default)]
    pub ecc: QrEcc,
    /// Whether to include the surrounding "quiet zone" margin. Defaults to true.
    #[serde(default = "default_margin")]
    pub margin: bool,
}

fn default_margin() -> bool {
    true
}

/// Render a QR code
///
/// Encodes `data` as a QR code and returns it as an SVG or PNG image.
#[utoipa::path(
    post,
    path = "/render/qr",
    request_body = QrRequest,
    responses(
        (status = 200, description = "The rendered QR code (SVG or PNG)", content_type = "image/svg+xml"),
        (status = 400, description = "Missing, oversized, or unencodable data", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["images:read"])),
    tag = "render"
)]
pub async fn render_qr(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    ApiJson(req): ApiJson<QrRequest>,
) -> Result<Response, ApiError> {
    let account = auth.require_account(&state, Scope::ImagesRead).await?;

    if req.data.is_empty() {
        return Err(ApiError::validation("data", "`data` is required"));
    }
    if req.data.len() > MAX_DATA_BYTES {
        return Err(ApiError::validation("data", "data too large for a QR code (4 KB max)"));
    }

    let size = req.size.unwrap_or(DEFAULT_SIZE).clamp(MIN_SIZE, MAX_SIZE);
    let data = req.data;
    let ecc = req.ecc.to_level();
    let margin = req.margin;
    let format = req.format;

    let (bytes, content_type) = tokio::task::spawn_blocking(move || render(&data, ecc, size, margin, format))
        .await
        .map_err(|_| ApiError::new("render task failed"))?
        .map_err(|e| ApiError::validation("data", e))?;

    state.audit("api.render.qr").actor(&account).ip_opt(client_ip).fire();

    Ok(([(header::CONTENT_TYPE, content_type)], bytes).into_response())
}

/// Pure render: encode `data` and rasterise/serialise to the requested format.
/// Returns the encoded bytes and their content type, or a user-facing message.
fn render(
    data: &str,
    ecc: qrcode::EcLevel,
    size: u32,
    margin: bool,
    format: QrFormat,
) -> Result<(Vec<u8>, String), &'static str> {
    use qrcode::QrCode;

    let code =
        QrCode::with_error_correction_level(data.as_bytes(), ecc).map_err(|_| "could not encode data as a QR code")?;

    match format {
        QrFormat::Svg => {
            use qrcode::render::svg;
            let svg = code
                .render::<svg::Color>()
                .min_dimensions(size, size)
                .quiet_zone(margin)
                .build();
            Ok((svg.into_bytes(), "image/svg+xml".to_string()))
        }
        QrFormat::Png => {
            use image::Luma;
            let img = code
                .render::<Luma<u8>>()
                .min_dimensions(size, size)
                .quiet_zone(margin)
                .build();
            let mut bytes = Cursor::new(Vec::new());
            img.write_to(&mut bytes, image::ImageFormat::Png)
                .map_err(|_| "failed to encode PNG")?;
            Ok((bytes.into_inner(), "image/png".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_svg_and_png() {
        let ecc = QrEcc::Medium.to_level();
        let (svg, ct) = render("https://klappstuhl.me", ecc, 256, true, QrFormat::Svg).unwrap();
        assert_eq!(ct, "image/svg+xml");
        assert!(svg.starts_with(b"<?xml") || svg.starts_with(b"<svg"));

        let (png, ct) = render("hello", ecc, 128, false, QrFormat::Png).unwrap();
        assert_eq!(ct, "image/png");
        // PNG magic number.
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    }
}
