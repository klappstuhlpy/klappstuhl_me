//! Accounts: login / signup / 2FA, and the multi-page account shell at
//! `/account/*`.
//!
//! Split by concern:
//!
//! - [`auth`] — login, signup, logout, the TOTP login challenge.
//! - [`pages`] — the GET handlers + Askama structs behind the account shell
//!   (overview, profile, security, sessions, api, content, danger) and the
//!   read-only `/user/:name` page.
//! - [`insights`] — the admin-only site traffic overview (aggregates over
//!   `requests.db`).
//! - [`security`] — password changes, TOTP enrollment, recovery codes.
//! - [`username`] — renaming the account (and the live availability check that
//!   signup shares).
//! - [`api_keys`] — API-token generation and the ShareX uploader config.
//! - [`sessions`] — revoking and renaming sessions.
//! - [`delete`] — the data export and the permanent account-deletion flow.
//!
//! Everything an authenticated page needs to read about its own account lives
//! in the query helpers at the bottom of this file, so the page handlers stay
//! thin and `delete`'s export can reuse the exact same reads.

pub mod api_keys;
pub mod auth;
pub mod delete;
pub mod insights;
pub(crate) mod lockout;
pub mod pages;
pub mod security;
pub mod sessions;
pub mod username;

use crate::{models::Account, ratelimit::RateLimit, AppState};
use axum::{
    http::HeaderValue,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use cookie::Cookie;
use serde::Serialize;
use time::OffsetDateTime;

/// Set the session cookie and redirect to `target` (an already-validated path).
///
/// The cookie is *appended*, so it survives alongside the flash layer's own
/// `Set-Cookie` on handlers that do both (signup, change_password).
/// See [`crate::cookies`].
pub(crate) fn cookie_redirect(cookie: Cookie<'static>, target: &str) -> Response {
    let mut response = Redirect::to(target).into_response();
    crate::cookies::set_cookie(&mut response, cookie);
    response
}

/// Returns a Set-Cookie header that expires the host-only `token` cookie (no
/// Domain attribute). This removes stale cookies from before domain-scoping was
/// introduced, preventing split-brain where the browser sends a host-only cookie
/// to the apex but not to subdomains.
pub(crate) fn clear_host_only_cookie() -> HeaderValue {
    HeaderValue::from_static("token=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax")
}

/// Record a failed authentication attempt for the site's login soft-lockout.
///
/// In-process and best-effort (see [`lockout`]); a `None` IP (unknown client)
/// is ignored. This is the site's own auth hardening — it no longer reaches
/// into the admin firewall, so it stands whether or not the admin app is up.
pub(crate) fn register_failure(ip: Option<std::net::IpAddr>) {
    if let Some(ip) = ip {
        lockout::register_failure(ip);
    }
}

/// Loads an account by id with all columns (so `totp_secret` is populated even
/// if a cached/JOINed copy wouldn't be).
pub(crate) async fn load_full_account(state: &AppState, id: i64) -> Option<Account> {
    state
        .database()
        .get::<Account, _, _>("SELECT * FROM account WHERE id = ?", [id])
        .await
        .ok()
        .flatten()
}

// ─── Account-scoped reads ───────────────────────────────────────────────────

/// The linked Discord username, or an empty string when nothing is linked.
pub(crate) async fn discord_username(state: &AppState, account_id: i64) -> String {
    state
        .database()
        .call(move |conn| {
            conn.prepare_cached("SELECT discord_username FROM user_discord_links WHERE account_id = ?")
                .and_then(|mut stmt| stmt.query_row([account_id], |row| row.get(0)))
        })
        .await
        .unwrap_or_default()
}

/// How many uploaded images this account owns, and their total size in bytes.
pub(crate) async fn image_totals(state: &AppState, account_id: i64) -> (i64, i64) {
    state
        .database()
        .call(move |conn| {
            conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM images WHERE uploader_id = ?",
                [account_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        })
        .await
        .unwrap_or((0, 0))
}

/// A cheap `COUNT(*)` over one of the account-owned tables.
async fn count_owned(state: &AppState, sql: &'static str, account_id: i64) -> i64 {
    state
        .database()
        .call(move |conn| conn.query_row(sql, [account_id], |row| row.get(0)))
        .await
        .unwrap_or(0)
}

pub(crate) async fn short_link_count(state: &AppState, account_id: i64) -> i64 {
    count_owned(
        state,
        "SELECT COUNT(*) FROM short_link WHERE account_id = ?",
        account_id,
    )
    .await
}

pub(crate) async fn paste_count(state: &AppState, account_id: i64) -> i64 {
    count_owned(state, "SELECT COUNT(*) FROM paste WHERE account_id = ?", account_id).await
}

/// Browser logins on this account. API keys live in the same table but are not
/// a way to *sign in*, so they don't count as sessions anywhere in the UI.
pub(crate) async fn count_sessions(state: &AppState, account_id: i64) -> i64 {
    count_owned(
        state,
        "SELECT COUNT(*) FROM session WHERE account_id = ? AND api_key = 0",
        account_id,
    )
    .await
}

/// Whether this account is the only admin left. The instance would have no way
/// back in without one (`cargo run -- admin` exists, but deleting your way into
/// needing it is not an outcome the button should allow), so deletion refuses.
pub(crate) async fn is_last_admin(state: &AppState, account: &Account) -> bool {
    if !account.flags.is_admin() {
        return false;
    }
    let admins: i64 = state
        .database()
        .call(|conn| conn.query_row("SELECT COUNT(*) FROM account WHERE flags & 1 = 1", [], |row| row.get(0)))
        .await
        .unwrap_or(0);
    admins <= 1
}

/// Recovery codes that have not been spent yet. `0` when 2FA was never set up.
pub(crate) async fn unused_recovery_codes(state: &AppState, account_id: i64) -> i64 {
    count_owned(
        state,
        "SELECT COUNT(*) FROM totp_recovery_code WHERE account_id = ? AND used_at IS NULL",
        account_id,
    )
    .await
}

/// One audit row as shown on the account pages. A trimmed-down view of
/// [`crate::audit::AuditEntry`]: no ids or meta blob, just what the owner of the
/// account needs to recognise (or fail to recognise) an event.
#[derive(Debug, Clone, Serialize)]
pub struct ActivityEntry {
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub action: String,
    pub ip: Option<String>,
}

impl ActivityEntry {
    /// A human sentence for the action, so the page never shows a raw dotted key.
    pub fn label(&self) -> &'static str {
        match self.action.as_str() {
            "auth.login.success" => "Signed in",
            "auth.login.fail" => "Failed sign-in attempt",
            "auth.login.2fa_challenge" => "Two-factor challenge issued",
            "auth.login.2fa_fail" => "Failed two-factor code",
            "auth.discord.login" => "Signed in with Discord",
            "auth.discord.signup" => "Account created with Discord",
            "auth.discord.unlink" => "Discord account unlinked",
            "auth.signup" => "Account created",
            "auth.logout" => "Signed out",
            "auth.logout_all" => "Signed out of all sessions",
            "auth.password.change" => "Password changed",
            "auth.username.change" => "Username changed",
            "auth.username.change_fail" => "Failed username change",
            "auth.session.invalidate" => "Session revoked",
            "auth.session.rename" => "Session renamed",
            "auth.api_key.generate" => "API key generated",
            "auth.2fa.setup" => "Two-factor setup started",
            "auth.2fa.enable" => "Two-factor enabled",
            "auth.2fa.disable" => "Two-factor disabled",
            "auth.2fa.recovery_codes" => "Recovery codes regenerated",
            "auth.account.export" => "Data exported",
            "auth.account.delete_fail" => "Failed account-deletion attempt",
            _ => "Account activity",
        }
    }

    /// Failed attempts get flagged in the UI so they stand out from the routine rows.
    pub fn is_failure(&self) -> bool {
        self.action.ends_with("fail") || self.action.ends_with("_fail")
    }
}

/// Recent `auth.*` audit rows belonging to this account.
///
/// Matches on the account id *or* the username: failed logins are recorded
/// before a session exists, so they carry `actor_id = NULL` and only the
/// attempted username as a label — exactly the rows a user most needs to see.
/// `action_prefix` narrows the feed (e.g. `"auth.login."` for the login history).
pub(crate) async fn recent_activity(
    state: &AppState,
    account: &Account,
    action_prefix: &str,
    limit: i64,
) -> Vec<ActivityEntry> {
    let account_id = account.id;
    let name = account.name.clone();
    let prefix = format!("{action_prefix}%");
    state
        .database()
        .call(move |conn| -> rusqlite::Result<Vec<ActivityEntry>> {
            let mut stmt = conn.prepare_cached(
                "SELECT ts, action, ip FROM audit_log
                  WHERE (actor_id = ? OR (actor_id IS NULL AND actor_label = ?))
                    AND action LIKE ?
                  ORDER BY id DESC LIMIT ?",
            )?;
            let rows: rusqlite::Result<Vec<ActivityEntry>> = stmt
                .query_map(rusqlite::params![account_id, name, prefix, limit], |row| {
                    Ok(ActivityEntry {
                        ts: row.get("ts")?,
                        action: row.get("action")?,
                        ip: row.get("ip")?,
                    })
                })?
                .collect();
            rows
        })
        .await
        .unwrap_or_default()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        // ── Login / signup / logout ──────────────────────────────────────
        .route("/login", get(auth::login))
        .route(
            "/account/authenticate",
            post(auth::login_form).layer(RateLimit::default().quota(10, 60.0).build()),
        )
        .route(
            "/login/2fa",
            get(auth::login_totp_page)
                .post(auth::login_totp_verify)
                .layer(RateLimit::default().quota(10, 60.0).build()),
        )
        .route(
            "/signup",
            get(auth::signup_page)
                .post(auth::signup_submit)
                .layer(RateLimit::default().quota(5, 60.0).build()),
        )
        .route("/logout", get(auth::logout))
        .route("/logout/all", get(auth::logout_all))
        // ── The account shell ────────────────────────────────────────────
        .route("/account", get(pages::overview))
        .route("/account/profile", get(pages::profile))
        .route("/account/security", get(pages::security_page))
        .route("/account/sessions", get(pages::sessions_page))
        .route("/account/api", get(pages::api_page))
        .route("/account/content", get(pages::content))
        .route("/account/danger", get(pages::danger))
        // Admin-only; both handlers check the flag themselves (there is no
        // admin extractor — the shell gates in the sidebar, not the router).
        .route("/account/insights", get(insights::page))
        .route("/account/insights/data", get(insights::data))
        .route("/user/:name", get(pages::user_public))
        // ── Mutations (paths unchanged from the single-page account) ─────
        .route(
            "/account/change_password",
            post(security::change_password).layer(RateLimit::default().quota(5, 60.0).build()),
        )
        .route(
            "/account/username",
            post(username::change_username).layer(RateLimit::default().quota(5, 600.0).build()),
        )
        // Reachable logged-out: the signup form's live "is this name free?" hint
        // calls it too. Rate-limited accordingly — it is an existence oracle.
        .route(
            "/account/username/check",
            get(username::check).layer(RateLimit::default().quota(30, 60.0).build()),
        )
        .route("/account/2fa/setup", post(security::totp_setup))
        .route("/account/2fa/enable", post(security::totp_enable))
        .route("/account/2fa/disable", post(security::totp_disable))
        .route(
            "/account/2fa/recovery_codes",
            post(security::totp_recovery_codes).layer(RateLimit::default().quota(5, 600.0).build()),
        )
        .route(
            "/account/api_key",
            post(api_keys::generate_api_key).layer(RateLimit::default().quota(1, 600.0).build()),
        )
        .route("/account/sharex.sxcu", get(api_keys::sharex_config))
        .route("/account/invalidate", post(sessions::invalidate_session))
        .route("/account/sessions/rename", post(sessions::rename_session))
        .route(
            "/account/export",
            get(delete::export).layer(RateLimit::default().quota(5, 600.0).build()),
        )
        .route(
            "/account/delete",
            post(delete::delete_account).layer(RateLimit::default().quota(3, 3600.0).build()),
        )
}
