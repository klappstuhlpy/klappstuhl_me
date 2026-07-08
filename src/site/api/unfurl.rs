//! Link unfurl / Open Graph metadata endpoint.
//!
//! Fetches a caller-supplied URL (SSRF-guarded, see [`fetch_guarded`]) and
//! extracts its Open Graph / `<meta>` preview data — the same information a chat
//! client shows in a rich link embed. Gated by the `images:read` scope.

use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::auth::ApiToken;
use super::utils::{fetch_guarded, ApiJson as Json, RateLimitResponse};
use crate::{error::ApiError, headers::ClientIp, models::Scope, AppState};

/// HTML larger than this is not worth parsing for `<head>` metadata.
const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;

/// Query parameters for [`unfurl`].
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UnfurlQuery {
    /// The public http(s) URL to unfurl.
    pub url: String,
}

/// The extracted preview metadata for a URL.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct UnfurlResult {
    /// The URL that was unfurled (as requested).
    pub url: String,
    /// The page title (`og:title`, falling back to `<title>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The page description (`og:description`, falling back to
    /// `<meta name="description">`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The preview image URL (`og:image`), resolved to an absolute URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The site name (`og:site_name`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site_name: Option<String>,
    /// The favicon URL, resolved to an absolute URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
}

/// Unfurl a URL
///
/// Fetches the URL and returns its Open Graph / `<meta>` preview metadata.
#[utoipa::path(
    get,
    path = "/unfurl",
    params(UnfurlQuery),
    responses(
        (status = 200, description = "The extracted preview metadata", body = UnfurlResult),
        (status = 400, description = "Invalid URL, blocked address, or non-HTML response", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["images:read"])),
    tag = "web"
)]
pub async fn unfurl(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    Query(query): Query<UnfurlQuery>,
    auth: ApiToken,
) -> Result<Json<UnfurlResult>, ApiError> {
    let account = auth.require_account(&state, Scope::ImagesRead).await?;

    let body = fetch_guarded(&query.url, MAX_HTML_BYTES, "klappstuhl.me link unfurler").await?;

    // Only parse things that look like HTML; refuse binary/other payloads early.
    if let Some(ct) = &body.content_type {
        if !ct.contains("html") && !ct.contains("xml") {
            return Err(ApiError::validation("url", "the URL did not return an HTML page"));
        }
    }

    let html = String::from_utf8_lossy(&body.bytes).into_owned();
    // scraper's `Html` is not `Send`; parse + extract entirely on a blocking
    // thread and return only owned strings.
    let raw = tokio::task::spawn_blocking(move || extract(&html))
        .await
        .map_err(|_| ApiError::new("unfurl task failed"))?;

    // Resolve relative image/favicon URLs against the requested URL.
    let base = reqwest::Url::parse(&query.url).ok();
    let resolve = |value: Option<String>| -> Option<String> {
        let value = value?;
        match &base {
            Some(base) => base.join(&value).map(|u| u.to_string()).ok().or(Some(value)),
            None => Some(value),
        }
    };

    state.audit("api.unfurl").actor(&account).ip_opt(client_ip).fire();

    Ok(Json(UnfurlResult {
        url: query.url,
        title: raw.title,
        description: raw.description,
        image: resolve(raw.image),
        site_name: raw.site_name,
        favicon: resolve(raw.favicon).or_else(|| base.and_then(|b| b.join("/favicon.ico").ok()).map(|u| u.to_string())),
    }))
}

/// The un-resolved extraction result (relative URLs not yet absolutised).
struct Extracted {
    title: Option<String>,
    description: Option<String>,
    image: Option<String>,
    site_name: Option<String>,
    favicon: Option<String>,
}

/// Pure HTML → metadata extraction. No network, no `discord`, no state.
fn extract(html: &str) -> Extracted {
    use scraper::{Html, Selector};

    let doc = Html::parse_document(html);

    // Reads the `content` attribute of the first element matching `selector`.
    let meta = |selector: &str| -> Option<String> {
        let sel = Selector::parse(selector).ok()?;
        doc.select(&sel)
            .next()
            .and_then(|el| el.value().attr("content"))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    let title = meta(r#"meta[property="og:title"]"#).or_else(|| {
        Selector::parse("title").ok().and_then(|sel| {
            doc.select(&sel)
                .next()
                .map(|el| el.text().collect::<String>().trim().to_string())
                .filter(|s| !s.is_empty())
        })
    });

    let description = meta(r#"meta[property="og:description"]"#).or_else(|| meta(r#"meta[name="description"]"#));

    let image = meta(r#"meta[property="og:image"]"#).or_else(|| meta(r#"meta[property="og:image:url"]"#));

    let site_name = meta(r#"meta[property="og:site_name"]"#);

    // Favicon: prefer an explicit <link rel="icon"|"shortcut icon">.
    let favicon = [
        "link[rel=\"icon\"]",
        "link[rel=\"shortcut icon\"]",
        "link[rel=\"apple-touch-icon\"]",
    ]
    .iter()
    .find_map(|selector| {
        let sel = Selector::parse(selector).ok()?;
        doc.select(&sel)
            .next()
            .and_then(|el| el.value().attr("href"))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    });

    Extracted {
        title,
        description,
        image,
        site_name,
        favicon,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_open_graph_tags() {
        let html = r#"
            <html><head>
              <title>Fallback Title</title>
              <meta property="og:title" content="OG Title">
              <meta property="og:description" content="A description.">
              <meta property="og:image" content="https://ex.com/img.png">
              <meta property="og:site_name" content="Example">
              <link rel="icon" href="/favicon.ico">
            </head><body></body></html>
        "#;
        let e = extract(html);
        assert_eq!(e.title.as_deref(), Some("OG Title"));
        assert_eq!(e.description.as_deref(), Some("A description."));
        assert_eq!(e.image.as_deref(), Some("https://ex.com/img.png"));
        assert_eq!(e.site_name.as_deref(), Some("Example"));
        assert_eq!(e.favicon.as_deref(), Some("/favicon.ico"));
    }

    #[test]
    fn falls_back_to_title_and_meta_description() {
        let html = r#"<html><head>
              <title>Just a Title</title>
              <meta name="description" content="Meta desc.">
            </head></html>"#;
        let e = extract(html);
        assert_eq!(e.title.as_deref(), Some("Just a Title"));
        assert_eq!(e.description.as_deref(), Some("Meta desc."));
        assert!(e.image.is_none());
    }
}
