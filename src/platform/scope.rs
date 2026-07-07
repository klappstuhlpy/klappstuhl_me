//! API-token scope enforcement.
//!
//! Handlers ask for `Scoped<ImagesWrite>` (or similar marker) as an extractor.
//! Resolution:
//!
//! 1. Pull the signed token out of the `token` cookie.
//! 2. Look up the corresponding session row.
//! 3. Resolve the matching account.
//! 4. Check [`crate::models::Session::has_scope`] — browser sessions and
//!    legacy (pre-scopes) API keys always pass, scope-limited API keys
//!    must have an explicit grant.
//!
//! On any failure the request gets `403 Forbidden`.
//!
//! ```ignore
//! async fn upload(
//!     State(state): State<AppState>,
//!     Scoped(account, _): Scoped<ImagesWrite>,
//! ) -> Response { … }
//! ```

use std::marker::PhantomData;

use axum::{
    extract::FromRequestParts,
    http::{request::Parts, StatusCode},
};
use cookie::Cookie;

use crate::{
    key::SecretKey,
    models::{Account, Scope, Session},
    token::Token,
    AppState,
};

/// Marker trait — each scope gets a zero-sized type that implements this.
pub trait ScopeMarker: Send + Sync + 'static {
    fn scope() -> Scope;
}

/// Helper for declaring all four scope markers without boilerplate.
macro_rules! scope_marker {
    ($($name:ident => $variant:ident),* $(,)?) => {
        $(
            #[derive(Debug, Clone, Copy)]
            pub struct $name;
            impl ScopeMarker for $name {
                fn scope() -> Scope { Scope::$variant }
            }
        )*
    };
}

scope_marker! {
    ImagesRead  => ImagesRead,
    ImagesWrite => ImagesWrite,
    AdminRead   => AdminRead,
    AdminWrite  => AdminWrite,
}

/// Extractor that succeeds only when the caller holds the marker's scope.
///
/// Carries the authenticated `Account` so handlers can use it without
/// double-extracting.
pub struct Scoped<S: ScopeMarker>(pub Account, pub PhantomData<S>);

#[async_trait::async_trait]
impl<S: ScopeMarker> FromRequestParts<AppState> for Scoped<S> {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        // 1. Token cookie + signature.
        let cookie = parts
            .extensions
            .get::<Vec<Cookie>>()
            .and_then(|cookies| cookies.iter().find(|c| c.name() == "token"))
            .ok_or(StatusCode::UNAUTHORIZED)?;

        let key = parts
            .extensions
            .get::<SecretKey>()
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
        let token = Token::from_signed(cookie.value(), key).ok_or(StatusCode::UNAUTHORIZED)?;

        // 2. Look up the session row directly so we can read `scopes`.
        let session: Session = match state
            .database()
            .get::<Session, _, _>(
                "SELECT * FROM session WHERE id = ? AND account_id = ?",
                (token.base64(), token.id),
            )
            .await
        {
            Ok(Some(s)) => s,
            _ => return Err(StatusCode::UNAUTHORIZED),
        };

        // 3. Scope check.
        if !session.has_scope(S::scope()) {
            return Err(StatusCode::FORBIDDEN);
        }

        // 4. Resolve account.
        let account = state.get_account(token.id).await.ok_or(StatusCode::UNAUTHORIZED)?;

        Ok(Scoped(account, PhantomData))
    }
}
