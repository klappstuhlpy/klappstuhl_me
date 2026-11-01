//! Signed flash messages carried across a redirect in a short-lived cookie.
//!
//! A self-contained, state-agnostic piece of web glue shared by every consumer:
//! the extractors pull [`Flasher`] /
//! [`Flashes`] out of request extensions, and [`process_flash_messages`] signs
//! the outgoing messages with the request's [`SecretKey`]. The cookie is signed
//! with the same HMAC kernel as the auth token, so nothing here is app-specific.

use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::{FromRequestParts, Request},
    http::{header::SET_COOKIE, request::Parts, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use cookie::Cookie;
use serde::{Deserialize, Serialize};

use crate::key::SecretKey;

// Clippy does not understand Bytes layout (https://github.com/rust-lang/rust-clippy/issues/5812)
#[allow(clippy::declare_interior_mutable_const)]
const REMOVE_FLASH_MESSAGES: HeaderValue =
    HeaderValue::from_static("flash_messages=; Path=/; HttpOnly; SameSite=Lax; Expires=Thu, 01 Jan 1970 00:00:00 GMT");

/// A container responsible for handling flash messages
#[derive(Debug, Clone, Default)]
pub struct Flasher {
    data: Arc<Mutex<Vec<FlashMessage>>>,
}

/// A read-only container for received flash messages from a prior request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flashes(Vec<FlashMessage>);

impl std::ops::Deref for Flashes {
    type Target = [FlashMessage];

    fn deref(&self) -> &Self::Target {
        self.0.as_slice()
    }
}

/// The data that represents an actual flash message.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Default, Clone)]
pub struct FlashMessage {
    /// The content of the message.
    #[serde(rename = "c")]
    pub content: String,
    /// The level of the message
    #[serde(rename = "l")]
    pub level: FlashLevel,
}

impl FlashMessage {
    /// Creates a new flash message with the given content and level.
    pub fn new(content: impl Into<String>, level: FlashLevel) -> Self {
        Self {
            content: content.into(),
            level,
        }
    }

    /// Changes the flash message level.
    pub fn with_level(mut self, level: FlashLevel) -> Self {
        self.level = level;
        self
    }

    /// Creates a new flash message with the given content with Info level.
    pub fn info(content: impl Into<String>) -> Self {
        Self::new(content, FlashLevel::Info)
    }

    /// Creates a new flash message with the given content with Success level.
    pub fn success(content: impl Into<String>) -> Self {
        Self::new(content, FlashLevel::Success)
    }

    /// Creates a new flash message with the given content with Warning level.
    pub fn warning(content: impl Into<String>) -> Self {
        Self::new(content, FlashLevel::Warning)
    }

    /// Creates a new flash message with the given content with Error level.
    pub fn error(content: impl Into<String>) -> Self {
        Self::new(content, FlashLevel::Error)
    }

    pub fn html(&self) -> impl std::fmt::Display + '_ {
        FlashMessageHtml { message: self }
    }
}

impl<'a> From<&'a str> for FlashMessage {
    fn from(value: &'a str) -> Self {
        Self {
            content: value.to_owned(),
            level: FlashLevel::Error,
        }
    }
}

impl From<String> for FlashMessage {
    fn from(content: String) -> Self {
        Self {
            content,
            level: FlashLevel::Error,
        }
    }
}

struct FlashMessageHtml<'a> {
    message: &'a FlashMessage,
}

impl<'a> std::fmt::Display for FlashMessageHtml<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, r#"<div class="alert-flash {}" role="alert">"#, self.message.level)?;
        writeln!(
            f,
            r#"<p>{}</p>"#,
            askama::MarkupDisplay::new_unsafe(&self.message.content, askama::Html)
        )?;
        writeln!(
            f,
            r#"<button type="button" aria-hidden=true class="close" onclick="closeAlert(event)"></button>"#
        )?;
        writeln!(f, r"</div>")?;
        Ok(())
    }
}

/// The level for the flash message.
#[derive(Debug, Default, PartialEq, Eq, Copy, Clone)]
#[repr(u8)]
pub enum FlashLevel {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

impl std::fmt::Display for FlashLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Success => write!(f, "success"),
            Self::Warning => write!(f, "warning"),
            Self::Error => write!(f, "error"),
        }
    }
}

impl Serialize for FlashLevel {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u8(*self as u8)
    }
}

impl<'de> Deserialize<'de> for FlashLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let level: u8 = Deserialize::deserialize(deserializer)?;
        match level {
            0 => Ok(FlashLevel::Info),
            1 => Ok(FlashLevel::Success),
            2 => Ok(FlashLevel::Warning),
            3 => Ok(FlashLevel::Error),
            _ => Err(serde::de::Error::custom("invalid flash level")),
        }
    }
}

impl Serialize for Flasher {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let inner = self.data.as_ref();
        inner.serialize(serializer)
    }
}

impl Flasher {
    pub fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Adds a flash message.
    pub fn add(&self, message: impl Into<FlashMessage>) -> &Self {
        let mut guard = self.data.lock().unwrap();
        guard.push(message.into());
        self
    }

