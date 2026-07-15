//! Password protection for pastes: Argon2id → ChaCha20-Poly1305.
//!
//! A password-protected paste stores *ciphertext*, not plaintext plus a hash.
//! Argon2id derives a 32-byte content key from `password + salt`, and the body
//! is sealed under that key with ChaCha20-Poly1305. **No password verifier is
//! stored** — the AEAD authentication tag *is* the verifier, so a wrong password
//! is simply a decryption failure and there is nothing else to attack offline.
//!
//! After a successful unlock the derived key is handed back to the browser in a
//! **paste-scoped, short-TTL, HMAC-signed, HttpOnly cookie** (signed with the app
//! [`SecretKey`], the same primitive the session cookie uses). That is what lets
//! `/p/:id/raw` and `/p/:id/embed` work after unlocking without prompting again.
//! The cookie carries the key, not the password, and it is scoped to that one
//! paste's path — a stolen cookie unlocks nothing else.
//!
//! The operator *could* read a paste if they held its password: this is
//! server-side encryption, not the zero-knowledge, key-in-the-URL-fragment model
//! PrivateBin uses. That trade is deliberate (it keeps server-side highlighting,
//! embeds and the JSON API working) and is documented in `docs/features.md`.

use argon2::Argon2;
use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit, Nonce};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::key::SecretKey;

/// Length of the per-paste Argon2id salt.
const SALT_LEN: usize = 16;
/// ChaCha20-Poly1305 nonce length.
const NONCE_LEN: usize = 12;
/// Derived content-key length.
const KEY_LEN: usize = 32;
/// How long an unlock cookie stays valid.
pub const UNLOCK_TTL_SECS: i64 = 30 * 60;

/// A sealed paste body plus the public parameters needed to open it again.
pub struct Sealed {
    pub ciphertext: Vec<u8>,
    pub salt: Vec<u8>,
    pub nonce: Vec<u8>,
}

/// Derives the content key from a password and the paste's salt.
fn derive_key(password: &str, salt: &[u8]) -> Option<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .ok()?;
    Some(key)
}

/// Seals `plaintext` under a fresh salt + nonce derived from `password`.
pub fn seal(password: &str, plaintext: &[u8]) -> Option<Sealed> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut salt).ok()?;
    getrandom::getrandom(&mut nonce).ok()?;

    let key = derive_key(password, &salt)?;
    let ciphertext = seal_with_key(&key, &nonce, plaintext)?;
    Some(Sealed {
        ciphertext,
        salt: salt.to_vec(),
        nonce: nonce.to_vec(),
    })
}

/// Seals `plaintext` under an already-derived key (used when re-encrypting an
/// edited body under the paste's existing salt).
pub fn seal_with_key(key: &[u8; KEY_LEN], nonce: &[u8], plaintext: &[u8]) -> Option<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).ok()?;
    cipher.encrypt(Nonce::from_slice(nonce), plaintext).ok()
}

/// Opens a sealed body with a password. `None` on a wrong password — the AEAD
/// tag fails to verify, and that is the whole check.
pub fn open(password: &str, salt: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>> {
    let key = derive_key(password, salt)?;
    open_with_key(&key, nonce, ciphertext)
}

/// Opens a sealed body with an already-derived key (the unlock-cookie path).
pub fn open_with_key(key: &[u8; KEY_LEN], nonce: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).ok()?;
    cipher.decrypt(Nonce::from_slice(nonce), ciphertext).ok()
}

/// Derives the key for `password` and returns it alongside the opened body, so a
/// successful unlock can mint a cookie without deriving twice.
pub fn open_and_keep_key(
    password: &str,
    salt: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
) -> Option<([u8; KEY_LEN], Vec<u8>)> {
    let key = derive_key(password, salt)?;
    let plaintext = open_with_key(&key, nonce, ciphertext)?;
    Some((key, plaintext))
}

// ─── The unlock cookie ───────────────────────────────────────────────────────

