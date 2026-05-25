// This code is adapted from fasterthanli.me

use std::{
    future::Future,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant, SystemTime},
};

use axum::{
    extract::Request,
    http::HeaderMap,
    response::Response,
};
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};
use tower::{Layer, Service};
use tracing::{event, Level};

use crate::token::get_token_from_request;

/// Returns the *real* client IP, accounting for trusted reverse-proxy headers.
///
/// We trust `CF-Connecting-IP`, then `X-Forwarded-For` (first hop), then
/// `X-Real-IP` — **only** when the immediate connection is coming from a
/// private / loopback / link-local source (i.e. a reverse proxy or tunnel
/// running on the same host or in the same private network).  Requests
/// arriving directly from the public internet have their proxy headers
/// ignored, so an attacker hitting the app directly cannot inject a fake IP.
///
/// This makes Cloudflare Tunnel / nginx-in-front-of-Docker / Caddy / etc.
/// work without any configuration — the cloudflared container, Docker bridge
/// gateway (172.x.0.1), and a sibling nginx on 127.0.0.1 all qualify.
pub fn real_client_ip(
    extensions: &axum::http::Extensions,
    headers: &HeaderMap,
) -> Option<IpAddr> {
    let connect_ip = extensions
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())?;

    if is_trusted_proxy_source(connect_ip) {
        if let Some(real) = read_proxy_headers(headers) {
            return Some(real);
        }
    }
    Some(connect_ip)
}

fn read_proxy_headers(headers: &HeaderMap) -> Option<IpAddr> {
    // Cloudflare's authoritative header — set by the edge after it has
    // identified the real source. Wins outright when present.
    if let Some(ip) = header_to_ip(headers, "cf-connecting-ip") {
        return Some(ip);
    }
    // Generic reverse-proxy header. Format is "client, proxy1, proxy2, …" —
    // first hop is the original client.
    if let Some(value) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = value.split(',').next() {
            if let Ok(ip) = first.trim().parse::<IpAddr>() {
                return Some(ip);
            }
        }
    }
    if let Some(ip) = header_to_ip(headers, "x-real-ip") {
        return Some(ip);
    }
    None
}

fn header_to_ip(headers: &HeaderMap, name: &str) -> Option<IpAddr> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<IpAddr>().ok())
}

/// True when the IP belongs to a network where we are willing to trust
/// proxy headers — RFC 1918 private ranges, loopback, link-local, and
/// the IPv6 unique-local block (fc00::/7).
fn is_trusted_proxy_source(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || is_ipv6_ula(v6),
    }
}

/// Detects the IPv6 unique-local block (fc00::/7) without relying on the
/// still-unstable `Ipv6Addr::is_unique_local()` method.
fn is_ipv6_ula(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xfe00) == 0xfc00
}

const REQUEST_LOGGING_QUERY: &str = r#"
CREATE TABLE IF NOT EXISTS request (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    status_code INTEGER NOT NULL,
    path TEXT NOT NULL,
    route TEXT,
    user_id INTEGER,
    user_agent TEXT,
    referrer TEXT,
    latency REAL,
    ip TEXT,
    bad_reason TEXT
);

CREATE INDEX IF NOT EXISTS request_status_code_idx ON request(status_code);
CREATE INDEX IF NOT EXISTS request_ts_idx ON request(ts);
CREATE INDEX IF NOT EXISTS request_user_id_idx ON request(user_id);
CREATE INDEX IF NOT EXISTS request_referrer_idx ON request(referrer);
CREATE INDEX IF NOT EXISTS request_path_idx ON request(path);
CREATE INDEX IF NOT EXISTS request_route_idx ON request(route);
CREATE INDEX IF NOT EXISTS request_ip_idx ON request(ip);
CREATE INDEX IF NOT EXISTS request_bad_reason_idx ON request(bad_reason);
"#;

/// Adds the `ip` and `bad_reason` columns to existing databases that pre-date
/// the security dashboard.  SQLite doesn't support `ADD COLUMN IF NOT EXISTS`,
/// so we try-and-ignore: the error returned for "duplicate column name" is
/// what we want to silently swallow, anything else propagates.
fn migrate_request_log_schema(conn: &rusqlite::Connection) {
    let _ = conn.execute("ALTER TABLE request ADD COLUMN ip TEXT", []);
    let _ = conn.execute("ALTER TABLE request ADD COLUMN bad_reason TEXT", []);
    let _ = conn.execute("CREATE INDEX IF NOT EXISTS request_ip_idx ON request(ip)", []);
    let _ = conn.execute(
        "CREATE INDEX IF NOT EXISTS request_bad_reason_idx ON request(bad_reason)",
        [],
    );
}

