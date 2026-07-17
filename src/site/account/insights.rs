//! Site insights — the admin-only traffic overview at `/account/insights`.
//!
//! - `GET /account/insights`            page (frame only)
//! - `GET /account/insights/data?range` JSON: the aggregates behind it
//!
//! This is the analytics half of the old `/admin` dashboard, kept here when the
//! control plane moved out to Vantage. It stayed because it answers *product*
//! questions about klappstuhl.me — which routes draw traffic, who leans on the
//! API — and because naming an API consumer needs `main.db`, which Vantage has
//! no access to (it only opens `requests.db`, read-only, for its security lens).
//! Host observability — server logs, uptime, 4xx feeds — lives in Vantage.
//!
//! **Everything aggregates in SQL.** The page it replaces shipped every raw
//! request row for the window to the browser (IPs, user-agents, referrers, up to
//! 30 days of them) and counted them in JavaScript. Only counts leave the server
//! now; keep it that way when adding a panel.

use crate::{error::ApiError, flash::Flashes, headers::ClientIp, models::Account, AppState};
use askama::Template;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Rows returned per ranked list. The old page used 25 and it reads fine.
const TOP_N: i64 = 25;

/// How many grouped referrers to pull before dropping our own domains.
///
/// Self-referrals (one page on the site linking to another) are the bulk of the
/// raw rows and are noise here, but they can only be recognised after parsing
/// the host — so the filter runs in Rust and the SQL has to over-fetch to still
/// have [`TOP_N`] genuine referrers left afterwards.
const REFERRER_SCAN: i64 = 200;

// ─── Time window ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RangeQuery {
    #[serde(default)]
    range: Option<String>,
}

fn unix_ms(dt: OffsetDateTime) -> i64 {
    (dt.unix_timestamp_nanos() / 1_000_000) as i64
}

/// Resolves a range key to an inclusive `(begin, end)` in unix milliseconds.
///
/// Anything unrecognised (including a missing key) falls back to today rather
/// than erroring — this drives a `<select>`, so a bad value means a stale tab,
/// not an attack worth a 400. Ranges beyond `requests.db`'s 45-day retention
/// would just render short, so they aren't offered.
fn window(range: Option<&str>) -> (i64, i64) {
    let now = OffsetDateTime::now_utc();
    let days = match range {
        Some("7d") => 7,
        Some("14d") => 14,
        Some("30d") => 30,
        _ => return (unix_ms(now.replace_time(time::Time::MIDNIGHT)), unix_ms(now)),
    };
    (unix_ms(now.saturating_sub(time::Duration::days(days))), unix_ms(now))
}

// ─── Referrer labelling ─────────────────────────────────────────────────────

/// The host part of an absolute URL, lowercased (`https://a.b/c?d` → `a.b`).
///
/// Hand-rolled because the crate has no URL parser and this needs no more than
/// the authority: referrers are attacker-supplied strings, so anything that
/// doesn't look like `scheme://host/…` is rejected rather than guessed at.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    // Strip userinfo and port: `user@host:443` → `host`.
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Collapses the big search engines to a plain name, so the table shows
/// "Google" once instead of a dozen `www.google.*` regional hosts.
fn search_engine(host: &str) -> Option<&'static str> {
    if host.starts_with("google.") || host.starts_with("www.google.") {
        Some("Google")
    } else if host.ends_with("bing.com") {
        Some("Bing")
    } else if host.ends_with("duckduckgo.com") {
        Some("DuckDuckGo")
    } else {
        None
    }
}

/// True when a referrer host is one of ours (the apex or any subdomain of a
/// configured domain). Those are internal navigation, not a referring site.
fn is_own_host(host: &str, domains: &[String]) -> bool {
    domains.iter().any(|d| {
        let d = d.to_ascii_lowercase();
        host == d || host.ends_with(&format!(".{d}"))
    })
}

