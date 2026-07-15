//! The paste mutations, driven from the browser: create, edit, delete, fork,
//! unlock, reveal.
//!
//! These handlers are thin on purpose. They parse a form, hand it to
//! [`super::service`], and turn the answer into either a redirect-with-flash (a
//! plain form post, so the pastebin works with JavaScript off) or JSON (the
//! editor's async submit, which needs the secret-scan warning and the edit token
//! back in-band). No validation, no quota logic, no SQL: that all lives in the
//! service, where the JSON API sees exactly the same rules.

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Extension, Form, Json,
};
use cookie::Cookie;
use serde::Deserialize;

use crate::cookies::set_cookie;
use crate::flash::{FlashMessage, Flasher};
use crate::headers::ClientIp;
use crate::key::SecretKey;
use crate::models::{Account, Visibility};
use crate::AppState;

use super::service::{self, Actor, Created, Creator, EditPaste, NewPaste, PasteError};
use super::{build_unlock_cookie, crypto, render, resolve_body, wants_json, Body};

// ─── Forms ───────────────────────────────────────────────────────────────────

/// The editor's form. Checkboxes arrive as `Some("on")` or not at all, which is
/// why the flags are `Option<String>` rather than `bool`.
#[derive(Debug, Default, Deserialize)]
pub struct PasteForm {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    /// Time-to-live in seconds; `0`/absent means "never".
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub burn_after_read: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// Set by the "publish anyway" button on the secret-scan warning.
    #[serde(default)]
    pub confirm_secrets: Option<String>,
    /// An anonymous author's edit token, when editing.
    #[serde(default)]
    pub token: Option<String>,
}

impl PasteForm {
    fn checked(field: &Option<String>) -> bool {
        field
            .as_deref()
            .is_some_and(|v| v != "false" && v != "off" && !v.is_empty())
    }

    fn into_new(self, fork_of: Option<String>) -> NewPaste {
        NewPaste {
            visibility: self
                .visibility
                .as_deref()
                .map(Visibility::parse)
                .unwrap_or(Visibility::Unlisted),
            burn_after_read: Self::checked(&self.burn_after_read),
            confirm_secrets: Self::checked(&self.confirm_secrets),
            password: self.password.filter(|p| !p.is_empty()),
            content: self.content,
            title: self.title,
            language: self.language,
            expires_in: self.expires_in,
            fork_of,
        }
    }

