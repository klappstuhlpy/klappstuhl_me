//! Setting cookies on an outgoing response.
//!
//! Always **append** — never `insert`. A handler that sets a cookie is often one
//! that also flashes a message (signup's auto-login, the new session
//! `change_password` hands back, the session teardown on account deletion), and
//! `insert` would replace whatever `Set-Cookie` is already on the response.
//! Some responses legitimately carry several: the session teardown expires the
//! domain-scoped `token`, the legacy host-only one, and the Percy `session`
//! cookie in a single response.
//!
//! This module used to be a workaround: `kls-web-core`'s flash layer wrote its
//! cookie with `insert`, clobbering any the handler had set, so handlers queued
//! cookies in a response extension and a middleware replayed them *below* the
//! flash layer. That bug is fixed upstream as of `kls-web-core` v0.1.9 — the
//! queue, the middleware and the layer-ordering constraint are all gone, and
//! what's left is the plain helper.

use axum::{
    http::{header::SET_COOKIE, HeaderValue},
    response::Response,
};
use cookie::Cookie;

/// Appends a `Set-Cookie` for `cookie` to `response`.
///
/// A cookie that can't be rendered into a header value is dropped rather than
/// panicking the handler — the values here are all built in-process, so this is
/// unreachable in practice.
pub fn set_cookie(response: &mut Response, cookie: Cookie<'static>) {
    if let Ok(value) = HeaderValue::from_str(&cookie.to_string()) {
        set_raw_cookie(response, value);
    }
}

/// Appends an already-built `Set-Cookie` header value (for hand-rolled cookies
/// like the legacy host-only teardown).
pub fn set_raw_cookie(response: &mut Response, value: HeaderValue) {
    response.headers_mut().append(SET_COOKIE, value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    /// The property the whole module exists for: cookies accumulate, they don't
    /// replace each other. The session teardown sets three at once.
    #[test]
    fn cookies_accumulate_instead_of_replacing() {
        let mut response = ().into_response();
        set_cookie(&mut response, Cookie::build(("token", "a")).path("/").build());
        set_cookie(&mut response, Cookie::build(("session", "b")).path("/").build());
        set_raw_cookie(&mut response, HeaderValue::from_static("legacy=; Max-Age=0"));

        let cookies: Vec<_> = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();

        assert_eq!(
            cookies.len(),
            3,
            "a cookie was replaced instead of appended: {cookies:?}"
        );
        assert!(cookies.iter().any(|c| c.starts_with("token=a")));
        assert!(cookies.iter().any(|c| c.starts_with("session=b")));
        assert!(cookies.iter().any(|c| c.starts_with("legacy=")));
    }
}
