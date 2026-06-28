//! URL shortener: per-account short links served from the `r.` subdomain.
//!
//! Management UI lives at `/links` (any signed-in account); short links resolve
//! at `r.<domain>/<code>` in production and at `/<port>/r/<code>` in dev. The
//! bare-subdomain form is handled by the router fallback ([`short_link_fallback`]),
//! which only treats a request as a short link when it targets the configured
//! short host — every other unmatched path still 404s as before.

use askama::Template;
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::database::is_unique_constraint_violation;
use crate::filters; // used by the `isoformat` filter in links.html
use crate::flash::{FlashMessage, Flasher, Flashes};
use crate::models::{Account, ShortLink};
use crate::utils::get_new_image_id;
use crate::AppState;

/// Maximum short links a non-admin account may own.
const FREE_LINK_LIMIT: usize = 10;
/// Maximum length of a custom alias.
const MAX_CODE_LEN: usize = 64;
/// Maximum length of a destination URL.
const MAX_URL_LEN: usize = 2048;
/// How many times to retry an auto-generated code on a (rare) collision.
const AUTO_CODE_ATTEMPTS: usize = 6;

/// Top-level paths a custom alias must not shadow. On the `r.` host real routes
/// win over the fallback, so an alias matching one of these would be unreachable.
const RESERVED_CODES: &[&str] = &[
    "r",
    "links",
    "images",
    "gallery",
    "api",
    "admin",
    "login",
    "logout",
    "signup",
    "register",
    "account",
    "percy",
    "status",
    "projects",
    "static",
    "m",
    "ws",
    "auth",
    "sw.js",
    "robots.txt",
    "favicon.ico",
    "percy_favicon.ico",
    "site.webmanifest",
];

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Normalises a user-supplied destination into a stored URL. Forgiving about a
/// missing scheme (defaults to `https://`) but rejects anything that isn't a
/// plain http/https URL with a host.
fn normalize_target(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Destination URL is required.");
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    if with_scheme.len() > MAX_URL_LEN {
        return Err("Destination URL is too long.");
    }
    let lower = with_scheme.to_ascii_lowercase();
    let rest = match lower.strip_prefix("http://").or_else(|| lower.strip_prefix("https://")) {
        Some(rest) => rest,
        None => return Err("Destination must be an http:// or https:// URL."),
    };
    if rest.is_empty() || rest.starts_with('/') {
        return Err("Destination URL is missing a host.");
    }
    Ok(with_scheme)
}

/// Validates a custom alias: `[A-Za-z0-9_-]`, length-bounded, not reserved.
fn validate_code(raw: &str) -> Result<String, &'static str> {
    let code = raw.trim();
    if code.is_empty() {
        return Err("Alias cannot be empty.");
    }
    if code.len() > MAX_CODE_LEN {
        return Err("Alias is too long.");
    }
    if !code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Alias may only contain letters, numbers, hyphens and underscores.");
    }
    if RESERVED_CODES.contains(&code.to_ascii_lowercase().as_str()) {
        return Err("That alias is reserved — please pick another.");
    }
    Ok(code.to_string())
}

// ---------------------------------------------------------------------------
// Management page (/links)
// ---------------------------------------------------------------------------

/// A short link prepared for display (with its resolved public URL).
struct LinkView {
    id: i64,
    code: String,
    short_url: String,
    target_url: String,
    clicks: i64,
    created_at: OffsetDateTime,
}

#[derive(Template)]
#[template(path = "links/links.html")]
struct LinksTemplate {
    account: Option<Account>,
    flashes: Flashes,
    links: Vec<LinkView>,
    /// Admins have no cap; non-admins are limited to [`FREE_LINK_LIMIT`].
    is_admin: bool,
    limit: usize,
    /// The host short links are served from, e.g. `r.klappstuhl.me`.
    short_host: String,
}