    /// Creates a redirect response.
    ///
    /// This is mainly for fluent-style chaining.
    pub fn bail(&self, url: impl AsRef<str>) -> Response {
        Redirect::to(url.as_ref()).into_response()
    }

    /// Returns the cookie for these flash messages.
    ///
    /// If no flash messages are found or an error happened, then this returns `None`.
    fn to_cookie(&self, key: &SecretKey) -> Option<Cookie<'static>> {
        let messages = self.data.lock().unwrap();
        if messages.is_empty() {
            None
        } else {
            let cookie = Cookie::build(("flash_messages", key.sign(&messages.as_slice()).ok()?))
                .same_site(cookie::SameSite::Lax)
                .expires(cookie::Expiration::DateTime(
                    time::OffsetDateTime::now_utc() + Duration::from_secs(3600),
                ))
                .path("/")
                .http_only(true)
                .build();
            Some(cookie)
        }
    }
}

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for Flasher
where
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Flasher>()
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)
            .cloned()
    }
}

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for Flashes
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(parts.extensions.get::<Flashes>().cloned().unwrap_or_default())
    }
}

/// A middleware that implements the actual flashing messages
pub async fn process_flash_messages(mut request: Request, next: Next) -> Response {
    let key = request
        .extensions()
        .get::<SecretKey>()
        .copied()
        .expect("missing secret key");
    let flashes = request
        .extensions()
        .get::<Vec<Cookie>>()
        .and_then(|cookies| cookies.iter().find(|c| c.name() == "flash_messages"))
        .and_then(|cookie| key.verify(cookie.value()).map(Flashes))
        .unwrap_or_default();

    let had_messages = !flashes.is_empty();
    let flasher = Flasher::new();
    request.extensions_mut().insert(flashes);
    request.extensions_mut().insert(flasher.clone());

    let mut response = next.run(request).await;

    // `append`, never `insert`: a handler that flashes a message *and* sets a
    // cookie (signup's auto-login, a password change handing back a new session,
    // the session teardown on account deletion) has already put its own
    // `Set-Cookie` on this response. `insert` would replace it and the login
    // would be silently dropped.
    if let Some(cookie) = flasher.to_cookie(&key) {
        response
            .headers_mut()
            .append(SET_COOKIE, HeaderValue::from_str(&cookie.to_string()).unwrap());
    } else if had_messages {
        response.headers_mut().append(SET_COOKIE, REMOVE_FLASH_MESSAGES);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get, Extension, Router};
    use tower::ServiceExt;

    /// Every `Set-Cookie` on the response, as strings.
    fn set_cookies(response: &Response) -> Vec<String> {
        response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(str::to_owned)
            .collect()
    }

    fn app(handler: axum::routing::MethodRouter) -> Router {
        Router::new()
            .route("/", handler)
            .layer(axum::middleware::from_fn(process_flash_messages))
            .layer(Extension(SecretKey::random().expect("a random key")))
            .layer(Extension(Vec::<Cookie>::new()))
    }

    async fn call(router: Router) -> Response {
        router
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// The regression this fix exists for: a handler that flashes *and* sets a
    /// cookie must keep both. With `insert` the session cookie vanished.
    #[tokio::test]
    async fn a_handler_cookie_survives_the_flash_cookie() {
        let router = app(get(|flasher: Flasher| async move {
            flasher.add(FlashMessage::success("welcome"));
            let mut response = Redirect::to("/account").into_response();
            response
                .headers_mut()
                .append(SET_COOKIE, HeaderValue::from_static("token=secret; Path=/"));
            response
        }));

        let cookies = set_cookies(&call(router).await);

        assert!(
            cookies.iter().any(|c| c.starts_with("token=secret")),
            "the handler's session cookie was dropped by the flash layer: {cookies:?}"
        );
        assert!(
            cookies.iter().any(|c| c.starts_with("flash_messages=")),
            "the flash cookie is missing: {cookies:?}"
        );
    }

    /// The clearing branch must not clobber a handler cookie either — logging out
    /// both flashes "signed out" and tears the session cookie down.
    #[tokio::test]
    async fn a_handler_cookie_survives_the_flash_clear() {
        let key = SecretKey::random().expect("a random key");
        let stale = Flasher::new();
        stale.add(FlashMessage::success("read me"));
        let carried = stale.to_cookie(&key).expect("a cookie to carry in");

        let router = Router::new()
            .route(
                "/",
                get(|| async {
                    let mut response = Redirect::to("/").into_response();
                    response
                        .headers_mut()
                        .append(SET_COOKIE, HeaderValue::from_static("token=; Max-Age=0"));
                    response
                }),
            )
            .layer(axum::middleware::from_fn(process_flash_messages))
            .layer(Extension(key))
            .layer(Extension(vec![Cookie::new(
                "flash_messages",
                carried.value().to_owned(),
            )]));

        let cookies = set_cookies(&call(router).await);

        assert!(
            cookies.iter().any(|c| c.starts_with("token=")),
            "the handler's teardown cookie was dropped: {cookies:?}"
        );
        assert!(
            cookies.iter().any(|c| c.starts_with("flash_messages=;")),
            "the consumed flash cookie was not cleared: {cookies:?}"
        );
    }
}
