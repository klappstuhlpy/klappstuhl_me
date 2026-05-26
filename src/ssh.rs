//! SSH key management — models, fingerprint parsing, and DB helpers.
//!
//! Fingerprint format matches OpenSSH's default (`SHA256:<base64url-no-pad>`),
//! so the value displayed in the UI is identical to `ssh-keygen -lf`.

use crate::database::Table;
use base64::{prelude::BASE64_STANDARD_NO_PAD, prelude::BASE64_URL_SAFE_NO_PAD, Engine};
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tracing;

// ─── Parsing ─────────────────────────────────────────────────────────────────

/// Everything extracted from an OpenSSH public-key line.
#[derive(Debug, Clone)]
pub struct ParsedSshKey {
    pub algo: String,
    pub fingerprint: String,
    pub comment: Option<String>,
}

/// Errors returned by [`parse_public_key`].
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("key line is empty or has no type field")]
    MissingType,
    #[error("key line is missing the base64 key material")]
    MissingKeyData,
    #[error("base64 key material is invalid: {0}")]
    BadBase64(#[from] base64::DecodeError),
}

/// Parse a single-line OpenSSH public key and compute its SHA-256 fingerprint.
///
/// Accepts the standard `<type> <base64> [comment]` format.
/// Returns [`ParseError`] for anything that doesn't fit that shape.
pub fn parse_public_key(line: &str) -> Result<ParsedSshKey, ParseError> {
    let line = line.trim();
    let mut parts = line.splitn(3, ' ');

    let algo = parts.next().filter(|s| !s.is_empty()).ok_or(ParseError::MissingType)?;
    let b64 = parts.next().ok_or(ParseError::MissingKeyData)?;
    let comment = parts.next().filter(|s| !s.is_empty()).map(str::to_owned);

    // The base64 blob may use standard or URL-safe alphabet; try both.
    let raw = BASE64_STANDARD_NO_PAD
        .decode(b64)
        .or_else(|_| BASE64_URL_SAFE_NO_PAD.decode(b64))?;

    let digest = Sha256::digest(&raw);
    // OpenSSH fingerprint: "SHA256:" + standard base64 no-pad
    let fingerprint = format!("SHA256:{}", BASE64_STANDARD_NO_PAD.encode(digest));

    Ok(ParsedSshKey {
        algo: algo.to_owned(),
        fingerprint,
        comment,
    })
}

// ─── Models ──────────────────────────────────────────────────────────────────

/// A stored SSH public key authorized for a user.
#[derive(Debug, Clone, Serialize)]
pub struct SshKey {
    pub id: i64,
    pub account_id: i64,
    pub name: String,
    /// Full OpenSSH key line (type + base64 + optional comment).
    pub public_key: String,
    /// SHA-256 fingerprint: `SHA256:<base64>`.
    pub fingerprint: String,
    pub algo: String,
    pub comment: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub added_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

impl SshKey {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

impl Table for SshKey {
    const NAME: &'static str = "ssh_key";
    const COLUMNS: &'static [&'static str] = &[
        "id", "account_id", "name", "public_key", "fingerprint", "algo",
        "comment", "added_at", "last_used_at", "revoked_at",
    ];
    type Id = i64;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            account_id: row.get("account_id")?,
            name: row.get("name")?,
            public_key: row.get("public_key")?,
            fingerprint: row.get("fingerprint")?,
            algo: row.get("algo")?,
            comment: row.get("comment")?,
            added_at: row.get("added_at")?,
            last_used_at: row.get("last_used_at")?,
            revoked_at: row.get("revoked_at")?,
        })
    }
}

/// A short-lived access token (CI/CD, scripts) tied to an account.
#[derive(Debug, Clone, Serialize)]
pub struct SshToken {
    pub id: i64,
    pub account_id: i64,
    /// SHA-256 of the raw token — never expose plaintext after creation.
    #[serde(skip_serializing)]
    pub token_hash: String,
    pub label: String,
    /// Comma-separated scope list (`""` = full access, same semantics as `Session.scopes`).
    pub scopes: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub used_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

impl SshToken {
    pub fn is_active(&self) -> bool {
        if self.revoked_at.is_some() {
            return false;
        }
        self.expires_at
            .map(|exp| OffsetDateTime::now_utc() < exp)
            .unwrap_or(true)
    }
}

impl Table for SshToken {
    const NAME: &'static str = "ssh_token";
    const COLUMNS: &'static [&'static str] = &[
        "id", "account_id", "token_hash", "label", "scopes",
        "expires_at", "created_at", "used_at", "revoked_at",
    ];
    type Id = i64;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            account_id: row.get("account_id")?,
            token_hash: row.get("token_hash")?,
            label: row.get("label")?,
            scopes: row.get("scopes")?,
            expires_at: row.get("expires_at")?,
            created_at: row.get("created_at")?,
            used_at: row.get("used_at")?,
            revoked_at: row.get("revoked_at")?,
        })
    }
}

