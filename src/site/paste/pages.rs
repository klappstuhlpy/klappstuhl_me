//! The paste GET surfaces: the editor, your list, the viewer, and the viewer's
//! satellites (raw, embed, OG image, history).
//!
//! Everything here reads through [`super::service`], so the expiry rule — an
//! expired paste is invisible to *every* read path — holds without each handler
//! having to remember it.

use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    Extension, Json,
};
use cookie::Cookie;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::filters; // the `isoformat` filter, used by the templates
use crate::flash::Flashes;
use crate::key::SecretKey;
use crate::models::{Account, Paste, Visibility};
use crate::AppState;

use super::render;
use super::service;
use super::{resolve_body, Body};

// ─── View models ─────────────────────────────────────────────────────────────

/// A paste reduced to what a list row needs.
pub struct PasteRow {
    pub id: String,
    pub title: String,
    pub language: String,
    pub visibility: &'static str,
    pub encrypted: bool,
    pub burn: bool,
    pub size: String,
    pub views: i64,
    pub created_at: OffsetDateTime,
    pub expires_at: Option<OffsetDateTime>,
}

impl From<&Paste> for PasteRow {
    fn from(p: &Paste) -> Self {
        Self {
            id: p.id.clone(),
            title: display_title(p),
            language: p.language.clone().unwrap_or_else(|| "text".to_string()),
            visibility: p.visibility.as_str(),
            encrypted: p.is_encrypted(),
            burn: p.burn_after_read,
            size: human_size(p.size_bytes),
            views: p.views,
            created_at: p.created_at,
            expires_at: p.expires_at,
        }
    }
}

/// The title to show. An untitled paste is addressed by its id, which is what
/// people actually paste into chat anyway.
pub fn display_title(p: &Paste) -> String {
    match p.title.as_deref() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => p.id.clone(),
    }
}