// ─── Response shapes ────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Summary {
    requests: i64,
    active_users: i64,
    /// Mean round-trip latency in milliseconds, rounded.
    avg_latency_ms: i64,
    /// 2xx/3xx as a fraction of the window. `None` when nothing was served —
    /// the UI shows a dash, which is honest; `0%` would read as an outage.
    success_rate: Option<f64>,
}

#[derive(Serialize)]
struct Ranked {
    label: String,
    /// Only set when the label is a thing the browser can usefully open.
    href: Option<String>,
    count: i64,
}

#[derive(Serialize)]
struct ApiConsumer {
    user_id: i64,
    /// The account name, or `None` if the account has since been deleted.
    name: Option<String>,
    total: i64,
    success: i64,
    failed: i64,
}

/// The whole payload of `GET /account/insights/data`. Public because it appears
/// in the handler's return type; it is not part of the documented `/api` surface.
#[derive(Serialize)]
pub struct InsightsData {
    summary: Summary,
    routes: Vec<Ranked>,
    referrers: Vec<Ranked>,
    api_routes: Vec<Ranked>,
    api_consumers: Vec<ApiConsumer>,
}

// ─── Queries ────────────────────────────────────────────────────────────────

/// Shared predicate: the window, minus static assets.
///
/// `/static/*` is served on every page load and would swamp every ranking with
/// CSS files; the old dashboard filtered it client-side for the same reason.
const BASE: &str = "ts >= ? AND ts <= ? AND path NOT LIKE '/static/%'";

// The four queries are built here rather than inline in the handlers so the
// tests can run the *exact* strings the handlers run against a real seeded
// `requests.db`. Nothing type-checks a query string, and these carry the whole
// meaning of the page — a typo or a wrong GROUP BY is invisible until it renders.

fn summary_sql() -> String {
    format!(
        "SELECT COUNT(*),
                COUNT(DISTINCT user_id),
                COALESCE(AVG(latency), 0.0),
                COALESCE(SUM(status_code >= 200 AND status_code < 400), 0)
           FROM request WHERE {BASE}"
    )
}

fn routes_sql(api_only: bool) -> String {
    let filter = if api_only { " AND path LIKE '/api/%'" } else { "" };
    format!(
        "SELECT COALESCE(route, path) AS label, COUNT(*) AS hits
           FROM request WHERE {BASE}{filter}
          GROUP BY label ORDER BY hits DESC LIMIT ?"
    )
}

fn referrers_sql() -> String {
    format!(
        "SELECT referrer, COUNT(*) AS hits
           FROM request
          WHERE {BASE} AND referrer IS NOT NULL AND referrer != ''
          GROUP BY referrer ORDER BY hits DESC LIMIT ?"
    )
}

fn consumers_sql() -> String {
    format!(
        "SELECT user_id,
                COUNT(*) AS total,
                COALESCE(SUM(status_code >= 200 AND status_code < 400), 0) AS ok
           FROM request
          WHERE {BASE} AND user_id IS NOT NULL AND path LIKE '/api/%'
          GROUP BY user_id ORDER BY total DESC LIMIT ?"
    )
}

