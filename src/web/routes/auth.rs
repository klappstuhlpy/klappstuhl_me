use crate::{
    auth::{hash_password, validate_password},
    error::ApiError,
    filters,
    flash::{FlashMessage, Flasher, Flashes},
    headers::{ClientIp, Referrer},
    key::SecretKey,
    logging::BadRequestReason,
    models::{is_valid_username, Account, ImageEntry, Scope, Session},
    ratelimit::RateLimit,
    token::{Token, TokenRejection},
    AppState,
};
use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::{header::SET_COOKIE, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Extension, Form, Json, Router,
};
use cookie::Cookie;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

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
struct NextQuery {
    #[serde(default)]
    next: Option<String>,
}

async fn login(
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
struct Credentials {
    username: String,
    password: String,
    #[serde(deserialize_with = "crate::utils::empty_string_is_none")]
    session_description: Option<String>,
    /// Present (with any value) when the "Eingeloggt bleiben" checkbox is ticked.
    /// Absent when unchecked – HTML form checkboxes omit the field entirely.
    #[serde(default)]
    remember_me: Option<String>,
    /// Post-login redirect target carried from the login page (validated server-side).
    #[serde(default, deserialize_with = "crate::utils::empty_string_is_none")]
    next: Option<String>,
}

/// Set the session cookie and redirect to `target` (an already-validated path).
/// Also clears any legacy host-only cookie (no Domain attribute) so the browser
/// stops sending it alongside the new domain-scoped one.
fn cookie_redirect(cookie: Cookie<'static>, target: &str) -> Response {
    let mut response = Redirect::to(target).into_response();
    response
        .headers_mut()
        .insert(SET_COOKIE, HeaderValue::from_str(&cookie.to_string()).unwrap());
    response
}

/// Returns a Set-Cookie header that expires the host-only `token` cookie (no
/// Domain attribute). This removes stale cookies from before domain-scoping was
/// introduced, preventing split-brain where the browser sends a host-only cookie
/// to the apex but not to subdomains.
pub(super) fn clear_host_only_cookie() -> HeaderValue {
    HeaderValue::from_static("token=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax")
}

fn cookie_to_response(cookie: Cookie<'static>) -> Response {
    cookie_redirect(cookie, "/")
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
                register_failure(&state, client_ip).await;
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
        register_failure(&state, client_ip).await;
        Err(ApiError::incorrect_login())
    }
}

/// Forward the failed-login to the firewall lockout counter.  Best-effort
/// — never blocks the login response and errors are swallowed (the audit
/// log row above is the source of truth for an incident review).
async fn register_failure(state: &AppState, ip: Option<std::net::IpAddr>) {
    if let Some(ip) = ip {
        let ip_str = ip.to_string();
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::firewall::lockout::register_failure(&state, &ip_str).await {
                tracing::warn!(error = %e, ip = %ip_str, "firewall lockout registration failed");
            }
        });
    }
}

