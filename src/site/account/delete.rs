//! The danger zone: exporting everything the site knows about you, and
//! permanently deleting the account.
//!
//! Deletion is immediate and hard — there is no soft-delete queue. The safety
//! net is the gauntlet in [`delete_account`]: a cookie session (never an API
//! key), the exact username typed back, the current password, a TOTP code when
//! 2FA is on (recovery codes are deliberately *not* accepted — they exist to
//! restore access, not to destroy it), a rate limit, and a refusal to delete the
//! last remaining admin.

use crate::{
    auth::validate_password,
    flash::{FlashMessage, Flasher},
    headers::ClientIp,
    models::{Account, Session},
    token::Token,
    AppState,
};
use axum::{
    extract::State,
    response::{IntoResponse, Redirect, Response},
    Form,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{auth::clear_session_cookies, is_last_admin, load_full_account, register_failure};

const DANGER_PAGE: &str = "/account/danger";

// ─── Data export ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ExportedAccount {
    id: i64,
    name: String,
    is_admin: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    two_factor_enabled: bool,
    discord_username: Option<String>,
}

#[derive(Serialize)]
struct ExportedImage {
    id: String,
    url: String,
    mimetype: String,
    size: i64,
    views: i64,
    original_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    uploaded_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct ExportedLink {
    code: String,
    url: String,
    target_url: String,
    clicks: i64,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

#[derive(Serialize)]
struct ExportedPaste {
    id: String,
    url: String,
    language: Option<String>,
    views: i64,
    size_bytes: i64,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
}

/// A login, by label and age only. The signed token is *never* exported — the
/// file is a download that could end up anywhere, and the token is a credential.
#[derive(Serialize)]
struct ExportedSession {
    description: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

#[derive(Serialize)]
struct Export {
    #[serde(with = "time::serde::rfc3339")]
    exported_at: OffsetDateTime,
    account: ExportedAccount,
    images: Vec<ExportedImage>,
    short_links: Vec<ExportedLink>,
    pastes: Vec<ExportedPaste>,
    sessions: Vec<ExportedSession>,
}

/// Raw row shapes. The public URL of each item is stitched on outside the DB
/// closure (it needs the config), so the queries hand back plain tuples.
type ImageRow = (
    String,
    String,
    i64,
    i64,
    Option<String>,
    OffsetDateTime,
    Option<OffsetDateTime>,
);
type LinkRow = (String, String, i64, OffsetDateTime);
type PasteRow = (String, Option<String>, i64, i64, OffsetDateTime, Option<OffsetDateTime>);

/// `GET /account/export` — everything this site stores about you, as JSON.
///
/// Paste bodies are not inlined: a paste can be megabytes, and its content is
/// one fetch away at the `url` in each entry. Password hashes, TOTP secrets and
/// session tokens are never included.
pub async fn export(State(state): State<AppState>, ClientIp(client_ip): ClientIp, account: Account) -> Response {
    let id = account.id;
    let config = state.config();

    let images = state
        .database()
        .call(move |conn| -> rusqlite::Result<Vec<ImageRow>> {
            let mut stmt = conn.prepare_cached(
                "SELECT id, mimetype, size, views, original_name, uploaded_at, expires_at
                   FROM images WHERE uploader_id = ? ORDER BY uploaded_at DESC",
            )?;
            let rows: rusqlite::Result<Vec<_>> = stmt
                .query_map([id], |row| {
                    Ok((
                        row.get("id")?,
                        row.get("mimetype")?,
                        row.get("size")?,
                        row.get("views")?,
                        row.get("original_name")?,
                        row.get("uploaded_at")?,
                        row.get("expires_at")?,
                    ))
                })?
                .collect();
            rows
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .map(
            |(id, mimetype, size, views, original_name, uploaded_at, expires_at)| ExportedImage {
                url: config.url_to(format!("/gallery/{id}")),
                id,
                mimetype,
                size,
                views,
                original_name,
                uploaded_at,
                expires_at,
            },
        )
        .collect();

    let short_links = state
        .database()
        .call(move |conn| -> rusqlite::Result<Vec<LinkRow>> {
            let mut stmt = conn.prepare_cached(
                "SELECT code, target_url, clicks, created_at FROM short_link
                  WHERE account_id = ? ORDER BY created_at DESC",
            )?;
            let rows: rusqlite::Result<Vec<_>> = stmt
                .query_map([id], |row| {
                    Ok((
                        row.get("code")?,
                        row.get("target_url")?,
                        row.get("clicks")?,
                        row.get("created_at")?,
                    ))
                })?
                .collect();
            rows
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(code, target_url, clicks, created_at)| ExportedLink {
            url: config.url_to(format!("/r/{code}")),
            code,
            target_url,
            clicks,
            created_at,
        })
        .collect();

    let pastes = state
        .database()
        .call(move |conn| -> rusqlite::Result<Vec<PasteRow>> {
            let mut stmt = conn.prepare_cached(
                "SELECT id, language, views, length(content) AS size_bytes, created_at, expires_at
                       FROM paste WHERE account_id = ? ORDER BY created_at DESC",
            )?;
            let rows: rusqlite::Result<Vec<_>> = stmt
                .query_map([id], |row| {
                    Ok((
                        row.get("id")?,
                        row.get("language")?,
                        row.get("views")?,
                        row.get("size_bytes")?,
                        row.get("created_at")?,
                        row.get("expires_at")?,
                    ))
                })?
                .collect();
            rows
        })
        .await
        .unwrap_or_default()
        .into_iter()
        .map(
            |(id, language, views, size_bytes, created_at, expires_at)| ExportedPaste {
                url: config.url_to(format!("/p/{id}")),
                id,
                language,
                views,
                size_bytes,
                created_at,
                expires_at,
            },
        )
        .collect();

    let sessions: Vec<Session> = state
        .database()
        .all("SELECT * FROM session WHERE account_id = ? AND api_key = 0", [id])
        .await
        .unwrap_or_default();

    let discord = super::discord_username(&state, id).await;
    let export = Export {
        exported_at: OffsetDateTime::now_utc(),
        account: ExportedAccount {
            id: account.id,
            name: account.name.clone(),
            is_admin: account.flags.is_admin(),
            created_at: account.created_at,
            two_factor_enabled: account.totp_enabled,
            discord_username: (!discord.is_empty()).then_some(discord),
        },
        images,
        short_links,
        pastes,
        sessions: sessions
            .into_iter()
            .map(|s| ExportedSession {
                description: s.description,
                created_at: s.created_at,
            })
            .collect(),
    };

    state
        .audit("auth.account.export")
        .actor(&account)
        .ip_opt(client_ip)
        .fire();

    let body = serde_json::to_vec_pretty(&export).unwrap_or_default();
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/json".to_string()),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"klappstuhl-{}-export.json\"", account.name),
            ),
        ],
        body,
    )
        .into_response()
}

// ─── Permanent deletion ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DeleteAccountForm {
    /// Must equal the account's username exactly — the type-to-confirm step.
    username: String,
    /// Current password. Passwordless (Discord-created) accounts must set one first.
    #[serde(default)]
    password: String,
    /// Current TOTP code; required when 2FA is enabled.
    #[serde(default)]
    code: String,
    /// Checkbox: when absent the images are orphaned instead of deleted
    /// (`images.uploader_id` is `ON DELETE SET NULL`).
    #[serde(default)]
    delete_images: Option<String>,
}

/// Records the refusal and sends the user back to the danger zone.
///
/// A failed deletion attempt is an authentication failure on the most dangerous
/// action the site has, so it feeds the firewall lockout counter exactly like a
/// bad login does.
async fn refuse(
    state: &AppState,
    account: &Account,
    flasher: &Flasher,
    client_ip: Option<std::net::IpAddr>,
    reason: &'static str,
    message: &str,
    lockout: bool,
) -> Response {
    state
        .audit("auth.account.delete_fail")
        .actor(account)
        .ip_opt(client_ip)
        .meta(serde_json::json!({ "reason": reason }))
        .fire();
    if lockout {
        register_failure(state, client_ip).await;
    }
    flasher.add(message).bail(DANGER_PAGE)
}

/// Removes the account row (and, when asked, its images) in one transaction.
///
/// Everything else the account owns is carried out by the schema's foreign
/// keys — sessions and API keys, recovery codes, the Discord link, short links,
/// pastes, SSH keys and tokens all `ON DELETE CASCADE`; `images.uploader_id`
/// and `audit_log.actor_id` are `ON DELETE SET NULL`, so kept images and the
/// audit trail survive without an owner. This relies on `PRAGMA foreign_keys`,
/// which the pool sets on every connection (see `core/database.rs`).
///
/// Returns the number of account rows deleted (0 if it was already gone).
fn purge_account(conn: &mut rusqlite::Connection, account_id: i64, delete_images: bool) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    if delete_images {
        tx.execute("DELETE FROM images WHERE uploader_id = ?", [account_id])?;
    }
    let rows = tx.execute("DELETE FROM account WHERE id = ?", [account_id])?;
    tx.commit()?;
    Ok(rows)
}

