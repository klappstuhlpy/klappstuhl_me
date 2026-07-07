//! Public paste views, served without authentication.
//!
//! - `GET /p/<id>` — a syntax-highlighted landing page for a paste.
//! - `GET /p/<id>.txt` — the raw paste body as `text/plain`.
//!
//! Pastes themselves are created/managed through the API (see
//! [`crate::site::api::pastes`]). An hourly reaper deletes expired rows, mirroring
//! the image expiry reaper.

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};

use crate::codeimage;
use crate::models::Paste;
use crate::AppState;

/// Loads a non-expired paste by id.
async fn load_paste(state: &AppState, id: &str) -> Option<Paste> {
    state
        .database()
        .get(
            "SELECT * FROM paste WHERE id = ?1 \
             AND (expires_at IS NULL OR datetime(expires_at) > datetime('now'))",
            [id.to_string()],
        )
        .await
        .ok()
        .flatten()
}

/// `GET /p/:id` — serves either the highlighted HTML page or, when the id ends
/// in `.txt`, the raw body. A single route avoids a static/param conflict.
async fn view_paste(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    // `abc.txt` → raw body for `abc`; a bare id → the highlighted page.
    let (bare, raw) = match id.strip_suffix(".txt") {
        Some(bare) => (bare.to_string(), true),
        None => (id.clone(), false),
    };

    let Some(paste) = load_paste(&state, &bare).await else {
        return (StatusCode::NOT_FOUND, "paste not found").into_response();
    };

    if raw {
        return ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], paste.content).into_response();
    }

    // Best-effort view count; never block rendering on it.
    let _ = state
        .database()
        .execute("UPDATE paste SET views = views + 1 WHERE id = ?1", [bare.clone()])
        .await;

    let language = paste.language.clone().unwrap_or_default();
    let content = paste.content.clone();
    let rendered =
        tokio::task::spawn_blocking(move || codeimage::render_html(&content, &language, codeimage::DEFAULT_THEME))
            .await
            .ok()
            .and_then(Result::ok);

    let raw_url = state.config().url_to(format!("/p/{bare}.txt"));
    match rendered {
        Some((body, bg)) => Html(page_html(&bare, &paste.language, &body, &bg, &raw_url)).into_response(),
        None => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], paste.content).into_response(),
    }
}

/// Minimal escaping for the few characters that matter in an HTML text node /
/// attribute (the paste id and language label are the only untrusted values —
/// the highlighted body is already escaped by syntect).
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Builds a self-contained dark viewer page wrapping syntect's `<pre>` block.
fn page_html(id: &str, language: &Option<String>, body: &str, bg: &str, raw_url: &str) -> String {
    let id = escape(id);
    let lang = language.as_deref().map(escape).unwrap_or_default();
    let lang_label = if lang.is_empty() {
        String::new()
    } else {
        format!("<span class=\"lang\">{lang}</span>")
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>paste {id} · klappstuhl.me</title>
<style>
  :root {{ color-scheme: dark; }}
  * {{ box-sizing: border-box; }}
  body {{ margin: 0; background: {bg}; color: #e6e6e6;
          font: 14px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }}
  header {{ display: flex; align-items: center; gap: .75rem; padding: .6rem 1rem;
            background: rgba(0,0,0,.28); position: sticky; top: 0;
            border-bottom: 1px solid rgba(255,255,255,.08); }}
  header .id {{ font-weight: 600; }}
  header .lang {{ font-size: 12px; padding: .1rem .5rem; border-radius: 999px;
                  background: rgba(255,255,255,.12); }}
  header a {{ margin-left: auto; color: #9ecbff; text-decoration: none; }}
  header a:hover {{ text-decoration: underline; }}
  main {{ padding: 1rem; overflow-x: auto; }}
  pre {{ margin: 0; }}
</style>
</head>
<body>
<header>
  <span class="id">{id}</span>{lang_label}
  <a href="{raw_url}">raw</a>
</header>
<main>{body}</main>
</body>
</html>"#,
    )
}

/// Deletes expired pastes hourly, mirroring the image expiry reaper.
pub fn spawn_paste_reaper(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            let deleted = state
                .database()
                .call(|conn| {
                    conn.execute(
                        "DELETE FROM paste WHERE expires_at IS NOT NULL \
                         AND datetime(expires_at) <= datetime('now')",
                        [],
                    )
                })
                .await
                .unwrap_or(0);
            if deleted > 0 {
                tracing::info!(count = deleted, "reaped expired pastes");
            }
        }
    });
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/p/:id", get(view_paste))
}
