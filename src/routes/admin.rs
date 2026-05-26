use crate::{
    database::Table,
    filters,
    flash::{FlashMessage, Flasher, Flashes},
    headers::ClientIp,
    logging::RequestLogEntry,
    utils::logs_directory,
};
use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Extension, Form, Json, Router,
};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::{
    cached::BodyCache,
    error::ApiError,
    models::Account,
    AppState,
};

// ─── Existing logs / cache / user lookup ────────────────────────────────

#[derive(Deserialize)]
struct LogsQuery {
    begin: Option<i64>,
    end: Option<i64>,
    days: Option<u8>,
}

fn datetime_to_unix_ms(dt: OffsetDateTime) -> i64 {
    (dt.unix_timestamp_nanos() / 1_000_000) as i64
}

const DATE_FORMAT: &[time::format_description::FormatItem<'_>] =
    time::macros::format_description!("[year]-[month]-[day]");

impl LogsQuery {
    fn limit(&self) -> (i64, i64) {
        // This is going to be represented as `ts >= begin AND ts <= end`
        // The default is "today" with proper day boundaries
        if let Some(days) = self.days {
            let now = OffsetDateTime::now_utc();
            let begin = now.saturating_sub(time::Duration::days(days as i64));
            return (datetime_to_unix_ms(begin), datetime_to_unix_ms(now));
        }

        match (self.begin, self.end) {
            (None, None) => {
                let now = OffsetDateTime::now_utc();
                let start = now.replace_time(time::Time::MIDNIGHT);
                let end = now.replace_time(time::macros::time!(23:59));
                (datetime_to_unix_ms(start), datetime_to_unix_ms(end))
            }
            (None, Some(end)) => (0, end),
            (Some(begin), None) => (begin, i64::MAX),
            (Some(begin), Some(end)) => (begin, end),
        }
    }
}

async fn get_last_logs(
    account: Account,
    State(state): State<AppState>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<Vec<RequestLogEntry>>, ApiError> {
    if !account.flags.is_admin() {
        return Err(ApiError::forbidden());
    }

    let (begin, end) = query.limit();
    Ok(Json(
        state
            .requests
            .query("SELECT * FROM request WHERE ts >= ? AND ts <= ?", (begin, end))
            .await?,
    ))
}

async fn get_server_logs(account: Account) -> Result<Json<serde_json::Value>, ApiError> {
    if !account.flags.is_admin() {
        return Err(ApiError::forbidden());
    }

    let today = OffsetDateTime::now_utc().date();
    let path = logs_directory().join(today.format(&DATE_FORMAT)?).with_extension("log");
    let file = tokio::fs::read_to_string(path).await?;
    let mut result = Vec::new();
    for line in file.lines() {
        let Ok(value) = serde_json::from_str(line) else {
            continue;
        };
        result.push(value);
    }

    Ok(Json(serde_json::Value::Array(result)))
}

#[derive(Template)]
#[template(path = "admin.html")]
struct AdminIndexTemplate {
    account: Option<Account>,
    active_page: &'static str,
}

async fn admin_index(account: Account) -> Result<AdminIndexTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    Ok(AdminIndexTemplate {
        account: Some(account),
        active_page: "dashboard",
    })
}

async fn admin_user_by_id(
    State(state): State<AppState>,
    account: Account,
    Path(user_id): Path<i64>,
) -> Result<Redirect, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    match state.get_account(user_id).await {
        Some(acc) => Ok(Redirect::to(&format!("/user/{}", acc.name))),
        None => Ok(Redirect::to("/")),
    }
}

async fn invalidate_caches(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Extension(cache): Extension<BodyCache>,
) -> Redirect {
    if account.flags.is_admin() {
        state.invalidate_image_caches().await;
        state.clear_account_cache();
        state.clear_session_cache();
        cache.invalidate_all();
        state
            .audit("admin.cache.invalidate")
            .actor(&account)
            .ip_opt(client_ip)
            .fire();
    }
    Redirect::to("/")
}

// ─── Invites ────────────────────────────────────────────────────────────

/// Joined view of an `invite` row plus the redeemer's username (when used).
///
/// Used by the template directly — not a database table, hence the empty
/// COLUMNS / placeholder NAME on the Table impl (we always provide a custom
/// SELECT to `Database::all`).
pub struct InviteRow {
    pub code: String,
    pub created_at: OffsetDateTime,
    pub expires_at: Option<OffsetDateTime>,
    pub used_at: Option<OffsetDateTime>,
    pub used_by_name: Option<String>,
    pub note: Option<String>,
}

