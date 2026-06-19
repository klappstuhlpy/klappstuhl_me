//! Percy bot dashboard: a multi-page guild management UI proxied to Percy.
//!
//! Split into [`templates`] (Askama views) and [`handlers`] (route handlers);
//! this module owns the shared helpers and the router assembly.

mod handlers;
mod templates;

use std::{
    collections::HashSet,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
    Json, Router,
};
use quick_cache::sync::Cache;

use crate::{
    flash::Flasher,
    percy::{Channel, PercyClient, PercyError, Role},
    AppState,
};

use handlers::*;

// -- Helpers -----------------------------------------------------------------

fn get_percy_client(state: &AppState) -> Option<PercyClient> {
    PercyClient::new(state.percy_client.clone(), &state.config().percy)
}

/// How long a resolved Discord-account link / manageable-guild set stays cached.
/// Short enough that a revoked admin loses dashboard access promptly, long enough
/// to spare Percy (and SQLite) a round-trip on every request of a browsing session.
const ACCESS_TTL: Duration = Duration::from_secs(60);
const DISCORD_LINK_TTL: Duration = Duration::from_secs(300);

/// A cache value paired with the instant it was stored, for TTL checks.
type Timed<T> = (Instant, T);

/// `account_id -> (resolved_at, discord_user_id)`. Avoids a SQLite hit per request.
fn discord_link_cache() -> &'static Cache<i64, Timed<String>> {
    static CACHE: OnceLock<Cache<i64, Timed<String>>> = OnceLock::new();
    CACHE.get_or_init(|| Cache::new(2000))
}

/// `discord_user_id -> (fetched_at, manageable guild ids)`. Removes one Percy
/// round-trip (`get_user_guilds`) from every dashboard page load and mutation.
fn guild_access_cache() -> &'static Cache<String, Timed<Arc<HashSet<String>>>> {
    static CACHE: OnceLock<Cache<String, Timed<Arc<HashSet<String>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Cache::new(2000))
}

async fn get_discord_id(state: &AppState, account_id: i64) -> Option<String> {
    if let Some((ts, id)) = discord_link_cache().get(&account_id) {
        if ts.elapsed() < DISCORD_LINK_TTL {
            return Some(id);
        }
    }

    let id: Option<String> = state
        .database()
        .call(move |conn| {
            conn.prepare_cached("SELECT discord_user_id FROM user_discord_links WHERE account_id = ?")
                .and_then(|mut stmt| stmt.query_row([account_id], |row| row.get(0)))
        })
        .await
        .ok();

    if let Some(ref id) = id {
        discord_link_cache().insert(account_id, (Instant::now(), id.clone()));
    }
    id
}

/// How long a guild's role/channel lists stay cached. They change Discord-side
/// (never via the dashboard), so a short TTL is the only staleness bound needed,
/// and it spares repeated `get_guild_roles`/`get_guild_channels` hops across the
/// many feature pages that render pickers.
const GUILD_META_TTL: Duration = Duration::from_secs(60);

fn roles_cache() -> &'static Cache<u64, Timed<Arc<Vec<Role>>>> {
    static CACHE: OnceLock<Cache<u64, Timed<Arc<Vec<Role>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Cache::new(1000))
}

fn channels_cache() -> &'static Cache<u64, Timed<Arc<Vec<Channel>>>> {
    static CACHE: OnceLock<Cache<u64, Timed<Arc<Vec<Channel>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Cache::new(1000))
}

/// Guild roles, cached for [`GUILD_META_TTL`]. Drop-in for `percy.get_guild_roles`.
pub(super) async fn cached_roles(percy: &PercyClient, guild_id: u64) -> Result<Vec<Role>, PercyError> {
    if let Some((ts, roles)) = roles_cache().get(&guild_id) {
        if ts.elapsed() < GUILD_META_TTL {
            return Ok((*roles).clone());
        }
    }
    let roles = percy.get_guild_roles(guild_id).await?;
    roles_cache().insert(guild_id, (Instant::now(), Arc::new(roles.clone())));
    Ok(roles)
}

/// Guild channels, cached for [`GUILD_META_TTL`]. Drop-in for `percy.get_guild_channels`.
pub(super) async fn cached_channels(percy: &PercyClient, guild_id: u64) -> Result<Vec<Channel>, PercyError> {
    if let Some((ts, channels)) = channels_cache().get(&guild_id) {
        if ts.elapsed() < GUILD_META_TTL {
            return Ok((*channels).clone());
        }
    }
    let channels = percy.get_guild_channels(guild_id).await?;
    channels_cache().insert(guild_id, (Instant::now(), Arc::new(channels.clone())));
    Ok(channels)
}

/// How long a fetched-and-rendered legal doc stays cached. The docs change only
/// when the canonical GitHub repo does, so a long TTL spares a network round-trip
/// per view while still picking up edits without a redeploy.
const LEGAL_DOC_TTL: Duration = Duration::from_secs(3600);