    fn into_edit(self) -> EditPaste {
        EditPaste {
            visibility: self.visibility.as_deref().map(Visibility::parse),
            confirm_secrets: Self::checked(&self.confirm_secrets),
            password: self.password.filter(|p| !p.is_empty()),
            content: self.content,
            title: self.title,
            language: self.language,
            expires_in: self.expires_in,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct UnlockForm {
    #[serde(default)]
    pub password: String,
}

// ─── Create ──────────────────────────────────────────────────────────────────

/// `POST /paste` — create from the editor. Anonymous when logged out (and
/// anonymous pastes are enabled).
pub async fn create(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Option<Account>,
    flasher: Flasher,
    headers: HeaderMap,
    Form(form): Form<PasteForm>,
) -> Response {
    let json = wants_json(&headers);
    let creator = match account.as_ref() {
        Some(account) => Creator::Account(account),
        None => Creator::Anonymous,
    };

    match service::create(&state, creator, client_ip, form.into_new(None)).await {
        Ok(created) => created_response(&state, created, json, &flasher),
        Err(error) => error_response(error, json, &flasher, "/paste"),
    }
}

/// `POST /p` — the `curl` endpoint.
///
/// ```sh
/// curl --data-binary @notes.txt https://klappstuhl.me/p
/// ```
///
/// Takes the raw request body as the paste, and answers in `text/plain` with the
/// URL — and, for an anonymous paste, the edit token, which is the only way the
/// author will ever be able to delete it. An API key in `Authorization` is
/// honoured, so the same command can create an *owned* paste.
pub async fn create_raw(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Option<Account>,
    Query(query): Query<RawCreateQuery>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let creator = match account.as_ref() {
        Some(account) => Creator::Account(account),
        None => Creator::Anonymous,
    };

    // `?filename=main.rs` (or a filename in the query) picks the highlighter, so
    // `curl --data-binary @main.rs 'klappstuhl.me/p?filename=main.rs'` is coloured.
    let language = query
        .language
        .or_else(|| query.filename.as_deref().and_then(render::language_from_filename));

    let new = NewPaste {
        content: body,
        language,
        title: query.title,
        visibility: query.visibility.as_deref().map(Visibility::parse).unwrap_or_default(),
        burn_after_read: query.burn.unwrap_or(false),
        expires_in: query.expires_in,
        ..Default::default()
    };

    match service::create(&state, creator, client_ip, new).await {
        Ok(created) => {
            let url = state.config().url_to(format!("/p/{}", created.paste.id));
            if wants_json(&headers) {
                return Json(serde_json::json!({
                    "id": created.paste.id,
                    "url": url,
                    "raw_url": state.config().url_to(format!("/p/{}.txt", created.paste.id)),
                    "edit_token": created.edit_token,
                }))
                .into_response();
            }

            // The token goes in a header *and* in the body: a header so scripts
            // can grab it without parsing, a body line so a human running the
            // command in a terminal actually sees it. It is shown exactly once.
            let mut text = format!("{url}\n");
            let mut response_headers = HeaderMap::new();
            response_headers.insert(header::CONTENT_TYPE, "text/plain; charset=utf-8".parse().unwrap());
            if let Some(token) = created.edit_token.as_deref() {
                if let Ok(value) = token.parse() {
                    response_headers.insert("x-edit-token", value);
                }
                text.push_str(&format!(
                    "edit token: {token}\n(keep it — it is the only way to edit or delete this paste)\n"
                ));
            }
            (StatusCode::CREATED, response_headers, text).into_response()
        }
        Err(error) => {
            let status = match error {
                PasteError::Db | PasteError::Crypto => StatusCode::INTERNAL_SERVER_ERROR,
                PasteError::AnonymousDisabled => StatusCode::FORBIDDEN,
                _ => StatusCode::BAD_REQUEST,
            };
            (
                status,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("{}\n", error.message()),
            )
                .into_response()
        }
    }
}

/// The knobs `POST /p` exposes as query parameters, since a `curl --data-binary`
/// body is the paste itself and has no room for fields.
#[derive(Debug, Default, Deserialize)]
pub struct RawCreateQuery {
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub burn: Option<bool>,
    #[serde(default)]
    pub expires_in: Option<i64>,
}

// ─── Edit ────────────────────────────────────────────────────────────────────

/// `POST /p/:id/edit` — save, snapshotting the previous body into the history.
pub async fn edit(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Path(id): Path<String>,
    account: Option<Account>,
    flasher: Flasher,
    headers: HeaderMap,
    Form(form): Form<PasteForm>,
) -> Response {
    let json = wants_json(&headers);
    let actor = Actor {
        account: account.as_ref(),
        edit_token: form.token.clone().filter(|t| !t.trim().is_empty()),
    };

    let paste = match service::load_for(&state, &id, &actor).await {
        Ok(paste) => paste,
        Err(error) => return error_response(error, json, &flasher, "/pastes"),
    };

    match service::edit(&state, &paste, &actor, client_ip, form.into_edit()).await {
        Ok(paste) => {
            let url = format!("/p/{}", paste.id);
            if json {
                Json(serde_json::json!({ "id": paste.id, "url": url })).into_response()
            } else {
                flasher.add(FlashMessage::success("Paste saved.")).bail(&url)
            }
        }
        Err(error) => error_response(error, json, &flasher, &format!("/p/{id}/edit")),
    }
}

// ─── Delete ──────────────────────────────────────────────────────────────────

/// `POST /p/:id/delete` — delete. The owner, or an anonymous author holding the
/// edit token.
pub async fn delete(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Path(id): Path<String>,
    account: Option<Account>,
    flasher: Flasher,
    headers: HeaderMap,
    Form(form): Form<PasteForm>,
) -> Response {
    let json = wants_json(&headers);
    let actor = Actor {
        account: account.as_ref(),
        edit_token: form.token.clone().filter(|t| !t.trim().is_empty()),
    };

    let paste = match service::load_for(&state, &id, &actor).await {
        Ok(paste) => paste,
        Err(error) => return error_response(error, json, &flasher, "/pastes"),
    };

    match service::delete(&state, &paste, &actor, client_ip).await {
        Ok(()) => {
            let destination = if account.is_some() { "/pastes" } else { "/paste" };
            if json {
                Json(serde_json::json!({ "ok": true, "redirect": destination })).into_response()
            } else {
                flasher.add(FlashMessage::success("Paste deleted.")).bail(destination)
            }
        }
        Err(error) => error_response(error, json, &flasher, &format!("/p/{id}")),
    }
}

// ─── Fork ────────────────────────────────────────────────────────────────────

/// `POST /p/:id/fork` — copy a paste into a new one of your own.
///
/// Forking needs the *plaintext*, so an encrypted paste can only be forked once
/// you've unlocked it, and a burn paste cannot be forked at all — forking it
/// would be a read that dodges the burn.
// Axum extractors are one parameter each; this handler needs the cookies and the
// secret to resolve an unlocked body before forking, which is what tips it over.
#[allow(clippy::too_many_arguments)]
pub async fn fork(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Path(id): Path<String>,
    Extension(cookies): Extension<Vec<Cookie<'static>>>,
    Extension(secret): Extension<SecretKey>,
    account: Option<Account>,
    flasher: Flasher,
    headers: HeaderMap,
) -> Response {
    let json = wants_json(&headers);
    let Some(source) = service::load(&state, &id).await else {
        return error_response(PasteError::NotFound, json, &flasher, "/pastes");
    };

    let Body::Plain(plaintext) = resolve_body(&source, &cookies, &secret, false) else {
        return error_response(PasteError::NotFound, json, &flasher, &format!("/p/{id}"));
    };

    let creator = match account.as_ref() {
        Some(account) => Creator::Account(account),
        None => Creator::Anonymous,
    };

    match service::fork(&state, &source, &plaintext, creator, client_ip).await {
        Ok(created) => created_response(&state, created, json, &flasher),
        Err(error) => error_response(error, json, &flasher, &format!("/p/{id}")),
    }
}

// ─── Unlock ──────────────────────────────────────────────────────────────────

/// `POST /p/:id/unlock` — the password gate.
///
/// A correct password mints a short-lived, paste-scoped, signed cookie carrying
/// the derived content key, so `/raw` and `/embed` work afterwards without
/// prompting again. A wrong one is **audited** (`paste.unlock.fail`) — that row
/// is the brute-force signal, the same way auto-lockout reads `auth.login.fail`.
pub async fn unlock(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Path(id): Path<String>,
    Extension(secret): Extension<SecretKey>,
    flasher: Flasher,
    Form(form): Form<UnlockForm>,
) -> Response {
    let Some(paste) = service::load(&state, &id).await else {
        return super::pages::not_found();
    };

    let (Some(salt), Some(nonce)) = (paste.enc_salt.as_deref(), paste.enc_nonce.as_deref()) else {
        return Redirect::to(&format!("/p/{id}")).into_response();
    };

    let Some((key, _)) = crypto::open_and_keep_key(&form.password, salt, nonce, &paste.content) else {
        state
            .audit("paste.unlock.fail")
            .target(paste.id.clone())
            .ip_opt(client_ip)
            .fire();
        return flasher
            .add(FlashMessage::error("Wrong password."))
            .bail(format!("/p/{id}"));
    };

    let Some(token) = crypto::sign_unlock(&secret, &paste.id, &key) else {
        return flasher
            .add(FlashMessage::error("Could not unlock the paste."))
            .bail(format!("/p/{id}"));
    };

    let mut response = Redirect::to(&format!("/p/{id}")).into_response();
    set_cookie(
        &mut response,
        build_unlock_cookie(&paste.id, token, state.config().production),
    );
    response
}

// ─── Burn ────────────────────────────────────────────────────────────────────

/// `POST /p/:id/reveal` — **the only request that ever burns a paste**.
///
/// A plain `GET` renders an interstitial instead, precisely so that Discord,
/// Slack and iMessage prefetching the link to build an embed cannot destroy the
/// paste before its recipient ever clicks it.
///
/// The burn itself is a single `DELETE … RETURNING` in the service, so two
/// concurrent reveals cannot both win: one deletes the row and gets the body, the
/// other gets nothing and sees a 404 — which is the truth, the paste is gone.
pub async fn reveal(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Path(id): Path<String>,
    account: Option<Account>,
    flasher: Flasher,
    Form(form): Form<UnlockForm>,
) -> Response {
    let Some(paste) = service::load(&state, &id).await else {
        return super::pages::not_found();
    };
    if !paste.burn_after_read {
        return Redirect::to(&format!("/p/{id}")).into_response();
    }

    // Check the password *before* destroying anything: a wrong password must not
    // consume the one read the paste has.
    if paste.is_encrypted() {
        let (Some(salt), Some(nonce)) = (paste.enc_salt.as_deref(), paste.enc_nonce.as_deref()) else {
            return super::pages::not_found();
        };
        if crypto::open(&form.password, salt, nonce, &paste.content).is_none() {
            state
                .audit("paste.unlock.fail")
                .target(paste.id.clone())
                .ip_opt(client_ip)
                .fire();
            return flasher
                .add(FlashMessage::error("Wrong password."))
                .bail(format!("/p/{id}"));
        }
    }

    let Some(burned) = service::burn(&state, &id).await else {
        // Someone else revealed it first. That is not an error — it is what a
        // burn-after-read paste *does*.
        return super::pages::not_found();
    };

    state
        .audit("paste.burn")
        .target(burned.id.clone())
        .ip_opt(client_ip)
        .fire();

    let plaintext = if burned.is_encrypted() {
        let salt = burned.enc_salt.clone().unwrap_or_default();
        let nonce = burned.enc_nonce.clone().unwrap_or_default();
        crypto::open(&form.password, &salt, &nonce, &burned.content)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or_default()
    } else {
        burned.text().unwrap_or_default().to_string()
    };

    super::pages::burned(&state, burned, plaintext, account).await
}

// ─── Shared responses ────────────────────────────────────────────────────────

/// A successful create, on either surface.
fn created_response(state: &AppState, created: Created, json: bool, flasher: &Flasher) -> Response {
    let url = format!("/p/{}", created.paste.id);
    if json {
        return Json(serde_json::json!({
            "id": created.paste.id,
            "url": url,
            "absolute_url": state.config().url_to(url.clone()),
            // Present only for an anonymous paste, and only here — it exists in
            // the clear exactly once.
            "edit_token": created.edit_token,
        }))
        .into_response();
    }

    // Without JS the edit token has to survive the redirect, and a flash is the
    // only channel there is. It is shown once and never stored.
    if let Some(token) = created.edit_token.as_deref() {
        flasher.add(FlashMessage::success(format!(
            "Paste created. Your edit token is {token} — keep it, it is the only way to edit or delete this paste."
        )));
        return flasher.bail(format!("{url}?token={token}"));
    }
    flasher.add(FlashMessage::success("Paste created.")).bail(&url)
}

/// A refusal, on either surface.
///
/// The secret-scan warning is the one that needs structure rather than prose:
/// the editor has to know *which* rules fired so it can show the finding and
/// offer "publish anyway", so it comes back as a 422 with the rule names.
fn error_response(error: PasteError, json: bool, flasher: &Flasher, back: &str) -> Response {
    if json {
        let status = match error {
            PasteError::NotFound => StatusCode::NOT_FOUND,
            PasteError::SecretsFound(_) | PasteError::SecretsRefused(_) => StatusCode::UNPROCESSABLE_ENTITY,
            PasteError::AnonymousDisabled => StatusCode::FORBIDDEN,
            PasteError::Db | PasteError::Crypto => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_REQUEST,
        };
        let secrets = match &error {
            PasteError::SecretsFound(rules) => Some((rules.clone(), true)),
            PasteError::SecretsRefused(rules) => Some((rules.clone(), false)),
            _ => None,
        };
        return (
            status,
            Json(serde_json::json!({
                "error": error.message(),
                "secrets": secrets.as_ref().map(|(rules, _)| rules),
                // Whether the author is allowed to say "publish anyway". An
                // anonymous author is not.
                "overridable": secrets.as_ref().map(|(_, overridable)| *overridable),
            })),
        )
            .into_response();
    }
    flasher.add(FlashMessage::error(error.message())).bail(back)
}