pub fn human_size(bytes: i64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

// ─── Templates ───────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "paste/editor.html")]
struct EditorTemplate {
    account: Option<Account>,
    flashes: Flashes,
    languages: &'static [render::PickerLang],
    /// Present when editing an existing paste; the form posts to a different
    /// action and pre-fills from it.
    existing: Option<EditorPaste>,
    /// The size cap that applies to *this* author, so the counter can turn red
    /// at the right number instead of a made-up one.
    max_bytes: usize,
    anonymous_allowed: bool,
}

/// The pre-filled state of the editor when editing.
struct EditorPaste {
    id: String,
    title: String,
    language: String,
    visibility: &'static str,
    content: String,
    encrypted: bool,
}

#[derive(Template)]
#[template(path = "paste/list.html")]
struct ListTemplate {
    account: Option<Account>,
    flashes: Flashes,
    pastes: Vec<PasteRow>,
    languages: Vec<String>,
    count: usize,
    limit: usize,
    used: String,
    quota: String,
    is_admin: bool,
}

#[derive(Template)]
#[template(path = "paste/view.html")]
struct ViewTemplate {
    account: Option<Account>,
    flashes: Flashes,
    id: String,
    /// The `<title>`/OG title — the paste title, or its id.
    heading: String,
    language: String,
    language_label: String,
    author: Option<String>,
    visibility: &'static str,
    indexable: bool,
    is_owner: bool,
    encrypted: bool,
    burn: bool,
    forked_from: Option<String>,
    size: String,
    line_count: usize,
    views: i64,
    revisions: usize,
    created_at: OffsetDateTime,
    expires_at: Option<OffsetDateTime>,
    /// The highlighted body, one entry per line — `None` when the paste is
    /// locked or sealed, in which case it is *not in the response at all*.
    lines: Option<Vec<String>>,
    background: String,
    foreground: String,
    /// The rendered markdown, for a markdown paste.
    markdown: Option<String>,
    /// Which gate to show, if any: `"locked"` or `"burn"`.
    gate: Option<&'static str>,
    /// A locked *and* burning paste takes the password in the reveal form.
    gate_needs_password: bool,
    url: String,
}

#[derive(Template)]
#[template(path = "paste/embed.html")]
struct EmbedTemplate {
    title: String,
    language: String,
    lines: Vec<String>,
    background: String,
    foreground: String,
    url: String,
}

#[derive(Template)]
#[template(path = "paste/burned.html")]
struct BurnedTemplate {
    account: Option<Account>,
    title: String,
    language: String,
    lines: Vec<String>,
    background: String,
    foreground: String,
}

#[derive(Template)]
#[template(path = "paste/history.html")]
struct HistoryTemplate {
    account: Option<Account>,
    flashes: Flashes,
    id: String,
    title: String,
    /// Newest first: the current body, then each superseded one.
    entries: Vec<RevisionView>,
}

struct RevisionView {
    label: String,
    created_at: OffsetDateTime,
    /// The unified diff against the *previous* (older) entry, as coloured lines.
    diff: Vec<DiffLine>,
    lines: usize,
}

pub struct DiffLine {
    pub kind: &'static str,
    pub text: String,
}

// ─── The editor ──────────────────────────────────────────────────────────────

/// `GET /paste` — the editor. Works logged-out when anonymous pastes are on.
pub async fn editor(State(state): State<AppState>, account: Option<Account>, flashes: Flashes) -> Response {
    let config = state.config();
    let max_bytes = if account.is_some() {
        config.paste.max_bytes
    } else {
        config.paste.anonymous_max_bytes
    };

    EditorTemplate {
        account,
        flashes,
        languages: render::picker_languages(),
        existing: None,
        max_bytes,
        anonymous_allowed: config.paste.anonymous,
    }
    .into_response()
}

/// `GET /p/:id/edit` — the editor, pre-filled.
///
/// Reachable by the owner, or by an anonymous author who still holds the edit
/// token (`?token=…`). Anything else is a 404, not a 403: "not yours" and
/// "doesn't exist" must look the same from outside.
pub async fn edit_form(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<TokenQuery>,
    Extension(cookies): Extension<Vec<Cookie<'static>>>,
    Extension(secret): Extension<SecretKey>,
    account: Option<Account>,
    flashes: Flashes,
) -> Response {
    let actor = actor_for(account.as_ref(), query.token.clone());
    let Ok(paste) = service::load_for(&state, &id, &actor).await else {
        return not_found();
    };

    // An encrypted paste can only be edited from its plaintext, so the editor
    // needs it unlocked first — the unlock cookie is what carries that.
    let content = match resolve_body(&paste, &cookies, &secret, false) {
        Body::Plain(text) => text,
        // A burn paste is sealed to *readers*; its author may still edit it.
        Body::Sealed => match paste.text() {
            Some(text) => text.to_string(),
            None => return redirect(&format!("/p/{id}")),
        },
        Body::Locked | Body::Undecodable => return redirect(&format!("/p/{id}")),
    };

    let config = state.config();
    let max_bytes = if paste.account_id.is_some() {
        config.paste.max_bytes
    } else {
        config.paste.anonymous_max_bytes
    };

    EditorTemplate {
        existing: Some(EditorPaste {
            id: paste.id.clone(),
            title: paste.title.clone().unwrap_or_default(),
            language: paste.language.clone().unwrap_or_default(),
            visibility: paste.visibility.as_str(),
            content,
            encrypted: paste.is_encrypted(),
        }),
        account,
        flashes,
        languages: render::picker_languages(),
        max_bytes,
        anonymous_allowed: config.paste.anonymous,
    }
    .into_response()
}

// ─── Live highlight preview (the editor overlay) ─────────────────────────────

/// What the editor POSTs as you type: the current body and picked language.
#[derive(Debug, Default, Deserialize)]
pub struct PreviewBody {
    #[serde(default)]
    content: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    theme: Option<String>,
}

/// The highlighted body the editor paints behind its textarea.
#[derive(Serialize)]
struct PreviewResponse {
    /// Per-line highlighted HTML, joined by `\n` — the same markup the viewer
    /// renders, so the editor preview and the final page can't drift apart.
    html: String,
    background: String,
    foreground: String,
    /// The language actually highlighted (the detected one, in Auto mode).
    language: String,
    /// The chip label — `Auto · Python` when detection kicked in.
    label: String,
}

/// `POST /paste/preview` — highlight a draft for the live editor overlay.
///
/// Read-only and public (the editor is), so it's CPU-capped: a draft past
/// `PREVIEW_CAP` bytes is highlighted only up to the cap, which is plenty for a
/// live preview and keeps a hostile caller from renting the box's CPU. The full
/// body is still highlighted for real when the paste is saved and viewed.
pub async fn preview(State(state): State<AppState>, Json(body): Json<PreviewBody>) -> Response {
    const PREVIEW_CAP: usize = 200 * 1024;

    let config = state.config();
    let theme = body
        .theme
        .filter(|t| crate::codeimage::available_themes().contains(t))
        .unwrap_or_else(|| config.paste.default_theme.clone());

    let mut content = body.content;
    if content.len() > PREVIEW_CAP {
        let mut end = PREVIEW_CAP;
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        content.truncate(end);
    }

    let stored = body.language.trim().to_string();
    let effective = if stored.is_empty() {
        render::detect_language(&content, None).unwrap_or_default()
    } else {
        stored.clone()
    };

    let lang = effective.clone();
    let theme_name = theme.clone();
    let source = content.clone();
    let Ok(highlighted) = tokio::task::spawn_blocking(move || render::highlight(&source, &lang, &theme_name)).await
    else {
        return internal_error();
    };

    let label = if stored.is_empty() {
        match effective.as_str() {
            "" => "Auto".to_string(),
            tok => format!("Auto · {}", language_label(tok)),
        }
    } else {
        language_label(&stored)
    };

    Json(PreviewResponse {
        html: highlighted.lines.join("\n"),
        background: highlighted.background,
        foreground: highlighted.foreground,
        language: effective,
        label,
    })
    .into_response()
}

/// The `?token=…` an anonymous author carries back to edit or delete a paste.
#[derive(Debug, Default, Deserialize)]
pub struct TokenQuery {
    #[serde(default)]
    pub token: Option<String>,
}

fn actor_for<'a>(account: Option<&'a Account>, token: Option<String>) -> service::Actor<'a> {
    service::Actor {
        account,
        edit_token: token.filter(|t| !t.trim().is_empty()),
    }
}

