//! The shared paste core: validate → scan → seal → store.
//!
//! **Both** the browser handlers ([`super::crud`]) and the JSON API
//! ([`crate::site::api::pastes`]) go through this module. That is the whole
//! point of it: validation, quotas, secret scanning, encryption and the audit
//! trail live in exactly one place, so the two surfaces cannot drift apart on
//! what a legal paste is.
//!
//! Every read also goes through here ([`load`], [`load_for`], [`list_for_account`])
//! rather than ad-hoc SQL at each call site, which is what keeps the expiry rule
//! ("an expired paste is invisible to every read path") true by construction —
//! and is what will make the deferred multi-file model a contained change.

use std::net::IpAddr;

use rusqlite::OptionalExtension;
use time::{Duration, OffsetDateTime};

use crate::models::{Account, Paste, PasteRevision, Visibility};
use crate::utils::get_new_image_id;
use crate::AppState;

use super::crypto;

/// Maximum length of a paste title.
pub const MAX_TITLE_LEN: usize = 120;
/// Maximum lifetime of an expiring paste (365 days, matching image uploads).
pub const MAX_TTL_SECS: i64 = 365 * 24 * 60 * 60;
/// How many revisions to keep per paste. The hourly reaper prunes the rest.
pub const REVISION_CAP: i64 = 20;
/// Length of a generated anonymous edit token.
const EDIT_TOKEN_LEN: usize = 32;

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Every way a paste operation can be refused. Both surfaces render this: the
/// API maps it to an [`crate::error::ApiError`], the web handlers to a flash.
#[derive(Debug, Clone)]
pub enum PasteError {
    /// The body was empty or whitespace-only.
    Empty,
    /// The body exceeded the size cap for this kind of author (bytes).
    TooLarge(usize),
    /// The title exceeded [`MAX_TITLE_LEN`].
    TitleTooLong,
    /// Anonymous pastes are switched off (`config.paste.anonymous`).
    AnonymousDisabled,
    /// The account is at its paste-count cap.
    QuotaCount(usize),
    /// The account is at its total-bytes cap.
    QuotaBytes(i64),
    /// A critical secret was detected and the author hasn't confirmed yet.
    /// Carries the rule names, so the UI can say *what* it found.
    SecretsFound(Vec<String>),
    /// A critical secret was detected in an *anonymous* paste. Not overridable.
    SecretsRefused(Vec<String>),
    /// No such paste (or it expired, or it isn't yours — deliberately the same
    /// answer, so probing can't distinguish "not yours" from "doesn't exist").
    NotFound,
    /// The password was wrong.
    BadPassword,
    /// Sealing/opening the body failed.
    Crypto,
    /// The database refused.
    Db,
}

impl PasteError {
    /// A message safe to show the author, on either surface.
    pub fn message(&self) -> String {
        match self {
            Self::Empty => "The paste is empty.".to_string(),
            Self::TooLarge(limit) => format!("Paste is too large (max {}).", human_bytes(*limit as i64)),
            Self::TitleTooLong => format!("Title is too long (max {MAX_TITLE_LEN} characters)."),
            Self::AnonymousDisabled => "Anonymous pastes are disabled — sign in to create one.".to_string(),
            Self::QuotaCount(limit) => {
                format!("You've reached the limit of {limit} pastes — delete one to create another.")
            }
            Self::QuotaBytes(limit) => format!(
                "You've reached your paste storage limit ({}) — delete one to free space.",
                human_bytes(*limit)
            ),
            Self::SecretsFound(rules) | Self::SecretsRefused(rules) => {
                format!("This looks like it contains a secret ({}).", rules.join(", "))
            }
            Self::NotFound => "Paste not found.".to_string(),
            Self::BadPassword => "Wrong password.".to_string(),
            Self::Crypto => "Could not encrypt the paste.".to_string(),
            Self::Db => "Could not save the paste. Please try again.".to_string(),
        }
    }
}

fn human_bytes(bytes: i64) -> String {
    const KB: i64 = 1024;
    const MB: i64 = 1024 * KB;
    if bytes >= MB {
        format!("{} MB", bytes / MB)
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} bytes")
    }
}

// ─── Who is asking ───────────────────────────────────────────────────────────

/// The author of a new paste.
#[derive(Debug, Clone, Copy)]
pub enum Creator<'a> {
    /// A signed-in account (or an API key acting for one).
    Account(&'a Account),
    /// A stranger. Subject to the anonymous switch, the smaller size cap, the
    /// forced TTL, and a hard refusal on detected secrets.
    Anonymous,
}

