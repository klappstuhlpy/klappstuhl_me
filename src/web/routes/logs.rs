//! Admin log viewer.
//!
//! - `GET /admin/logs/view`   page
//! - `GET /admin/logs/data`   JSON: tailed + filtered log lines
//!
//! (`/admin/logs` and `/admin/logs/server` are pre-existing request-log /
//! raw-dump JSON endpoints in `routes::admin`; this is the interactive viewer.)
//!
//! Reads the rolling log files written by the tracing appenders in
//! `utils::logs_directory()`. The main application log is JSON (one object per
//! line); the bad-request log is compact text. Both are parsed best-effort.

use std::path::PathBuf;

use crate::{models::Account, AppState};
use askama::Template;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};

#[derive(Template)]
#[template(path = "admin/admin_logs.html")]
struct AdminLogsTemplate {
    account: Option<Account>,
    active_page: &'static str,
}

async fn page(account: Account) -> Result<AdminLogsTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(AdminLogsTemplate {
        account: Some(account),
        active_page: "logs",
    })
}

/// Resolves the newest log file for a kind: `main` (application) or `bad`
/// (bad requests). Prefers the appender's stable symlink, falling back to the
/// newest matching rotated file (symlinks need privileges on Windows).
fn resolve_log_file(kind: &str) -> Option<PathBuf> {
    let dir = crate::utils::logs_directory();
    let (symlink, is_bad) = match kind {
        "bad" => ("bad_requests.log", true),
        _ => ("today.log", false),
    };
    let direct = dir.join(symlink);
    if direct.exists() {
        return Some(direct);
    }
    // Fall back to the newest matching .log file.
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".log") {
            continue;
        }
        let is_bad_file = name.starts_with("bad_requests");
        if is_bad_file != is_bad {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
            best = Some((modified, entry.path()));
        }
    }
    best.map(|(_, p)| p)
}

#[derive(Serialize)]
struct LogLine {
    ts: String,
    level: String,
    target: String,
    message: String,
    raw: String,
}

/// Parses one log line. JSON lines (the main appender) are decomposed into
/// fields; anything else is treated as a plain message.
fn parse_line(raw: &str) -> LogLine {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
        let ts = v.get("timestamp").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let level = v.get("level").and_then(|x| x.as_str()).unwrap_or("INFO").to_string();
        let target = v.get("target").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let message = v
            .get("fields")
            .and_then(|f| f.get("message"))
            .and_then(|m| m.as_str())
            .map(|s| s.to_string())
            .or_else(|| v.get("fields").map(|f| f.to_string()))
            .unwrap_or_default();
        LogLine {
            ts,
            level,
            target,
            message,
            raw: raw.to_string(),
        }
    } else {
        LogLine {
            ts: String::new(),
            level: "INFO".to_string(),
            target: String::new(),
            message: raw.to_string(),
            raw: raw.to_string(),
        }
    }
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn data(State(_state): State<AppState>, account: Account, Query(query): Query<LogQuery>) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let kind = query.file.as_deref().unwrap_or("main");
    let Some(path) = resolve_log_file(kind) else {
        return Json(serde_json::json!({ "file": serde_json::Value::Null, "lines": [] })).into_response();
    };
    let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();

    let limit = query.limit.unwrap_or(500).clamp(1, 5000);
    let needle = query.q.unwrap_or_default().to_lowercase();
    let level = query.level.unwrap_or_default();

    // Walk newest-first, filter, cap, then restore chronological order.
    let mut lines: Vec<LogLine> = content
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .map(parse_line)
        .filter(|ll| level.is_empty() || ll.level.eq_ignore_ascii_case(&level))
        .filter(|ll| needle.is_empty() || ll.raw.to_lowercase().contains(&needle))
        .take(limit)
        .collect();
    lines.reverse();

    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    Json(serde_json::json!({ "file": file_name, "lines": lines })).into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/logs/view", get(page))
        .route("/admin/logs/data", get(data))
}
