//! Strips privacy-sensitive metadata (EXIF, XMP, text chunks) from uploaded
//! images.
//!
//! This works at the container level — removing metadata segments/chunks
//! without touching the pixel data — rather than decoding and re-encoding.
//! That keeps the image byte-for-byte identical apart from the removed
//! metadata, preserves quality (no JPEG recompression), and crucially keeps
//! animation intact for APNG (re-encoding through the `image` crate would
//! flatten it to a single frame).
//!
//! Anything we don't understand is returned unchanged — stripping is a
//! best-effort privacy measure, never a correctness requirement.

/// Removes metadata from `data` based on the file extension. Unknown or
/// malformed inputs are returned unchanged.
pub fn strip(ext: &str, data: &[u8]) -> Vec<u8> {
    let stripped = match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => strip_jpeg(data),
        "png" | "apng" => strip_png(data),
        // GIF/AVIF rarely carry GPS EXIF and need format-specific handling we
        // don't do here; leave them untouched.
        _ => None,
    };
    stripped.unwrap_or_else(|| data.to_vec())
}

/// Drops EXIF (APP1), XMP (APP1), IPTC/Photoshop (APP13), and comment (COM)
/// segments from a JPEG, preserving JFIF (APP0), ICC (APP2), Adobe (APP14),
/// and the compressed scan data. Returns `None` on anything unexpected.
fn strip_jpeg(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 2 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&data[0..2]); // SOI
    let mut i = 2;
    while i + 1 < data.len() {
        if data[i] != 0xFF {
            return None; // not aligned on a marker — bail, keep original
        }
        let marker = data[i + 1];
        // Start of scan or end of image: copy the remainder verbatim (the
        // entropy-coded data after SOS has no length prefix to walk).
        if marker == 0xDA || marker == 0xD9 {
            out.extend_from_slice(&data[i..]);
            return Some(out);
        }
        if i + 3 >= data.len() {
            return None;
        }
        let len = ((data[i + 2] as usize) << 8) | data[i + 3] as usize;
        if len < 2 || i + 2 + len > data.len() {
            return None;
        }
        let segment = &data[i..i + 2 + len];
        let drop = matches!(marker, 0xE1 | 0xED | 0xFE); // APP1, APP13, COM
        if !drop {
            out.extend_from_slice(segment);
        }
        i += 2 + len;
    }
    Some(out)
}

const PNG_SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

/// Drops textual/EXIF/time metadata chunks from a PNG/APNG, preserving every
/// critical and animation chunk (IHDR, PLTE, IDAT, IEND, acTL, fcTL, fdAT) as
/// well as the ICC colour profile (iCCP). Returns `None` on anything
/// unexpected.
fn strip_png(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 8 || data[0..8] != PNG_SIG {
        return None;
    }
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&data[0..8]);
    let mut i = 8;
    while i + 8 <= data.len() {
        let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        let ctype = &data[i + 4..i + 8];
        // length(4) + type(4) + data(len) + crc(4)
        let chunk_end = i.checked_add(12)?.checked_add(len)?;
        if chunk_end > data.len() {
            return None;
        }
        let drop = matches!(ctype, b"eXIf" | b"tEXt" | b"zTXt" | b"iTXt" | b"tIME");
        if !drop {
            out.extend_from_slice(&data[i..chunk_end]);
        }
        let is_end = ctype == b"IEND";
        i = chunk_end;
        if is_end {
            break;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crc32(bytes: &[u8]) -> u32 {
        // Minimal CRC-32 (IEEE) so tests can build valid-enough PNG chunks.
        let mut crc: u32 = 0xFFFF_FFFF;
        for &b in bytes {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn png_chunk(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(ty);
        v.extend_from_slice(data);
        let mut crc_input = Vec::from(&ty[..]);
        crc_input.extend_from_slice(data);
        v.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        v
    }

    #[test]
    fn png_text_and_exif_chunks_removed_others_kept() {
        let mut png = Vec::from(PNG_SIG);
        png.extend(png_chunk(b"IHDR", &[0; 13]));
        png.extend(png_chunk(b"tEXt", b"Comment\0secret gps"));
        png.extend(png_chunk(b"eXIf", b"\x49\x49fake-exif"));
        png.extend(png_chunk(b"iCCP", b"profile-keep"));
        png.extend(png_chunk(b"IDAT", b"pixels"));
        png.extend(png_chunk(b"IEND", b""));

        let out = strip("png", &png);
        assert!(out.len() < png.len());
        // metadata gone
        assert!(!contains(&out, b"tEXt"));
        assert!(!contains(&out, b"eXIf"));
        assert!(!contains(&out, b"secret gps"));
        // critical + colour profile kept
        assert!(contains(&out, b"IHDR"));
        assert!(contains(&out, b"iCCP"));
        assert!(contains(&out, b"IDAT"));
        assert!(contains(&out, b"IEND"));
    }

    #[test]
    fn jpeg_app1_exif_removed_app0_kept() {
        let mut jpg = vec![0xFF, 0xD8]; // SOI
                                        // APP0/JFIF (keep)
        jpg.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x05, b'J', b'F', b'I']);
        // APP1/Exif (drop)
        let payload = b"Exif\0\0GPS-here";
        let len = (payload.len() + 2) as u16;
        jpg.extend_from_slice(&[0xFF, 0xE1]);
        jpg.extend_from_slice(&len.to_be_bytes());
        jpg.extend_from_slice(payload);
        // SOS + scan data (keep verbatim)
        jpg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0x11, 0x22, 0x33]);

        let out = strip("jpeg", &jpg);
        assert!(!contains(&out, b"GPS-here"));
        assert!(contains(&out, b"JFI"));
        assert!(contains(&out, &[0xFF, 0xDA])); // scan preserved
    }

    #[test]
    fn malformed_input_returned_unchanged() {
        let junk = vec![1u8, 2, 3, 4, 5];
        assert_eq!(strip("png", &junk), junk);
        assert_eq!(strip("jpg", &junk), junk);
        // unknown extension passes through
        assert_eq!(strip("gif", &junk), junk);
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }
}
