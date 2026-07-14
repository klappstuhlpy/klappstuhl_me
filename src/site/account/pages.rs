//! The account shell: seven GET pages behind a shared sidebar layout, plus the
//! read-only `/user/:name` profile.
//!
//! Every template struct carries `account` (the signed-in user, for the site
//! header), `flashes`, and `active_page` — the last one is what
//! `auth/account_layout.html` highlights in the sidebar.

use crate::{
    filters,
    flash::Flashes,
    key::SecretKey,
    models::{Account, Session},
    AppState,
};
use askama::Template;
use axum::{
    extract::{Path, State},
    response::{IntoResponse, Redirect, Response},
};
use serde::Serialize;
use time::OffsetDateTime;

use super::{
    discord_username, image_totals, paste_count, recent_activity, short_link_count, unused_recovery_codes,
    ActivityEntry,
};
use crate::token::Token;

/// Renders a byte count for a stat tile ("1.4 MB").
pub fn human_bytes(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The counts behind the overview tiles and the deletion summary. Fanned out
/// across the DB pool rather than reusing `state.resolve_images()`, which would
/// load every image in the site just to count one user's.
struct Totals {
    images: i64,
    image_bytes: i64,
    links: i64,
    pastes: i64,
    sessions: i64,
}

async fn totals(state: &AppState, account_id: i64) -> Totals {
    let (images, links, pastes, sessions) = tokio::join!(
        image_totals(state, account_id),
        short_link_count(state, account_id),
        paste_count(state, account_id),
        super::count_sessions(state, account_id),
    );
    Totals {
        images: images.0,
        image_bytes: images.1,
        links,
        pastes,
        sessions,
    }
}

/// What the browser sessions list needs: the API key is pulled out of the
/// session rows (it lives in the same table) so it can be shown on its own page.
struct SessionSplit {
    current: Option<Session>,
    others: Vec<Session>,
    api_key: Option<String>,
    api_key_scopes: String,
}

async fn split_sessions(state: &AppState, account_id: i64, current_token: Option<&Token>) -> SessionSplit {
    let mut sessions: Vec<Session> = state
        .database()
        .all("SELECT * FROM session WHERE account_id = ?", [account_id])
        .await
        .unwrap_or_default();

    let current = current_token
        .map(|t| t.base64())
        .and_then(|id| sessions.iter().position(|s| s.id == id))
        .map(|idx| sessions.swap_remove(idx));

    let api_key_session = sessions
        .iter()
        .position(|s| s.api_key)
        .map(|idx| sessions.swap_remove(idx));
    let api_key = api_key_session.as_ref().map(|s| s.id.clone());
    let api_key_scopes = api_key_session.map(|s| s.scopes).unwrap_or_default();

    sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));

    SessionSplit {
        current,
        others: sessions,
        api_key,
        api_key_scopes,
    }
}

// ─── Overview ───────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "auth/account/overview.html")]
struct OverviewTemplate {
    account: Option<Account>,
    flashes: Flashes,
    active_page: &'static str,
    images: i64,
    images_size: String,
    links: i64,
    pastes: i64,
    sessions: i64,
    /// Security checklist state — each row links to the page that fixes it.
    has_password: bool,
    totp_enabled: bool,
    has_api_key: bool,
    /// A key with no scopes is a legacy full-access token; worth nagging about.
    api_key_scoped: bool,
    discord_linked: bool,
    discord_enabled: bool,
    activity: Vec<ActivityEntry>,
}

pub async fn overview(State(state): State<AppState>, flashes: Flashes, token: Token, account: Account) -> Response {
    let (counts, split, activity, linked) = tokio::join!(
        totals(&state, account.id),
        split_sessions(&state, account.id, Some(&token)),
        recent_activity(&state, &account, "auth.", 5),
        discord_username(&state, account.id),
    );

    OverviewTemplate {
        active_page: "overview",
        flashes,
        images: counts.images,
        images_size: human_bytes(counts.image_bytes),
        links: counts.links,
        pastes: counts.pastes,
        // The current session counts too — it is a way in like any other.
        sessions: counts.sessions,
        has_password: account.has_password(),
        totp_enabled: account.totp_enabled,
        has_api_key: split.api_key.is_some(),
        api_key_scoped: !split.api_key_scopes.is_empty(),
        discord_linked: !linked.is_empty(),
        discord_enabled: state.config().discord.enabled(),
        activity,
        account: Some(account),
    }
    .into_response()
}