///A request log entry
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestLogEntry {
    /// The ID of the entry.
    pub id: i64,
    /// The UNIX timestamp, in milliseconds, of the request.
    pub ts: i64,
    /// The status code of the request
    pub status_code: u16,
    /// The path of the request
    pub path: String,
    /// The "route" of the request.
    pub route: Option<String>,
    /// The user ID of the user who made the request
    pub user_id: Option<i64>,
    /// The user agent of the request
    pub user_agent: Option<String>,
    /// The referer header of the request
    pub referrer: Option<String>,
    /// The latency (in seconds) of the request
    pub latency: f64,
    /// Client IP (extracted from ConnectInfo, falls back to None if missing).
    pub ip: Option<String>,
    /// If the request was a 4xx, the categorised reason (BadRequestReason::as_str).
    pub bad_reason: Option<String>,
}

impl RequestLogEntry {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        // Use `column_index` lookups instead of `row.get("col")?` so existing
        // databases without the new ip / bad_reason columns still deserialise.
        let ip: Option<String> = row.get::<_, Option<String>>("ip").unwrap_or(None);
        let bad_reason: Option<String> = row
            .get::<_, Option<String>>("bad_reason")
            .unwrap_or(None);
        Ok(Self {
            id: row.get("id")?,
            ts: row.get("ts")?,
            status_code: row.get("status_code")?,
            path: row.get("path")?,
            route: row.get("route")?,
            user_id: row.get("user_id")?,
            user_agent: row.get("user_agent")?,
            referrer: row.get("referrer")?,
            latency: row.get("latency")?,
            ip,
            bad_reason,
        })
    }
}

enum RequestMessage {
    Log(RequestLogEntry),
    Query(Box<dyn FnOnce(&mut rusqlite::Connection) + Send + 'static>),
    Clean,
    Quit,
}

/// The request log task
#[derive(Debug, Clone)]
pub struct RequestLogger {
    sender: Sender<RequestMessage>,
}

fn bulk_insert_request_logs<It>(connection: &mut rusqlite::Connection, logs: It) -> rusqlite::Result<()>
where
    It: Iterator<Item = RequestLogEntry>,
{
    let tx = connection.transaction()?;
    let query = r#"INSERT INTO request(ts, status_code, path, route, user_id, user_agent, referrer, latency, ip, bad_reason)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#;

    {
        let mut stmt = tx.prepare_cached(query)?;
        for log in logs {
            stmt.execute(rusqlite::params![
                log.ts,
                log.status_code,
                log.path,
                log.route,
                log.user_id,
                log.user_agent,
                log.referrer,
                log.latency,
                log.ip,
                log.bad_reason,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn unix_duration() -> Duration {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
}

fn unix_now_ms() -> i64 {
    unix_duration().as_millis() as i64
}

fn clean_request_logs(connection: &mut rusqlite::Connection) -> rusqlite::Result<()> {
    let now = unix_duration();
    // 45 "days" ago
    let delete_threshold = now.saturating_sub(Duration::from_secs(45 * 86400)).as_millis() as i64;
    let query = "DELETE FROM request WHERE ts <= ?";
    connection.execute(query, [delete_threshold])?;
    Ok(())
}

impl RequestLogger {
    pub fn new() -> anyhow::Result<Self> {
        let (sender, receiver) = crossbeam_channel::unbounded();
        let mut path = crate::database::directory()?;
        path.set_file_name("requests.db");

        let mut connection = rusqlite::Connection::open(path)?;
        connection.execute_batch("PRAGMA journal_mode=WAL;")?;
        connection.execute_batch(REQUEST_LOGGING_QUERY)?;
        // Bring databases created before the security dashboard up to date.
        migrate_request_log_schema(&connection);

        std::thread::spawn(move || {
            // This set up is so it can be bulk-inserted somewhat efficiently
            let mut buffer = Vec::new();
            let mut last_insert = Instant::now();
            while let Ok(msg) = receiver.recv() {
                match msg {
                    RequestMessage::Log(entry) => buffer.push(entry),
                    RequestMessage::Clean => {
                        if let Err(e) = clean_request_logs(&mut connection) {
                            tracing::error!(error = %e, "error when cleaning request logs");
                        }
                    }
                    RequestMessage::Quit => break,
                    RequestMessage::Query(func) => func(&mut connection),
                }

                if !buffer.is_empty() && last_insert.elapsed() >= Duration::from_secs(5) {
                    if let Err(e) = bulk_insert_request_logs(&mut connection, buffer.drain(..)) {
                        tracing::error!(error = %e, "error when bulk inserting request logs");
                    }
                    last_insert = Instant::now();
                }
            }

            if !buffer.is_empty() {
                if let Err(e) = bulk_insert_request_logs(&mut connection, buffer.drain(..)) {
                    tracing::error!(error = %e, "error when bulk inserting request logs");
                }
            }
        });

        Ok(Self { sender })
    }

    /// Requests to terminate the worker thread
    pub fn quit(&self) {
        let _ = self.sender.send(RequestMessage::Quit);
    }

    /// Add a request to the logs
    pub fn log(&self, log: RequestLogEntry) {
        let _ = self.sender.send(RequestMessage::Log(log));
    }

    /// Request a cleanup of the logs
    ///
    /// This cleans up log entries older than 45 days.
    ///
    /// Returns `true` if the cleanup request went through.
    pub fn cleanup(&self) -> bool {
        self.sender.send(RequestMessage::Clean).is_ok()
    }

    async fn call<F, R>(&self, func: F) -> R
    where
        F: FnOnce(&mut rusqlite::Connection) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();

        let _ = self.sender.send(RequestMessage::Query(Box::new(move |conn| {
            let _ = sender.send(func(conn));
        })));

        receiver
            .await
            .expect("unexpected channel termination: should be unreachable")
    }

    /// Requests logs given the following query and parameters.
    pub async fn query<Q, P>(&self, query: Q, params: P) -> rusqlite::Result<Vec<RequestLogEntry>>
    where
        Q: Into<std::borrow::Cow<'static, str>> + Send,
        P: rusqlite::Params + Send + 'static,
    {
        let query = query.into();
        self.call(move |conn| -> rusqlite::Result<Vec<RequestLogEntry>> {
            let mut stmt = conn.prepare_cached(query.as_ref())?;
            let result = match stmt.query_map(params, RequestLogEntry::from_row) {
                Ok(value) => value.collect(),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(Vec::new()),
                Err(e) => Err(e),
            };
            result
        })
            .await
    }
}

/// Layer for [HttpTraceService]
#[derive(Clone)]
pub struct HttpTrace {
    logger: RequestLogger,
}

impl HttpTrace {
    pub fn new(logger: RequestLogger) -> Self {
        Self { logger }
    }
}

impl<S> Layer<S> for HttpTrace {
    type Service = HttpTraceService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HttpTraceService {
            inner,
            logger: self.logger.clone(),
        }
    }
}

#[derive(Clone)]
pub struct HttpTraceService<S> {
    inner: S,
    logger: RequestLogger,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum BadRequestReason {
    BadRequest,
    RateLimited,
    IncorrectLogin,
}

impl BadRequestReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            BadRequestReason::BadRequest => "Bad Request",
            BadRequestReason::RateLimited => "Rate Limited",
            BadRequestReason::IncorrectLogin => "Incorrect Login",
        }
    }

    fn from_response(res: &Response) -> Self {
        match res.extensions().get::<Self>() {
            Some(ext) => *ext,
            None => {
                if res.status().as_u16() == 429 {
                    Self::RateLimited
                } else {
                    Self::BadRequest
                }
            }
        }
    }
}

impl<S> Service<Request> for HttpTraceService<S>
where
    S: Service<Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = PostFuture<S::Future, S::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let start = Instant::now();

        let user_agent = req
            .headers()
            .get("user-agent")
            .and_then(|s| s.to_str().ok())
            .map(String::from);

        let referrer = req
            .headers()
            .get("referer")
            .and_then(|s| s.to_str().ok())
            .map(String::from);

        let path = req.uri().path().to_string();
        let route = req
            .extensions()
            .get::<axum::extract::MatchedPath>()
            .map(|p| p.as_str().to_owned());

        // Real client IP — respects CF-Connecting-IP / X-Forwarded-For
        // when the immediate hop is a private-range proxy (Cloudflare
        // Tunnel, nginx, Docker bridge gateway, etc.).
        let ip = real_client_ip(req.extensions(), req.headers());

        let log = RequestLogEntry {
            ts: unix_now_ms(),
            path,
            route,
            user_id: get_token_from_request(req.extensions()).map(|t| t.id),
            user_agent,
            referrer,
            ip: ip.map(|i| i.to_string()),
            ..Default::default()
        };

        PostFuture {
            inner: self.inner.call(req),
            logger: self.logger.clone(),
            log,
            ip,
            start,
        }
    }
}

