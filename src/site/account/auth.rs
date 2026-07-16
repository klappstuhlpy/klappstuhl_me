//! Getting in and out: the login page, the password + TOTP login steps, open
//! signup, and logout. Everything here is reachable while logged *out* — the
//! account shell itself lives in [`super::pages`].

use crate::{
    auth::{hash_password, validate_password},
    error::ApiError,
    flash::{FlashMessage, Flasher, Flashes},
    headers::ClientIp,
    logging::BadRequestReason,
    models::{is_valid_username, Account},
    token::{Token, TokenRejection},
    AppState,
};
use askama::Template;
use axum::{
    extract::{Query, State},
    response::{IntoResponse, Redirect, Response},
    Extension, Form,
};
use cookie::Cookie;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{clear_host_only_cookie, cookie_redirect, load_full_account, register_failure};

#[derive(Template)]
#[template(path = "auth/login.html")]
struct LoginTemplate {
    account: Option<Account>,
    flashes: Flashes,
    discord_enabled: bool,
    /// Sanitized post-login redirect path (empty = none). Submitted as a hidden
    /// form field so the password POST can honour it.
    next: String,
    /// `?next=<encoded>` query suffix (empty = none) for the Discord / signup links.
    next_query: String,
}

/// Query string carrying a post-auth redirect target (`?next=/some/path`).
#[derive(Deserialize)]
pub struct NextQuery {
    #[serde(default)]
    next: Option<String>,
}