/// The signed payload of an unlock cookie: which paste it unlocks, the content
/// key (base64), and when it stops being valid.
#[derive(Serialize, Deserialize)]
struct UnlockClaim {
    id: String,
    key: String,
    exp: i64,
}

/// The cookie name for a given paste. One cookie per paste, so unlocking one
/// never unlocks another.
pub fn cookie_name(id: &str) -> String {
    format!("paste_unlock_{id}")
}

/// Signs an unlock claim for `id` carrying the derived content key.
pub fn sign_unlock(secret: &SecretKey, id: &str, content_key: &[u8; KEY_LEN]) -> Option<String> {
    secret
        .sign(&UnlockClaim {
            id: id.to_string(),
            key: STANDARD.encode(content_key),
            exp: OffsetDateTime::now_utc().unix_timestamp() + UNLOCK_TTL_SECS,
        })
        .ok()
}

/// Verifies an unlock cookie and returns the content key it carries. Rejects a
/// claim minted for a *different* paste and one whose TTL has run out — the
/// signature alone is not enough.
pub fn verify_unlock(secret: &SecretKey, id: &str, token: &str) -> Option<[u8; KEY_LEN]> {
    let claim: UnlockClaim = secret.verify(token)?;
    if claim.id != id || claim.exp <= OffsetDateTime::now_utc().unix_timestamp() {
        return None;
    }
    let bytes = STANDARD.decode(claim.key).ok()?;
    bytes.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> SecretKey {
        SecretKey::random().unwrap()
    }

    #[test]
    fn seal_open_roundtrip() {
        let sealed = seal("hunter2", b"secret body").unwrap();
        let opened = open("hunter2", &sealed.salt, &sealed.nonce, &sealed.ciphertext).unwrap();
        assert_eq!(opened, b"secret body");
    }

    #[test]
    fn ciphertext_is_not_the_plaintext() {
        let sealed = seal("hunter2", b"secret body").unwrap();
        assert_ne!(sealed.ciphertext.as_slice(), b"secret body");
        // The AEAD tag makes it longer than the input, and nothing of the
        // plaintext survives in the clear.
        assert!(sealed.ciphertext.len() > b"secret body".len());
        assert!(!sealed.ciphertext.windows(6).any(|w| w == b"secret"));
    }

    #[test]
    fn wrong_password_fails_to_open() {
        let sealed = seal("hunter2", b"secret body").unwrap();
        assert!(open("hunter3", &sealed.salt, &sealed.nonce, &sealed.ciphertext).is_none());
    }

    #[test]
    fn tampered_ciphertext_fails_to_open() {
        let mut sealed = seal("hunter2", b"secret body").unwrap();
        sealed.ciphertext[0] ^= 0xff;
        assert!(open("hunter2", &sealed.salt, &sealed.nonce, &sealed.ciphertext).is_none());
    }

    #[test]
    fn unlock_cookie_roundtrips_and_is_paste_scoped() {
        let secret = test_key();
        let sealed = seal("hunter2", b"body").unwrap();
        let (content_key, _) = open_and_keep_key("hunter2", &sealed.salt, &sealed.nonce, &sealed.ciphertext).unwrap();

        let token = sign_unlock(&secret, "abc", &content_key).unwrap();
        let recovered = verify_unlock(&secret, "abc", &token).unwrap();
        assert_eq!(recovered, content_key);

        // A cookie minted for `abc` must not unlock `xyz`, even though the
        // signature over it is perfectly valid.
        assert!(verify_unlock(&secret, "xyz", &token).is_none());
        // Nor may another server's key forge one.
        assert!(verify_unlock(&test_key(), "abc", &token).is_none());
    }

    #[test]
    fn expired_unlock_cookie_is_rejected() {
        let secret = test_key();
        let claim = UnlockClaim {
            id: "abc".to_string(),
            key: STANDARD.encode([7u8; KEY_LEN]),
            exp: OffsetDateTime::now_utc().unix_timestamp() - 1,
        };
        let token = secret.sign(&claim).unwrap();
        assert!(verify_unlock(&secret, "abc", &token).is_none());
    }
}