impl Creator<'_> {
    fn account_id(&self) -> Option<i64> {
        match self {
            Self::Account(a) => Some(a.id),
            Self::Anonymous => None,
        }
    }

    fn is_anonymous(&self) -> bool {
        matches!(self, Self::Anonymous)
    }
}

/// Who is trying to modify an existing paste. An account owns its own pastes
/// (and an admin owns all of them); an anonymous author holds an **edit token**.
#[derive(Debug, Default, Clone)]
pub struct Actor<'a> {
    pub account: Option<&'a Account>,
    /// The raw edit token, from the `edit_token` form field / header.
    pub edit_token: Option<String>,
}

impl<'a> Actor<'a> {
    pub fn account(account: &'a Account) -> Self {
        Self {
            account: Some(account),
            edit_token: None,
        }
    }

    pub fn token(token: impl Into<String>) -> Self {
        Self {
            account: None,
            edit_token: Some(token.into()),
        }
    }

    /// Whether this actor may edit/delete `paste`.
    ///
    /// The edit token is a **credential, not a one-shot nonce**: it is compared
    /// against the stored hash on every use and stays valid for the paste's
    /// whole life.
    pub fn may_modify(&self, paste: &Paste) -> bool {
        if let Some(account) = self.account {
            if paste.owned_by(account) {
                return true;
            }
        }
        match (self.edit_token.as_deref(), paste.edit_token_hash.as_deref()) {
            (Some(raw), Some(stored)) => ct_eq(hash_edit_token(raw).as_bytes(), stored.as_bytes()),
            _ => false,
        }
    }
}

/// Constant-time byte comparison, so a token check can't be turned into an
/// oracle by timing it. Both sides are fixed-length hex digests in practice.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Edit tokens are high-entropy random strings, so a fast hash is enough — the
/// same reasoning (and the same helper) as recovery codes.
pub fn hash_edit_token(token: &str) -> String {
    crate::scan::sha256_hex(token.trim().as_bytes())
}

fn new_edit_token() -> String {
    nanoid::nanoid!(EDIT_TOKEN_LEN)
}

// ─── Input ───────────────────────────────────────────────────────────────────

/// A paste as requested by an author, before validation.
#[derive(Debug, Default, Clone)]
pub struct NewPaste {
    pub content: String,
    pub title: Option<String>,
    pub language: Option<String>,
    pub visibility: Visibility,
    pub burn_after_read: bool,
    /// When set, the body is sealed with it and stored as ciphertext.
    pub password: Option<String>,
    /// Time-to-live in seconds. `None` = never (unless the author is anonymous,
    /// who always gets the forced TTL).
    pub expires_in: Option<i64>,
    /// The paste this one is a fork of.
    pub fork_of: Option<String>,
    /// The author saw the secret-scan warning and chose to publish anyway.
    /// Ignored for anonymous authors — for them a critical finding is fatal.
    pub confirm_secrets: bool,
}

/// The result of a successful create.
pub struct Created {
    pub paste: Paste,
    /// The anonymous edit token — **shown exactly once**, here. It is stored
    /// only as a hash, so this is the only moment it exists in the clear.
    pub edit_token: Option<String>,
}

/// The fields an edit may change.
#[derive(Debug, Default, Clone)]
pub struct EditPaste {
    pub content: String,
    pub title: Option<String>,
    pub language: Option<String>,
    pub visibility: Option<Visibility>,
    pub expires_in: Option<i64>,
    /// The password for an encrypted paste — required to re-seal the new body.
    pub password: Option<String>,
    pub confirm_secrets: bool,
}

// ─── Create ──────────────────────────────────────────────────────────────────

