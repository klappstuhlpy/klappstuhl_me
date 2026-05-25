use crate::{
    auth::{hash_password, validate_password},
    error::ApiError,
    filters,
    flash::{FlashMessage, Flasher, Flashes},
    headers::Referrer,
    key::SecretKey,
    logging::BadRequestReason,
    models::{is_valid_username, Account, ImageEntry, Invite, Session},
    ratelimit::RateLimit,
    token::{Token, TokenRejection},
    AppState,
};
use time::OffsetDateTime;
use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::{header::SET_COOKIE, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Json, Router,
};
use cookie::Cookie;
use serde::{Deserialize, Serialize};

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    account: Option<Account>,
    flashes: Flashes,
}

async fn login(account: Option<Account>, flashes: Flashes) -> Response {
    if account.is_some() {
        Redirect::to("/").into_response()
    } else {
        LoginTemplate { account, flashes }.into_response()
    }
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
}

fn cookie_to_response(cookie: Cookie<'static>) -> Response {
    let mut response = Redirect::to("/").into_response();
    response
        .headers_mut()
        .insert(SET_COOKIE, HeaderValue::from_str(&cookie.to_string()).unwrap());
    response
}

async fn authenticate(state: &AppState, credentials: Credentials) -> Result<Response, ApiError> {
    if !is_valid_username(&credentials.username) {
        return Err(ApiError::new("invalid username given"));
    }

    if !((8..=128).contains(&credentials.password.len())) {
        return Err(ApiError::new("password length must be 8 to 128 characters"));
    }

    let account: Option<Account> = state
        .database()
        .get("SELECT * FROM account WHERE name = ?", [credentials.username])
        .await?;

    // Mitigate timing attacks by always comparing password hashes regardless of whether it's found or not
    let hash = account
        .as_ref()
        .map(|a| &a.password)
        .unwrap_or(&state.incorrect_default_password_hash);

    if validate_password(&credentials.password, hash).is_ok() {
        match account {
            Some(acc) => {
                state.invalidate_account_cache(acc.id);
                let token = Token::new(acc.id)?;
                let key = &state.config().secret_key;
                let cookie = if credentials.remember_me.is_some() {
                    token.to_cookie(key)
                } else {
                    token.to_session_cookie(key)
                };
                state.save_session(&token, credentials.session_description).await;
                Ok(cookie_to_response(cookie))
            }
            None => Err(ApiError::incorrect_login()),
        }
    } else {
        Err(ApiError::incorrect_login())
    }
}

async fn logout(State(state): State<AppState>, token: Token) -> TokenRejection {
    state.invalidate_account_cache(token.id);
    state.invalidate_session(&token.base64()).await;
    TokenRejection
}

async fn logout_all(State(state): State<AppState>, account: Account) -> TokenRejection {
    state.invalidate_account_cache(account.id);
    state.invalidate_account_sessions(account.id).await;
    TokenRejection
}

#[derive(Deserialize)]
struct InvalidateSessionPayload {
    session_id: String,
}