/// `raw GitHub url -> (fetched_at, rendered HTML)`.
fn legal_doc_cache() -> &'static Cache<&'static str, Timed<Arc<String>>> {
    static CACHE: OnceLock<Cache<&'static str, Timed<Arc<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Cache::new(8))
}

/// Fetches a markdown document from `url`, renders it to HTML, and caches the
/// result for [`LEGAL_DOC_TTL`]. There is intentionally no local fallback — the
/// page reflects the live source of truth, so a fetch failure surfaces as an
/// error to the caller rather than serving a stale embedded copy.
pub(super) async fn cached_legal_doc(state: &AppState, url: &'static str) -> Result<Arc<String>, reqwest::Error> {
    if let Some((ts, html)) = legal_doc_cache().get(&url) {
        if ts.elapsed() < LEGAL_DOC_TTL {
            return Ok(html);
        }
    }
    let markdown = state
        .client
        .get(url)
        .timeout(Duration::from_secs(8))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let html = Arc::new(render_markdown(&markdown));
    legal_doc_cache().insert(url, (Instant::now(), html.clone()));
    Ok(html)
}

/// Renders CommonMark (plus tables, strikethrough, task-lists, footnotes) to
/// HTML. The input is a trusted document from our own repo, so embedded raw
/// HTML is passed through.
fn render_markdown(markdown: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(markdown, options);
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

async fn check_guild_access(percy: &PercyClient, discord_id: &str, guild_id: u64) -> bool {
    let guild_id_str = guild_id.to_string();

    // Fast path: an unexpired manageable-guild set for this user.
    if let Some((ts, guilds)) = guild_access_cache().get(discord_id) {
        if ts.elapsed() < ACCESS_TTL {
            return guilds.contains(&guild_id_str);
        }
    }

    // Miss/expired: refetch from Percy. A transient failure is *not* cached, so a
    // Percy blip can't lock an admin out for the whole TTL.
    let guilds = match percy.get_user_guilds(discord_id).await {
        Ok(g) => g,
        Err(_) => return false,
    };
    let set: HashSet<String> = guilds.into_iter().map(|g| g.id).collect();
    let allowed = set.contains(&guild_id_str);
    guild_access_cache().insert(discord_id.to_string(), (Instant::now(), Arc::new(set)));
    allowed
}

fn invite_client_id(state: &AppState) -> String {
    state
        .config()
        .percy
        .bot_client_id
        .as_deref()
        .or(state.config().discord.client_id.as_deref())
        .unwrap_or("MISSING_CLIENT_ID")
        .to_string()
}

fn build_invite_url(state: &AppState, guild_id: u64) -> String {
    let client_id = invite_client_id(state);
    format!(
        "https://discord.com/oauth2/authorize?client_id={client_id}&scope=bot+applications.commands&permissions=8&guild_id={guild_id}"
    )
}

/// Invite URL without a pre-selected guild — lets the user pick which server to
/// add Percy to from Discord's own dropdown.
fn build_general_invite_url(state: &AppState) -> String {
    let client_id = invite_client_id(state);
    format!("https://discord.com/oauth2/authorize?client_id={client_id}&scope=bot+applications.commands&permissions=8")
}

fn is_ajax(headers: &HeaderMap) -> bool {
    headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("application/json"))
        .unwrap_or(false)
}

fn json_or_flash(headers: &HeaderMap, flasher: &Flasher, ok: bool, msg: &str, redirect: &str) -> Response {
    if is_ajax(headers) {
        if ok {
            Json(serde_json::json!({"ok": true, "message": msg})).into_response()
        } else {
            Json(serde_json::json!({"ok": false, "error": msg})).into_response()
        }
    } else {
        flasher.add(msg).bail(redirect)
    }
}