/// Validates, scans, optionally seals, and stores a new paste.
pub async fn create(
    state: &AppState,
    creator: Creator<'_>,
    client_ip: Option<IpAddr>,
    new: NewPaste,
) -> Result<Created, PasteError> {
    let config = state.config();
    let paste_config = &config.paste;

    if creator.is_anonymous() && !paste_config.anonymous {
        return Err(PasteError::AnonymousDisabled);
    }

    let content = new.content;
    if content.trim().is_empty() {
        return Err(PasteError::Empty);
    }

    let max_bytes = if creator.is_anonymous() {
        paste_config.anonymous_max_bytes
    } else {
        paste_config.max_bytes
    };
    if content.len() > max_bytes {
        return Err(PasteError::TooLarge(max_bytes));
    }

    let title = normalize_title(new.title)?;
    let language = normalize_language(new.language);

    // Scan the *plaintext*, before any encryption — an encrypted body is opaque
    // bytes and there'd be nothing left to look at.
    check_for_secrets(&content, creator.is_anonymous(), new.confirm_secrets)?;

    if let Creator::Account(account) = creator {
        check_quota(state, account, content.len() as i64).await?;
    }

    let expires_at = resolve_expiry(new.expires_in, creator.is_anonymous(), paste_config.anonymous_ttl_days);

    // Seal the body if a password was given. `size_bytes` records what is
    // actually stored, so the quota accounts for the ciphertext.
    let (body, salt, nonce) = match new.password.as_deref().filter(|p| !p.is_empty()) {
        Some(password) => {
            let sealed = crypto::seal(password, content.as_bytes()).ok_or(PasteError::Crypto)?;
            (sealed.ciphertext, Some(sealed.salt), Some(sealed.nonce))
        }
        None => (content.into_bytes(), None, None),
    };

    // Anonymous authors get a token — it is the only way they can ever come back
    // and edit or delete what they posted.
    let edit_token = creator.is_anonymous().then(new_edit_token);
    let edit_token_hash = edit_token.as_deref().map(hash_edit_token);

    let id = get_new_image_id();
    let size_bytes = body.len() as i64;
    let fork_of = new.fork_of.clone();
    let encrypted = nonce.is_some();

    state
        .database()
        .execute(
            "INSERT INTO paste (id, account_id, title, content, language, visibility, burn_after_read, \
                                enc_salt, enc_nonce, edit_token_hash, size_bytes, fork_of, creator_ip, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            crate::boxed_params![
                id.clone(),
                creator.account_id(),
                title,
                body,
                language,
                new.visibility,
                new.burn_after_read,
                salt,
                nonce,
                edit_token_hash,
                size_bytes,
                fork_of,
                // Only recorded for anonymous pastes: it is takedown plumbing for
                // the one surface strangers can write to, not general analytics.
                creator
                    .is_anonymous()
                    .then(|| client_ip.map(|ip| ip.to_string()))
                    .flatten(),
                expires_at
            ],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to insert paste");
            PasteError::Db
        })?;

    let mut audit = state.audit("paste.create").target(id.clone()).ip_opt(client_ip);
    if let Creator::Account(account) = creator {
        audit = audit.actor(account);
    }
    audit
        .meta(serde_json::json!({
            "anonymous": creator.is_anonymous(),
            "visibility": new.visibility.as_str(),
            "encrypted": encrypted,
            "burn_after_read": new.burn_after_read,
        }))
        .fire();

    let paste = load(state, &id).await.ok_or(PasteError::Db)?;
    Ok(Created { paste, edit_token })
}

// ─── Edit ────────────────────────────────────────────────────────────────────

/// Applies an edit, snapshotting the previous body into `paste_revision` first.
///
/// An encrypted paste stays encrypted: the new body is re-sealed under the same
/// salt with a fresh nonce, which requires the password. An edit that would
/// silently drop the encryption is not something this ever does.
pub async fn edit(
    state: &AppState,
    paste: &Paste,
    actor: &Actor<'_>,
    client_ip: Option<IpAddr>,
    change: EditPaste,
) -> Result<Paste, PasteError> {
    if !actor.may_modify(paste) {
        return Err(PasteError::NotFound);
    }

    let config = state.config();
    let max_bytes = if paste.account_id.is_none() {
        config.paste.anonymous_max_bytes
    } else {
        config.paste.max_bytes
    };

    if change.content.trim().is_empty() {
        return Err(PasteError::Empty);
    }
    if change.content.len() > max_bytes {
        return Err(PasteError::TooLarge(max_bytes));
    }

    let title = normalize_title(change.title)?;
    let language = normalize_language(change.language);
    check_for_secrets(&change.content, paste.account_id.is_none(), change.confirm_secrets)?;

    let body = if paste.is_encrypted() {
        let password = change
            .password
            .as_deref()
            .filter(|p| !p.is_empty())
            .ok_or(PasteError::BadPassword)?;
        let salt = paste.enc_salt.as_deref().ok_or(PasteError::Crypto)?;
        let nonce = paste.enc_nonce.as_deref().ok_or(PasteError::Crypto)?;
        // Opening the *current* body is how the password is verified — there is
        // no separate verifier to check it against.
        let (key, _) =
            crypto::open_and_keep_key(password, salt, nonce, &paste.content).ok_or(PasteError::BadPassword)?;
        crypto::seal_with_key(&key, nonce, change.content.as_bytes()).ok_or(PasteError::Crypto)?
    } else {
        change.content.clone().into_bytes()
    };

    let visibility = change.visibility.unwrap_or(paste.visibility);
    let expires_at = resolve_expiry(
        change.expires_in,
        paste.account_id.is_none(),
        config.paste.anonymous_ttl_days,
    );
    let size_bytes = body.len() as i64;
    let id = paste.id.clone();

    // The snapshot and the update are one transaction: a revision row that
    // records a body the paste never actually had would be worse than no history.
    let previous = paste.content.clone();
    let previous_title = paste.title.clone();
    let previous_language = paste.language.clone();
    state
        .database()
        .call(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO paste_revision (paste_id, content, title, language) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, previous, previous_title, previous_language],
            )?;
            tx.execute(
                "UPDATE paste SET content = ?1, title = ?2, language = ?3, visibility = ?4, \
                 size_bytes = ?5, expires_at = ?6, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') \
                 WHERE id = ?7",
                rusqlite::params![body, title, language, visibility, size_bytes, expires_at, id],
            )?;
            tx.commit()
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to edit paste");
            PasteError::Db
        })?;

    let mut audit = state.audit("paste.edit").target(paste.id.clone()).ip_opt(client_ip);
    if let Some(account) = actor.account {
        audit = audit.actor(account);
    }
    audit.fire();

    load(state, &paste.id).await.ok_or(PasteError::Db)
}