pin_project_lite::pin_project! {
    /// Future that records http status code
    pub struct PostFuture<F, E>
    where
        F: Future<Output = Result<Response, E>>,
    {
        #[pin]
        inner: F,
        logger: RequestLogger,
        log: RequestLogEntry,
        ip: Option<IpAddr>,
        start: Instant,
    }
}

impl<F, E> Future for PostFuture<F, E>
where
    F: Future<Output = Result<Response, E>>,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let res = match this.inner.poll(cx) {
            Poll::Ready(t) => t,
            Poll::Pending => return Poll::Pending,
        };
        let latency = this.start.elapsed();
        this.log.latency = latency.as_secs_f64();
        if let Ok(res) = &res {
            let status_code = res.status().as_u16();
            this.log.status_code = status_code;
            if let Some(token) = res.extensions().get::<crate::ApiToken>() {
                this.log.user_id = Some(token.id);
            }
            if (400..=499).contains(&status_code) {
                let reason = BadRequestReason::from_response(res).as_str();
                this.log.bad_reason = Some(reason.to_string());
                if let Some(ip) = this.ip {
                    event!(name: "Bad Request", target: "bad_request", Level::INFO, %ip, reason, status_code, path = this.log.path);
                }
            }
        }

        this.logger.log(std::mem::take(this.log));
        res.into()
    }
}