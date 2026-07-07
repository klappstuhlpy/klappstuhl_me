use crate::{headers::ClientIp, logging::RequestLogEntry, utils::logs_directory};
use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Redirect,
    routing::get,
    Extension, Json, Router,
};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::{cached::BodyCache, error::ApiError, models::Account, AppState};

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
    ClientIp(client_ip): ClientIp,
    Query(query): Query<LogsQuery>,
) -> Result<Json<Vec<RequestLogEntry>>, ApiError> {
    if !account.flags.is_admin() {
        return Err(ApiError::forbidden());
    }

    let (begin, end) = query.limit();
    let rows: Vec<RequestLogEntry> = state
        .requests
        .query("SELECT * FROM request WHERE ts >= ? AND ts <= ?", (begin, end))
        .await?;

    // Sensitive read — request logs include URLs, IPs, user-agents, and
    // referrers, so an admin pulling them is worth recording. Audit once
    // per call (not per row).
    state
        .audit("admin.request_log.read")
        .actor(&account)
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "from_ts": begin,
            "to_ts":   end,
            "count":   rows.len(),
        }))
        .fire();

    Ok(Json(rows))
}

async fn get_server_logs(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !account.flags.is_admin() {
        return Err(ApiError::forbidden());
    }

    let today = OffsetDateTime::now_utc().date();
    let path = logs_directory().join(today.format(&DATE_FORMAT)?).with_extension("log");
    let file = tokio::fs::read_to_string(&path).await?;
    let mut result = Vec::new();
    for line in file.lines() {
        let Ok(value) = serde_json::from_str(line) else {
            continue;
        };
        result.push(value);
    }

    // Tracing JSON logs can leak request bodies, error contexts, secrets
    // that got logged accidentally — same threat surface as a journal
    // grep. Worth recording who pulled them and when.
    state
        .audit("admin.server_log.read")
        .actor(&account)
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "date":  today.format(&DATE_FORMAT).ok(),
            "lines": result.len(),
        }))
        .fire();

    Ok(Json(serde_json::Value::Array(result)))
}

#[derive(Template)]
#[template(path = "admin/admin.html")]
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
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(user_id): Path<i64>,
) -> Result<Redirect, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    match state.get_account(user_id).await {
        Some(acc) => {
            // Admin browsing another user's profile is a privileged read
            // (the page shows their email, API keys, session list etc.).
            state
                .audit("admin.user.view")
                .actor(&account)
                .target(format!("user:{user_id} ({})", acc.name))
                .ip_opt(client_ip)
                .fire();
            Ok(Redirect::to(&format!("/user/{}", acc.name)))
        }
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

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/logs", get(get_last_logs))
        .route("/admin/logs/server", get(get_server_logs))
        .route("/admin", get(admin_index))
        .route("/admin/user/:id", get(admin_user_by_id))
        .route("/admin/cache/invalidate", get(invalidate_caches))
}
