//! App-side session auth: the `Account` request extractor, layered on the shared
//! [`Token`] wire format.
//!
//! The tamper-proof cookie format itself — [`Token`], its HMAC signing, the
//! [`CookieDomain`]/[`TokenRejection`] cookie plumbing, and the state-agnostic
//! `FromRequestParts` impl for [`Token`] — lives in the shared `kls-web-core`
//! crate (see DASHBOARD_DECOUPLING_PLAN.md, Phase 4) and is re-exported here so
//! `crate::token::…` call sites are unchanged. What stays here is the piece that
//! is inherently app-specific: turning a validated token into this app's
//! [`Account`] by hitting its session store.

use axum::{extract::FromRequestParts, http::request::Parts};
use cookie::Cookie;

use crate::{key::SecretKey, models::Account, AppState};

pub use kls_web_core::token::*;

#[async_trait::async_trait]
impl FromRequestParts<AppState> for Account {
    type Rejection = TokenRejection;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let cookie = parts
            .extensions
            .get::<Vec<Cookie>>()
            .and_then(|cookies| cookies.iter().find(|c| c.name() == "token"))
            .ok_or_else(|| TokenRejection(cookie_domain_from(&parts.extensions)))?;

        let token = parts
            .extensions
            .get::<SecretKey>()
            .and_then(|key| Token::from_signed(cookie.value(), key))
            .ok_or_else(|| TokenRejection(cookie_domain_from(&parts.extensions)))?;

        // This unwrap is safe because it's validated above
        let (session_id, _) = cookie.value().split_once('.').unwrap();
        let domain = cookie_domain_from(&parts.extensions);
        state
            .get_session_account(session_id, token.id, false)
            .await
            .ok_or(TokenRejection(domain))
    }
}
