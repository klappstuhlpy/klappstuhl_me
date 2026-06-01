//! On-demand thumbnail generation for the image gallery grid.
//!
//! Decodes an uploaded image, downscales it to fit a small box, and re-encodes
//! it. Photos (no alpha) become JPEG to keep the bytes tiny; images with
//! transparency stay PNG so the checkerboard doesn't turn black. Animated
//! inputs collapse to their first frame — fine for a static grid preview.
//!
//! Anything the configured decoders can't read (e.g. AVIF in this build)
//! returns `None`, and the caller falls back to serving the original bytes.

use std::io::Cursor;

/// Longest edge of a generated thumbnail, in pixels. Images already smaller
/// than this in both dimensions are re-encoded but never upscaled.
const MAX_DIM: u32 = 400;

/// JPEG quality (0–100) for opaque thumbnails. 80 is visually clean while
/// keeping grid tiles in the tens-of-KB range.
const JPEG_QUALITY: u8 = 80;

/// Generates a thumbnail from raw image bytes.
///
/// Returns the encoded thumbnail and its MIME type, or `None` if the input
/// could not be decoded.
pub fn generate(bytes: &[u8]) -> Option<(Vec<u8>, &'static str)> {
    let img = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;

    // `thumbnail` preserves aspect ratio and only ever fits *within* the box,
    // so portrait/landscape both end up with their longest edge at MAX_DIM.
    let thumb = if img.width() > MAX_DIM || img.height() > MAX_DIM {
        img.thumbnail(MAX_DIM, MAX_DIM)
    } else {
        img
    };

    let mut out = Cursor::new(Vec::new());
    if thumb.color().has_alpha() {
        thumb.write_to(&mut out, image::ImageFormat::Png).ok()?;
        Some((out.into_inner(), "image/png"))
    } else {
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
        encoder.encode_image(&thumb).ok()?;
        Some((out.into_inner(), "image/jpeg"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage, RgbaImage};

    fn encode_png(img: DynamicImage) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn downscales_large_opaque_image_to_jpeg() {
        let png = encode_png(DynamicImage::ImageRgb8(RgbImage::new(1000, 800)));
        let (bytes, ct) = generate(&png).expect("should generate");
        assert_eq!(ct, "image/jpeg");
        let dims = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        // Longest edge clamped to MAX_DIM, aspect ratio preserved.
        assert_eq!(dims, (MAX_DIM, 320));
    }

    #[test]
    fn keeps_alpha_as_png() {
        let png = encode_png(DynamicImage::ImageRgba8(RgbaImage::new(800, 800)));
        let (_bytes, ct) = generate(&png).expect("should generate");
        assert_eq!(ct, "image/png");
    }

    #[test]
    fn does_not_upscale_small_images() {
        let png = encode_png(DynamicImage::ImageRgb8(RgbImage::new(100, 50)));
        let (bytes, _ct) = generate(&png).expect("should generate");
        let dims = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!(dims, (100, 50));
    }

    #[test]
    fn undecodable_input_returns_none() {
        assert!(generate(b"not an image at all").is_none());
    }
}
