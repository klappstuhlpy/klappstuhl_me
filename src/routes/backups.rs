//! Admin SQLite backup management.
//!
//! - `GET    /admin/backups`                  list page
//! - `POST   /admin/backups`                  take a backup now (then prune)
//! - `GET    /admin/backups/:name/download`   download a backup file
//! - `POST   /admin/backups/:name/delete`     delete a backup file
//!
//! Restore is deliberately not offered in-app (replacing the live DB under
//! WAL is unsafe); download and swap `main.db` with the server stopped.

use crate::{backup, headers::ClientIp, models::Account, AppState};
use askama::Template;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};

#[derive(Template)]
#[template(path = "admin_backups.html")]
struct AdminBackupsTemplate {
    account: Option<Account>,
    active_page: &'static str,
    backups: Vec<backup::BackupInfo>,
    total_size_human: String,
    keep: usize,
}

async fn page(State(state): State<AppState>, account: Account) -> Result<AdminBackupsTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    let backups = backup::list();
    let total: u64 = backups.iter().map(|b| b.size).sum();
    Ok(AdminBackupsTemplate {
        account: Some(account),
        active_page: "backups",
        total_size_human: backup::human_size(total),
        keep: backup::keep_count(&state),
        backups,
    })
}

async fn create_now(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    match backup::create(&state).await {
        Ok(path) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            backup::prune(backup::keep_count(&state));
            state
                .audit("backup.create")
                .actor(&account)
                .target(name)
                .ip_opt(client_ip)
                .fire();
        }
        Err(e) => tracing::warn!(error = %e, "manual backup failed"),
    }
    Redirect::to("/admin/backups").into_response()
}

async fn download(account: Account, Path(name): Path<String>) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(path) = backup::resolve(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "backup download failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn delete_backup(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Path(name): Path<String>,
) -> Response {
    if !account.flags.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(path) = backup::resolve(&name) {
        if std::fs::remove_file(&path).is_ok() {
            state
                .audit("backup.delete")
                .actor(&account)
                .target(name)
                .ip_opt(client_ip)
                .fire();
        }
    }
    Redirect::to("/admin/backups").into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/admin/backups", get(page).post(create_now))
        .route("/admin/backups/:name/download", get(download))
        .route("/admin/backups/:name/delete", post(delete_backup))
}
