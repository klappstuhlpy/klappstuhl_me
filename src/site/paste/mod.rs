//! The pastebin: a browser-first paste host at `/paste` (editor), `/pastes`
//! (yours), and `/p/<id>` (the viewer), plus the `curl`-friendly `POST /p`.
//!
//! The slice is split by role:
//!
//! - [`service`] — the shared core. Validation, quotas, secret scanning,
//!   encryption, the audit trail, and every read. Both these handlers **and**
//!   [`crate::site::api::pastes`] go through it, so the browser and the JSON API
//!   cannot disagree about what a legal paste is.
//! - [`crypto`] — Argon2id + ChaCha20-Poly1305 password protection and the signed
//!   unlock cookie.
//! - [`render`] — syntax highlighting with per-line anchors, and the *sanitised*
//!   markdown parser.
//! - [`pages`] / [`crud`] — the GET pages and the form-post handlers, both thin
//!   shells over `service`.
//!
//! ## The one routing constraint
//!
//! `/p/:id` cannot gain a *static* sibling: matchit treats `/p/:id` and
//! `/p/new` as a conflict, which is why the raw suffix (`/p/<id>.txt`) is parsed
//! **inside** the handler rather than being its own route. New static paths
//! therefore live off `/p/*` entirely (`/paste`, `/pastes`); deeper sub-paths
//! (`/p/:id/raw`) are fine. `routes::tests::full_router_builds` catches a slip.

pub mod crud;
pub mod crypto;
pub mod pages;
pub mod render;
pub mod service;

use axum::{
    routing::{get, post},
    Router,
};
use cookie::Cookie;

use crate::key::SecretKey;
use crate::models::Paste;
use crate::AppState;

pub use service::spawn_paste_reaper;

/// What a reader is allowed to see of a paste's body, right now.
pub enum Body {
    /// The plaintext, ready to render.
    Plain(String),
    /// Password-protected, and this reader hasn't unlocked it. The body is not
    /// merely hidden in the page — it never enters the response at all.
    Locked,
    /// Burn-after-read, and this is a plain `GET`. Reading it is an explicit act
    /// (`POST /p/:id/reveal`), so a link-preview crawler can't destroy it.
    Sealed,
    /// The stored bytes aren't valid UTF-8 and aren't encrypted either — a paste
    /// that predates this and got corrupted, or a byte-for-byte binary upload.
    Undecodable,
}

/// Resolves what this reader may see, honouring the unlock cookie.
///
/// `for_reveal` is set by the burn-reveal path, which has *already* decided the
/// reader may see it — so it skips the [`Body::Sealed`] gate but still enforces
/// the password.
pub fn resolve_body(paste: &Paste, cookies: &[Cookie<'static>], secret: &SecretKey, for_reveal: bool) -> Body {
    if paste.burn_after_read && !for_reveal {
        return Body::Sealed;
    }

    if paste.is_encrypted() {
        let (Some(nonce), Some(token)) = (paste.enc_nonce.as_deref(), unlock_cookie(cookies, &paste.id)) else {
            return Body::Locked;
        };
        let Some(key) = crypto::verify_unlock(secret, &paste.id, &token) else {
            return Body::Locked;
        };
        return match crypto::open_with_key(&key, nonce, &paste.content) {
            Some(plain) => match String::from_utf8(plain) {
                Ok(text) => Body::Plain(text),
                Err(_) => Body::Undecodable,
            },
            None => Body::Locked,
        };
    }

    match paste.text() {
        Some(text) => Body::Plain(text.to_string()),
        None => Body::Undecodable,
    }
}

/// The signed unlock cookie for `id`, if the request carries one.
pub fn unlock_cookie(cookies: &[Cookie<'static>], id: &str) -> Option<String> {
    let name = crypto::cookie_name(id);
    cookies.iter().find(|c| c.name() == name).map(|c| c.value().to_string())
}

/// Builds the unlock cookie for a paste. Scoped to that paste's path (so it
/// unlocks nothing else), `HttpOnly` (so script can't read the content key), and
/// short-lived.
pub fn build_unlock_cookie(id: &str, token: String, production: bool) -> Cookie<'static> {
    Cookie::build((crypto::cookie_name(id), token))
        .path(format!("/p/{id}"))
        .http_only(true)
        .secure(production)
        .same_site(cookie::SameSite::Lax)
        .max_age(time::Duration::seconds(crypto::UNLOCK_TTL_SECS))
        .build()
}

/// Whether a request wants JSON back (the editor's async submit) rather than a
/// redirect. Mirrors the dashboard's `is_ajax` convention.
pub fn wants_json(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        // The editor and the list. Static paths, deliberately *not* under `/p/`.
        .route("/paste", get(pages::editor).post(crud::create))
        // The live-highlight overlay the editor paints as you type.
        .route("/paste/preview", post(pages::preview))
        .route("/pastes", get(pages::list))
        // `POST /p` — the curl endpoint. `text/plain` in, a URL in plain text out.
        .route("/p", post(crud::create_raw))
        // The viewer. The `.txt` suffix is handled inside — see the module docs.
        .route("/p/:id", get(pages::view))
        .route("/p/:id/raw", get(pages::raw))
        .route("/p/:id/embed", get(pages::embed))
        .route("/p/:id/og.svg", get(pages::og_image))
        .route("/p/:id/history", get(pages::history))
        .route("/p/:id/edit", get(pages::edit_form).post(crud::edit))
        .route("/p/:id/unlock", post(crud::unlock))
        .route("/p/:id/reveal", post(crud::reveal))
        .route("/p/:id/delete", post(crud::delete))
        .route("/p/:id/fork", post(crud::fork))
}