async fn summary(state: &AppState, begin: i64, end: i64) -> Result<Summary, ApiError> {
    let rows = state
        .requests
        .query_map(summary_sql(), (begin, end), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .await?;

    let (requests, active_users, avg_latency, successes) = rows.into_iter().next().unwrap_or((0, 0, 0.0, 0));
    Ok(Summary {
        requests,
        active_users,
        // `latency` is stored in seconds.
        avg_latency_ms: (avg_latency * 1000.0).round() as i64,
        success_rate: (requests > 0).then(|| successes as f64 / requests as f64),
    })
}

/// Ranked route patterns.
///
/// Groups on `route` (the matched pattern, e.g. `/p/:id`) and only falls back to
/// the raw `path` when the request never matched a route — a 404 sweep, say.
/// Grouping on `path` like the old page did split every paste and image view
/// into its own row, which buried the actual routes.
async fn routes(state: &AppState, begin: i64, end: i64, api_only: bool) -> Result<Vec<Ranked>, ApiError> {
    let rows = state
        .requests
        .query_map(routes_sql(api_only), (begin, end, TOP_N), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .await?;

    Ok(rows
        .into_iter()
        .map(|(label, count)| Ranked {
            // A pattern like `/p/:id` is not a URL; only offer a link when the
            // label is one the browser can actually resolve.
            href: (!label.contains(':')).then(|| label.clone()),
            label,
            count,
        })
        .collect())
}

async fn referrers(state: &AppState, begin: i64, end: i64) -> Result<Vec<Ranked>, ApiError> {
    let rows = state
        .requests
        .query_map(referrers_sql(), (begin, end, REFERRER_SCAN), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .await?;

    let domains = state.config().domains.clone();
    let mut out: Vec<Ranked> = Vec::new();
    for (referrer, count) in rows {
        let Some(host) = host_of(&referrer) else { continue };
        if is_own_host(&host, &domains) {
            continue;
        }
        match search_engine(&host) {
            // Search engines are named, not linked: the referring URL carries
            // the query and there is nothing useful on the other end anyway.
            Some(name) => out.push(Ranked {
                label: name.to_string(),
                href: None,
                count,
            }),
            None => out.push(Ranked {
                label: host,
                href: Some(referrer),
                count,
            }),
        }
        if out.len() as i64 >= TOP_N {
            break;
        }
    }
    Ok(out)
}

/// The heaviest API callers, resolved to account names.
///
/// This join is the reason the page lives in this repo: the counts come from
/// `requests.db` but the names come from `main.db`, and only this binary holds
/// both. The old page rendered a bare numeric user id for exactly that reason.
async fn api_consumers(state: &AppState, begin: i64, end: i64) -> Result<Vec<ApiConsumer>, ApiError> {
    let rows = state
        .requests
        .query_map(consumers_sql(), (begin, end, TOP_N), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
        })
        .await?;

    let mut out = Vec::with_capacity(rows.len());
    for (user_id, total, success) in rows {
        // Bounded by TOP_N and `get_account` is cache-backed, so this stays cheap.
        let name = state.get_account(user_id).await.map(|a| a.name);
        out.push(ApiConsumer {
            user_id,
            name,
            total,
            success,
            failed: total - success,
        });
    }
    Ok(out)
}

// ─── Handlers ───────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "auth/account/insights.html")]
pub struct InsightsTemplate {
    account: Option<Account>,
    flashes: Flashes,
    active_page: &'static str,
}

pub async fn page(flashes: Flashes, account: Account) -> Result<InsightsTemplate, StatusCode> {
    if !account.flags.is_admin() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(InsightsTemplate {
        account: Some(account),
        flashes,
        active_page: "insights",
    })
}

pub async fn data(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    account: Account,
    Query(query): Query<RangeQuery>,
) -> Result<Json<InsightsData>, ApiError> {
    if !account.flags.is_admin() {
        return Err(ApiError::forbidden());
    }
    let (begin, end) = window(query.range.as_deref());

    let (summary, routes_all, referrers, api_routes) = tokio::try_join!(
        summary(&state, begin, end),
        routes(&state, begin, end, false),
        referrers(&state, begin, end),
        routes(&state, begin, end, true),
    )?;
    // Sequential: it resolves account names off the shared DB pool per row.
    let api_consumers = api_consumers(&state, begin, end).await?;

    // Aggregates only — no IPs or user-agents in the response — but this is
    // still a privileged read of who used the site, so record that it happened.
    // Once per call, not per row.
    state
        .audit("insights.read")
        .actor(&account)
        .ip_opt(client_ip)
        .meta(serde_json::json!({
            "range":    query.range.as_deref().unwrap_or("today"),
            "from_ts":  begin,
            "to_ts":    end,
            "requests": summary.requests,
        }))
        .fire();

    Ok(Json(InsightsData {
        summary,
        routes: routes_all,
        referrers,
        api_routes,
        api_consumers,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `requests.db` — the actual migrations, not a hand-written schema,
    /// so a column the page reads can't drift away from the one the logger writes.
    fn seeded_db() -> rusqlite::Connection {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::migrations::run(&mut conn, &crate::logging::REQUESTS_MIGRATIONS, "schema_migrations").unwrap();

        /// (ts, status, path, route, user_id, referrer, latency)
        type Row = (
            i64,
            u16,
            &'static str,
            Option<&'static str>,
            Option<i64>,
            Option<&'static str>,
            f64,
        );

        let rows: &[Row] = &[
            // Two hits on one route pattern via different paths: they must collapse.
            (
                100,
                200,
                "/p/abc",
                Some("/p/:id"),
                None,
                Some("https://news.ycombinator.com/x"),
                0.010,
            ),
            (
                110,
                200,
                "/p/def",
                Some("/p/:id"),
                None,
                Some("https://klappstuhl.me/"),
                0.030,
            ),
            (120, 404, "/nope", None, None, None, 0.020),
            // Static assets are excluded from every panel.
            (
                130,
                200,
                "/static/css/account.css",
                Some("/static/*path"),
                None,
                None,
                0.001,
            ),
            // API traffic, two users, one failure.
            (140, 200, "/api/images", Some("/api/images"), Some(7), None, 0.040),
            (150, 500, "/api/images", Some("/api/images"), Some(7), None, 0.060),
            (160, 200, "/api/links", Some("/api/links"), Some(9), None, 0.020),
            // Outside the window on both sides.
            (10, 200, "/early", Some("/early"), Some(7), None, 0.010),
            (900, 200, "/late", Some("/late"), Some(7), None, 0.010),
        ];
        for (ts, status, path, route, user_id, referrer, latency) in rows {
            conn.execute(
                "INSERT INTO request(ts, status_code, path, route, user_id, referrer, latency)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![ts, status, path, route, user_id, referrer, latency],
            )
            .unwrap();
        }
        conn
    }

    /// The window is `[100, 200]`: it holds every row above except `/early` and
    /// `/late`, which sit just outside each edge.
    const WINDOW: (i64, i64) = (100, 200);

    #[test]
    fn summary_counts_the_window_and_skips_static() {
        let conn = seeded_db();
        let (requests, users, avg_latency, successes): (i64, i64, f64, i64) = conn
            .query_row(&summary_sql(), WINDOW, |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();

        // 7 rows in the window, minus the static asset.
        assert_eq!(requests, 6);
        // Users 7 and 9; the NULLs don't count as a user.
        assert_eq!(users, 2);
        // 4 of the 6 are 2xx/3xx (the 404 and the 500 are not).
        assert_eq!(successes, 4);
        // Mean of 10/30/20/40/60/20 ms, stored in seconds.
        assert!((avg_latency - 0.030).abs() < 1e-9, "got {avg_latency}");
    }

    #[test]
    fn routes_group_on_the_pattern_and_fall_back_to_the_path() {
        let conn = seeded_db();
        let mut stmt = conn.prepare(&routes_sql(false)).unwrap();
        let rows: Vec<(String, i64)> = stmt
            .query_map((WINDOW.0, WINDOW.1, TOP_N), |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();

        // Both /p/ hits collapse into the one pattern — the whole point of
        // grouping on `route` rather than `path`.
        assert_eq!(rows[0], ("/p/:id".to_string(), 2));
        // A routeless request (the 404) still shows, under its raw path.
        assert!(rows.contains(&("/nope".to_string(), 1)));
        // Static never appears.
        assert!(!rows.iter().any(|(label, _)| label.starts_with("/static")));
    }

    #[test]
    fn api_routes_are_limited_to_the_api_prefix() {
        let conn = seeded_db();
        let mut stmt = conn.prepare(&routes_sql(true)).unwrap();
        let rows: Vec<(String, i64)> = stmt
            .query_map((WINDOW.0, WINDOW.1, TOP_N), |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();

        assert_eq!(
            rows,
            vec![("/api/images".to_string(), 2), ("/api/links".to_string(), 1)]
        );
    }

    #[test]
    fn referrers_group_and_keep_self_referrals_for_rust_to_drop() {
        let conn = seeded_db();
        let mut stmt = conn.prepare(&referrers_sql()).unwrap();
        let rows: Vec<(String, i64)> = stmt
            .query_map((WINDOW.0, WINDOW.1, REFERRER_SCAN), |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();

        // The SQL only drops NULL/empty; own-domain filtering is `is_own_host`'s
        // job, so the self-referral must still be here for it to remove.
        assert_eq!(rows.len(), 2);
        let labels: Vec<&str> = rows.iter().map(|(r, _)| r.as_str()).collect();
        assert!(labels.contains(&"https://klappstuhl.me/"));
        assert!(labels.contains(&"https://news.ycombinator.com/x"));
    }

    #[test]
    fn consumers_split_success_from_failure_per_user() {
        let conn = seeded_db();
        let mut stmt = conn.prepare(&consumers_sql()).unwrap();
        let rows: Vec<(i64, i64, i64)> = stmt
            .query_map((WINDOW.0, WINDOW.1, TOP_N), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();

        // User 7: two /api calls in-window, one 200 one 500. Their /early and
        // /late rows are outside the window and aren't /api anyway.
        assert_eq!(rows[0], (7, 2, 1));
        assert_eq!(rows[1], (9, 1, 1));
    }

    #[test]
    fn host_of_extracts_the_authority() {
        assert_eq!(
            host_of("https://news.ycombinator.com/item?id=1"),
            Some("news.ycombinator.com".into())
        );
        assert_eq!(host_of("http://Example.COM"), Some("example.com".into()));
        assert_eq!(host_of("https://user:pw@host.dev:8443/x"), Some("host.dev".into()));
        // Referrers are attacker-supplied; anything unparseable is dropped, not guessed.
        assert_eq!(host_of("not a url"), None);
        assert_eq!(host_of("https://"), None);
        assert_eq!(host_of("/relative/path"), None);
    }

    #[test]
    fn own_hosts_cover_subdomains_but_not_lookalikes() {
        let domains = vec!["klappstuhl.me".to_string()];
        assert!(is_own_host("klappstuhl.me", &domains));
        assert!(is_own_host("percy.klappstuhl.me", &domains));
        // The suffix check must not match a domain that merely ends the same way.
        assert!(!is_own_host("notklappstuhl.me", &domains));
        assert!(!is_own_host("klappstuhl.me.evil.com", &domains));
    }

    #[test]
    fn search_engines_collapse_to_a_name() {
        assert_eq!(search_engine("www.google.de"), Some("Google"));
        assert_eq!(search_engine("google.com"), Some("Google"));
        assert_eq!(search_engine("duckduckgo.com"), Some("DuckDuckGo"));
        assert_eq!(search_engine("github.com"), None);
        // Substring, not suffix: a host that just contains "google" isn't Google.
        assert_eq!(search_engine("googleblog.example.com"), None);
    }

    #[test]
    fn window_defaults_to_today_and_rejects_junk() {
        let (today_begin, today_end) = window(None);
        assert!(today_begin <= today_end);
        // Unknown keys degrade to today rather than erroring.
        assert_eq!(window(Some("bogus")).0, today_begin);

        // Longer ranges reach further back, and never past the 45-day retention.
        let (week, _) = window(Some("7d"));
        let (month, _) = window(Some("30d"));
        assert!(month < week);
        assert!(today_end - month < 46 * 86_400_000);
    }
}
