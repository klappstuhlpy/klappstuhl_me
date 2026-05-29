//! TOTP (RFC 6238) two-factor authentication primitives.
//!
//! This module is pure logic — generation, verification, at-rest encryption of
//! the shared secret, and recovery codes. The HTTP/enrollment/login wiring
//! lives in `routes::auth`.
//!
//! The shared secret is encrypted at rest with ChaCha20-Poly1305 keyed by the
//! app `SecretKey`, so a leaked database (or a downloaded backup) does not
//! expose usable 2FA secrets. Recovery codes are stored only as SHA-256
//! hashes.

use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit, Nonce};
use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::key::SecretKey;

type HmacSha1 = Hmac<Sha1>;

/// Number of digits in a generated code.
pub const DIGITS: u32 = 6;
/// Time step in seconds.
pub const STEP: u64 = 30;
/// How many steps of clock skew to tolerate on each side.
const SKEW: i64 = 1;
/// Length of a freshly generated shared secret, in bytes (160 bits).
const SECRET_LEN: usize = 20;
/// Issuer shown in the authenticator app.
const ISSUER: &str = "klappstuhl.me";
/// Number of recovery codes generated at enrollment.
pub const RECOVERY_CODE_COUNT: usize = 10;

// ─── HOTP / TOTP core ──────────────────────────────────────────────────────

/// RFC 4226 HOTP for a given counter.
fn hotp(secret: &[u8], counter: u64, digits: u32) -> u32 {
    let mut mac = <HmacSha1 as Mac>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&counter.to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = (hash[19] & 0x0f) as usize;
    let bin = ((hash[offset] as u32 & 0x7f) << 24)
        | ((hash[offset + 1] as u32) << 16)
        | ((hash[offset + 2] as u32) << 8)
        | (hash[offset + 3] as u32);
    bin % 10u32.pow(digits)
}

/// TOTP code for a specific unix timestamp.
fn totp_at(secret: &[u8], unix_time: u64, digits: u32) -> u32 {
    hotp(secret, unix_time / STEP, digits)
}

fn format_code(value: u32, digits: u32) -> String {
    format!("{value:0width$}", width = digits as usize)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The current 6-digit code for a secret (used during enrollment display/tests).
pub fn current_code(secret: &[u8]) -> String {
    format_code(totp_at(secret, now_unix(), DIGITS), DIGITS)
}

/// Verifies a user-supplied code against the secret, tolerating ±[`SKEW`]
/// time steps. Whitespace and a single separating space are ignored.
pub fn verify(secret: &[u8], code: &str) -> bool {
    let code = code.trim().replace(' ', "");
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let base = (now_unix() / STEP) as i64;
    for delta in -SKEW..=SKEW {
        let counter = (base + delta).max(0) as u64;
        if format_code(hotp(secret, counter, DIGITS), DIGITS) == code {
            return true;
        }
    }
    false
}

// ─── Secret generation / presentation ────────────────────────────────────────

/// Generates a new random 160-bit shared secret.
pub fn generate_secret() -> [u8; SECRET_LEN] {
    let mut buf = [0u8; SECRET_LEN];
    getrandom::getrandom(&mut buf).expect("getrandom failed");
    buf
}

/// Base32 (RFC 4648, unpadded) encoding of the secret, as shown for manual
/// entry into an authenticator app.
pub fn base32_secret(secret: &[u8]) -> String {
    base32::encode(base32::Alphabet::Rfc4648 { padding: false }, secret)
}

/// Builds the `otpauth://` provisioning URI for QR display.
pub fn otpauth_uri(secret: &[u8], account: &str) -> String {
    let label = format!("{ISSUER}:{account}");
    format!(
        "otpauth://totp/{label}?secret={secret}&issuer={ISSUER}&algorithm=SHA1&digits={DIGITS}&period={STEP}",
        label = percent_encode(&label),
        secret = base32_secret(secret),
        ISSUER = percent_encode(ISSUER),
    )
}

/// Renders the provisioning URI as an inline SVG QR code (no raster deps).
pub fn qr_svg(uri: &str) -> Option<String> {
    use qrcode::{render::svg, QrCode};
    let code = QrCode::new(uri.as_bytes()).ok()?;
    Some(
        code.render::<svg::Color>()
            .min_dimensions(220, 220)
            .quiet_zone(true)
            .build(),
    )
}

/// Minimal percent-encoding for the few characters that matter in the
/// otpauth label/issuer (`:` ` ` `/` `?` `&` `=` `#` `%`).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ─── At-rest encryption of the secret ────────────────────────────────────────

/// Encrypts a secret with ChaCha20-Poly1305 (key = app secret key). The output
/// is `base64(nonce ‖ ciphertext)`, suitable for a TEXT column.
pub fn encrypt_secret(key: &SecretKey, secret: &[u8]) -> anyhow::Result<String> {
    let cipher = ChaCha20Poly1305::new_from_slice(&key.0)
        .map_err(|_| anyhow::anyhow!("invalid cipher key length"))?;
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce)?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), secret)
        .map_err(|_| anyhow::anyhow!("TOTP secret encryption failed"))?;
    let mut blob = Vec::with_capacity(12 + ciphertext.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ciphertext);
    Ok(STANDARD.encode(blob))
}