/// `POST /account/delete` — permanently deletes the account.
///
/// The `account` cascade in the schema does the heavy lifting: sessions (all
/// logins *and* API keys), recovery codes, the Discord link, short links,
/// pastes, and SSH keys/tokens all go with it. Audit rows and (optionally)
/// images survive with a NULL owner, so the site's history isn't rewritten by
/// someone leaving.
pub async fn delete_account(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    token: Token,
    account: Account,
    flasher: Flasher,
    Form(form): Form<DeleteAccountForm>,
) -> Response {
    // An API token must never be able to destroy the account. The `Account`
    // extractor already resolves cookie sessions only, but the session row is
    // the authority — read it back and insist it is a browser login.
    let session: Option<Session> = state
        .database()
        .get::<Session, _, _>(
            "SELECT * FROM session WHERE id = ? AND account_id = ?",
            (token.base64(), account.id),
        )
        .await
        .ok()
        .flatten();
    match session {
        Some(s) if !s.api_key => {}
        Some(_) => {
            return refuse(
                &state,
                &account,
                &flasher,
                client_ip,
                "api_key_session",
                "An API key cannot delete an account. Sign in through the website first.",
                false,
            )
            .await;
        }
        None => {
            return refuse(
                &state,
                &account,
                &flasher,
                client_ip,
                "no_session",
                "Your session could not be verified. Sign in again and retry.",
                false,
            )
            .await;
        }
    }

    if is_last_admin(&state, &account).await {
        return refuse(
            &state,
            &account,
            &flasher,
            client_ip,
            "last_admin",
            "This is the last admin account — promote another admin before deleting it.",
            false,
        )
        .await;
    }

    if form.username.trim() != account.name {
        return refuse(
            &state,
            &account,
            &flasher,
            client_ip,
            "username_mismatch",
            "The username you typed does not match this account.",
            false,
        )
        .await;
    }

    // Re-authentication. Discord-created accounts have no password to check
    // against, so they must set one first — the same gate 2FA enrollment uses.
    let Some(full) = load_full_account(&state, account.id).await else {
        return refuse(
            &state,
            &account,
            &flasher,
            client_ip,
            "account_missing",
            "This account could not be loaded. Try again.",
            false,
        )
        .await;
    };
    if !full.has_password() {
        return refuse(
            &state,
            &account,
            &flasher,
            client_ip,
            "no_password",
            "Set a password before deleting your account — it is how we confirm it's you.",
            false,
        )
        .await;
    }
    if validate_password(&form.password, &full.password).is_err() {
        return refuse(
            &state,
            &account,
            &flasher,
            client_ip,
            "bad_password",
            "Invalid password.",
            true,
        )
        .await;
    }

    // Second factor, when enabled. Recovery codes are not accepted here.
    if full.has_totp() {
        let key = state.config().secret_key;
        let secret = full
            .totp_secret
            .as_deref()
            .and_then(|enc| crate::totp::decrypt_secret(&key, enc));
        let valid = secret
            .map(|secret| crate::totp::verify(&secret, form.code.trim()))
            .unwrap_or(false);
        if !valid {
            return refuse(
                &state,
                &account,
                &flasher,
                client_ip,
                "bad_totp",
                "That two-factor code didn't match. Recovery codes cannot be used to delete an account.",
                true,
            )
            .await;
        }
    }

    // ── Point of no return ───────────────────────────────────────────────
    let delete_images = form.delete_images.is_some();
    let (image_count, _) = super::image_totals(&state, account.id).await;
    let account_id = account.id;

    let deleted: rusqlite::Result<usize> = state
        .database()
        .call(move |conn| purge_account(conn, account_id, delete_images))
        .await;

    if let Err(e) = deleted {
        tracing::error!(error = %e, account_id, "account deletion failed");
        return refuse(
            &state,
            &account,
            &flasher,
            client_ip,
            "sql_error",
            "Something went wrong deleting the account. Nothing was removed.",
            false,
        )
        .await;
    }

    state.invalidate_account_cache(account_id);
    state.invalidate_account_sessions(account_id).await;
    if delete_images {
        state.invalidate_image_caches().await;
    }

    // `actor(&account)` would write an actor_id that no longer resolves, so the
    // row is keyed by the username label instead — the audit trail outlives the
    // account on purpose.
    state
        .audit("auth.account.delete")
        .actor_label(account.name.clone())
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "account_id": account_id,
            "images": image_count,
            "images_deleted": delete_images,
        }))
        .fire();

    flasher.add(FlashMessage::success("Your account has been permanently deleted."));
    let mut response = Redirect::to("/").into_response();
    clear_session_cookies(&state, &mut response);
    response
}