async fn links_page(State(state): State<AppState>, account: Account, flashes: Flashes) -> Response {
    let links: Vec<ShortLink> = state
        .database()
        .all(
            "SELECT * FROM short_link WHERE account_id = ?1 ORDER BY created_at DESC",
            [account.id],
        )
        .await
        .unwrap_or_default();

    let config = state.config();
    let links: Vec<LinkView> = links
        .into_iter()
        .map(|l| LinkView {
            short_url: config.short_link_url(&l.code),
            id: l.id,
            code: l.code,
            target_url: l.target_url,
            clicks: l.clicks,
            created_at: l.created_at,
        })
        .collect();

    LinksTemplate {
        is_admin: account.flags.is_admin(),
        limit: FREE_LINK_LIMIT,
        short_host: config.short_domain(),
        account: Some(account),
        flashes,
        links,
    }
    .into_response()
}

// ---------------------------------------------------------------------------
// Create / edit / delete
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateForm {
    target_url: String,
    #[serde(default)]
    code: String,
}

async fn create_link(
    State(state): State<AppState>,
    account: Account,
    flasher: Flasher,
    Form(form): Form<CreateForm>,
) -> Response {
    // Enforce the free-tier cap for non-admins.
    if !account.flags.is_admin() && count_links(&state, account.id).await >= FREE_LINK_LIMIT {
        return flasher
            .add(FlashMessage::error(format!(
                "You've reached the limit of {FREE_LINK_LIMIT} short links — delete one to create another."
            )))
            .bail("/links");
    }

    let target = match normalize_target(&form.target_url) {
        Ok(t) => t,
        Err(e) => return flasher.add(FlashMessage::error(e)).bail("/links"),
    };

    let custom_alias = !form.code.trim().is_empty();
    let code = if custom_alias {
        match validate_code(&form.code) {
            Ok(c) => c,
            Err(e) => return flasher.add(FlashMessage::error(e)).bail("/links"),
        }
    } else {
        get_new_image_id()
    };

    match insert_link(&state, code, &target, account.id, custom_alias).await {
        Ok(()) => flasher.add(FlashMessage::success("Short link created.")).bail("/links"),
        Err(InsertError::Taken) => flasher
            .add(FlashMessage::error(
                "That alias is already taken — please pick another.",
            ))
            .bail("/links"),
        Err(InsertError::Db) => flasher
            .add(FlashMessage::error(
                "Could not create the short link. Please try again.",
            ))
            .bail("/links"),
    }
}

enum InsertError {
    /// The code/alias is already in use.
    Taken,
    /// An unexpected database error occurred.
    Db,
}

/// Inserts a link. Custom aliases get a single attempt (collision → `Taken`);
/// auto-generated codes are retried with fresh codes on the (rare) collision.
async fn insert_link(
    state: &AppState,
    mut code: String,
    target: &str,
    account_id: i64,
    custom_alias: bool,
) -> Result<(), InsertError> {
    let attempts = if custom_alias { 1 } else { AUTO_CODE_ATTEMPTS };
    for _ in 0..attempts {
        let result = state
            .database()
            .execute(
                "INSERT INTO short_link (code, target_url, account_id) VALUES (?1, ?2, ?3)",
                (code.clone(), target.to_string(), account_id),
            )
            .await;
        match result {
            Ok(_) => return Ok(()),
            Err(e) if is_unique_constraint_violation(&e) => {
                if custom_alias {
                    return Err(InsertError::Taken);
                }
                code = get_new_image_id(); // collision on an auto code — try again
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to insert short link");
                return Err(InsertError::Db);
            }
        }
    }
    Err(InsertError::Taken)
}

async fn count_links(state: &AppState, account_id: i64) -> usize {
    state
        .database()
        .get_row(
            "SELECT COUNT(*) FROM short_link WHERE account_id = ?1",
            [account_id],
            |row| row.get::<_, i64>(0),
        )
        .await
        .unwrap_or(0)
        .max(0) as usize
}

#[derive(Deserialize)]
struct EditForm {
    target_url: String,
    code: String,
}