// -- Router ------------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/percy",
            get(|| async { axum::response::Redirect::to("/percy/dashboard") }),
        )
        // Public legal docs, rendered live from the canonical GitHub repo.
        .route("/percy/privacy-policy", get(percy_privacy))
        .route("/percy/terms-of-service", get(percy_terms))
        .route("/percy/dashboard", get(guild_list))
        .route("/percy/dashboard/guild/:guild_id", get(guild_detail))
        .route("/percy/dashboard/guild/:guild_id/config", post(guild_config_update))
        .route("/percy/dashboard/guild/:guild_id/sentinel", post(guild_sentinel_update))
        .route("/percy/dashboard/guild/:guild_id/members", get(guild_members))
        .route("/percy/dashboard/guild/:guild_id/members.json", get(guild_members_json))
        .route(
            "/percy/dashboard/guild/:guild_id/members/:user_id",
            get(guild_user_lookup),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/members/:user_id/avatars",
            get(guild_member_avatars),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/members/:user_id/action",
            post(guild_member_action),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/members/:user_id/roles",
            post(guild_member_roles),
        )
        // Feature pages
        .route("/percy/dashboard/guild/:guild_id/leveling", get(guild_leveling))
        .route(
            "/percy/dashboard/guild/:guild_id/leveling/users/:user_id",
            post(guild_leveling_update),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/leveling/config",
            post(guild_leveling_config_update),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/leveling/roles",
            post(guild_leveling_roles),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/leveling/roles/preset",
            post(guild_leveling_roles_preset),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/leveling/multipliers",
            post(guild_leveling_multipliers),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/leveling/blacklist",
            post(guild_leveling_blacklist),
        )
        .route("/percy/dashboard/guild/:guild_id/economy", get(guild_economy))
        .route(
            "/percy/dashboard/guild/:guild_id/economy/items",
            post(guild_economy_items_create),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/economy/items/:name",
            delete(guild_economy_item_delete),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/economy/lottery",
            post(guild_economy_lottery_create).delete(guild_economy_lottery_delete),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/economy/balances/:user_id",
            patch(guild_economy_balance_update),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/status-feed",
            post(guild_status_feed_subscribe).delete(guild_status_feed_unsubscribe),
        )
        .route("/percy/dashboard/guild/:guild_id/music", get(guild_music))
        .route("/percy/dashboard/guild/:guild_id/music/status", get(guild_music_status))
        .route("/percy/dashboard/guild/:guild_id/music/setup", post(guild_music_setup))
        .route("/percy/dashboard/guild/:guild_id/music/reset", post(guild_music_reset))
        .route(
            "/percy/dashboard/guild/:guild_id/music/equalizer",
            post(guild_music_equalizer),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/music/filters",
            post(guild_music_filters),
        )
        .route("/percy/dashboard/guild/:guild_id/music/247", post(guild_music_247))
        .route("/percy/dashboard/guild/:guild_id/music/dj-mode", patch(guild_music_dj_mode))
        .route(
            "/percy/dashboard/guild/:guild_id/autoresponders",
            get(guild_autoresponders).post(guild_autoresponders_action),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/autoresponders/:trigger",
            patch(guild_autoresponder_patch).delete(guild_autoresponder_delete),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/comics",
            get(guild_comics).post(guild_comic_create),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/comics/:brand",
            patch(guild_comic_patch).delete(guild_comic_delete),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/comics/:brand/push",
            post(guild_comics_push),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/temp-channels",
            get(guild_temp_channels).post(guild_temp_channel_create),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/temp-channels/:channel_id",
            patch(guild_temp_channel_patch).delete(guild_temp_channel_delete),
        )
        // Browse pages
        .route(
            "/percy/dashboard/guild/:guild_id/polls",
            get(guild_polls).post(guild_poll_create),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/polls/image",
            post(guild_poll_image_upload),
        )
        .route("/percy/dashboard/guild/:guild_id/polls/:poll_id", post(guild_poll_edit))
        .route(
            "/percy/dashboard/guild/:guild_id/polls/:poll_id/end",
            post(guild_poll_end),
        )
        .route("/percy/dashboard/guild/:guild_id/giveaways", get(guild_giveaways))
        .route("/percy/dashboard/guild/:guild_id/tags", get(guild_tags))
        .route("/percy/dashboard/guild/:guild_id/highlights", get(guild_highlights))
        .route(
            "/percy/dashboard/guild/:guild_id/highlights/:user_id",
            delete(guild_highlight_delete),
        )
        .route("/percy/dashboard/guild/:guild_id/emoji-stats", get(guild_emoji_stats))
        .route("/percy/dashboard/guild/:guild_id/commands", get(guild_commands))
        .route(
            "/percy/dashboard/guild/:guild_id/commands/toggle",
            post(guild_command_toggle),
        )
        .route("/percy/dashboard/guild/:guild_id/plonks", post(guild_plonk_manage))
        .route(
            "/percy/dashboard/guild/:guild_id/lockdowns/lock",
            post(guild_lockdown_lock),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/lockdowns/unlock",
            post(guild_lockdown_unlock),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/moderation/ignore",
            post(guild_moderation_ignore),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/audit-log-flags",
            patch(guild_audit_log_flags),
        )
        .route("/percy/dashboard/guild/:guild_id/stats", get(guild_stats))
        // Phase 5: Audit log, bulk actions, activity, export
        .route("/percy/dashboard/guild/:guild_id/audit-log", get(guild_audit_log))
        .route(
            "/percy/dashboard/guild/:guild_id/audit-log.json",
            get(guild_audit_log_json),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/audit-log/recent",
            get(guild_cases_recent),
        )
        .route("/percy/dashboard/guild/:guild_id/cases", post(guild_case_create))
        .route(
            "/percy/dashboard/guild/:guild_id/cases/:case_index",
            patch(guild_case_update).delete(guild_case_delete),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/members/bulk-action",
            post(guild_bulk_action),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/members/:user_id/activity",
            get(guild_member_activity),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/export/leaderboard.csv",
            get(guild_export_leaderboard),
        )
        .route(
            "/percy/dashboard/guild/:guild_id/export/cases.csv",
            get(guild_export_cases),
        )
}