/// Drives `delete_account` over HTTP against a real (in-memory) database.
///
/// Every test starts from [`guards::Request::valid`] — a request that satisfies
/// every guard and really does delete the account (`deleting_for_real_removes_the_account`
/// proves it). Each guard test then breaks exactly *one* precondition, so a
/// refusal can only be the guard under test: nothing else about the request changed.
#[cfg(test)]
mod guards {
    use crate::{
        auth::hash_password,
        models::Account,
        token::Token,
        totp::{current_code, encrypt_secret, generate_secret},
        AppState, Database,
    };
    use axum::{
        body::Body,
        http::{header::LOCATION, Request, StatusCode},
        routing::post,
        Extension, Router,
    };
    use tower::ServiceExt;

    const PASSWORD: &str = "correct-horse-battery";

    async fn test_state() -> AppState {
        // One connection: every `:memory:` connection is a separate database.
        let database = Database::file(":memory:")
            .connections(1)
            .with_init(crate::migrations::migrate)
            .open()
            .await
            .expect("open in-memory db");
        AppState::for_tests(database).await
    }

    /// The handler alone — the real route also carries a rate-limit layer, which
    /// is not what these tests are about.
    fn router(state: AppState) -> Router {
        let key = state.config().secret_key;
        // Mirrors main.rs: layers run bottom-to-top, so the secret key is in the
        // extensions before `parse_cookies` runs, and both are there for flash.
        Router::new()
            .route("/account/delete", post(super::delete_account))
            .with_state(state)
            .layer(axum::middleware::from_fn(crate::flash::process_flash_messages))
            .layer(axum::middleware::from_fn(crate::parse_cookies))
            .layer(Extension(key))
    }