async fn invalidate_session(
    State(state): State<AppState>,
    account: Account,
    Json(payload): Json<InvalidateSessionPayload>,
) -> StatusCode {
    let key = state.config().secret_key;
    match Token::from_signed(&payload.session_id, &key) {
        Some(token) => {
            if token.id == account.id {
                state.invalidate_session(&token.base64()).await;
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
    old_password: String,
    new_password: String,
    #[serde(deserialize_with = "crate::utils::empty_string_is_none")]
    session_description: Option<String>,
}

async fn change_password(
    State(state): State<AppState>,
    referrer: Option<Referrer>,
    token: Token,
    flasher: Flasher,
    Form(form): Form<ChangePasswordForm>,
) -> Response {
    let url = referrer.map(|r| r.0).unwrap_or_else(|| "/account".to_string());
    if !((8..=128).contains(&form.new_password.len())) {
        return flasher.add("Password length must be 8 to 128 characters").bail(&url);
    }

    let result = state
        .database()
        .get::<Account, _, _>("SELECT * FROM account WHERE id = ?", [token.id])
        .await;

    let account = match result {
        Ok(Some(account)) => account,
        Ok(None) => {
            flasher.add("Somehow, this account does not exist.");
            return TokenRejection.into_response();
        }
        Err(e) => {
            return flasher.add(format!("SQL error: {e}")).bail(&url);
        }
    };

    if validate_password(&form.old_password, &account.password).is_err() {
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
            let cookie = token.to_cookie(&state.config().secret_key);
            state.invalidate_account_sessions(account.id).await;
            state.save_session(&token, form.session_description).await;
            flasher.add(FlashMessage::success("Successfully changed password."));
            cookie_to_response(cookie)
        }
        Err(e) => flasher.add(format!("SQL error: {e}")).bail(&url),
    }
}

async fn login_form(
    State(state): State<AppState>,
    flasher: Flasher,
    Form(credentials): Form<Credentials>,
) -> Response {
    match authenticate(&state, credentials).await {
        Ok(r) => r,
        Err(e) => {
            let mut response = flasher.add(e.error.into_owned()).bail("/login");
            response.extensions_mut().insert(BadRequestReason::IncorrectLogin);
            response
        }
    }
}

// ─── Invite-only signup ─────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "signup.html")]
struct SignupTemplate {
    account: Option<Account>,
    flashes: Flashes,
    /// The code from the URL query string (or empty if none was provided).
    prefilled_code: String,
    /// The invite's note shown above the form when a valid code is supplied.
    invite_label: Option<String>,
    /// An inline error displayed on this render (not via flash cookie).
    /// Used when the user lands on /signup?code=… with an invalid code.
    error: Option<&'static str>,
}

#[derive(Deserialize)]
struct SignupQuery {
    #[serde(default)]
    code: Option<String>,
}

async fn signup_page(
    State(state): State<AppState>,
    account: Option<Account>,
    flashes: Flashes,
    Query(query): Query<SignupQuery>,
) -> Response {
    if account.is_some() {
        return Redirect::to("/").into_response();
    }

    let mut prefilled_code = String::new();
    let mut invite_label: Option<String> = None;
    let mut error: Option<&'static str> = None;

    if let Some(code) = query.code.filter(|c| !c.is_empty()) {
        match state
            .database()
            .get::<Invite, _, _>("SELECT * FROM invite WHERE code = ?", [code.clone()])
            .await
        {
            Ok(Some(invite)) if invite.is_redeemable() => {
                invite_label = invite.note.clone();
                prefilled_code = code;
            }
            Ok(Some(invite)) if invite.is_used() => {
                error = Some("This invite has already been used.");
            }
            Ok(Some(_)) => {
                error = Some("This invite has expired.");
            }
            Ok(None) => {
                error = Some("Invite code not recognised.");
            }
            Err(_) => {
                error = Some("Failed to look up invite. Please try again.");
            }
        }
    }

    SignupTemplate {
        account,
        flashes,
        prefilled_code,
        invite_label,
        error,
    }
    .into_response()
}

#[derive(Deserialize)]
struct SignupForm {
    code: String,
    username: String,
    password: String,
    #[serde(deserialize_with = "crate::utils::empty_string_is_none")]
    session_description: Option<String>,
}

async fn signup_submit(
    State(state): State<AppState>,
    flasher: Flasher,
    Form(form): Form<SignupForm>,
) -> Response {
    let bail_url = format!("/signup?code={}", form.code);

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

    // Look up + validate the invite
    let invite: Invite = match state
        .database()
        .get::<Invite, _, _>("SELECT * FROM invite WHERE code = ?", [form.code.clone()])
        .await
    {
        Ok(Some(inv)) => inv,
        Ok(None) => return flasher.add("Invalid invite code").bail("/signup"),
        Err(e) => return flasher.add(format!("Database error: {e}")).bail(&bail_url),
    };

    if invite.is_used() {
        return flasher.add("This invite has already been used").bail("/signup");
    }
    if invite.is_expired() {
        return flasher.add("This invite has expired").bail("/signup");
    }

    let Ok(password_hash) = hash_password(&form.password) else {
        return flasher
            .add("Failed to hash password. Try again?")
            .bail(&bail_url);
    };

    let code = form.code.clone();
    let username = form.username.clone();
    let now = OffsetDateTime::now_utc();

    // Atomic: create account AND consume the invite (or do neither).
    // The UPDATE's `WHERE used_at IS NULL` guard prevents a race where two
    // concurrent signups redeem the same invite — the second one finds
    // 0 rows affected and we roll back its account insert.
    let tx_result: rusqlite::Result<i64> = state
        .database()
        .call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO account(name, password) VALUES (?, ?)",
                rusqlite::params![username, password_hash],
            )?;
            let new_id: i64 = tx.last_insert_rowid();
            let rows = tx.execute(
                "UPDATE invite SET used_at = ?, used_by = ? WHERE code = ? AND used_at IS NULL",
                rusqlite::params![now, new_id, code],
            )?;
            if rows == 0 {
                // Invite was claimed by someone else between our checks. Roll back.
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                    Some("invite already redeemed".to_string()),
                ));
            }
            tx.commit()?;
            Ok(new_id)
        })
        .await;

    let new_account_id = match tx_result {
        Ok(id) => id,
        Err(e) => {
            let msg = e.to_string();
            let user_msg = if msg.contains("UNIQUE constraint failed: account.name") {
                "That username is already taken".to_string()
            } else if msg.contains("invite already redeemed") {
                "This invite was just used by someone else.".to_string()
            } else {
                format!("Failed to create account: {msg}")
            };
            return flasher.add(user_msg).bail(&bail_url);
        }
    };

    // Auto-login: create session token + cookie (persistent for new signups)
    let Ok(token) = Token::new(new_account_id) else {
        return flasher
            .add("Account created — please log in")
            .bail("/login");
    };
    let key = &state.config().secret_key;
    let cookie = token.to_cookie(key);
    state.save_session(&token, form.session_description).await;
    flasher.add(FlashMessage::success(format!(
        "Welcome, {}!",
        form.username
    )));
    cookie_to_response(cookie)
}