async fn logout(
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
    let target = crate::utils::safe_next_for_domain(query.next.as_deref(), Some(&trusted)).unwrap_or_else(|| "/".to_string());
    let mut builder = Cookie::build(("token", ""))
        .path("/")
        .expires(cookie::time::OffsetDateTime::UNIX_EPOCH);
    if let Some(d) = state.config().cookie_domain() {
        builder = builder.domain(d);
    }
    let cookie = builder.build().to_string();
    let mut response = Redirect::to(&target).into_response();
    let headers = response.headers_mut();
    headers.append(SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    headers.append(SET_COOKIE, clear_host_only_cookie());
    response
}

async fn logout_all(State(state): State<AppState>, ClientIp(client_ip): ClientIp, account: Account) -> TokenRejection {
    state.invalidate_account_cache(account.id);
    state.invalidate_account_sessions(account.id).await;
    state.audit("auth.logout_all").actor(&account).ip_opt(client_ip).fire();
    TokenRejection(state.config().cookie_domain())
}

#[derive(Deserialize)]
struct InvalidateSessionPayload {
    session_id: String,
}

async fn invalidate_session(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Json(payload): Json<InvalidateSessionPayload>,
) -> StatusCode {
    let key = state.config().secret_key;
    match Token::from_signed(&payload.session_id, &key) {
        Some(token) => {
            if token.id == account.id {
                state.invalidate_session(&token.base64()).await;
                state
                    .audit("auth.session.invalidate")
                    .actor(&account)
                    .target(token.base64())
                    .ip_opt(client_ip)
                    .fire();
                StatusCode::NO_CONTENT
            } else {
                StatusCode::NOT_FOUND
            }
        }
        None => StatusCode::NOT_FOUND,
    }
}

#[derive(Deserialize)]
struct ChangePasswordForm {
    /// Absent / empty for accounts that have no password yet (Discord signups).
    #[serde(default)]
    old_password: String,
    new_password: String,
    /// Must match `new_password`; guards against typos in the new password.
    #[serde(default)]
    confirm_password: String,
    #[serde(deserialize_with = "crate::utils::empty_string_is_none")]
    session_description: Option<String>,
}

async fn change_password(
    State(state): State<AppState>,
    referrer: Option<Referrer>,
    ClientIp(client_ip): ClientIp,
    token: Token,
    flasher: Flasher,
    Form(form): Form<ChangePasswordForm>,
) -> Response {
    let url = referrer.map(|r| r.0).unwrap_or_else(|| "/account".to_string());
    if !((8..=128).contains(&form.new_password.len())) {
        return flasher.add("Password length must be 8 to 128 characters").bail(&url);
    }
    if form.new_password != form.confirm_password {
        return flasher.add("The two passwords do not match").bail(&url);
    }

    let result = state
        .database()
        .get::<Account, _, _>("SELECT * FROM account WHERE id = ?", [token.id])
        .await;

    let account = match result {
        Ok(Some(account)) => account,
        Ok(None) => {
            flasher.add("Somehow, this account does not exist.");
            return TokenRejection(state.config().cookie_domain()).into_response();
        }
        Err(e) => {
            return flasher.add(format!("SQL error: {e}")).bail(&url);
        }
    };

    // Accounts created via Discord have no password to verify — for them this is
    // setting a first password, so only require the current one when it exists.
    if account.has_password() && validate_password(&form.old_password, &account.password).is_err() {
        return flasher.add("Invalid password").bail(&url);
    }

    let Ok(changed_hash) = hash_password(&form.new_password) else {
        return flasher
            .add("Failed to hash password somehow. Try again later?")
            .bail(&url);
    };

    match state
        .database()
        .execute(
            "UPDATE account SET password = ? WHERE id = ?",
            (changed_hash, account.id),
        )
        .await
    {
        Ok(_) => {
            let Ok(token) = Token::new(account.id) else {
                return flasher.add("Failed to obtain new token cookie").bail(&url);
            };
            let cfg = state.config();
            let cookie = token.to_cookie(&cfg.secret_key, cfg.cookie_domain().as_deref());
            state.invalidate_account_sessions(account.id).await;
            state.save_session(&token, form.session_description).await;
            state
                .audit("auth.password.change")
                .actor(&account)
                .ip_opt(client_ip)
                .fire();
            flasher.add(FlashMessage::success("Successfully changed password."));
            cookie_to_response(cookie)
        }
        Err(e) => flasher.add(format!("SQL error: {e}")).bail(&url),
    }
}

async fn login_form(
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

async fn signup_page(
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
struct SignupForm {
    username: String,
    password: String,
    #[serde(deserialize_with = "crate::utils::empty_string_is_none")]
    session_description: Option<String>,
    #[serde(default, deserialize_with = "crate::utils::empty_string_is_none")]
    next: Option<String>,
}

async fn signup_submit(
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

#[derive(Template)]
#[template(path = "auth/account.html")]
struct AccountInfoTemplate {
    account: Option<Account>,
    user: Account,
    entries: Vec<ImageEntry>,
    sessions: Vec<Session>,
    current_session: Option<Session>,
    api_key: Option<String>,
    /// Comma-separated string of scopes attached to the current API key
    /// (empty for legacy / unscoped keys). Used to pre-check the boxes
    /// in the scopes selection UI.
    api_key_scopes: String,
    /// Whether this account has TOTP 2FA enabled (drives the account-page UI).
    totp_enabled: bool,
    key: SecretKey,
    /// Discord username if a Discord account is linked (empty string = not linked).
    discord_username: String,
    /// Whether Discord OAuth is configured (controls showing the link button).
    discord_enabled: bool,
    /// Whether the account has a real password set. Discord-created accounts don't,
    /// so the change-password dialog hides the "current password" field and shows
    /// a hint that setting one is optional.
    has_password: bool,
}

impl AccountInfoTemplate {
    async fn new(account: Account, user: Account, current_token: Token, state: &AppState) -> Self {
        let user_id_for_discord = user.id;
        let entries = state
            .resolve_images()
            .await
            .iter()
            .filter(|e| e.uploader_id == Option::from(user.id))
            .cloned()
            .collect::<Vec<_>>();

        let mut sessions = if user.id == account.id {
            state
                .database()
                .all("SELECT * FROM session WHERE account_id = ?", [user.id])
                .await
                .unwrap_or_default()
        } else {
            Vec::<Session>::new()
        };

        let totp_enabled = user.totp_enabled;
        let session_id = current_token.base64();
        let current_session = sessions
            .iter()
            .position(|s| s.id == session_id)
            .map(|idx| sessions.swap_remove(idx));

        // Pull out the API-key row (if any) so we can show its scopes
        // separately from the regular browser sessions list.
        let api_key_session = sessions
            .iter()
            .position(|s| s.api_key)
            .map(|idx| sessions.swap_remove(idx));
        let api_key = api_key_session.as_ref().map(|s| s.id.clone());
        let api_key_scopes = api_key_session.as_ref().map(|s| s.scopes.clone()).unwrap_or_default();

        sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        let key = state.config().secret_key;

        let discord_username: String = state
            .database()
            .call(move |conn| {
                conn.prepare_cached("SELECT discord_username FROM user_discord_links WHERE account_id = ?")
                    .and_then(|mut stmt| stmt.query_row([user_id_for_discord], |row| row.get(0)))
            })
            .await
            .unwrap_or_default();
        let discord_enabled = state.config().discord.enabled();
        let has_password = user.has_password();

        Self {
            account: Some(account),
            user,
            entries,
            sessions,
            current_session,
            api_key,
            api_key_scopes,
            totp_enabled,
            key,
            discord_username,
            discord_enabled,
            has_password,
        }
    }
}

async fn account_info(State(state): State<AppState>, token: Token, account: Account) -> impl IntoResponse {
    AccountInfoTemplate::new(account.clone(), account, token, &state).await
}

async fn show_other_account_info(
    State(state): State<AppState>,
    token: Token,
    account: Account,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, Redirect> {
    let user = state
        .database()
        .get::<Account, _, _>("SELECT * FROM account WHERE name = ?", [name])
        .await
        .ok()
        .flatten()
        .ok_or_else(|| Redirect::to("/"))?;

    Ok(AccountInfoTemplate::new(account, user, token, &state).await)
}

#[derive(Deserialize)]
struct GenerateApiKey {
    new: bool,
    /// Scopes to grant the new token. String values matching `Scope::as_str`
    /// (e.g. "images:read"); unknown values are silently dropped.
    /// Empty array means "legacy / unrestricted" — same as a pre-scopes key.
    #[serde(default)]
    scopes: Vec<String>,
}

#[derive(Serialize)]
struct GeneratedApiKey {
    token: String,
}

async fn generate_api_key(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Json(payload): Json<GenerateApiKey>,
) -> Result<Json<GeneratedApiKey>, ApiError> {
    if !payload.new {
        state.invalidate_api_keys(account.id).await;
    }
    // Map the string list from the form into typed Scopes, dropping any
    // we don't recognise (a stale client shouldn't be able to inject a
    // spurious permission name into the DB).
    let scopes: Vec<Scope> = payload.scopes.iter().filter_map(|s| Scope::from_str(s)).collect();
    let scopes_str = scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",");
    let token = state.generate_api_key(account.id, &scopes).await?;
    state
        .audit("auth.api_key.generate")
        .actor(&account)
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "regenerated": !payload.new,
            "scopes": scopes_str,
        }))
        .fire();
    Ok(Json(GeneratedApiKey { token }))
}

// ─── Two-factor (TOTP) ──────────────────────────────────────────────────────

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
    resp.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&pending_totp_cookie(signed).to_string()).unwrap(),
    );
    resp
}