    /// Inserts an account and returns it.
    async fn seed_account(state: &AppState, name: &str, admin: bool, totp_secret: Option<&[u8]>) -> Account {
        let password = hash_password(PASSWORD).expect("hash");
        let flags = i64::from(admin); // AccountFlags::ADMIN is bit 0
        let encrypted = totp_secret.map(|s| encrypt_secret(&state.config().secret_key, s).expect("encrypt totp"));
        let name = name.to_owned();
        state
            .database()
            .execute(
                "INSERT INTO account(name, password, flags, totp_secret, totp_enabled)
                 VALUES (?, ?, ?, ?, ?)",
                (name.clone(), password, flags, encrypted, totp_secret.is_some()),
            )
            .await
            .expect("insert account");
        state
            .database()
            .get::<Account, _, _>("SELECT * FROM account WHERE name = ?", [name])
            .await
            .expect("load account")
            .expect("account exists")
    }

    /// A browser login for `account`, returning the signed `token` cookie value.
    async fn login(state: &AppState, account: &Account) -> String {
        let token = Token::new(account.id).expect("token");
        state.save_session(&token, Some("laptop".to_owned())).await;
        token.to_cookie(&state.config().secret_key, None).value().to_owned()
    }

    /// The form fields of a deletion request.
    struct Params {
        username: String,
        password: String,
        code: String,
        delete_images: bool,
    }

    impl Params {
        /// Every guard satisfied.
        fn valid(account: &Account) -> Self {
            Self {
                username: account.name.clone(),
                password: PASSWORD.to_owned(),
                code: String::new(),
                delete_images: false,
            }
        }