// ─── The list ────────────────────────────────────────────────────────────────

/// `GET /pastes` — your pastes.
pub async fn list(State(state): State<AppState>, account: Account, flashes: Flashes) -> Response {
    let pastes = service::list_for_account(&state, account.id).await;
    let (_, bytes) = service::usage(&state, account.id).await;

    let mut languages: Vec<String> = pastes
        .iter()
        .filter_map(|p| p.language.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    languages.sort();

    let config = state.config();
    let rows: Vec<PasteRow> = pastes.iter().map(PasteRow::from).collect();

    ListTemplate {
        count: rows.len(),
        limit: config.paste.account_limit,
        used: human_size(bytes),
        quota: human_size(config.paste.account_max_total_bytes),
        is_admin: account.flags.is_admin(),
        pastes: rows,
        languages,
        account: Some(account),
        flashes,
    }
    .into_response()
}

// ─── The viewer ──────────────────────────────────────────────────────────────

/// A `?theme=` override for the viewer's syntect theme picker.
#[derive(Debug, Default, Deserialize)]
pub struct ViewQuery {
    #[serde(default)]
    pub theme: Option<String>,
}

/// `GET /p/:id` — the viewer, and (when the id ends in `.txt`) the raw body.
///
/// The `.txt` suffix is parsed here rather than being its own route: `/p/:id`
/// and `/p/:id.txt` conflict in matchit. `/p/<id>.txt` is a documented API field
/// (`raw_url`), so its behaviour is frozen — it is the one hard
/// backward-compatibility promise of this whole redesign.
pub async fn view(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ViewQuery>,
    Extension(cookies): Extension<Vec<Cookie<'static>>>,
    Extension(secret): Extension<SecretKey>,
    account: Option<Account>,
    flashes: Flashes,
) -> Response {
    if let Some(bare) = id.strip_suffix(".txt") {
        return raw_body(&state, bare, &cookies, &secret, false).await;
    }

    let Some(paste) = service::load(&state, &id).await else {
        return not_found();
    };

    let body = resolve_body(&paste, &cookies, &secret, false);
    // A locked or sealed paste is never counted as read — the view count would
    // otherwise tick up for every link-preview crawler.
    if matches!(body, Body::Plain(_)) {
        service::count_view(&state, &id).await;
    }

    let config = state.config();
    let theme = query
        .theme
        .filter(|t| crate::codeimage::available_themes().contains(t))
        .unwrap_or_else(|| config.paste.default_theme.clone());

    let stored_language = paste.language.clone().unwrap_or_default();
    let is_owner = account.as_ref().is_some_and(|a| paste.owned_by(a));

    // In Auto mode the stored language is empty. Detect one for *highlighting*
    // only — the stored value stays empty, so re-editing still shows "Auto".
    let mut effective_language = stored_language.clone();

    let (lines, markdown, line_count, background, foreground, gate) = match &body {
        Body::Plain(text) => {
            if effective_language.is_empty() {
                effective_language = render::detect_language(text, paste.title.as_deref()).unwrap_or_default();
            }
            let source = text.clone();
            let lang = effective_language.clone();
            let theme_name = theme.clone();
            // Highlighting a 512 KB paste is real CPU work — keep it off the
            // async runtime's worker.
            let highlighted = tokio::task::spawn_blocking(move || render::highlight(&source, &lang, &theme_name)).await;
            let Ok(highlighted) = highlighted else {
                return internal_error();
            };
            let markdown = render::is_markdown((!effective_language.is_empty()).then_some(effective_language.as_str()))
                .then(|| render::markdown(text));
            let count = highlighted.lines.len();
            (
                Some(highlighted.lines),
                markdown,
                count,
                highlighted.background,
                highlighted.foreground,
                None,
            )
        }
        Body::Locked => (None, None, 0, String::new(), String::new(), Some("locked")),
        Body::Sealed => (None, None, 0, String::new(), String::new(), Some("burn")),
        Body::Undecodable => (None, None, 0, String::new(), String::new(), Some("locked")),
    };

    let author = author_name(&state, paste.account_id).await;
    let revisions = service::revisions(&state, &paste.id).await.len();

    // The chip reads "Auto · Rust" when a language was inferred, plain "Auto"
    // when nothing was, or the picked label when it wasn't Auto at all.
    let (label, disp_language) = if stored_language.is_empty() {
        match effective_language.as_str() {
            "" => ("Auto".to_string(), String::new()),
            tok => (format!("Auto · {}", language_label(tok)), effective_language.clone()),
        }
    } else {
        (language_label(&stored_language), stored_language.clone())
    };

    ViewTemplate {
        heading: display_title(&paste),
        language_label: label,
        language: disp_language,
        author,
        visibility: paste.visibility.as_str(),
        // Only `public` pastes are indexable. Everything else — unlisted,
        // private, anonymous, encrypted, burning — carries `noindex`.
        indexable: paste.visibility == Visibility::Public && !paste.burn_after_read && !paste.is_encrypted(),
        is_owner,
        encrypted: paste.is_encrypted(),
        burn: paste.burn_after_read,
        // A locked-and-burning paste takes the password in the reveal form: once
        // it burns there is nothing left to revisit, so no unlock cookie is set.
        gate_needs_password: paste.is_encrypted() && paste.burn_after_read,
        forked_from: paste.fork_of.clone(),
        size: human_size(paste.size_bytes),
        line_count,
        views: paste.views,
        revisions,
        created_at: paste.created_at,
        expires_at: paste.expires_at,
        lines,
        background,
        foreground,
        markdown,
        gate,
        url: config.url_to(format!("/p/{}", paste.id)),
        id: paste.id.clone(),
        account,
        flashes,
    }
    .into_response()
}

/// `GET /p/:id/raw` — the body as a download, with a real filename.
pub async fn raw(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(cookies): Extension<Vec<Cookie<'static>>>,
    Extension(secret): Extension<SecretKey>,
) -> Response {
    raw_body(&state, &id, &cookies, &secret, true).await
}

/// The shared body of `/p/<id>.txt` (inline) and `/p/<id>/raw` (attachment).
///
/// A locked paste 401s and a burning one 403s rather than serving anything: the
/// raw path must never become the way around the gate the viewer puts up.
async fn raw_body(
    state: &AppState,
    id: &str,
    cookies: &[Cookie<'static>],
    secret: &SecretKey,
    download: bool,
) -> Response {
    let Some(paste) = service::load(state, id).await else {
        return not_found();
    };

    match resolve_body(&paste, cookies, secret, false) {
        Body::Plain(text) => {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, "text/plain; charset=utf-8".parse().unwrap());
            if download {
                let name = render::download_name(&paste.id, paste.language.as_deref());
                if let Ok(value) = format!("attachment; filename=\"{name}\"").parse() {
                    headers.insert(header::CONTENT_DISPOSITION, value);
                }
            }
            (headers, text).into_response()
        }
        Body::Locked | Body::Undecodable => {
            (StatusCode::UNAUTHORIZED, "this paste is password-protected").into_response()
        }
        Body::Sealed => (
            StatusCode::FORBIDDEN,
            "this paste is burn-after-read — open it in a browser to reveal it",
        )
            .into_response(),
    }
}

/// `GET /p/:id/embed` — a bare, iframe-able view: no nav, no layout, no chrome.
pub async fn embed(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(cookies): Extension<Vec<Cookie<'static>>>,
    Extension(secret): Extension<SecretKey>,
) -> Response {
    let Some(paste) = service::load(&state, &id).await else {
        return not_found();
    };
    let Body::Plain(text) = resolve_body(&paste, &cookies, &secret, false) else {
        return (StatusCode::FORBIDDEN, "this paste cannot be embedded").into_response();
    };

    let mut language = paste.language.clone().unwrap_or_default();
    if language.is_empty() {
        language = render::detect_language(&text, paste.title.as_deref()).unwrap_or_default();
    }
    let theme = state.config().paste.default_theme.clone();
    let lang = language.clone();
    let Ok(highlighted) = tokio::task::spawn_blocking(move || render::highlight(&text, &lang, &theme)).await else {
        return internal_error();
    };

    EmbedTemplate {
        title: display_title(&paste),
        language,
        lines: highlighted.lines,
        background: highlighted.background,
        foreground: highlighted.foreground,
        url: state.config().url_to(format!("/p/{}", paste.id)),
    }
    .into_response()
}

/// `GET /p/:id/og.svg` — the link-preview image: a code screenshot of the first
/// few lines, rendered by the same `codeimage` the render API exposes.
///
/// A paste whose body is gated (encrypted or burning) gets a *contentless* card:
/// the whole point of §6.1 is that a crawler building an embed can neither read
/// nor destroy the paste.
pub async fn og_image(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    const OG_LINES: usize = 14;

    let Some(paste) = service::load(&state, &id).await else {
        return not_found();
    };

    let (snippet, language) = match (paste.is_encrypted() || paste.burn_after_read, paste.text()) {
        (false, Some(text)) => {
            let snippet: String = text.lines().take(OG_LINES).collect::<Vec<_>>().join("\n");
            (snippet, paste.language.clone().unwrap_or_default())
        }
        _ => ("🔒 This paste is protected.".to_string(), String::new()),
    };

    let theme = state.config().paste.default_theme.clone();
    let rendered = tokio::task::spawn_blocking(move || crate::codeimage::render_svg(&snippet, &language, &theme)).await;

    match rendered {
        Ok(Ok(svg)) => (
            [
                (header::CONTENT_TYPE, "image/svg+xml"),
                (header::CACHE_CONTROL, "public, max-age=600"),
            ],
            svg,
        )
            .into_response(),
        _ => internal_error(),
    }
}

/// The page a burn-after-read paste renders **once**, at the moment it is
/// destroyed. There is no second chance to load it, so the body is on the page
/// in full and the copy button is the point of the whole screen.
pub async fn burned(state: &AppState, paste: Paste, plaintext: String, account: Option<Account>) -> Response {
    let language = paste.language.clone().unwrap_or_default();
    let theme = state.config().paste.default_theme.clone();
    let lang = language.clone();
    let Ok(highlighted) = tokio::task::spawn_blocking(move || render::highlight(&plaintext, &lang, &theme)).await
    else {
        return internal_error();
    };

    BurnedTemplate {
        title: display_title(&paste),
        language,
        lines: highlighted.lines,
        background: highlighted.background,
        foreground: highlighted.foreground,
        account,
    }
    .into_response()
}

/// `GET /p/:id/history` — the revision list, each shown as a diff against the
/// version before it.
pub async fn history(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(cookies): Extension<Vec<Cookie<'static>>>,
    Extension(secret): Extension<SecretKey>,
    account: Option<Account>,
    flashes: Flashes,
) -> Response {
    let Some(paste) = service::load(&state, &id).await else {
        return not_found();
    };
    // History is the body, over time — so it is gated exactly like the body is.
    let Body::Plain(current) = resolve_body(&paste, &cookies, &secret, false) else {
        return redirect(&format!("/p/{id}"));
    };

    let revisions = service::revisions(&state, &paste.id).await;

    // Oldest → newest, so each entry can diff against the one before it.
    let mut bodies: Vec<(String, Option<OffsetDateTime>)> = revisions
        .iter()
        .rev()
        .map(|r| {
            (
                String::from_utf8(r.content.clone()).unwrap_or_else(|_| "(encrypted)".to_string()),
                Some(r.created_at),
            )
        })
        .collect();
    bodies.push((current, paste.updated_at));

    let mut entries: Vec<RevisionView> = Vec::with_capacity(bodies.len());
    for (i, (body, at)) in bodies.iter().enumerate() {
        let previous = if i == 0 { "" } else { bodies[i - 1].0.as_str() };
        entries.push(RevisionView {
            label: if i == 0 {
                "Original".to_string()
            } else if i == bodies.len() - 1 {
                "Current".to_string()
            } else {
                format!("Revision {i}")
            },
            created_at: at.unwrap_or(paste.created_at),
            diff: diff_lines(previous, body),
            lines: body.lines().count(),
        });
    }
    entries.reverse(); // newest first

    HistoryTemplate {
        title: display_title(&paste),
        id: paste.id.clone(),
        entries,
        account,
        flashes,
    }
    .into_response()
}

/// A minimal line-level diff: the common prefix and suffix are context, and what
/// is left in the middle is shown as removed-then-added.
///
/// This is not Myers — it doesn't try to find the smallest edit script. For
/// "what changed in this paste between two saves" the prefix/suffix trim is
/// almost always what a human would have marked anyway, and it costs no
/// dependency.
pub fn diff_lines(before: &str, after: &str) -> Vec<DiffLine> {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();

    let mut head = 0;
    while head < old.len() && head < new.len() && old[head] == new[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < old.len() - head && tail < new.len() - head && old[old.len() - 1 - tail] == new[new.len() - 1 - tail] {
        tail += 1;
    }

    const CONTEXT: usize = 2;
    let mut out = Vec::new();
    let context_start = head.saturating_sub(CONTEXT);
    for line in &new[context_start..head] {
        out.push(DiffLine {
            kind: "ctx",
            text: (*line).to_string(),
        });
    }
    for line in &old[head..old.len() - tail] {
        out.push(DiffLine {
            kind: "del",
            text: (*line).to_string(),
        });
    }
    for line in &new[head..new.len() - tail] {
        out.push(DiffLine {
            kind: "add",
            text: (*line).to_string(),
        });
    }
    let context_end = (new.len() - tail + CONTEXT).min(new.len());
    for line in &new[new.len() - tail..context_end] {
        out.push(DiffLine {
            kind: "ctx",
            text: (*line).to_string(),
        });
    }
    out
}

// ─── Shared helpers ──────────────────────────────────────────────────────────

/// The owner's username, for the viewer's byline. `None` for anonymous pastes.
async fn author_name(state: &AppState, account_id: Option<i64>) -> Option<String> {
    let account_id = account_id?;
    state
        .database()
        .get_row("SELECT name FROM account WHERE id = ?1", [account_id], |row| {
            row.get::<_, String>(0)
        })
        .await
        .ok()
}

/// The display label for a language token (`rs` → `Rust`), falling back to the
/// token itself for anything outside the curated list.
fn language_label(token: &str) -> String {
    if token.is_empty() {
        return "text".to_string();
    }
    render::picker_languages()
        .iter()
        .find(|l| l.token == token)
        .map(|l| l.name.clone())
        .unwrap_or_else(|| token.to_string())
}

pub fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Html("<h1>404</h1><p>No such paste.</p>")).into_response()
}