impl InviteRow {
    pub fn label(&self) -> &str {
        self.note.as_deref().unwrap_or("Unnamed invite")
    }
}

impl Table for InviteRow {
    const NAME: &'static str = "invite";
    const COLUMNS: &'static [&'static str] = &[];
    type Id = String;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            code: row.get("code")?,
            created_at: row.get("created_at")?,
            expires_at: row.get("expires_at")?,
            used_at: row.get("used_at")?,
            used_by_name: row.get("used_by_name")?,
            note: row.get("note")?,
        })
    }
}

#[derive(Template)]
#[template(path = "admin_invites.html")]
struct AdminInvitesTemplate {
    account: Option<Account>,
    active_page: &'static str,
    flashes: Flashes,
    active: Vec<InviteRow>,
    used: Vec<InviteRow>,
    base_url: String,
}

async fn admin_invites(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
) -> Result<AdminInvitesTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }

    // Active = unused AND (no expiry OR expiry is in the future)
    let active = state
        .database()
        .all::<InviteRow, _, _>(
            "SELECT i.code, i.created_at, i.expires_at, i.used_at, i.note,
                    NULL AS used_by_name
             FROM invite i
             WHERE i.used_at IS NULL
               AND (i.expires_at IS NULL OR i.expires_at > CURRENT_TIMESTAMP)
             ORDER BY i.created_at DESC",
            [],
        )
        .await
        .unwrap_or_default();

    let used = state
        .database()
        .all::<InviteRow, _, _>(
            "SELECT i.code, i.created_at, i.expires_at, i.used_at, i.note,
                    a.name AS used_by_name
             FROM invite i
             LEFT JOIN account a ON i.used_by = a.id
             WHERE i.used_at IS NOT NULL
             ORDER BY i.used_at DESC
             LIMIT 50",
            [],
        )
        .await
        .unwrap_or_default();

    Ok(AdminInvitesTemplate {
        account: Some(account),
        active_page: "invites",
        flashes,
        active,
        used,
        base_url: state.config().canonical_url(),
    })
}

#[derive(Deserialize)]
struct CreateInviteForm {
    #[serde(deserialize_with = "crate::utils::empty_string_is_none")]
    note: Option<String>,
    /// 0 = never, otherwise number of days from now.
    expires_in_days: i64,
}

async fn create_invite(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    flasher: Flasher,
    Form(form): Form<CreateInviteForm>,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }

    let code = nanoid::nanoid!(
        20,
        &[
            'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm',
            'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
            'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M',
            'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
            '0', '1', '2', '3', '4', '5', '6', '7', '8', '9',
        ]
    );

    let expires_at = if form.expires_in_days > 0 {
        Some(OffsetDateTime::now_utc() + time::Duration::days(form.expires_in_days))
    } else {
        None
    };

    let result = state
        .database()
        .execute(
            "INSERT INTO invite(code, created_by, expires_at, note) VALUES (?, ?, ?, ?)",
            (code.clone(), account.id, expires_at, form.note.clone()),
        )
        .await;

    match result {
        Ok(_) => {
            state
                .audit("invite.create")
                .actor(&account)
                .target(code.clone())
                .ip_opt(client_ip)
                .meta(serde_json::json!({
                    "expires_in_days": form.expires_in_days,
                    "note": form.note,
                }))
                .fire();
            flasher.add(FlashMessage::success(format!("Invite created: {code}")));
        }
        Err(e) => {
            flasher.add(format!("Failed to create invite: {e}"));
        }
    }

    Redirect::to("/admin/invites").into_response()
}

async fn delete_invite(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    flasher: Flasher,
    Path(code): Path<String>,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }

    let result = state
        .database()
        .execute(
            "DELETE FROM invite WHERE code = ? AND used_at IS NULL",
            [code.clone()],
        )
        .await;

    match result {
        Ok(0) => {
            flasher.add("Invite not found or already redeemed");
        }
        Ok(_) => {
            state
                .audit("invite.revoke")
                .actor(&account)
                .target(code.clone())
                .ip_opt(client_ip)
                .fire();
            flasher.add(FlashMessage::success("Invite revoked"));
        }
        Err(e) => {
            flasher.add(format!("Failed to revoke invite: {e}"));
        }
    }

    Redirect::to("/admin/invites").into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/logs", get(get_last_logs))
        .route("/admin/logs/server", get(get_server_logs))
        .route("/admin", get(admin_index))
        .route("/admin/user/:id", get(admin_user_by_id))
        .route("/admin/cache/invalidate", get(invalidate_caches))
        .route("/admin/invites", get(admin_invites).post(create_invite))
        .route("/admin/invites/:code/delete", post(delete_invite))
}