/// Loads an account by id with all columns (so `totp_secret` is populated even
/// if a cached/JOINed copy wouldn't be).
async fn load_full_account(state: &AppState, id: i64) -> Option<Account> {
    state
        .database()
        .get::<Account, _, _>("SELECT * FROM account WHERE id = ?", [id])
        .await
        .ok()
        .flatten()
}

/// Marks a matching unused recovery code as used. Returns true if one was
/// consumed.
async fn consume_recovery_code(state: &AppState, account_id: i64, code: &str) -> bool {
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

async fn login_totp_page(
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
struct TotpCodeForm {
    code: String,
}

async fn login_totp_verify(
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
        register_failure(&state, client_ip).await;
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
    let headers = resp.headers_mut();
    headers.append(SET_COOKIE, HeaderValue::from_str(&session_cookie.to_string()).unwrap());
    headers.append(
        SET_COOKIE,
        HeaderValue::from_str(&clear_pending_cookie().to_string()).unwrap(),
    );
    resp
}

#[derive(Template)]
#[template(path = "auth/account_2fa.html")]
struct Totp2faSetupTemplate {
    account: Option<Account>,
    qr_svg: String,
    secret_base32: String,
    recovery_codes: Vec<String>,
}

/// Begins enrollment: generates and stores a fresh (still-disabled) secret and
/// recovery codes, then shows the QR + codes. Enabling requires verifying a
/// code via [`totp_enable`], so a misconfigured authenticator can never lock
/// the user out.
async fn totp_setup(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    flasher: Flasher,
) -> Response {
    // 2FA only protects the password login — Discord sign-in deliberately skips
    // the TOTP challenge. With no password set, enabling it would guard nothing,
    // so require a password first.
    if !account.has_password() {
        return flasher
            .add("Set a password before enabling two-factor authentication.")
            .bail("/account");
    }

    let key = state.config().secret_key;
    let secret = crate::totp::generate_secret();
    let Ok(encrypted) = crate::totp::encrypt_secret(&key, &secret) else {
        return flasher.add("Could not generate a 2FA secret.").bail("/account");
    };

    if state
        .database()
        .execute(
            "UPDATE account SET totp_secret = ?, totp_enabled = 0 WHERE id = ?",
            (encrypted, account.id),
        )
        .await
        .is_err()
    {
        return flasher.add("Could not store the 2FA secret.").bail("/account");
    }

    // Fresh recovery codes (replace any prior set).
    let codes = crate::totp::generate_recovery_codes();
    let _ = state
        .database()
        .execute("DELETE FROM totp_recovery_code WHERE account_id = ?", [account.id])
        .await;
    for code in &codes {
        let _ = state
            .database()
            .execute(
                "INSERT INTO totp_recovery_code (account_id, code_hash) VALUES (?, ?)",
                (account.id, crate::totp::hash_recovery_code(code)),
            )
            .await;
    }
    state.invalidate_account_cache(account.id);
    state.audit("auth.2fa.setup").actor(&account).ip_opt(client_ip).fire();

    let uri = crate::totp::otpauth_uri(&secret, &account.name);
    let qr_svg = crate::totp::qr_svg(&uri).unwrap_or_default();
    Totp2faSetupTemplate {
        account: Some(account),
        qr_svg,
        secret_base32: crate::totp::base32_secret(&secret),
        recovery_codes: codes,
    }
    .into_response()
}

async fn totp_enable(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    flasher: Flasher,
    Form(form): Form<TotpCodeForm>,
) -> Response {
    let key = state.config().secret_key;
    let Some(acc) = load_full_account(&state, account.id).await else {
        return flasher.add("Account not found.").bail("/account");
    };
    let secret = acc
        .totp_secret
        .as_deref()
        .and_then(|enc| crate::totp::decrypt_secret(&key, enc));
    let Some(secret) = secret else {
        return flasher.add("Start 2FA setup first.").bail("/account");
    };
    if !crate::totp::verify(&secret, form.code.trim()) {
        return flasher.add("That code didn't match. Try again.").bail("/account");
    }
    if state
        .database()
        .execute("UPDATE account SET totp_enabled = 1 WHERE id = ?", [account.id])
        .await
        .is_err()
    {
        return flasher.add("Could not enable 2FA.").bail("/account");
    }
    state.invalidate_account_cache(account.id);
    state.audit("auth.2fa.enable").actor(&account).ip_opt(client_ip).fire();
    flasher
        .add(FlashMessage::success("Two-factor authentication is now enabled."))
        .bail("/account")
}

#[derive(Deserialize)]
struct TotpDisableForm {
    password: String,
}

async fn totp_disable(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    flasher: Flasher,
    Form(form): Form<TotpDisableForm>,
) -> Response {
    let Some(acc) = load_full_account(&state, account.id).await else {
        return flasher.add("Account not found.").bail("/account");
    };
    if validate_password(&form.password, &acc.password).is_err() {
        return flasher.add("Invalid password.").bail("/account");
    }
    let _ = state
        .database()
        .execute(
            "UPDATE account SET totp_secret = NULL, totp_enabled = 0 WHERE id = ?",
            [account.id],
        )
        .await;
    let _ = state
        .database()
        .execute("DELETE FROM totp_recovery_code WHERE account_id = ?", [account.id])
        .await;
    state.invalidate_account_cache(account.id);
    state.audit("auth.2fa.disable").actor(&account).ip_opt(client_ip).fire();
    flasher
        .add(FlashMessage::success("Two-factor authentication disabled."))
        .bail("/account")
}

/// Generates a [ShareX](https://getsharex.com) custom-uploader config
/// (`.sxcu`) pre-filled with the user's API key and this site's upload
/// endpoint. Importing it makes ShareX (and Flameshot/other tools that read
/// the same format) upload screenshots straight to the gallery, copying the
/// returned link to the clipboard.
async fn sharex_config(State(state): State<AppState>, account: Account) -> Response {
    let Some(api_key) = state.get_api_key(account.id).await else {
        return (
            StatusCode::BAD_REQUEST,
            "Generate an API key on your account page first, then download this again.",
        )
            .into_response();
    };

    let upload_url = state.config().url_to("/api/images/upload");
    let config = serde_json::json!({
        "Version": "15.0.0",
        "Name": "klappstuhl.me",
        "DestinationType": "ImageUploader, FileUploader",
        "RequestMethod": "POST",
        "RequestURL": upload_url,
        "Headers": { "Authorization": api_key },
        "Body": "MultipartFormData",
        "FileFormName": "file",
        // UploadResult.links is the array of canonical URLs; take the first.
        "URL": "{json:links[0]}",
        "ErrorMessage": "{json:error}"
    });
    let body = serde_json::to_vec_pretty(&config).unwrap_or_default();

    (
        [
            (axum::http::header::CONTENT_TYPE, "application/json".to_string()),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"klappstuhl.sxcu\"".to_string(),
            ),
        ],
        body,
    )
        .into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/account/authenticate",
            post(login_form).layer(RateLimit::default().quota(10, 60.0).build()),
        )
        .route(
            "/login/2fa",
            get(login_totp_page)
                .post(login_totp_verify)
                .layer(RateLimit::default().quota(10, 60.0).build()),
        )
        .route("/account/2fa/setup", post(totp_setup))
        .route("/account/2fa/enable", post(totp_enable))
        .route("/account/2fa/disable", post(totp_disable))
        .route("/login", get(login))
        .route("/logout", get(logout))
        .route("/logout/all", get(logout_all))
        .route("/account/invalidate", post(invalidate_session))
        .route("/account", get(account_info))
        .route(
            "/account/api_key",
            post(generate_api_key).layer(RateLimit::default().quota(1, 600.0).build()),
        )
        .route("/account/change_password", post(change_password))
        .route("/account/sharex.sxcu", get(sharex_config))
        .route("/user/:name", get(show_other_account_info))
        .route(
            "/signup",
            get(signup_page)
                .post(signup_submit)
                .layer(RateLimit::default().quota(5, 60.0).build()),
        )
}
