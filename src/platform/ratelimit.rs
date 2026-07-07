#![allow(clippy::declare_interior_mutable_const)]

use axum::{
    extract::Request,
    http::{header::RETRY_AFTER, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::future::Either;
use quick_cache::sync::Cache;
use serde::Serialize;

use std::{
    future::{ready, Future, Ready},
    hash::Hash,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tower::{Layer, Service};

use crate::error::ApiErrorCode;

const X_RATELIMIT_LIMIT: HeaderName = HeaderName::from_static("x-ratelimit-limit");
const X_RATELIMIT_REMAINING: HeaderName = HeaderName::from_static("x-ratelimit-remaining");
const X_RATELIMIT_RESET: HeaderName = HeaderName::from_static("x-ratelimit-reset");
const X_RATELIMIT_RESET_AFTER: HeaderName = HeaderName::from_static("x-ratelimit-reset-after");
const X_RATELIMIT_SCOPE: HeaderName = HeaderName::from_static("x-ratelimit-scope");
const X_RATELIMIT_BUCKET: HeaderName = HeaderName::from_static("x-ratelimit-bucket");

/// The JSON body returned on a 429, shaped after Discord's rate-limit response
/// so client libraries can read a precise `retry_after` (fractional seconds)
/// alongside the RFC 7231 `Retry-After` header.
#[derive(Serialize)]
struct RateLimitedBody {
    message: &'static str,
    /// Seconds to wait before retrying (fractional).
    retry_after: f32,
    /// Whether this is a global limit (vs. a per-bucket one). Always `false`
    /// here — every klappstuhl limit is scoped to a key (IP/global-route).
    global: bool,
    /// Machine-readable error code, matching [`crate::error::ApiErrorCode`].
    code: u8,
}

fn diff_seconds(a: SystemTime, b: SystemTime) -> f32 {
    let a = a.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();
    let b = b.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();
    (a - b) as f32
}

/// Implements rate limiting using the GCRA algorithm.
///
/// Note that axum clones this *every* request.
#[derive(Clone)]
pub struct RateLimitLayer<T: KeyExtractor> {
    lookup: Arc<Cache<T::Key, SystemTime>>,
    rate: u16,
    per: f32,
    extractor: T,
    /// Optional bucket label surfaced as the `X-RateLimit-Bucket` header, so a
    /// client can tell which limit it hit when several are layered.
    bucket: Option<&'static str>,
}

/// A trait that describes the rate limit policy
pub trait KeyExtractor: Clone {
    /// The underlying type of the key
    type Key: Hash + Eq + Clone + Send + Sync + 'static;

    /// Extracts the lookup key from a request.
    ///
    /// If no key is found for this request then `None` should be returned.
    fn extract(&self, req: &Request) -> Option<Self::Key>;

    /// A short label describing what the limit is keyed on, surfaced as the
    /// `X-RateLimit-Scope` header (Discord-style: `user`, `global`, …).
    fn scope_label(&self) -> &'static str {
        "ip"
    }
}

#[derive(Debug, Copy, Clone)]
struct RateLimitInfo {
    limit: u16,
    remaining: u16,
    ratelimited: bool,
    reset_time: SystemTime,
    retry_after: f32,
    scope: &'static str,
    bucket: Option<&'static str>,
}

impl<T: KeyExtractor> RateLimitLayer<T> {
    fn emission_interval(&self) -> f32 {
        self.per / self.rate as f32
    }

    fn process(&self, request: &Request) -> RateLimitInfo {
        let emission_interval = self.emission_interval();
        let limit = self.rate;
        let delay_variation_tolerance = self.per;
        let now = SystemTime::now();
        let scope = self.extractor.scope_label();
        let bucket = self.bucket;
        let Some(key) = self.extractor.extract(request) else {
            return RateLimitInfo::banned(scope, bucket);
        };

        let tat = self.lookup.get(&key).unwrap_or(now);
        let new_tat = tat.max(now) + Duration::from_secs_f32(emission_interval);
        let tau = delay_variation_tolerance - emission_interval;

        let allow_at = new_tat - Duration::from_secs_f32(delay_variation_tolerance);
        let diff = diff_seconds(now, allow_at);
        let mut remaining = ((diff / emission_interval) + 0.5).floor() as u16;
        let retry_after = if remaining < 1 {
            remaining = 0;
            emission_interval - diff
        } else {
            0.0
        };

        let ratelimited = now < (tat - Duration::from_secs_f32(tau));
        if !ratelimited {
            self.lookup.insert(key, new_tat);
        }
        RateLimitInfo {
            limit,
            remaining,
            ratelimited,
            reset_time: now + Duration::from_secs_f32(retry_after),
            retry_after,
            scope,
            bucket,
        }
    }
}

impl RateLimitInfo {
    fn is_ratelimited(&self) -> bool {
        self.ratelimited
    }

    fn banned(scope: &'static str, bucket: Option<&'static str>) -> Self {
        Self {
            limit: 0,
            ratelimited: true,
            remaining: 0,
            reset_time: UNIX_EPOCH,
            retry_after: 0.0,
            scope,
            bucket,
        }
    }

    fn modify_headers(&self, resp: &mut Response) {
        let headers = resp.headers_mut();
        headers.insert(
            X_RATELIMIT_LIMIT,
            HeaderValue::from_str(&self.limit.to_string()).unwrap(),
        );
        headers.insert(
            X_RATELIMIT_REMAINING,
            HeaderValue::from_str(&self.remaining.to_string()).unwrap(),
        );
        headers.insert(X_RATELIMIT_SCOPE, HeaderValue::from_static(self.scope));
        if let Some(bucket) = self.bucket {
            headers.insert(X_RATELIMIT_BUCKET, HeaderValue::from_static(bucket));
        }
        if let Ok(epoch) = self.reset_time.duration_since(UNIX_EPOCH) {
            headers.insert(
                X_RATELIMIT_RESET,
                HeaderValue::from_str(&epoch.as_secs_f32().to_string()).unwrap(),
            );
        }

        if self.remaining == 0 {
            headers.insert(
                X_RATELIMIT_RESET_AFTER,
                HeaderValue::from_str(&self.retry_after.to_string()).unwrap(),
            );
            // RFC 7231 `Retry-After` is integer seconds — round up so a client
            // that honours it never retries too early.
            let secs = self.retry_after.ceil().max(0.0) as u64;
            headers.insert(RETRY_AFTER, HeaderValue::from_str(&secs.to_string()).unwrap());
        }
    }
}

impl IntoResponse for RateLimitInfo {
    fn into_response(self) -> Response {
        let body = RateLimitedBody {
            message: "You are being rate limited.",
            retry_after: self.retry_after,
            global: false,
            code: ApiErrorCode::RateLimited as u8,
        };
        let mut resp = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
        self.modify_headers(&mut resp);
        resp
    }
}

#[derive(Clone)]
pub struct RateLimitService<S, T: KeyExtractor> {
    layer: RateLimitLayer<T>,
    inner: S,
}

impl<S, T: KeyExtractor> Layer<S> for RateLimitLayer<T> {
    type Service = RateLimitService<S, T>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService {
            layer: self.clone(),
            inner,
        }
    }
}

pin_project_lite::pin_project! {
    pub struct ModifyHeaders<F, E>
    where
        F: Future<Output = Result<Response, E>>
    {
        #[pin]
        inner: F,
        info: RateLimitInfo,
    }
}

impl<F, E> Future for ModifyHeaders<F, E>
where
    F: Future<Output = Result<Response, E>>,
{
    type Output = F::Output;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let mut res = match this.inner.poll(cx) {
            Poll::Ready(t) => t,
            Poll::Pending => return Poll::Pending,
        };
        if let Ok(resp) = &mut res {
            this.info.modify_headers(resp);
        }
        res.into()
    }
}