pub async fn login(
    State(state): State<AppState>,
    account: Option<Account>,
    flashes: Flashes,
    Query(query): Query<NextQuery>,
) -> Response {
    let trusted = state.config().trusted_domain();
    let next = crate::utils::safe_next_for_domain(query.next.as_deref(), Some(&trusted));
    if account.is_some() {
        return Redirect::to(next.as_deref().unwrap_or("/")).into_response();
    }
    let next_query = next
        .as_deref()
        .map(|n| format!("?next={}", crate::utils::urlencode(n)))
        .unwrap_or_default();
    LoginTemplate {
        account,
        flashes,
        discord_enabled: state.config().discord.enabled(),
        next: next.unwrap_or_default(),
        next_query,
    }
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct Credentials {
    username: String,
    password: String,
    #[serde(deserialize_with = "crate::utils::empty_string_is_none")]
    session_description: Option<String>,
    /// Present (with any value) when the "stay logged in" checkbox is ticked.
    /// Absent when unchecked – HTML form checkboxes omit the field entirely.
    #[serde(default)]
    remember_me: Option<String>,
    /// Post-login redirect target carried from the login page (validated server-side).
    #[serde(default, deserialize_with = "crate::utils::empty_string_is_none")]
    next: Option<String>,
}

async fn authenticate(
    state: &AppState,
    credentials: Credentials,
    client_ip: Option<std::net::IpAddr>,
) -> Result<Response, ApiError> {
    if !is_valid_username(&credentials.username) {
        return Err(ApiError::new("invalid username given"));
    }

    if !((8..=128).contains(&credentials.password.len())) {
        return Err(ApiError::new("password length must be 8 to 128 characters"));
    }

    // Soft-lockout: refuse outright once this IP has failed too many times in
    // the window, before touching the account row. In-process, so it holds even
    // with no firewall/admin app present.
    if let Some(ip) = client_ip {
        if super::lockout::is_locked(ip) {
            state
                .audit("auth.login.throttled")
                .actor_label(credentials.username.clone())
                .ip(ip)
                .fire();
            return Err(ApiError::new(
                "Too many failed attempts. Please wait a few minutes and try again.",
            ));
        }
    }

    let username_for_audit = credentials.username.clone();
    let account: Option<Account> = state
        .database()
        .get("SELECT * FROM account WHERE name = ?", [credentials.username])
        .await?;

    // Mitigate timing attacks by always comparing password hashes regardless of whether it's found or not
    let hash = account
        .as_ref()
        .map(|a| &a.password)
        .unwrap_or(&state.incorrect_default_password_hash);

    let trusted = state.config().trusted_domain();
    let next = crate::utils::safe_next_for_domain(credentials.next.as_deref(), Some(&trusted));
    if validate_password(&credentials.password, hash).is_ok() {
        match account {
            Some(acc) => {
                state.invalidate_account_cache(acc.id);
                let key = &state.config().secret_key;

                // Second factor: if the account has verified TOTP, defer
                // session creation until the code is checked. We hand back a
                // short-lived signed "pending" cookie and bounce to /login/2fa.
                if acc.has_totp() {
                    let pending = PendingTotp {
                        account_id: acc.id,
                        remember: credentials.remember_me.is_some(),
                        description: credentials.session_description.clone(),
                        next: next.clone(),
                        exp: (OffsetDateTime::now_utc() + time::Duration::minutes(5)).unix_timestamp(),
                    };
                    let Ok(signed) = key.sign(&pending) else {
                        return Err(ApiError::new("could not start the 2FA challenge"));
                    };
                    state
                        .audit("auth.login.2fa_challenge")
                        .actor(&acc)
                        .ip_opt(client_ip)
                        .fire();
                    return Ok(pending_totp_response(signed));
                }

                let token = Token::new(acc.id)?;
                let domain = state.config().cookie_domain();
                let cookie = if credentials.remember_me.is_some() {
                    token.to_cookie(key, domain.as_deref())
                } else {
                    token.to_session_cookie(key, domain.as_deref())
                };
                state.save_session(&token, credentials.session_description).await;
                state.audit("auth.login.success").actor(&acc).ip_opt(client_ip).fire();
                Ok(cookie_redirect(cookie, next.as_deref().unwrap_or("/")))
            }
            None => {
                state
                    .audit("auth.login.fail")
                    .actor_label(username_for_audit)
                    .ip_opt(client_ip)
                    .meta(serde_json::json!({ "reason": "unknown_user" }))
                    .fire();
                register_failure(client_ip);
                Err(ApiError::incorrect_login())
            }
        }
    } else {
        state
            .audit("auth.login.fail")
            .actor_label(username_for_audit)
            .ip_opt(client_ip)
            .meta(serde_json::json!({ "reason": "bad_password" }))
            .fire();
        register_failure(client_ip);
        Err(ApiError::incorrect_login())
    }
}

pub async fn login_form(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    flasher: Flasher,
    Form(credentials): Form<Credentials>,
) -> Response {
    // Preserve the redirect target across a failed attempt so the user stays
    // in the "log in to continue" flow.
    let trusted = state.config().trusted_domain();
    let bail_url = match crate::utils::safe_next_for_domain(credentials.next.as_deref(), Some(&trusted)) {
        Some(n) => format!("/login?next={}", crate::utils::urlencode(&n)),
        None => "/login".to_string(),
    };
    match authenticate(&state, credentials, client_ip).await {
        Ok(r) => r,
        Err(e) => {
            let mut response = flasher.add(e.error.into_owned()).bail(&bail_url);
            response.extensions_mut().insert(BadRequestReason::IncorrectLogin);
            response
        }
    }
}

pub async fn logout(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Query(query): Query<NextQuery>,
    token: Token,
) -> Response {
    state.invalidate_account_cache(token.id);
    state.invalidate_session(&token.base64()).await;
    state
        .audit("auth.logout")
        .actor_label(format!("id:{}", token.id))
        .ip_opt(client_ip)
        .fire();
    let trusted = state.config().trusted_domain();
    let target =
        crate::utils::safe_next_for_domain(query.next.as_deref(), Some(&trusted)).unwrap_or_else(|| "/".to_string());
    let mut response = Redirect::to(&target).into_response();
    clear_session_cookies(&state, &mut response);
    response
}

/// Expire every cookie that identifies the user: the domain-scoped `token`, the
/// legacy host-only one, and the Percy dashboard's `session` cookie (single
/// sign-out across the `percy.` subdomain). Shared by logout and account deletion.
///
/// All three are appended, so they coexist on one response. See [`crate::cookies`].
pub(crate) fn clear_session_cookies(state: &AppState, response: &mut Response) {
    let mut builder = Cookie::build(("token", ""))
        .path("/")
        .expires(cookie::time::OffsetDateTime::UNIX_EPOCH);
    if let Some(d) = state.config().cookie_domain() {
        builder = builder.domain(d);
    }
    crate::cookies::set_cookie(response, builder.build());
    crate::cookies::set_raw_cookie(response, clear_host_only_cookie());
    if let Some(d) = state.config().cookie_domain() {
        let percy_session = Cookie::build(("session", ""))
            .path("/")
            .domain(d)
            .expires(cookie::time::OffsetDateTime::UNIX_EPOCH)
            .build();
        crate::cookies::set_cookie(response, percy_session);
    }
}

pub async fn logout_all(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
) -> TokenRejection {
    state.invalidate_account_cache(account.id);
    state.invalidate_account_sessions(account.id).await;
    state.audit("auth.logout_all").actor(&account).ip_opt(client_ip).fire();
    TokenRejection(state.config().cookie_domain())
}

// ─── Open signup ────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "auth/signup.html")]
struct SignupTemplate {
    account: Option<Account>,
    flashes: Flashes,
    /// Whether Discord OAuth is configured (controls showing the Discord signup button).
    discord_enabled: bool,
    /// Sanitized post-signup redirect path (empty = none), for the hidden form field.
    next: String,
    /// `?next=<encoded>` query suffix (empty = none) for the Discord / login links.
    next_query: String,
}

pub async fn signup_page(
    State(state): State<AppState>,
    account: Option<Account>,
    flashes: Flashes,
    Query(query): Query<NextQuery>,
) -> Response {
    let trusted = state.config().trusted_domain();
    let next = crate::utils::safe_next_for_domain(query.next.as_deref(), Some(&trusted));
    if account.is_some() {
        return Redirect::to(next.as_deref().unwrap_or("/")).into_response();
    }
    let next_query = next
        .as_deref()
        .map(|n| format!("?next={}", crate::utils::urlencode(n)))
        .unwrap_or_default();
    SignupTemplate {
        account,
        flashes,
        discord_enabled: state.config().discord.enabled(),
        next: next.unwrap_or_default(),
        next_query,
    }
    .into_response()
}

#[derive(Deserialize)]
pub struct SignupForm {
    username: String,
    password: String,
    #[serde(deserialize_with = "crate::utils::empty_string_is_none")]
    session_description: Option<String>,
    #[serde(default, deserialize_with = "crate::utils::empty_string_is_none")]
    next: Option<String>,
}

pub async fn signup_submit(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    flasher: Flasher,
    Form(form): Form<SignupForm>,
) -> Response {
    let trusted = state.config().trusted_domain();
    let next = crate::utils::safe_next_for_domain(form.next.as_deref(), Some(&trusted));
    let bail_url = match &next {
        Some(n) => format!("/signup?next={}", crate::utils::urlencode(n)),
        None => "/signup".to_string(),
    };

    if !is_valid_username(&form.username) {
        return flasher
            .add("Username must be 3-32 characters, lowercase letters, digits, dot, dash, or underscore.")
            .bail(&bail_url);
    }
    if !(8..=128).contains(&form.password.len()) {
        return flasher
            .add("Password length must be 8 to 128 characters")
            .bail(&bail_url);
    }

    let Ok(password_hash) = hash_password(&form.password) else {
        return flasher.add("Failed to hash password. Try again?").bail(&bail_url);
    };

    let username = form.username.clone();
    let tx_result: rusqlite::Result<i64> = state
        .database()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO account(name, password) VALUES (?, ?)",
                rusqlite::params![username, password_hash],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await;

    let new_account_id = match tx_result {
        Ok(id) => id,
        Err(e) => {
            let msg = e.to_string();
            let user_msg = if msg.contains("UNIQUE constraint failed: account.name") {
                "That username is already taken".to_string()
            } else {
                format!("Failed to create account: {msg}")
            };
            return flasher.add(user_msg).bail(&bail_url);
        }
    };

    // Auto-login: create session token + cookie (persistent for new signups)
    let Ok(token) = Token::new(new_account_id) else {
        return flasher.add("Account created — please log in").bail("/login");
    };
    let cfg = state.config();
    let cookie = token.to_cookie(&cfg.secret_key, cfg.cookie_domain().as_deref());
    state.save_session(&token, form.session_description).await;
    state
        .audit("auth.signup")
        .actor_label(form.username.clone())
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "new_account_id": new_account_id }))
        .fire();
    flasher.add(FlashMessage::success(format!("Welcome, {}!", form.username)));
    cookie_redirect(cookie, next.as_deref().unwrap_or("/"))
}

