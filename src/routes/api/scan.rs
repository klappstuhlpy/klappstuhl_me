use axum::extract::{Multipart, State};
use utoipa::ToSchema;

use crate::scan::ScanReport;
use crate::{error::ApiError, headers::ClientIp, AppState};

use super::{
    auth::ApiToken,
    utils::{ApiJson as Json, RateLimitResponse},
};

#[derive(ToSchema)]
#[allow(dead_code)]
struct ScanUpload {
    /// The file to scan. Any file type is accepted.
    #[schema(format = Binary)]
    file: String,
}

/// Scan
///
/// Scan an uploaded file for malware.
///
/// The file is run through the server's configured malware backends:
///
/// - **ClamAV** — the raw bytes are streamed to a ClamAV daemon (signature match).
/// - **VirusTotal** — the file's SHA-256 is looked up against VirusTotal's
///   aggregated multi-engine report. The file contents are never uploaded;
///   only the hash leaves the server.
///
/// Both backends are optional. When one is not configured on the server its
/// fields are returned as `null` — inspect `verdict` for the combined result
/// (`"clean"`, `"infected"`, or `"unknown"`). Nothing is persisted: each call
/// is stateless.
#[utoipa::path(
    post,
    path = "/api/scan",
    request_body(
        content = inline(ScanUpload),
        content_type = "multipart/form-data",
        description = "The file to scan, sent as a `file` field."
    ),
    responses(
        (status = 200, description = "Scan complete", body = ScanReport),
        (status = 400, description = "No file was provided", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(
        ("api_key" = [])
    ),
    tag = "scan"
)]
pub async fn scan_file(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    mut multipart: Multipart,
) -> Result<Json<ScanReport>, ApiError> {
    let Some(account) = state.get_account(auth.id).await else {
        return Err(ApiError::unauthorized());
    };

    // Pull the first `file` field out of the multipart body.
    let mut data: Option<Vec<u8>> = None;
    while let Some(field) = multipart.next_field().await.map_err(|e| ApiError::new(e.to_string()))? {
        if field.name() == Some("file") {
            let bytes = field.bytes().await.map_err(|e| ApiError::new(e.to_string()))?;
            if !bytes.is_empty() {
                data = Some(bytes.to_vec());
            }
            break;
        }
    }
    let Some(data) = data else {
        return Err(ApiError::new("no `file` field in upload"));
    };

    let report = crate::scan::scan_bytes(&state, &data).await;

    state
        .audit("api.scan")
        .actor(&account)
        .target(format!("{} bytes", report.file_size))
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "verdict":      report.verdict,
            "sha256":       report.sha256,
            "clamav_clean": report.clamav_clean,
            "vt_status":    report.vt_status,
        }))
        .fire();

    Ok(Json(report))
}