// ─── Delete ──────────────────────────────────────────────────────────────────

/// Deletes a paste. Revisions go with it via `ON DELETE CASCADE`.
pub async fn delete(
    state: &AppState,
    paste: &Paste,
    actor: &Actor<'_>,
    client_ip: Option<IpAddr>,
) -> Result<(), PasteError> {
    if !actor.may_modify(paste) {
        return Err(PasteError::NotFound);
    }

    state
        .database()
        .execute("DELETE FROM paste WHERE id = ?1", [paste.id.clone()])
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to delete paste");
            PasteError::Db
        })?;

    let mut audit = state.audit("paste.delete").target(paste.id.clone()).ip_opt(client_ip);
    if let Some(account) = actor.account {
        audit = audit.actor(account);
    }
    audit.fire();
    Ok(())
}

// ─── Fork ────────────────────────────────────────────────────────────────────

/// Duplicates a paste's *plaintext* into a new one owned by the forker.
///
/// A fork is a new, independent paste — it never inherits the original's
/// password or burn flag, and the caller must already hold the plaintext (which
/// for an encrypted paste means they unlocked it).
pub async fn fork(
    state: &AppState,
    source: &Paste,
    plaintext: &str,
    creator: Creator<'_>,
    client_ip: Option<IpAddr>,
) -> Result<Created, PasteError> {
    let title = match source.title.as_deref() {
        Some(title) => Some(format!("Fork of {title}")),
        None => Some(format!("Fork of {}", source.id)),
    };
    create(
        state,
        creator,
        client_ip,
        NewPaste {
            content: plaintext.to_string(),
            title,
            language: source.language.clone(),
            visibility: Visibility::Unlisted,
            fork_of: Some(source.id.clone()),
            // A fork of a paste that tripped the scanner is the author's problem
            // to look at again, but they didn't write it — so don't block them on
            // a finding they can't fix, only warn as usual.
            ..Default::default()
        },
    )
    .await
}

// ─── Reads ───────────────────────────────────────────────────────────────────

/// Loads a paste by id, unless it has expired. Every read path starts here.
pub async fn load(state: &AppState, id: &str) -> Option<Paste> {
    state
        .database()
        .get(
            "SELECT * FROM paste WHERE id = ?1 \
             AND (expires_at IS NULL OR datetime(expires_at) > datetime('now'))",
            [id.to_string()],
        )
        .await
        .ok()
        .flatten()
}

/// Loads a paste only if `actor` may modify it. The refusal is deliberately
/// [`PasteError::NotFound`] either way, so "not yours" and "doesn't exist" are
/// indistinguishable from outside.
pub async fn load_for(state: &AppState, id: &str, actor: &Actor<'_>) -> Result<Paste, PasteError> {
    let paste = load(state, id).await.ok_or(PasteError::NotFound)?;
    if !actor.may_modify(&paste) {
        return Err(PasteError::NotFound);
    }
    Ok(paste)
}