// ─── Two-factor login challenge ─────────────────────────────────────────────

/// Signed, short-lived payload bridging the password step and the TOTP step of
/// login. Stored in the `totp_pending` cookie; never written to the database.
#[derive(Serialize, Deserialize)]
struct PendingTotp {
    account_id: i64,
    remember: bool,
    description: Option<String>,
    /// Post-login redirect target carried from the password step.
    #[serde(default)]
    next: Option<String>,
    exp: i64,
}

const PENDING_COOKIE: &str = "totp_pending";

fn pending_totp_cookie(signed: String) -> Cookie<'static> {
    Cookie::build((PENDING_COOKIE, signed))
        .path("/")
        .http_only(true)
        .same_site(cookie::SameSite::Lax)
        .max_age(time::Duration::minutes(5))
        .build()
}

fn clear_pending_cookie() -> Cookie<'static> {
    Cookie::build((PENDING_COOKIE, ""))
        .path("/")
        .expires(cookie::time::OffsetDateTime::UNIX_EPOCH)
        .build()
}

/// 303 to /login/2fa carrying the pending-challenge cookie.
fn pending_totp_response(signed: String) -> Response {
    let mut resp = Redirect::to("/login/2fa").into_response();
    crate::cookies::set_cookie(&mut resp, pending_totp_cookie(signed));
    resp
}