// ─── Profile ────────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "auth/account/profile.html")]
struct ProfileTemplate {
    account: Option<Account>,
    flashes: Flashes,
    active_page: &'static str,
    discord_username: String,
    discord_enabled: bool,
    has_password: bool,
    /// `Some` while this account is inside the rename cooldown — the card then
    /// says when it lapses instead of offering the dialog. The POST re-checks it.
    rename_available_at: Option<OffsetDateTime>,
    /// Names this account gave up, newest first. Each is reserved for it for
    /// `RELEASE_HOLD_DAYS`, so it is a list of names it can still take back.
    previous_names: Vec<String>,
    /// Days a released name stays reserved / days between renames — rendered
    /// into the card's copy so the page and the rules can't drift apart.
    release_hold_days: i64,
    cooldown_days: i64,
}

pub async fn profile(State(state): State<AppState>, flashes: Flashes, account: Account) -> Response {
    let (discord, rename_available_at, previous_names) = tokio::join!(
        discord_username(&state, account.id),
        super::username::cooldown_until(&state, account.id),
        super::username::previous_names(&state, account.id),
    );

    ProfileTemplate {
        active_page: "profile",
        flashes,
        discord_username: discord,
        discord_enabled: state.config().discord.enabled(),
        has_password: account.has_password(),
        rename_available_at,
        previous_names,
        release_hold_days: super::username::RELEASE_HOLD_DAYS,
        cooldown_days: super::username::COOLDOWN_DAYS,
        account: Some(account),
    }
    .into_response()
}

// ─── Security ───────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "auth/account/security.html")]
struct SecurityTemplate {
    account: Option<Account>,
    flashes: Flashes,
    active_page: &'static str,
    has_password: bool,
    totp_enabled: bool,
    recovery_codes_left: i64,
    login_history: Vec<ActivityEntry>,
}

pub async fn security_page(State(state): State<AppState>, flashes: Flashes, account: Account) -> Response {
    let (recovery_codes_left, login_history) = tokio::join!(
        unused_recovery_codes(&state, account.id),
        recent_activity(&state, &account, "auth.login.", 20),
    );

    SecurityTemplate {
        active_page: "security",
        flashes,
        has_password: account.has_password(),
        totp_enabled: account.totp_enabled,
        recovery_codes_left,
        login_history,
        account: Some(account),
    }
    .into_response()
}

// ─── Sessions ───────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "auth/account/sessions.html")]
struct SessionsTemplate {
    account: Option<Account>,
    flashes: Flashes,
    active_page: &'static str,
    current_session: Option<Session>,
    sessions: Vec<Session>,
    /// Signs each session id for the revoke / rename calls (see `sessions.rs`).
    key: SecretKey,
}

pub async fn sessions_page(
    State(state): State<AppState>,
    flashes: Flashes,
    token: Token,
    account: Account,
) -> Response {
    let split = split_sessions(&state, account.id, Some(&token)).await;
    SessionsTemplate {
        active_page: "sessions",
        flashes,
        current_session: split.current,
        sessions: split.others,
        key: state.config().secret_key,
        account: Some(account),
    }
    .into_response()
}

// ─── API & tools ────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "auth/account/api.html")]
struct ApiTemplate {
    account: Option<Account>,
    flashes: Flashes,
    active_page: &'static str,
    api_key: Option<String>,
    /// Comma-separated scopes on the current key (empty for legacy/unscoped keys).
    /// Used to pre-check the boxes in the scope picker.
    api_key_scopes: String,
    /// Gates the privileged scope checkboxes (`admin:*`, `images:guild`).
    is_admin: bool,
    /// Absolute base for the copy-paste `curl` example, e.g. `https://klappstuhl.me/api/v1`.
    api_base: String,
}

pub async fn api_page(State(state): State<AppState>, flashes: Flashes, token: Token, account: Account) -> Response {
    let split = split_sessions(&state, account.id, Some(&token)).await;
    ApiTemplate {
        active_page: "api",
        flashes,
        api_key: split.api_key,
        api_key_scopes: split.api_key_scopes,
        is_admin: account.flags.is_admin(),
        api_base: state.config().url_to(crate::site::api::api_base_path()),
        account: Some(account),
    }
    .into_response()
}

// ─── My content ─────────────────────────────────────────────────────────────

/// A paste without its body — the content page only lists them.
#[derive(Debug, Serialize)]
pub struct PasteSummary {
    pub id: String,
    pub language: Option<String>,
    pub views: i64,
    pub created_at: OffsetDateTime,
    pub expires_at: Option<OffsetDateTime>,
}

/// An image row without its blob, for the same reason.
#[derive(Debug, Serialize)]
pub struct ImageSummary {
    pub id: String,
    pub size: i64,
    pub uploaded_at: OffsetDateTime,
    pub views: i64,
}