        fn body(&self) -> String {
            let mut body = format!(
                "username={}&password={}&code={}",
                self.username, self.password, self.code
            );
            if self.delete_images {
                body.push_str("&delete_images=on");
            }
            body
        }
    }

    /// `POST /account/delete`, returning the `Location` it redirects to.
    async fn post_delete(state: &AppState, cookie: &str, params: &Params) -> String {
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/account/delete")
                    .header("Cookie", format!("token={cookie}"))
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(Body::from(params.body()))
                    .unwrap(),
            )
            .await
            .expect("handler ran");

        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "expected a redirect, got {}",
            response.status()
        );
        response
            .headers()
            .get(LOCATION)
            .expect("a Location header")
            .to_str()
            .expect("ascii location")
            .to_owned()
    }

    async fn account_exists(state: &AppState, id: i64) -> bool {
        state
            .database()
            .get::<Account, _, _>("SELECT * FROM account WHERE id = ?", [id])
            .await
            .expect("query")
            .is_some()
    }

    /// Asserts the request was refused and the account is still there.
    async fn assert_refused(state: &AppState, location: &str, id: i64) {
        assert_eq!(location, super::DANGER_PAGE, "expected a bounce to the danger zone");
        assert!(
            account_exists(state, id).await,
            "the account was deleted despite the refusal"
        );
    }

    // ── The baseline: a request that satisfies every guard really does delete ──

    #[tokio::test]
    async fn deleting_for_real_removes_the_account() {
        let state = test_state().await;
        let account = seed_account(&state, "victim", false, None).await;
        let cookie = login(&state, &account).await;

        let location = post_delete(&state, &cookie, &Params::valid(&account)).await;

        assert_eq!(location, "/", "a successful deletion goes home");
        assert!(!account_exists(&state, account.id).await, "the account survived");
    }

    /// The teardown sets three expiry cookies *and* flashes a farewell. Before
    /// kls-web-core v0.1.9 the flash layer's `insert` dropped all three, leaving
    /// the browser holding a session cookie for an account that no longer exists.
    #[tokio::test]
    async fn deletion_clears_the_session_cookies_alongside_the_flash() {
        let state = test_state().await;
        let account = seed_account(&state, "leaver", false, None).await;
        let cookie = login(&state, &account).await;

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/account/delete")
                    .header("Cookie", format!("token={cookie}"))
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(Body::from(Params::valid(&account).body()))
                    .unwrap(),
            )
            .await
            .expect("handler ran");

        let cookies: Vec<_> = response
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();

        assert!(
            cookies.iter().any(|c| c.starts_with("token=;")),
            "the session cookie was not torn down: {cookies:?}"
        );
        assert!(
            cookies.iter().any(|c| c.starts_with("flash_messages=")),
            "the farewell flash was lost: {cookies:?}"
        );
    }

    // ── One guard broken per test ────────────────────────────────────────────

    #[tokio::test]
    async fn the_last_admin_cannot_delete_itself() {
        let state = test_state().await;
        let account = seed_account(&state, "onlyadmin", true, None).await;
        let cookie = login(&state, &account).await;

        let location = post_delete(&state, &cookie, &Params::valid(&account)).await;

        assert_refused(&state, &location, account.id).await;
    }

    /// The same admin, once they are no longer the *last* one, may leave.
    #[tokio::test]
    async fn an_admin_with_a_peer_can_delete_itself() {
        let state = test_state().await;
        let account = seed_account(&state, "admin", true, None).await;
        seed_account(&state, "coadmin", true, None).await;
        let cookie = login(&state, &account).await;

        let location = post_delete(&state, &cookie, &Params::valid(&account)).await;

        assert_eq!(location, "/");
        assert!(!account_exists(&state, account.id).await);
    }

    #[tokio::test]
    async fn a_mistyped_username_is_refused() {
        let state = test_state().await;
        let account = seed_account(&state, "victim", false, None).await;
        let cookie = login(&state, &account).await;

        let mut params = Params::valid(&account);
        params.username = "victjm".to_owned();
        let location = post_delete(&state, &cookie, &params).await;

        assert_refused(&state, &location, account.id).await;
    }

    #[tokio::test]
    async fn a_wrong_password_is_refused() {
        let state = test_state().await;
        let account = seed_account(&state, "victim", false, None).await;
        let cookie = login(&state, &account).await;

        let mut params = Params::valid(&account);
        params.password = "hunter2".to_owned();
        let location = post_delete(&state, &cookie, &params).await;

        assert_refused(&state, &location, account.id).await;
    }

    #[tokio::test]
    async fn a_missing_totp_code_is_refused_when_2fa_is_on() {
        let state = test_state().await;
        let secret = generate_secret();
        let account = seed_account(&state, "careful", false, Some(&secret)).await;
        let cookie = login(&state, &account).await;

        // Password is right; the second factor is simply absent.
        let location = post_delete(&state, &cookie, &Params::valid(&account)).await;

        assert_refused(&state, &location, account.id).await;
    }

    #[tokio::test]
    async fn the_right_totp_code_lets_the_deletion_through() {
        let state = test_state().await;
        let secret = generate_secret();
        let account = seed_account(&state, "careful", false, Some(&secret)).await;
        let cookie = login(&state, &account).await;

        let mut params = Params::valid(&account);
        params.code = current_code(&secret);
        let location = post_delete(&state, &cookie, &params).await;

        assert_eq!(location, "/");
        assert!(!account_exists(&state, account.id).await);
    }

    /// Recovery codes restore *access*; they must never authorise destruction.
    /// A valid, unused recovery code is offered where the TOTP code goes and is
    /// rejected exactly like any other wrong code.
    #[tokio::test]
    async fn a_recovery_code_cannot_authorise_deletion() {
        let state = test_state().await;
        let secret = generate_secret();
        let account = seed_account(&state, "careful", false, Some(&secret)).await;
        let cookie = login(&state, &account).await;

        let recovery = crate::totp::generate_recovery_codes();
        let code = recovery.first().expect("a recovery code").clone();
        state
            .database()
            .execute(
                "INSERT INTO totp_recovery_code(account_id, code_hash) VALUES (?, ?)",
                (account.id, crate::totp::hash_recovery_code(&code)),
            )
            .await
            .expect("insert recovery code");

        let mut params = Params::valid(&account);
        params.code = code.clone();
        let location = post_delete(&state, &cookie, &params).await;

        assert_refused(&state, &location, account.id).await;

        // Without this the test proves nothing: it would pass just as happily if
        // we had offered a code that was never valid in the first place. The code
        // is still live and unused, so deletion *refused* it rather than failing
        // to recognise it.
        assert!(
            super::super::auth::consume_recovery_code(&state, account.id, &code).await,
            "the code offered was not a valid, unused recovery code — the refusal above was meaningless"
        );
    }

    /// An API key must never be able to destroy the account.
    ///
    /// Reaching this guard takes some doing, which is the point: the `Account`
    /// extractor asks `get_session_account(.., api_key = false)`, whose SQL
    /// filters API-key sessions out — but its cache branch returns whatever is
    /// cached for that session id *without* re-checking the flag, and the `/api`
    /// token path (`site::api::auth`) populates that cache with `api_key = true`
    /// entries. So we prime the cache the way a prior `/api` request would, which
    /// gets an API-key session past the extractor and onto the handler, where the
    /// handler's own re-read of the session row is the thing that stops it.
    #[tokio::test]
    async fn an_api_key_session_cannot_delete_the_account() {
        let state = test_state().await;
        let account = seed_account(&state, "victim", false, None).await;

        let mut token = Token::new(account.id).expect("token");
        token.api_key = true;
        state
            .database()
            .execute(
                "INSERT INTO session(id, account_id, description, api_key) VALUES (?, ?, 'API Key', 1)",
                (token.base64(), account.id),
            )
            .await
            .expect("insert api key session");
        state.is_session_valid(&token.base64()).await;

        let cookie = token.to_cookie(&state.config().secret_key, None).value().to_owned();
        let location = post_delete(&state, &cookie, &Params::valid(&account)).await;

        assert_refused(&state, &location, account.id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::purge_account;
    use crate::{database::Table, models::Account};

    /// A real, fully-migrated database — the cascade under test is defined by the
    /// schema, so testing it against anything less would test nothing.
    fn migrated_db() -> rusqlite::Connection {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
        conn.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        crate::migrations::migrate(&mut conn).expect("migrate");
        // The pool sets this on every connection; an in-memory one starts without it,
        // and without it the cascade silently does nothing.
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        conn
    }

    /// Seeds one account that owns one of everything, plus an audit row.
    fn seed_account(conn: &rusqlite::Connection, name: &str) -> i64 {
        conn.execute("INSERT INTO account(name, password) VALUES (?, 'hash')", [name])
            .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO session(id, account_id, description, api_key) VALUES ('sess', ?, 'laptop', 0)",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session(id, account_id, description, api_key) VALUES ('key', ?, 'api', 1)",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO totp_recovery_code(account_id, code_hash) VALUES (?, 'abc')",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO user_discord_links(account_id, discord_user_id) VALUES (?, '123')",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO short_link(code, target_url, account_id) VALUES ('abc', 'https://example.com', ?)",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO paste(id, account_id, content) VALUES ('p1', ?, 'hello')",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO images(id, image_data, mimetype, uploader_id) VALUES ('i1', x'00', 'image/png', ?)",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO audit_log(actor_id, actor_label, action) VALUES (?, ?, 'auth.login.success')",
            rusqlite::params![id, name],
        )
        .unwrap();
        id
    }

    fn count(conn: &rusqlite::Connection, sql: &str, id: i64) -> i64 {
        conn.query_row(sql, [id], |row| row.get(0)).unwrap()
    }

    #[test]
    fn deleting_an_account_cascades_to_everything_it_owns() {
        let mut conn = migrated_db();
        let id = seed_account(&conn, "victim");

        assert_eq!(purge_account(&mut conn, id, false).unwrap(), 1);

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM account WHERE id = ?", id), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM session WHERE account_id = ?", id), 0);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM totp_recovery_code WHERE account_id = ?",
                id
            ),
            0
        );
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM user_discord_links WHERE account_id = ?",
                id
            ),
            0
        );
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM short_link WHERE account_id = ?", id),
            0
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM paste WHERE account_id = ?", id), 0);
    }

    #[test]
    fn kept_images_and_audit_rows_survive_with_a_null_owner() {
        let mut conn = migrated_db();
        let id = seed_account(&conn, "leaver");

        purge_account(&mut conn, id, false).unwrap();

        // The image is still served; it just has no uploader any more.
        let orphans: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM images WHERE id = 'i1' AND uploader_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 1);

        // The audit trail outlives the account: the row stays, keyed by the label.
        let audit: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE actor_label = 'leaver' AND actor_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(audit, 1);
    }

    #[test]
    fn opting_in_removes_the_images_too() {
        let mut conn = migrated_db();
        let id = seed_account(&conn, "thorough");

        purge_account(&mut conn, id, true).unwrap();

        let images: i64 = conn
            .query_row("SELECT COUNT(*) FROM images", [], |row| row.get(0))
            .unwrap();
        assert_eq!(images, 0);
    }

    #[test]
    fn other_accounts_are_untouched() {
        let mut conn = migrated_db();
        let victim = seed_account(&conn, "victim");
        conn.execute("INSERT INTO account(name, password) VALUES ('bystander', 'hash')", [])
            .unwrap();
        let bystander = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO short_link(code, target_url, account_id) VALUES ('xyz', 'https://example.com', ?)",
            [bystander],
        )
        .unwrap();

        purge_account(&mut conn, victim, true).unwrap();

        assert_eq!(count(&conn, "SELECT COUNT(*) FROM account WHERE id = ?", bystander), 1);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM short_link WHERE account_id = ?", bystander),
            1
        );
    }

    /// `Account::from_row` gained `created_at`; a plain `SELECT *` must populate it
    /// (the JOIN-based loaders alias the session's own timestamp apart — see state.rs).
    #[test]
    fn account_from_row_reads_created_at() {
        let conn = migrated_db();
        seed_account(&conn, "dated");

        let account = conn
            .query_row("SELECT * FROM account WHERE name = 'dated'", [], Account::from_row)
            .unwrap();

        assert_eq!(account.name, "dated");
        assert!(
            account.created_at > time::OffsetDateTime::UNIX_EPOCH,
            "created_at fell back to the epoch, so the column wasn't read"
        );
    }
}