fn internal_error() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "could not render the paste").into_response()
}

fn redirect(to: &str) -> Response {
    axum::response::Redirect::to(to).into_response()
}

/// `Accept: application/json`? (Re-exported for the crud handlers.)
pub fn wants_json(headers: &HeaderMap) -> bool {
    super::wants_json(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_marks_only_the_changed_lines() {
        let diff = diff_lines("a\nb\nc\n", "a\nB\nc\n");
        let del: Vec<_> = diff.iter().filter(|l| l.kind == "del").map(|l| &l.text).collect();
        let add: Vec<_> = diff.iter().filter(|l| l.kind == "add").map(|l| &l.text).collect();
        assert_eq!(del, vec!["b"]);
        assert_eq!(add, vec!["B"]);
    }

    #[test]
    fn diff_of_identical_bodies_has_no_changes() {
        let diff = diff_lines("a\nb\n", "a\nb\n");
        assert!(diff.iter().all(|l| l.kind == "ctx"));
    }

    #[test]
    fn diff_against_nothing_is_all_additions() {
        let diff = diff_lines("", "hello\nworld\n");
        assert_eq!(diff.iter().filter(|l| l.kind == "add").count(), 2);
        assert_eq!(diff.iter().filter(|l| l.kind == "del").count(), 0);
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }
}