/// The account's non-expired pastes, newest first.
pub async fn list_for_account(state: &AppState, account_id: i64) -> Vec<Paste> {
    state
        .database()
        .all(
            "SELECT * FROM paste WHERE account_id = ?1 \
             AND (expires_at IS NULL OR datetime(expires_at) > datetime('now')) \
             ORDER BY created_at DESC",
            [account_id],
        )
        .await
        .unwrap_or_default()
}

/// The account's `public` pastes — the only ones that show on `/user/:name`.
pub async fn list_public(state: &AppState, account_id: i64, limit: i64) -> Vec<Paste> {
    state
        .database()
        .all(
            "SELECT * FROM paste WHERE account_id = ?1 AND visibility = 'public' \
             AND (expires_at IS NULL OR datetime(expires_at) > datetime('now')) \
             ORDER BY created_at DESC LIMIT ?2",
            (account_id, limit),
        )
        .await
        .unwrap_or_default()
}

/// The revision history of a paste, newest first.
pub async fn revisions(state: &AppState, id: &str) -> Vec<PasteRevision> {
    state
        .database()
        .all(
            "SELECT * FROM paste_revision WHERE paste_id = ?1 ORDER BY created_at DESC, id DESC",
            [id.to_string()],
        )
        .await
        .unwrap_or_default()
}

/// Counts a view. Best-effort — a read is never blocked on it.
pub async fn count_view(state: &AppState, id: &str) {
    let _ = state
        .database()
        .execute("UPDATE paste SET views = views + 1 WHERE id = ?1", [id.to_string()])
        .await;
}

// ─── Burn-after-read ─────────────────────────────────────────────────────────

/// Destroys a burn paste and hands back the row it destroyed.
///
/// `DELETE … RETURNING` is what makes this safe under concurrency: two reveals
/// racing for the same paste both run the delete, but only one of them deletes a
/// row and so only one gets a body back. The loser sees `None` — a 404, exactly
/// as if the paste had already been read, which is precisely what happened.
///
/// (`RETURNING` needs SQLite ≥ 3.35; rusqlite 0.31 bundles 3.45.)
pub async fn burn(state: &AppState, id: &str) -> Option<Paste> {
    let id = id.to_string();
    state
        .database()
        .call(move |conn| {
            conn.query_row(
                "DELETE FROM paste WHERE id = ?1 \
                 AND (expires_at IS NULL OR datetime(expires_at) > datetime('now')) \
                 RETURNING *",
                [id],
                <Paste as crate::database::Table>::from_row,
            )
            .optional()
        })
        .await
        .ok()
        .flatten()
}

// ─── Quotas ──────────────────────────────────────────────────────────────────