#[derive(Template)]
#[template(path = "account.html")]
struct AccountInfoTemplate {
    account: Option<Account>,
    user: Account,
    entries: Vec<ImageEntry>,
    sessions: Vec<Session>,
    current_session: Option<Session>,
    api_key: Option<String>,
    key: SecretKey,
}

impl AccountInfoTemplate {
    async fn new(account: Account, user: Account, current_token: Token, state: &AppState) -> Self {
        let entries = state.resolve_images()
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

        let session_id = current_token.base64();
        let current_session = sessions
            .iter()
            .position(|s| s.id == session_id)
            .map(|idx| sessions.swap_remove(idx));

        let api_key = sessions
            .iter()
            .position(|s| s.api_key)
            .map(|idx| sessions.swap_remove(idx).id);

        sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        let key = state.config().secret_key;

        Self {
            account: Some(account),
            user,
            entries,
            sessions,
            current_session,
            api_key,
            key,
        }
    }
}

async fn account_info(
    State(state): State<AppState>,
    token: Token,
    account: Account,
) -> impl IntoResponse {
    AccountInfoTemplate::new(account.clone(), account, token, &state).await
}

async fn show_other_account_info(
    State(state): State<AppState>,
    token: Token,
    account: Account,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, Redirect> {
    let user = state.database().get::<Account, _, _>(
        "SELECT * FROM account WHERE name = ?", [name]
    ).await.ok().flatten().ok_or_else(|| Redirect::to("/"))?;

    Ok(AccountInfoTemplate::new(account, user, token, &state).await)
}

#[derive(Deserialize)]
struct GenerateApiKey {
    new: bool,
}

#[derive(Serialize)]
struct GeneratedApiKey {
    token: String,
}

async fn generate_api_key(
    State(state): State<AppState>,
    account: Account,
    Json(payload): Json<GenerateApiKey>,
) -> Result<Json<GeneratedApiKey>, ApiError> {
    if !payload.new {
        state.invalidate_api_keys(account.id).await;
    }
    let token = state.generate_api_key(account.id).await?;
    Ok(Json(GeneratedApiKey { token }))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/account/authenticate",
            post(login_form).layer(RateLimit::default().quota(10, 60.0).build()),
        )
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
        .route("/user/:name", get(show_other_account_info))
        .route(
            "/signup",
            get(signup_page).post(signup_submit).layer(
                RateLimit::default().quota(5, 60.0).build(),
            ),
        )
}