impl<S, K> Service<Request> for RateLimitService<S, K>
where
    S: Service<Request, Response = Response> + Send + 'static,
    S::Future: Send + 'static,
    K: KeyExtractor,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Either<ModifyHeaders<S::Future, S::Error>, Ready<Result<Self::Response, Self::Error>>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let info = self.layer.process(&req);
        if info.is_ratelimited() {
            Either::Right(ready(Ok(info.into_response())))
        } else {
            Either::Left(ModifyHeaders {
                inner: self.inner.call(req),
                info,
            })
        }
    }
}

/// A global key extractor for a global rate limit
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct GlobalKeyExtractor;

impl KeyExtractor for GlobalKeyExtractor {
    type Key = ();

    fn extract(&self, _req: &Request) -> Option<Self::Key> {
        Some(())
    }

    fn scope_label(&self) -> &'static str {
        "global"
    }
}

/// A key extractor based on IPs
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct IpKeyExtractor;

impl KeyExtractor for IpKeyExtractor {
    type Key = IpAddr;

    fn extract(&self, req: &Request) -> Option<Self::Key> {
        req.extensions()
            .get::<axum::extract::ConnectInfo<SocketAddr>>()
            .map(|addr| addr.ip())
    }
}

/// A builder for creating [`RateLimitLayer`].
pub struct RateLimit<T: KeyExtractor> {
    max_capacity: usize,
    rate: u16,
    per: f32,
    extractor: T,
    bucket: Option<&'static str>,
}

impl Default for RateLimit<IpKeyExtractor> {
    /// Creates the default rate limit configuration with 5 requests per 5 seconds
    /// using [`IpKeyExtractor`] as the key.
    fn default() -> Self {
        Self {
            max_capacity: 10_000,
            rate: 5,
            per: 5.0,
            extractor: IpKeyExtractor,
            bucket: None,
        }
    }
}

impl<T: KeyExtractor> RateLimit<T> {
    /// Replaces the key extractor used to identify clients.
    pub fn extractor<U: KeyExtractor>(self, key: U) -> RateLimit<U> {
        RateLimit {
            max_capacity: self.max_capacity,
            rate: self.rate,
            per: self.per,
            extractor: key,
            bucket: self.bucket,
        }
    }

    /// Sets the maximum number of unique keys (clients) tracked in memory.
    pub fn max_capacity(mut self, capacity: usize) -> Self {
        self.max_capacity = capacity;
        self
    }

    /// Sets the rate limit quota: `rate` requests allowed per `per` seconds.
    pub fn quota(mut self, rate: u16, per: f32) -> Self {
        self.rate = rate;
        self.per = per;
        self
    }

    /// Labels this limit's bucket, surfaced as `X-RateLimit-Bucket` so clients
    /// can distinguish which of several layered limits they hit.
    pub fn bucket(mut self, bucket: &'static str) -> Self {
        self.bucket = Some(bucket);
        self
    }

    /// Builds the [`RateLimitLayer`] ready to be used as a tower middleware.
    pub fn build(self) -> RateLimitLayer<T> {
        RateLimitLayer {
            lookup: Arc::new(Cache::new(self.max_capacity)),
            rate: self.rate,
            per: self.per,
            extractor: self.extractor,
            bucket: self.bucket,
        }
    }
}