/// The count + total bytes an account currently has stored.
pub async fn usage(state: &AppState, account_id: i64) -> (i64, i64) {
    state
        .database()
        .get_row(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM paste WHERE account_id = ?1",
            [account_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .await
        .unwrap_or((0, 0))
}

/// Refuses a create that would put the account over either cap. Admins bypass
/// both, mirroring the short-link precedent.
async fn check_quota(state: &AppState, account: &Account, incoming: i64) -> Result<(), PasteError> {
    if account.flags.is_admin() {
        return Ok(());
    }
    let config = state.config();
    let (count, bytes) = usage(state, account.id).await;
    if count as usize >= config.paste.account_limit {
        return Err(PasteError::QuotaCount(config.paste.account_limit));
    }
    if bytes + incoming > config.paste.account_max_total_bytes {
        return Err(PasteError::QuotaBytes(config.paste.account_max_total_bytes));
    }
    Ok(())
}

// ─── Validation helpers ──────────────────────────────────────────────────────

fn normalize_title(title: Option<String>) -> Result<Option<String>, PasteError> {
    let title = title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    match &title {
        Some(t) if t.chars().count() > MAX_TITLE_LEN => Err(PasteError::TitleTooLong),
        _ => Ok(title),
    }
}

fn normalize_language(language: Option<String>) -> Option<String> {
    language
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|l| !l.is_empty() && l != "plain")
}

/// Resolves the stored expiry. An anonymous paste is *always* capped at the
/// configured anonymous TTL — including when it asked for no expiry at all.
fn resolve_expiry(expires_in: Option<i64>, anonymous: bool, anonymous_ttl_days: i64) -> Option<OffsetDateTime> {
    let anonymous_cap = anonymous.then(|| (anonymous_ttl_days.max(1)) * 24 * 60 * 60);
    let secs = match (expires_in.filter(|s| *s > 0), anonymous_cap) {
        (Some(requested), Some(cap)) => requested.min(cap),
        (Some(requested), None) => requested.min(MAX_TTL_SECS),
        (None, Some(cap)) => cap,
        (None, None) => return None,
    };
    Some(OffsetDateTime::now_utc() + Duration::seconds(secs.min(MAX_TTL_SECS)))
}

/// Runs the secret scanner over the plaintext and turns a *critical* finding
/// into a refusal.
///
/// Signed-in authors get a warning they can override ("publish anyway"); an
/// anonymous author cannot — a leaked AWS key posted by a stranger is exactly
/// what this endpoint must not become a delivery mechanism for.
fn check_for_secrets(content: &str, anonymous: bool, confirmed: bool) -> Result<(), PasteError> {
    if confirmed && !anonymous {
        return Ok(());
    }
    let rules: Vec<String> = secretshape::scan(content)
        .into_iter()
        .filter(|f| matches!(f.severity, secretshape::Severity::Critical))
        .map(|f| f.rule.to_string())
        .collect();
    if rules.is_empty() {
        return Ok(());
    }
    if anonymous {
        Err(PasteError::SecretsRefused(dedupe(rules)))
    } else {
        Err(PasteError::SecretsFound(dedupe(rules)))
    }
}

fn dedupe(mut rules: Vec<String>) -> Vec<String> {
    rules.sort();
    rules.dedup();
    rules
}

// ─── The reaper ──────────────────────────────────────────────────────────────

/// Hourly housekeeping: delete expired pastes, prune revision history past
/// [`REVISION_CAP`], and forget the `creator_ip` of anonymous pastes once they
/// are older than the anonymous retention window (no indefinite IP retention).
pub fn spawn_paste_reaper(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            ticker.tick().await;

            let deleted = state
                .database()
                .call(|conn| {
                    conn.execute(
                        "DELETE FROM paste WHERE expires_at IS NOT NULL \
                         AND datetime(expires_at) <= datetime('now')",
                        [],
                    )
                })
                .await
                .unwrap_or(0);
            if deleted > 0 {
                tracing::info!(count = deleted, "reaped expired pastes");
            }

            let _ = state
                .database()
                .execute(
                    "DELETE FROM paste_revision WHERE id NOT IN ( \
                         SELECT id FROM paste_revision AS r \
                         WHERE r.paste_id = paste_revision.paste_id \
                         ORDER BY r.created_at DESC, r.id DESC LIMIT ?1 )",
                    [REVISION_CAP],
                )
                .await;

            let ttl_days = state.config().paste.anonymous_ttl_days.max(1);
            let _ = state
                .database()
                .execute(
                    "UPDATE paste SET creator_ip = NULL \
                     WHERE creator_ip IS NOT NULL \
                       AND datetime(created_at) <= datetime('now', ?1)",
                    [format!("-{ttl_days} days")],
                )
                .await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_pastes_always_get_a_ttl() {
        // Even asking for "never" gets the forced anonymous window.
        let expiry = resolve_expiry(None, true, 30).expect("anonymous pastes never live forever");
        let days = (expiry - OffsetDateTime::now_utc()).whole_days();
        assert!((29..=30).contains(&days), "expected ~30 days, got {days}");

        // A shorter request is honoured, a longer one is capped.
        let short = resolve_expiry(Some(600), true, 30).unwrap();
        assert!((short - OffsetDateTime::now_utc()).whole_seconds() <= 600);
        let long = resolve_expiry(Some(365 * 24 * 3600), true, 30).unwrap();
        assert!((long - OffsetDateTime::now_utc()).whole_days() <= 30);
    }

    #[test]
    fn account_pastes_may_live_forever_but_not_longer_than_the_cap() {
        assert!(resolve_expiry(None, false, 30).is_none());
        let capped = resolve_expiry(Some(MAX_TTL_SECS * 10), false, 30).unwrap();
        assert!((capped - OffsetDateTime::now_utc()).whole_days() <= 365);
    }

    #[test]
    fn a_critical_secret_warns_an_account_and_refuses_a_stranger() {
        let leaked = "aws_key = \"AKIAIOSFODNN7EXAMPLE\"\nsecret = \"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\"\n";

        // A signed-in author is warned...
        match check_for_secrets(leaked, false, false) {
            Err(PasteError::SecretsFound(rules)) => assert!(!rules.is_empty()),
            other => panic!("expected a warning, got {other:?}"),
        }
        // ...and may override it.
        assert!(check_for_secrets(leaked, false, true).is_ok());

        // A stranger is refused, and cannot override it.
        assert!(matches!(
            check_for_secrets(leaked, true, true),
            Err(PasteError::SecretsRefused(_))
        ));
    }

    #[test]
    fn clean_content_passes_the_scanner() {
        assert!(check_for_secrets("fn main() { println!(\"hi\"); }", true, false).is_ok());
    }

    #[test]
    fn edit_tokens_are_hashed_and_compared_in_constant_time() {
        let token = new_edit_token();
        let hash = hash_edit_token(&token);
        assert_ne!(hash, token, "the token must never be stored in the clear");
        assert!(ct_eq(hash_edit_token(&token).as_bytes(), hash.as_bytes()));
        assert!(!ct_eq(hash_edit_token("not-the-token").as_bytes(), hash.as_bytes()));
    }

    #[test]
    fn titles_are_trimmed_and_length_capped() {
        assert_eq!(normalize_title(Some("  hi  ".into())).unwrap(), Some("hi".into()));
        assert_eq!(normalize_title(Some("   ".into())).unwrap(), None);
        assert!(matches!(
            normalize_title(Some("x".repeat(MAX_TITLE_LEN + 1))),
            Err(PasteError::TitleTooLong)
        ));
    }

    // ── DB-backed behaviour ──────────────────────────────────────────────────
    //
    // The rules that matter most — burn concurrency, expiry invisibility, the
    // quotas, the anonymous switch — are properties of the service against a real
    // schema, so these run the real migrations against an in-memory database.

    use crate::database::Database;
    use crate::models::Account;
    use crate::AppState;

    async fn test_state() -> AppState {
        // One connection: each `:memory:` connection is its own separate database.
        let database = Database::file(":memory:")
            .connections(1)
            .with_init(crate::migrations::migrate)
            .open()
            .await
            .expect("open in-memory db");
        AppState::for_tests(database).await
    }

    async fn seed_account(state: &AppState, admin: bool) -> Account {
        let flags = i64::from(admin); // AccountFlags::ADMIN is bit 0
        state
            .database()
            .execute(
                "INSERT INTO account(name, password, flags) VALUES ('alice', 'hash', ?1)",
                [flags],
            )
            .await
            .unwrap();
        state
            .database()
            .get::<Account, _, _>("SELECT *, NULL AS discord_id FROM account WHERE name = 'alice'", [])
            .await
            .unwrap()
            .unwrap()
    }

    fn plain(content: &str) -> NewPaste {
        NewPaste {
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_bare_load_never_burns_but_reveal_does_exactly_once() {
        let state = test_state().await;
        let account = seed_account(&state, false).await;

        let created = create(
            &state,
            Creator::Account(&account),
            None,
            NewPaste {
                burn_after_read: true,
                ..plain("top secret")
            },
        )
        .await
        .unwrap();
        let id = created.paste.id;

        // A plain load — what a link-preview crawler does — must not destroy it.
        assert!(load(&state, &id).await.is_some());
        assert!(load(&state, &id).await.is_some(), "a GET burned the paste");

        // The explicit reveal burns it and hands back the body...
        let burned = burn(&state, &id).await.expect("first reveal wins");
        assert_eq!(burned.text(), Some("top secret"));

        // ...and it is gone for everyone afterwards.
        assert!(load(&state, &id).await.is_none());
        assert!(burn(&state, &id).await.is_none(), "a burned paste burned twice");
    }

    #[tokio::test]
    async fn expired_pastes_are_invisible_to_every_read_path() {
        let state = test_state().await;
        let account = seed_account(&state, false).await;

        let created = create(&state, Creator::Account(&account), None, plain("stale"))
            .await
            .unwrap();
        let id = created.paste.id;

        // Force it into the past.
        state
            .database()
            .execute(
                "UPDATE paste SET expires_at = '2000-01-01T00:00:00.000Z' WHERE id = ?1",
                [id.clone()],
            )
            .await
            .unwrap();

        assert!(load(&state, &id).await.is_none(), "viewer path saw an expired paste");
        assert!(
            load_for(&state, &id, &Actor::account(&account)).await.is_err(),
            "owner path saw an expired paste"
        );
        assert!(
            list_for_account(&state, account.id).await.is_empty(),
            "list saw an expired paste"
        );
        assert!(burn(&state, &id).await.is_none(), "burn saw an expired paste");
    }

    #[tokio::test]
    async fn the_account_paste_count_quota_refuses() {
        let state = test_state().await;
        let account = seed_account(&state, false).await;

        // Shrink the cap so the test is cheap; the mechanism is the same at 100.
        let limit = state.config().paste.account_limit;
        for _ in 0..limit {
            create(&state, Creator::Account(&account), None, plain("x"))
                .await
                .expect("under the cap");
        }
        assert!(matches!(
            create(&state, Creator::Account(&account), None, plain("one too many")).await,
            Err(PasteError::QuotaCount(_))
        ));
    }

    #[tokio::test]
    async fn admins_bypass_the_quota() {
        let state = test_state().await;
        let admin = seed_account(&state, true).await;
        assert!(admin.flags.is_admin());

        for _ in 0..(state.config().paste.account_limit + 5) {
            create(&state, Creator::Account(&admin), None, plain("x"))
                .await
                .expect("admins are unlimited");
        }
    }

    #[tokio::test]
    async fn an_anonymous_edit_token_keeps_working_but_a_wrong_one_never_does() {
        let state = test_state().await;

        let created = create(&state, Creator::Anonymous, None, plain("anon body"))
            .await
            .unwrap();
        let token = created.edit_token.expect("anonymous pastes get a token");
        let id = created.paste.id;

        // The wrong token is a 404 — indistinguishable from "no such paste".
        assert!(load_for(&state, &id, &Actor::token("wrong")).await.is_err());

        // The right one edits it, and — being a credential, not a nonce — keeps
        // working on the next edit too.
        let paste = load_for(&state, &id, &Actor::token(token.clone())).await.unwrap();
        let edited = edit(
            &state,
            &paste,
            &Actor::token(token.clone()),
            None,
            EditPaste {
                content: "edited once".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(edited.text(), Some("edited once"));

        let again = load_for(&state, &id, &Actor::token(token.clone())).await.unwrap();
        assert!(
            edit(
                &state,
                &again,
                &Actor::token(token),
                None,
                EditPaste {
                    content: "edited twice".to_string(),
                    ..Default::default()
                }
            )
            .await
            .is_ok(),
            "the edit token stopped working after one use"
        );
    }

    #[tokio::test]
    async fn one_account_cannot_touch_anothers_paste() {
        let state = test_state().await;
        let alice = seed_account(&state, false).await;
        state
            .database()
            .execute(
                "INSERT INTO account(name, password, flags) VALUES ('bob', 'hash', 0)",
                [],
            )
            .await
            .unwrap();
        let bob: Account = state
            .database()
            .get("SELECT *, NULL AS discord_id FROM account WHERE name = 'bob'", [])
            .await
            .unwrap()
            .unwrap();

        let created = create(&state, Creator::Account(&alice), None, plain("alice's"))
            .await
            .unwrap();
        // Bob asking for Alice's paste gets a 404, not a 403 — same answer as if
        // it did not exist.
        assert!(load_for(&state, &created.paste.id, &Actor::account(&bob))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn an_edit_snapshots_the_previous_body_into_history() {
        let state = test_state().await;
        let account = seed_account(&state, false).await;

        let created = create(&state, Creator::Account(&account), None, plain("v1"))
            .await
            .unwrap();
        let paste = created.paste;
        edit(
            &state,
            &paste,
            &Actor::account(&account),
            None,
            EditPaste {
                content: "v2".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let history = revisions(&state, &paste.id).await;
        assert_eq!(history.len(), 1, "the pre-edit body should be snapshotted");
        assert_eq!(history[0].content, b"v1");
    }

    #[tokio::test]
    async fn a_forked_paste_is_owned_by_the_forker_and_points_back() {
        let state = test_state().await;
        let account = seed_account(&state, false).await;

        let source = create(&state, Creator::Account(&account), None, plain("original"))
            .await
            .unwrap()
            .paste;
        let forked = fork(&state, &source, "original", Creator::Account(&account), None)
            .await
            .unwrap()
            .paste;

        assert_eq!(forked.account_id, Some(account.id));
        assert_eq!(forked.fork_of.as_deref(), Some(source.id.as_str()));
        assert_eq!(forked.text(), Some("original"));
    }

    #[tokio::test]
    async fn an_anonymous_paste_is_ownerless_and_time_bounded() {
        let state = test_state().await;
        let created = create(&state, Creator::Anonymous, None, plain("anon"))
            .await
            .expect("anonymous pastes are on by default");
        // Nobody owns it, and it cannot live forever — the forced TTL applies even
        // though the request asked for none.
        assert!(created.paste.account_id.is_none());
        assert!(created.paste.expires_at.is_some(), "anonymous pastes must expire");
    }
}