/// Marks a matching unused recovery code as used. Returns true if one was
/// consumed. Recovery codes restore *access* — they are deliberately not
/// accepted anywhere that destroys state (see `delete::delete_account`).
pub(crate) async fn consume_recovery_code(state: &AppState, account_id: i64, code: &str) -> bool {
    let hash = crate::totp::hash_recovery_code(code);
    state
        .database()
        .execute(
            "UPDATE totp_recovery_code
                SET used_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
              WHERE account_id = ? AND code_hash = ? AND used_at IS NULL",
            (account_id, hash),
        )
        .await
        .unwrap_or(0)
        > 0
}

#[derive(Template)]
#[template(path = "auth/login_2fa.html")]
struct Login2faTemplate {
    account: Option<Account>,
    flashes: Flashes,
}

pub async fn login_totp_page(
    account: Option<Account>,
    flashes: Flashes,
    Extension(cookies): Extension<Vec<Cookie<'static>>>,
) -> Response {
    if account.is_some() {
        return Redirect::to("/").into_response();
    }
    // No pending challenge → nothing to verify; send back to login.
    if !cookies.iter().any(|c| c.name() == PENDING_COOKIE) {
        return Redirect::to("/login").into_response();
    }
    Login2faTemplate { account: None, flashes }.into_response()
}

#[derive(Deserialize)]
pub struct TotpCodeForm {
    pub code: String,
}

pub async fn login_totp_verify(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    flasher: Flasher,
    Extension(cookies): Extension<Vec<Cookie<'static>>>,
    Form(form): Form<TotpCodeForm>,
) -> Response {
    let key = state.config().secret_key;
    let Some(signed) = cookies
        .iter()
        .find(|c| c.name() == PENDING_COOKIE)
        .map(|c| c.value().to_string())
    else {
        return flasher
            .add("Your 2FA session expired. Please log in again.")
            .bail("/login");
    };
    let Some(pending) = key.verify::<PendingTotp>(&signed) else {
        return flasher
            .add("Your 2FA session was invalid. Please log in again.")
            .bail("/login");
    };
    if OffsetDateTime::now_utc().unix_timestamp() > pending.exp {
        return flasher
            .add("Your 2FA session expired. Please log in again.")
            .bail("/login");
    }

    let Some(acc) = load_full_account(&state, pending.account_id).await else {
        return flasher.add("Account not found.").bail("/login");
    };
    let secret = acc
        .totp_secret
        .as_deref()
        .and_then(|enc| crate::totp::decrypt_secret(&key, enc));
    let Some(secret) = secret else {
        return flasher
            .add("Two-factor is not configured for this account.")
            .bail("/login");
    };

    let code = form.code.trim();
    let ok = crate::totp::verify(&secret, code) || consume_recovery_code(&state, acc.id, code).await;
    if !ok {
        state.audit("auth.login.2fa_fail").actor(&acc).ip_opt(client_ip).fire();
        register_failure(client_ip);
        return flasher.add("Invalid code. Please try again.").bail("/login/2fa");
    }

    // Second factor cleared — issue the real session.
    let Ok(token) = Token::new(acc.id) else {
        return flasher.add("Could not create a session. Try again.").bail("/login");
    };
    let domain = state.config().cookie_domain();
    let session_cookie = if pending.remember {
        token.to_cookie(&key, domain.as_deref())
    } else {
        token.to_session_cookie(&key, domain.as_deref())
    };
    state.save_session(&token, pending.description.clone()).await;
    state
        .audit("auth.login.success")
        .actor(&acc)
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "second_factor": "totp" }))
        .fire();

    let trusted = state.config().trusted_domain();
    let target = crate::utils::safe_next_for_domain(pending.next.as_deref(), Some(&trusted));
    let mut resp = Redirect::to(target.as_deref().unwrap_or("/")).into_response();
    crate::cookies::set_cookie(&mut resp, session_cookie);
    crate::cookies::set_cookie(&mut resp, clear_pending_cookie());
    resp
}
