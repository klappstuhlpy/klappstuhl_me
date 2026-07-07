use axum::{
    extract::{FromRequestParts, Request, State},
    http::{header::AUTHORIZATION, request::Parts, HeaderMap},
    middleware::Next,
    response::Response,
};

use crate::{error::ApiError, models::Scope, AppState};

/// An API token, carrying the account id and the token's granted scopes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApiToken {
    pub id: i64,
    /// Comma-separated granted scopes. Empty means legacy / unrestricted
    /// (a key minted before scopes existed, or with no boxes ticked).
    pub scopes: String,
}

impl ApiToken {
    /// Returns true if the token is allowed to perform `needed`. An empty
    /// scope string is treated as full access for backwards compatibility.
    pub fn has_scope(&self, needed: Scope) -> bool {
        if self.scopes.is_empty() {
            return true;
        }
        self.scopes
            .split(',')
            .filter_map(|s| Scope::from_str(s.trim()))
            .any(|s| s == needed)
    }

    /// `Ok(())` when the token holds `needed`, otherwise a 403 `ApiError`.
    pub fn require(&self, needed: Scope) -> Result<(), ApiError> {
        if self.has_scope(needed) {
            Ok(())
        } else {
            Err(ApiError::forbidden().with_message(format!("this API key is missing the `{}` scope", needed.as_str())))
        }
    }
}

/// Strips an optional `Bearer ` / `Key ` scheme prefix (ASCII-case-insensitive)
/// from an `Authorization` value, returning the
/// bare key. A value with no recognised scheme is returned trimmed and unchanged,
/// so keys presented without a prefix keep working.
fn strip_scheme(raw: &str) -> &str {
    let raw = raw.trim();
    for scheme in ["bearer", "key", "token"] {
        if let Some(rest) = raw.get(..scheme.len()) {
            if rest.eq_ignore_ascii_case(scheme) {
                if let Some(after) = raw.get(scheme.len()..) {
                    // Require whitespace after the scheme so we don't mangle a
                    // bare key that merely starts with these letters.
                    if let Some(stripped) = after.strip_prefix(char::is_whitespace) {
                        return stripped.trim_start();
                    }
                }
            }
        }
    }
    raw
}

async fn extract_api_token_from_headers(headers: &HeaderMap, state: &AppState) -> Option<ApiToken> {
    let auth = headers
        .get(AUTHORIZATION)
        .and_then(|x| x.to_str().ok())
        .map(|value| strip_scheme(value).to_string())?;
    let info = state.is_session_valid(&auth).await?;
    if info.api_key {
        Some(ApiToken {
            id: info.id,
            scopes: info.scopes,
        })
    } else {
        None
    }
}

#[async_trait::async_trait]
impl FromRequestParts<AppState> for ApiToken {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        extract_api_token_from_headers(&parts.headers, state)
            .await
            .ok_or_else(ApiError::unauthorized)
    }
}

pub async fn copy_api_token(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let api_token = extract_api_token_from_headers(request.headers(), &state).await;
    let mut response = next.run(request).await;
    if let Some(token) = api_token {
        response.extensions_mut().insert(token);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(scopes: &str) -> ApiToken {
        ApiToken {
            id: 1,
            scopes: scopes.to_string(),
        }
    }

    #[test]
    fn empty_scopes_grant_everything() {
        let t = tok("");
        assert!(t.has_scope(Scope::ImagesWrite));
        assert!(t.has_scope(Scope::AdminWrite));
        assert!(t.require(Scope::ImagesWrite).is_ok());
    }

    #[test]
    fn specific_scopes_are_enforced() {
        let t = tok("images:read");
        assert!(t.has_scope(Scope::ImagesRead));
        assert!(!t.has_scope(Scope::ImagesWrite));
        assert!(t.require(Scope::ImagesRead).is_ok());
        assert!(t.require(Scope::ImagesWrite).is_err());
    }

    #[test]
    fn multiple_scopes_parse() {
        let t = tok("images:read, images:write");
        assert!(t.has_scope(Scope::ImagesRead));
        assert!(t.has_scope(Scope::ImagesWrite));
        assert!(!t.has_scope(Scope::AdminRead));
    }

    #[test]
    fn strip_scheme_handles_prefixes() {
        // Bare key (legacy) is returned unchanged.
        assert_eq!(strip_scheme("klp_live_abc"), "klp_live_abc");
        // Recognised schemes are stripped, case-insensitively.
        assert_eq!(strip_scheme("Bearer klp_live_abc"), "klp_live_abc");
        assert_eq!(strip_scheme("bearer   klp_live_abc"), "klp_live_abc");
        assert_eq!(strip_scheme("KEY klp_live_abc"), "klp_live_abc");
        assert_eq!(strip_scheme("Token klp_live_abc"), "klp_live_abc");
        assert_eq!(strip_scheme("  Bearer klp_live_abc  "), "klp_live_abc");
        // A bare key that merely starts with a scheme's letters is untouched
        // (no whitespace separator).
        assert_eq!(strip_scheme("Keyabc123"), "Keyabc123");
        assert_eq!(strip_scheme("bearerish"), "bearerish");
    }
}