#[derive(Template)]
#[template(path = "auth/account/content.html")]
struct ContentTemplate {
    account: Option<Account>,
    flashes: Flashes,
    active_page: &'static str,
    images: i64,
    images_size: String,
    links: i64,
    pastes: i64,
    recent_images: Vec<ImageSummary>,
    recent_pastes: Vec<PasteSummary>,
}

pub async fn content(State(state): State<AppState>, flashes: Flashes, account: Account) -> Response {
    let account_id = account.id;
    let recent_images = state
        .database()
        .call(move |conn| -> rusqlite::Result<Vec<ImageSummary>> {
            let mut stmt = conn.prepare_cached(
                "SELECT id, size, uploaded_at, views FROM images
                  WHERE uploader_id = ? ORDER BY uploaded_at DESC LIMIT 8",
            )?;
            let rows: rusqlite::Result<Vec<ImageSummary>> = stmt
                .query_map([account_id], |row| {
                    Ok(ImageSummary {
                        id: row.get("id")?,
                        size: row.get("size")?,
                        uploaded_at: row.get("uploaded_at")?,
                        views: row.get("views")?,
                    })
                })?
                .collect();
            rows
        })
        .await
        .unwrap_or_default();

    let recent_pastes = state
        .database()
        .call(move |conn| -> rusqlite::Result<Vec<PasteSummary>> {
            let mut stmt = conn.prepare_cached(
                "SELECT id, language, views, created_at, expires_at FROM paste
                  WHERE account_id = ? ORDER BY created_at DESC LIMIT 8",
            )?;
            let rows: rusqlite::Result<Vec<PasteSummary>> = stmt
                .query_map([account_id], |row| {
                    Ok(PasteSummary {
                        id: row.get("id")?,
                        language: row.get("language")?,
                        views: row.get("views")?,
                        created_at: row.get("created_at")?,
                        expires_at: row.get("expires_at")?,
                    })
                })?
                .collect();
            rows
        })
        .await
        .unwrap_or_default();

    let counts = totals(&state, account.id).await;

    ContentTemplate {
        active_page: "content",
        flashes,
        images: counts.images,
        images_size: human_bytes(counts.image_bytes),
        links: counts.links,
        pastes: counts.pastes,
        recent_images,
        recent_pastes,
        account: Some(account),
    }
    .into_response()
}

// ─── Danger zone ────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "auth/account/danger.html")]
struct DangerTemplate {
    account: Option<Account>,
    flashes: Flashes,
    active_page: &'static str,
    images: i64,
    images_size: String,
    links: i64,
    pastes: i64,
    sessions: i64,
    /// Deletion needs a password to re-authenticate against; Discord-only
    /// accounts are told to set one first (same pattern 2FA already uses).
    has_password: bool,
    totp_enabled: bool,
    /// The instance must keep at least one admin — deleting the last one is refused
    /// server-side, so the page says so up front instead of failing the submit.
    is_last_admin: bool,
}

pub async fn danger(State(state): State<AppState>, flashes: Flashes, account: Account) -> Response {
    let (counts, is_last_admin) = tokio::join!(totals(&state, account.id), super::is_last_admin(&state, &account));

    DangerTemplate {
        active_page: "danger",
        flashes,
        images: counts.images,
        images_size: human_bytes(counts.image_bytes),
        links: counts.links,
        pastes: counts.pastes,
        sessions: counts.sessions,
        has_password: account.has_password(),
        totp_enabled: account.totp_enabled,
        is_last_admin,
        account: Some(account),
    }
    .into_response()
}

// ─── Public profile ─────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "auth/user_public.html")]
struct UserPublicTemplate {
    account: Option<Account>,
    flashes: Flashes,
    /// The account being looked at (not necessarily the viewer).
    user: Account,
    images: i64,
    images_size: String,
    links: i64,
    /// True when you're looking at your own public page — the template then
    /// offers a link back into the account shell.
    is_self: bool,
}

/// `GET /user/:name` — a slim, read-only view of another account. Deliberately
/// its own template: piggybacking on the owner page meant hiding half of it
/// behind `if account.id == user.id` branches.
pub async fn user_public(
    State(state): State<AppState>,
    flashes: Flashes,
    account: Account,
    Path(name): Path<String>,
) -> Result<Response, Redirect> {
    let user = state
        .database()
        .get::<Account, _, _>("SELECT * FROM account WHERE name = ?", [name])
        .await
        .ok()
        .flatten()
        .ok_or_else(|| Redirect::to("/"))?;

    let ((images, image_bytes), links) = tokio::join!(image_totals(&state, user.id), short_link_count(&state, user.id));

    Ok(UserPublicTemplate {
        is_self: account.id == user.id,
        flashes,
        user,
        images,
        images_size: human_bytes(image_bytes),
        links,
        account: Some(account),
    }
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    #[test]
    fn bytes_are_humanised_per_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}
