//! Account introspection endpoints (`/api/v1/me`, `/api/v1/me/usage`).
//!
//! Lets an integration discover who it is acting as, what its key may do, and
//! how much of the account's resources it is using — without scraping the web
//! UI. `/me/usage` also returns a zero-filled per-day upload series shaped so
//! it can be fed straight into `POST /render/chart` (or uPlot) as-is.
//!
//! Neither endpoint requires a specific scope: any valid key may inspect its
//! own identity and usage.

use axum::extract::State;
use serde::Serialize;
use time::{Duration, OffsetDateTime};
use utoipa::ToSchema;

use super::auth::ApiToken;
use super::utils::{ApiJson as Json, RateLimitResponse};
use crate::{error::ApiError, models::Scope, AppState};

/// How many days of history `/me/usage` returns.
const SERIES_DAYS: i64 = 30;

/// The calling account, as seen by the API.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiMe {
    /// The account id.
    pub id: i64,
    /// The account's username.
    pub name: String,
    /// Whether the account is an operator/admin account.
    pub admin: bool,
    /// Whether two-factor authentication is active.
    pub totp_enabled: bool,
    /// Whether a Discord account is linked.
    pub discord_linked: bool,
    /// The scopes granted to the key making this request. An empty list means
    /// the key is a legacy full-access key (every scope check passes).
    pub key_scopes: Vec<String>,
}

/// Get the calling account
///
/// Returns the account behind the presented API key, plus the scopes that key
/// holds — useful for "connected as …" UI and for debugging scoped keys.
#[utoipa::path(
    get,
    path = "/me",
    responses(
        (status = 200, description = "The calling account", body = ApiMe),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = [])),
    tag = "account"
)]
pub async fn get_me(State(state): State<AppState>, auth: ApiToken) -> Result<Json<ApiMe>, ApiError> {
    let account = auth.account(&state).await?;
    let key_scopes = auth
        .scopes
        .split(',')
        .filter_map(|s| Scope::from_str(s.trim()))
        .map(|s| s.as_str().to_string())
        .collect();

    Ok(Json(ApiMe {
        id: account.id,
        admin: account.flags.is_admin(),
        totp_enabled: account.has_totp(),
        discord_linked: account.discord_id.is_some(),
        name: account.name,
        key_scopes,
    }))
}

/// Totals for one resource kind.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct ResourceUsage {
    /// How many of this resource the account owns.
    pub count: i64,
    /// Total stored bytes (images only; 0 elsewhere).
    pub bytes: i64,
    /// Aggregate view/click count across the resource.
    pub views: i64,
}

/// A zero-filled per-day activity series (oldest first), chart-ready: `days`
/// are the x labels, the value arrays are the series.
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageSeries {
    /// The day of each bucket (`YYYY-MM-DD`, UTC), oldest first.
    pub days: Vec<String>,
    /// Images uploaded on each day.
    pub uploads: Vec<i64>,
    /// Bytes uploaded on each day.
    pub upload_bytes: Vec<i64>,
}

/// The account's usage snapshot.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiUsage {
    /// Hosted images: count, stored bytes, landing-page views.
    pub images: ResourceUsage,
    /// Short links: count and total clicks (`views`).
    pub links: ResourceUsage,
    /// Pastes: count and total views.
    pub pastes: ResourceUsage,
    /// Upload activity over the last 30 days, ready to plot.
    pub series: UsageSeries,
}

/// Get account usage
///
/// Returns resource totals (images, short links, pastes) plus a zero-filled
/// 30-day upload series. The `series` object is shaped to drop straight into
/// `POST /render/chart`: use `days` as `labels` and `uploads` /
/// `upload_bytes` as series data.
#[utoipa::path(
    get,
    path = "/me/usage",
    responses(
        (status = 200, description = "The account's usage snapshot", body = ApiUsage),
        (status = 401, description = "Unauthenticated", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = [])),
    tag = "account"
)]
pub async fn get_usage(State(state): State<AppState>, auth: ApiToken) -> Result<Json<ApiUsage>, ApiError> {
    let account = auth.account(&state).await?;
    let account_id = account.id;

    // One pool dispatch for all aggregates: totals + the per-day buckets.
    // Timestamps are TEXT (RFC 3339-ish), so `substr(…, 1, 10)` is the day key
    // regardless of the exact separator between date and time.
    let (images, links, pastes, buckets) = state
        .database()
        .call(move |conn| -> rusqlite::Result<_> {
            let images = conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(size), 0), COALESCE(SUM(views), 0) FROM images WHERE uploader_id = ?1",
                [account_id],
                |row| {
                    Ok(ResourceUsage {
                        count: row.get(0)?,
                        bytes: row.get(1)?,
                        views: row.get(2)?,
                    })
                },
            )?;
            let links = conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(clicks), 0) FROM short_link WHERE account_id = ?1",
                [account_id],
                |row| {
                    Ok(ResourceUsage {
                        count: row.get(0)?,
                        views: row.get(1)?,
                        ..Default::default()
                    })
                },
            )?;
            let pastes = conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(views), 0) FROM paste WHERE account_id = ?1",
                [account_id],
                |row| {
                    Ok(ResourceUsage {
                        count: row.get(0)?,
                        views: row.get(1)?,
                        ..Default::default()
                    })
                },
            )?;

            let mut stmt = conn.prepare_cached(
                "SELECT substr(uploaded_at, 1, 10) AS day, COUNT(*), COALESCE(SUM(size), 0) \
                 FROM images \
                 WHERE uploader_id = ?1 AND uploaded_at >= ?2 \
                 GROUP BY day",
            )?;
            let since = (OffsetDateTime::now_utc() - Duration::days(SERIES_DAYS - 1))
                .date()
                .to_string();
            let buckets: Vec<(String, i64, i64)> = stmt
                .query_map(rusqlite::params![account_id, since], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<rusqlite::Result<_>>()?;

            Ok((images, links, pastes, buckets))
        })
        .await
        .map_err(|_| ApiError::new("failed to load usage"))?;

    // Zero-fill the window so the series is directly plottable.
    let today = OffsetDateTime::now_utc().date();
    let mut series = UsageSeries {
        days: Vec::with_capacity(SERIES_DAYS as usize),
        uploads: vec![0; SERIES_DAYS as usize],
        upload_bytes: vec![0; SERIES_DAYS as usize],
    };
    for offset in (0..SERIES_DAYS).rev() {
        series.days.push((today - Duration::days(offset)).to_string());
    }
    for (day, count, bytes) in buckets {
        if let Some(i) = series.days.iter().position(|d| *d == day) {
            series.uploads[i] = count;
            series.upload_bytes[i] = bytes;
        }
    }

    Ok(Json(ApiUsage {
        images,
        links,
        pastes,
        series,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_window_is_contiguous_and_ends_today() {
        let today = OffsetDateTime::now_utc().date();
        let days: Vec<String> = (0..SERIES_DAYS)
            .rev()
            .map(|offset| (today - Duration::days(offset)).to_string())
            .collect();
        assert_eq!(days.len(), SERIES_DAYS as usize);
        assert_eq!(days.last().unwrap(), &today.to_string());
        // `Date::to_string` is the `YYYY-MM-DD` shape the substr() day key uses.
        assert_eq!(days[0].len(), 10);
    }
}
