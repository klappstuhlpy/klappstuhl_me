use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    extract::{FromRequest, Request},
    response::{IntoResponse, Response},
    Json,
};
use serde::{de::DeserializeOwned, Deserialize};

use crate::error::ApiError;

/// Returns true if `ip` is loopback, private, link-local, or otherwise
/// reserved — i.e. an address an SSRF attacker might target. Shared by every
/// endpoint that fetches a caller-supplied URL server-side.
pub(crate) fn ip_is_blocked(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.octets()[0] == 0
                // Carrier-grade NAT, 100.64.0.0/10
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 0x40)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // Unique local, fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local, fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .map(|v4| ip_is_blocked(&IpAddr::V4(v4)))
                    .unwrap_or(false)
        }
    }
}

/// The body returned by [`fetch_guarded`].
pub(crate) struct GuardedBody {
    /// The raw response bytes (size-capped).
    pub bytes: Vec<u8>,
    /// The response `Content-Type` header, lower-cased, if present.
    pub content_type: Option<String>,
}

/// Performs an SSRF-guarded `GET`: http(s) only, every resolved address must be
/// public, redirects are disabled (so a redirect can't bounce past the address
/// check), a short timeout applies, and the body is capped at `max_bytes`.
pub(crate) async fn fetch_guarded(url_str: &str, max_bytes: usize, user_agent: &str) -> Result<GuardedBody, ApiError> {
    let url = reqwest::Url::parse(url_str).map_err(|_| ApiError::new("invalid url"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ApiError::new("url must use http or https"));
    }
    let host = url.host_str().ok_or_else(|| ApiError::new("url has no host"))?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err(ApiError::new("refusing to fetch a local address"));
    }

    // Resolve up front and verify every candidate address is public. This
    // defends against hostnames that point at internal infrastructure.
    let port = url.port_or_known_default().unwrap_or(80);
    let mut resolved = false;
    for addr in tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| ApiError::new("could not resolve url host"))?
    {
        resolved = true;
        if ip_is_blocked(&addr.ip()) {
            return Err(ApiError::new("refusing to fetch a private or reserved address"));
        }
    }
    if !resolved {
        return Err(ApiError::new("could not resolve url host"));
    }

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(ApiError::from)?;

    let resp = client
        .get(url)
        .header(reqwest::header::USER_AGENT, user_agent.to_string())
        .send()
        .await
        .map_err(|e| ApiError::new(format!("fetch failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(ApiError::new(format!(
            "remote returned HTTP {}",
            resp.status().as_u16()
        )));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase());

    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::new(format!("fetch failed: {e}")))?;
        if bytes.len() + chunk.len() > max_bytes {
            return Err(ApiError::new("remote response exceeds the size limit"));
        }
        bytes.extend_from_slice(&chunk);
    }

    Ok(GuardedBody { bytes, content_type })
}

/// Discord-style cursor pagination query parameters, shared by every list
/// endpoint. `before`/`after` are opaque cursors (a resource id); results are
/// returned newest-first, so `after` walks towards older items and `before`
/// towards newer ones. `limit` is clamped into [`Page::MIN_LIMIT`,
/// [`Page::MAX_LIMIT`]] and defaults to [`Page::DEFAULT_LIMIT`].
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct Page {
    /// Maximum number of items to return (clamped to 1..=200, default 50).
    pub limit: Option<u32>,
    /// Return items strictly newer than this cursor id.
    pub before: Option<String>,
    /// Return items strictly older than this cursor id.
    pub after: Option<String>,
}

impl Page {
    pub const DEFAULT_LIMIT: u32 = 50;
    pub const MIN_LIMIT: u32 = 1;
    pub const MAX_LIMIT: u32 = 200;

    /// The clamped, safe page size to use in a `LIMIT` clause.
    pub fn effective_limit(&self) -> u32 {
        self.limit
            .unwrap_or(Self::DEFAULT_LIMIT)
            .clamp(Self::MIN_LIMIT, Self::MAX_LIMIT)
    }
}

pub struct ApiJson<T>(pub T);

#[async_trait]
impl<S, T> FromRequest<S> for ApiJson<T>
where
    // these trait bounds are copied from `impl FromRequest for axum::extract::path::Path`
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(value) => Ok(Self(value.0)),
            Err(rejection) => Err(ApiError::new(rejection.to_string())),
        }
    }
}

impl<T> IntoResponse for ApiJson<T>
where
    Json<T>: IntoResponse,
{
    fn into_response(self) -> Response {
        Json(self.0).into_response()
    }
}

/// Rate limit exceeded. The JSON body is Discord-shaped
/// (`{ message, retry_after, global, code }`).
#[allow(dead_code)]
#[derive(utoipa::ToResponse)]
#[response(headers(
    ("x-ratelimit-limit" = u16, description = "The number of requests you can make"),
    ("x-ratelimit-remaining" = u16, description = "The number of requests remaining"),
    ("x-ratelimit-reset" = f32, description = "The time, in UNIX timestamp seconds, when you can make requests again. Note this has a fractional component for milliseconds."),
    ("x-ratelimit-reset-after" = f32, description = "The number of seconds before you can try again. Note this has a fractional component for milliseconds."),
    ("x-ratelimit-scope" = String, description = "What the limit is keyed on (e.g. `ip`, `global`)."),
    ("x-ratelimit-bucket" = String, description = "The bucket the limit belongs to, when several are layered."),
    ("retry-after" = u64, description = "RFC 7231 integer seconds to wait before retrying (rounded up)."),
))]
pub struct RateLimitResponse(#[to_schema] ApiError);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_addresses_are_blocked() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "::1",
            "fe80::1",
            "100.64.0.1",
        ] {
            assert!(ip_is_blocked(&ip.parse::<IpAddr>().unwrap()), "{ip} should be blocked");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!ip_is_blocked(&ip.parse::<IpAddr>().unwrap()), "{ip} should be allowed");
        }
    }

    #[test]
    fn page_limit_is_clamped() {
        let page = Page {
            limit: Some(9999),
            before: None,
            after: None,
        };
        assert_eq!(page.effective_limit(), Page::MAX_LIMIT);
        assert_eq!(Page::default().effective_limit(), Page::DEFAULT_LIMIT);
        let zero = Page {
            limit: Some(0),
            before: None,
            after: None,
        };
        assert_eq!(zero.effective_limit(), Page::MIN_LIMIT);
    }
}