async fn edit_link(
    State(state): State<AppState>,
    account: Account,
    flasher: Flasher,
    Path(id): Path<i64>,
    Form(form): Form<EditForm>,
) -> Response {
    let Some(link) = owned_link(&state, id, &account).await else {
        return flasher
            .add(FlashMessage::error("Short link not found, or not yours to edit."))
            .bail("/links");
    };

    let target = match normalize_target(&form.target_url) {
        Ok(t) => t,
        Err(e) => return flasher.add(FlashMessage::error(e)).bail("/links"),
    };
    let code = match validate_code(&form.code) {
        Ok(c) => c,
        Err(e) => return flasher.add(FlashMessage::error(e)).bail("/links"),
    };

    let result = state
        .database()
        .execute(
            "UPDATE short_link SET target_url = ?1, code = ?2, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?3",
            (target, code, link.id),
        )
        .await;

    match result {
        Ok(_) => flasher.add(FlashMessage::success("Short link updated.")).bail("/links"),
        Err(e) if is_unique_constraint_violation(&e) => flasher
            .add(FlashMessage::error(
                "That alias is already taken — please pick another.",
            ))
            .bail("/links"),
        Err(e) => {
            tracing::error!(error = %e, "failed to update short link");
            flasher
                .add(FlashMessage::error("Could not update the short link."))
                .bail("/links")
        }
    }
}

async fn delete_link(
    State(state): State<AppState>,
    account: Account,
    flasher: Flasher,
    Path(id): Path<i64>,
) -> Response {
    let Some(link) = owned_link(&state, id, &account).await else {
        return flasher
            .add(FlashMessage::error("Short link not found, or not yours to delete."))
            .bail("/links");
    };
    let _ = state
        .database()
        .execute("DELETE FROM short_link WHERE id = ?1", [link.id])
        .await;
    flasher.add(FlashMessage::success("Short link deleted.")).bail("/links")
}

/// Loads a link by id only if the account owns it (or is an admin).
async fn owned_link(state: &AppState, id: i64, account: &Account) -> Option<ShortLink> {
    let link: ShortLink = state
        .database()
        .get("SELECT * FROM short_link WHERE id = ?1", [id])
        .await
        .ok()
        .flatten()?;
    (link.account_id == account.id || account.flags.is_admin()).then_some(link)
}

// ---------------------------------------------------------------------------
// Resolution (redirect)
// ---------------------------------------------------------------------------

/// Resolves a code to its destination, counting the click, and returns a
/// redirect — or a 404 when the code is unknown.
async fn resolve_and_redirect(state: &AppState, code: &str) -> Response {
    let link: Option<ShortLink> = state
        .database()
        .get("SELECT * FROM short_link WHERE code = ?1", [code.to_string()])
        .await
        .unwrap_or(None);

    match link {
        Some(link) => {
            // Best-effort click count; never block the redirect on it.
            let _ = state
                .database()
                .execute("UPDATE short_link SET clicks = clicks + 1 WHERE id = ?1", [link.id])
                .await;
            Redirect::temporary(&link.target_url).into_response()
        }
        None => not_found(),
    }
}

/// `GET /r/:code` — path-based resolution that works on any host (used in dev,
/// and as a fallback before the `r.` subdomain is wired up).
async fn resolve_path(State(state): State<AppState>, Path(code): Path<String>) -> Response {
    resolve_and_redirect(&state, &code).await
}

/// Router fallback: resolves bare `r.<domain>/<code>` requests, and 404s
/// everything else (preserving the previous default-404 behaviour).
pub async fn short_link_fallback(State(state): State<AppState>, headers: HeaderMap, uri: Uri) -> Response {
    let config = state.config();
    // Bare-path resolution is only meaningful in production on the real short
    // host. In dev every request is to localhost, so resolution goes through
    // the explicit `/r/:code` route instead.
    if config.production {
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .or_else(|| headers.get("x-forwarded-host").and_then(|h| h.to_str().ok()))
            .unwrap_or("");
        if host_eq(host, &config.short_domain()) {
            let code = uri.path().trim_matches('/');
            if !code.is_empty() && !code.contains('/') {
                return resolve_and_redirect(&state, code).await;
            }
        }
    }
    not_found()
}

/// Compares two `host` strings ignoring any `:port` suffix and ASCII case.
fn host_eq(a: &str, b: &str) -> bool {
    let strip = |s: &str| s.split(':').next().unwrap_or(s).to_ascii_lowercase();
    strip(a) == strip(b)
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/links", get(links_page).post(create_link))
        .route("/links/:id/edit", post(edit_link))
        .route("/links/:id/delete", post(delete_link))
        .route("/r/:code", get(resolve_path))
}