/// One row in the per-key action log.
#[derive(Debug, Clone, Serialize)]
pub struct SshSessionAudit {
    pub id: i64,
    pub account_id: Option<i64>,
    pub key_id: Option<i64>,
    pub action: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Table for SshSessionAudit {
    const NAME: &'static str = "ssh_session_audit";
    const COLUMNS: &'static [&'static str] =
        &["id", "account_id", "key_id", "action", "ip", "user_agent", "created_at"];
    type Id = i64;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            account_id: row.get("account_id")?,
            key_id: row.get("key_id")?,
            action: row.get("action")?,
            ip: row.get("ip")?,
            user_agent: row.get("user_agent")?,
            created_at: row.get("created_at")?,
        })
    }
}

// ─── DB helpers ──────────────────────────────────────────────────────────────

use crate::AppState;
use std::time::Duration;

/// Alphabet for generated tokens: alphanumeric only (no lookalike chars).
const TOKEN_ALPHABET: [char; 62] = [
    'a','b','c','d','e','f','g','h','i','j','k','l','m','n','o','p','q','r','s','t','u','v','w','x','y','z',
    'A','B','C','D','E','F','G','H','I','J','K','L','M','N','O','P','Q','R','S','T','U','V','W','X','Y','Z',
    '0','1','2','3','4','5','6','7','8','9',
];

/// Generate a new plaintext token (`sshtkn_<32 random chars>`).
pub fn generate_token() -> String {
    let rand = nanoid::nanoid!(32, &TOKEN_ALPHABET);
    format!("sshtkn_{rand}")
}

/// Background task: mark all expired `ssh_token` rows as revoked every hour.
/// Call once at startup — spawns a detached tokio task.
pub fn spawn_token_sweeper(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            interval.tick().await;
            let result = state
                .database()
                .execute(
                    "UPDATE ssh_token
                     SET    revoked_at = CURRENT_TIMESTAMP
                     WHERE  revoked_at IS NULL
                       AND  expires_at IS NOT NULL
                       AND  expires_at <= CURRENT_TIMESTAMP",
                    [],
                )
                .await;
            match result {
                Ok(n) if n > 0 => tracing::info!(count = n, "swept expired SSH tokens"),
                Err(e) => tracing::warn!(error = %e, "SSH token sweeper error"),
                _ => {}
            }
        }
    });
}

/// Record one entry in `ssh_session_audit` (fire-and-forget).
pub fn audit(
    state: &AppState,
    account_id: Option<i64>,
    key_id: Option<i64>,
    action: &'static str,
    ip: Option<String>,
    user_agent: Option<String>,
) {
    let state = state.clone();
    tokio::spawn(async move {
        let _ = state
            .database()
            .execute(
                "INSERT INTO ssh_session_audit(account_id, key_id, action, ip, user_agent)
                 VALUES (?, ?, ?, ?, ?)",
                (account_id, key_id, action, ip, user_agent),
            )
            .await;
    });
}

/// Hash a raw token with SHA-256 and return the hex string used for storage.
pub fn hash_token(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    format!("{:x}", digest)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ed25519_key() {
        let line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBxDPXFO8BFiTL6Z5XB9D1fBXaJPkPFDZI5y6d5X1234 user@host";
        let parsed = parse_public_key(line).expect("should parse");
        assert_eq!(parsed.algo, "ssh-ed25519");
        assert!(parsed.fingerprint.starts_with("SHA256:"));
        assert_eq!(parsed.comment.as_deref(), Some("user@host"));
    }

    #[test]
    fn parse_key_no_comment() {
        let line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBxDPXFO8BFiTL6Z5XB9D1fBXaJPkPFDZI5y6d5X1234";
        let parsed = parse_public_key(line).expect("should parse");
        assert!(parsed.comment.is_none());
    }

    #[test]
    fn parse_empty_fails() {
        assert!(parse_public_key("").is_err());
    }
}
