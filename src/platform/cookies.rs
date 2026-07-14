//! Cookies that survive the flash middleware.
//!
//! `flash::process_flash_messages` writes its cookie with `HeaderMap::insert`,
//! which *replaces* every `Set-Cookie` already on the response. Any handler that
//! both flashes a message and sets a cookie therefore loses the cookie — which
//! silently broke signup's auto-login, the new session `change_password` hands
//! back, and the session teardown on account deletion.
//!
//! Rather than have every such handler fight the layer, they queue their cookies
//! in a response extension and [`apply_pending_cookies`] appends them **outside**
//! the flash layer, after it has had its say. (The real fix is `append` instead
//! of `insert` in `kls-web-core`; until that ships, this keeps the two from
//! stepping on each other.)
//!
//! Ordering matters: the layer must sit *below* the flash layer in `main.rs`'s
//! list, since a later `.layer(...)` wraps the earlier ones and so processes the
//! response last.

use axum::{
    extract::Request,
    http::{header::SET_COOKIE, HeaderValue},
    middleware::Next,
    response::Response,
};
use cookie::Cookie;

/// `Set-Cookie` values waiting to be written onto the response.
#[derive(Clone, Default)]
pub struct PendingCookies(pub Vec<HeaderValue>);

/// Queues a cookie to be set on `response` once the flash layer is done with it.
pub fn queue_cookie(response: &mut Response, cookie: Cookie<'static>) {
    if let Ok(value) = HeaderValue::from_str(&cookie.to_string()) {
        queue_raw(response, value);
    }
}

/// Queues an already-built `Set-Cookie` header value (for hand-rolled cookies
/// like the legacy host-only teardown).
pub fn queue_raw(response: &mut Response, value: HeaderValue) {
    let mut pending = response.extensions_mut().remove::<PendingCookies>().unwrap_or_default();
    pending.0.push(value);
    response.extensions_mut().insert(pending);
}

/// Appends every queued cookie to the outgoing response.
pub async fn apply_pending_cookies(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    if let Some(PendingCookies(values)) = response.extensions_mut().remove::<PendingCookies>() {
        let headers = response.headers_mut();
        for value in values {
            headers.append(SET_COOKIE, value);
        }
    }
    response
}