/// Reverses [`encrypt_secret`]. Returns `None` on any decode/auth failure.
pub fn decrypt_secret(key: &SecretKey, stored: &str) -> Option<Vec<u8>> {
    let blob = STANDARD.decode(stored).ok()?;
    if blob.len() < 12 + 16 {
        return None;
    }
    let (nonce, ciphertext) = blob.split_at(12);
    let cipher = ChaCha20Poly1305::new_from_slice(&key.0).ok()?;
    cipher.decrypt(Nonce::from_slice(nonce), ciphertext).ok()
}

// ─── Recovery codes ──────────────────────────────────────────────────────────

/// Generates a fresh batch of human-friendly recovery codes (e.g. `a1b2c-d3e4f`).
pub fn generate_recovery_codes() -> Vec<String> {
    const ALPHABET: [char; 32] = [
        'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'j', 'k', 'm', 'n', 'p', 'q', 'r', 's', 't', 'u',
        'v', 'w', 'x', 'y', 'z', '2', '3', '4', '5', '6', '7', '8', '9', '0',
    ];
    (0..RECOVERY_CODE_COUNT)
        .map(|_| {
            let raw = nanoid::nanoid!(10, &ALPHABET);
            format!("{}-{}", &raw[..5], &raw[5..])
        })
        .collect()
}

/// Hashes a recovery code for storage. Codes are high-entropy random strings,
/// so a fast hash (SHA-256) is sufficient — no need for a slow KDF.
pub fn hash_recovery_code(code: &str) -> String {
    crate::scan::sha256_hex(code.trim().replace(' ', "").to_lowercase().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 Appendix B test vectors (SHA-1, seed = ASCII "12345678901234567890").
    const SEED: &[u8] = b"12345678901234567890";

    fn code8_at(t: u64) -> String {
        format_code(totp_at(SEED, t, 8), 8)
    }

    #[test]
    fn rfc6238_vectors() {
        assert_eq!(code8_at(59), "94287082");
        assert_eq!(code8_at(1111111109), "07081804");
        assert_eq!(code8_at(1111111111), "14050471");
        assert_eq!(code8_at(1234567890), "89005924");
        assert_eq!(code8_at(2000000000), "69279037");
    }

    #[test]
    fn verify_accepts_current_and_rejects_garbage() {
        let secret = generate_secret();
        let code = current_code(&secret);
        assert!(verify(&secret, &code));
        assert!(verify(&secret, &format!(" {code} ")));
        assert!(!verify(&secret, "000000"));
        assert!(!verify(&secret, "12"));
        assert!(!verify(&secret, "abcdef"));
    }

    #[test]
    fn encrypt_roundtrip() {
        let key = SecretKey::random().unwrap();
        let secret = generate_secret();
        let blob = encrypt_secret(&key, &secret).unwrap();
        // Ciphertext is not the plaintext.
        assert_ne!(blob.as_bytes(), &secret[..]);
        assert_eq!(decrypt_secret(&key, &blob).unwrap(), secret.to_vec());
        // Wrong key fails to decrypt.
        let other = SecretKey::random().unwrap();
        assert!(decrypt_secret(&other, &blob).is_none());
    }

    #[test]
    fn recovery_codes_are_unique_and_hashable() {
        let codes = generate_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODE_COUNT);
        let mut seen = std::collections::HashSet::new();
        for c in &codes {
            assert!(seen.insert(c.clone()), "duplicate recovery code");
            // Hash is stable and case/space-insensitive.
            assert_eq!(hash_recovery_code(c), hash_recovery_code(&c.to_uppercase()));
        }
    }
}
