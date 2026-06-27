//! Percy bot dashboard: a multi-page guild management UI proxied to Percy.
//!
//! Split into [`templates`] (Askama views) and [`handlers`] (route handlers);
//! this module owns the shared helpers and the router assembly.
//!
//! The dashboard is reached at the bare `percy.<domain>` subdomain (e.g.
//! `percy.klappstuhl.me/dashboard`); its routes are registered here at their
//! bare paths (`/dashboard`, `/lb`, `/privacy-policy`, `/terms-of-service`) and
//! are served on whatever host the request arrives at. Auth carries over from
//! the apex because the session cookie is scoped to the registrable domain (see
//! [`crate::config::Config::cookie_domain`]).

mod handlers;
mod templates;

use std::{
    collections::HashMap,
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

/// `discord_user_id -> (fetched_at, {guild id -> manageable})`. Removes one Percy
/// round-trip (`get_user_guilds`) from every dashboard page load and mutation.
/// The bool distinguishes a managed server (admin/Manage Server) from one the
/// user is merely a member of (read-only public overview).
fn guild_access_cache() -> &'static Cache<String, Timed<Arc<HashMap<String, bool>>>> {
    static CACHE: OnceLock<Cache<String, Timed<Arc<HashMap<String, bool>>>>> = OnceLock::new();
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

fn md_files_cache() -> &'static Cache<String, Arc<String>> {
    // storage for locally saved md files, files that get saved in this cache are cached once
    // and will remain there for as long as the server runs so we don't have to fetch them from the disk every time we need them
    // so we dont need a timed cache
    static CACHE: OnceLock<Cache<String, Arc<String>>> = OnceLock::new();
    CACHE.get_or_init(|| Cache::new(100))
}

fn cached_md_file(path: String) -> Result<Arc<String>, std::io::Error> {
    if let Some(html) = md_files_cache().get(&path) {
        return Ok(html);
    }

    // Notice the `?` operator here to propagate actual I/O errors
    let content = std::fs::read_to_string(&path)?;
    let html = Arc::new(render_markdown(&content));

    md_files_cache().insert(path.to_string(), html.clone());
    Ok(html)
}

/// Cached bot version string (60 s TTL, matching the stats poller interval).
fn bot_version_cache() -> &'static Cache<(), Timed<String>> {
    static CACHE: OnceLock<Cache<(), Timed<String>>> = OnceLock::new();
    CACHE.get_or_init(|| Cache::new(1))
}

pub(super) async fn cached_bot_version(state: &AppState) -> Option<String> {
    if let Some((ts, v)) = bot_version_cache().get(&()) {
        if ts.elapsed() < ACCESS_TTL {
            return Some(v);
        }
    }
    let percy = get_percy_client(state)?;
    let stats = percy.get_bot_stats().await.ok()?;
    let version = stats.version?;
    bot_version_cache().insert((), (Instant::now(), version.clone()));
    Some(version)
}

/// Changelog entries (10 min TTL — git history changes only on deploy).
const CHANGELOG_TTL: Duration = Duration::from_secs(600);

fn changelog_cache() -> &'static Cache<(), Timed<Arc<Vec<crate::percy::ChangelogEntry>>>> {
    static CACHE: OnceLock<Cache<(), Timed<Arc<Vec<crate::percy::ChangelogEntry>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Cache::new(1))
}

pub(super) async fn cached_changelog(state: &AppState) -> Vec<crate::percy::ChangelogEntry> {
    if let Some((ts, entries)) = changelog_cache().get(&()) {
        if ts.elapsed() < CHANGELOG_TTL {
            return (*entries).clone();
        }
    }
    let Some(percy) = get_percy_client(state) else {
        return Vec::new();
    };
    let Ok(resp) = percy.get_changelog().await else {
        return Vec::new();
    };
    let entries = Arc::new(resp.entries);
    changelog_cache().insert((), (Instant::now(), entries.clone()));
    (*entries).clone()
}

/// Fetches (cached) the user's mutual-guild map (`guild_id -> manageable`).
/// `None` on a Percy failure, which is deliberately *not* cached so a blip can't
/// lock a user out for the whole TTL.
async fn user_guild_map(percy: &PercyClient, discord_id: &str) -> Option<Arc<HashMap<String, bool>>> {
    if let Some((ts, map)) = guild_access_cache().get(discord_id) {
        if ts.elapsed() < ACCESS_TTL {
            return Some(map);
        }
    }
    let guilds = percy.get_user_guilds(discord_id).await.ok()?;
    let map: Arc<HashMap<String, bool>> = Arc::new(guilds.into_iter().map(|g| (g.id, g.manageable)).collect());
    guild_access_cache().insert(discord_id.to_string(), (Instant::now(), map.clone()));
    Some(map)
}

/// Whether the user can *manage* the guild (admin / Manage Server). Gates every
/// admin dashboard page and mutation.
async fn check_guild_access(percy: &PercyClient, discord_id: &str, guild_id: u64) -> bool {
    user_guild_map(percy, discord_id)
        .await
        .map(|m| m.get(&guild_id.to_string()).copied().unwrap_or(false))
        .unwrap_or(false)
}

/// Whether the user is merely a *member* of the guild (manageable or not). Gates
/// the read-only public overview and its live music panel.
async fn check_guild_membership(percy: &PercyClient, discord_id: &str, guild_id: u64) -> bool {
    user_guild_map(percy, discord_id)
        .await
        .map(|m| m.contains_key(&guild_id.to_string()))
        .unwrap_or(false)
}

/// A guild the user can manage, captured from Discord OAuth at login. Used to
/// build the dashboard's "Add Percy" cards for servers Percy isn't in yet.
pub(super) struct StoredAdminGuild {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) icon: Option<String>,
    pub(super) owner: bool,
}

/// Loads the user's stored manageable guilds (from the last Discord OAuth login).
async fn get_admin_guilds(state: &AppState, discord_id: &str) -> Vec<StoredAdminGuild> {
    let discord_id = discord_id.to_string();
    state
        .database()
        .call(move |conn| -> rusqlite::Result<Vec<StoredAdminGuild>> {
            let mut stmt = conn.prepare_cached(
                "SELECT guild_id, name, icon, owner FROM user_discord_admin_guilds \
                 WHERE discord_user_id = ? ORDER BY name COLLATE NOCASE",
            )?;
            let rows = stmt.query_map([discord_id], |row| {
                Ok(StoredAdminGuild {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    icon: row.get(2)?,
                    owner: row.get::<_, i64>(3)? != 0,
                })
            })?;
            rows.collect()
        })
        .await
        .unwrap_or_default()
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
        // Public pages (percy subdomain landing page-linked).
        .route("/commands", get(percy_commands))
        .route("/changelog", get(percy_changelog))
        // Public legal docs, rendered live from the canonical GitHub repo.
        .route("/privacy-policy", get(percy_privacy))
        .route("/terms-of-service", get(percy_terms))
        // Public leaderboard (no auth required to view).
        .route("/lb/:target", get(public_leaderboard))
        .route(
            "/lb/:guild_id/vanity",
            post(public_leaderboard_vanity_claim).delete(public_leaderboard_vanity_delete),
        )
        .route("/dashboard/bot-version", get(percy_bot_version))
        .route("/dashboard", get(guild_list))
        // Personal dashboard for non-admin members (own stats/leveling/economy).
        .route("/dashboard/guild/:guild_id/me", get(guild_user_self))
        .route(
            "/dashboard/guild/:guild_id/me/settings",
            post(guild_user_settings_update),
        )
        .route("/dashboard/guild/:guild_id/me/history", get(guild_user_history))
        .route("/dashboard/guild/:guild_id/me/data-export", get(guild_user_data_export))
        .route(
            "/dashboard/guild/:guild_id/me/delete-data",
            post(guild_user_data_delete),
        )
        // Public read-only server overview for members without manage access.
        .route("/dashboard/guild/:guild_id/overview", get(guild_overview))
        .route(
            "/dashboard/guild/:guild_id/overview/music",
            get(guild_overview_music_status),
        )
        .route(
            "/dashboard/guild/:guild_id/overview/music/control",
            post(guild_overview_music_control),
        )
        .route(
            "/dashboard/guild/:guild_id/overview/music/lyrics",
            get(guild_overview_music_lyrics),
        )
        .route("/dashboard/guild/:guild_id", get(guild_detail))
        .route("/dashboard/guild/:guild_id/config", post(guild_config_update))
        .route("/dashboard/guild/:guild_id/ai", post(guild_ai_flags_update))
        .route("/dashboard/guild/:guild_id/ai/override", post(guild_ai_override_update))
        .route(
            "/dashboard/guild/:guild_id/ai/override/:channel_id/delete",
            post(guild_ai_override_delete),
        )
        .route("/dashboard/guild/:guild_id/sentinel", post(guild_sentinel_update))
        .route("/dashboard/guild/:guild_id/members", get(guild_members))
        .route("/dashboard/guild/:guild_id/members.json", get(guild_members_json))
        .route("/dashboard/guild/:guild_id/members/:user_id", get(guild_user_lookup))
        .route(
            "/dashboard/guild/:guild_id/members/:user_id/avatars",
            get(guild_member_avatars),
        )
        .route(
            "/dashboard/guild/:guild_id/members/:user_id/action",
            post(guild_member_action),
        )
        .route(
            "/dashboard/guild/:guild_id/members/:user_id/roles",
            post(guild_member_roles),
        )
        // Feature pages
        .route("/dashboard/guild/:guild_id/leveling", get(guild_leveling))
        .route(
            "/dashboard/guild/:guild_id/leveling/users/:user_id",
            post(guild_leveling_update),
        )
        .route(
            "/dashboard/guild/:guild_id/leveling/config",
            post(guild_leveling_config_update),
        )
        .route("/dashboard/guild/:guild_id/leveling/roles", post(guild_leveling_roles))
        .route(
            "/dashboard/guild/:guild_id/leveling/roles/preset",
            post(guild_leveling_roles_preset),
        )
        .route(
            "/dashboard/guild/:guild_id/leveling/multipliers",
            post(guild_leveling_multipliers),
        )
        .route(
            "/dashboard/guild/:guild_id/leveling/blacklist",
            post(guild_leveling_blacklist),
        )
        .route("/dashboard/guild/:guild_id/economy", get(guild_economy))
        .route(
            "/dashboard/guild/:guild_id/economy/items",
            post(guild_economy_items_create),
        )
        .route(
            "/dashboard/guild/:guild_id/economy/items/:name",
            delete(guild_economy_item_delete),
        )
        .route(
            "/dashboard/guild/:guild_id/economy/lottery",
            post(guild_economy_lottery_create).delete(guild_economy_lottery_delete),
        )
        .route(
            "/dashboard/guild/:guild_id/economy/balances/:user_id",
            patch(guild_economy_balance_update),
        )
        .route(
            "/dashboard/guild/:guild_id/status-feed",
            post(guild_status_feed_subscribe).delete(guild_status_feed_unsubscribe),
        )
        .route("/dashboard/guild/:guild_id/music", get(guild_music))
        .route("/dashboard/guild/:guild_id/music/status", get(guild_music_status))
        .route("/dashboard/guild/:guild_id/music/control", post(guild_music_control))
        .route("/dashboard/guild/:guild_id/music/lyrics", get(guild_music_lyrics))
        .route("/dashboard/guild/:guild_id/music/setup", post(guild_music_setup))
        .route("/dashboard/guild/:guild_id/music/reset", post(guild_music_reset))
        .route(
            "/dashboard/guild/:guild_id/music/equalizer",
            post(guild_music_equalizer),
        )
        .route("/dashboard/guild/:guild_id/music/filters", post(guild_music_filters))
        .route("/dashboard/guild/:guild_id/music/247", post(guild_music_247))
        .route("/dashboard/guild/:guild_id/music/dj-mode", patch(guild_music_dj_mode))
        .route("/dashboard/guild/:guild_id/custom-bot", patch(guild_custom_bot_update))
        .route(
            "/dashboard/guild/:guild_id/custom-bot/reset",
            post(guild_custom_bot_reset),
        )
        .route(
            "/dashboard/guild/:guild_id/autoresponders",
            get(guild_autoresponders).post(guild_autoresponders_action),
        )
        .route(
            "/dashboard/guild/:guild_id/autoresponders/:trigger",
            patch(guild_autoresponder_patch).delete(guild_autoresponder_delete),
        )
        .route(
            "/dashboard/guild/:guild_id/comics",
            get(guild_comics).post(guild_comic_create),
        )
        .route(
            "/dashboard/guild/:guild_id/comics/:brand",
            patch(guild_comic_patch).delete(guild_comic_delete),
        )
        .route("/dashboard/guild/:guild_id/comics/:brand/push", post(guild_comics_push))
        .route(
            "/dashboard/guild/:guild_id/temp-channels",
            get(guild_temp_channels).post(guild_temp_channel_create),
        )
        .route(
            "/dashboard/guild/:guild_id/temp-channels/:channel_id",
            patch(guild_temp_channel_patch).delete(guild_temp_channel_delete),
        )
        // Browse pages
        .route(
            "/dashboard/guild/:guild_id/polls",
            get(guild_polls).post(guild_poll_create),
        )
        .route("/dashboard/guild/:guild_id/polls/image", post(guild_poll_image_upload))
        .route("/dashboard/guild/:guild_id/polls/:poll_id", post(guild_poll_edit))
        .route("/dashboard/guild/:guild_id/polls/:poll_id/end", post(guild_poll_end))
        .route(
            "/dashboard/guild/:guild_id/giveaways",
            get(guild_giveaways).post(guild_giveaway_create),
        )
        .route(
            "/dashboard/guild/:guild_id/giveaways/:giveaway_id/end",
            post(guild_giveaway_end),
        )
        .route(
            "/dashboard/guild/:guild_id/giveaways/:giveaway_id",
            delete(guild_giveaway_delete),
        )
        .route("/dashboard/guild/:guild_id/tags", get(guild_tags))
        .route("/dashboard/guild/:guild_id/tags/export.csv", get(guild_tags_export))
        .route("/dashboard/guild/:guild_id/tags/import", post(guild_tags_import))
        .route(
            "/dashboard/guild/:guild_id/tags/:tag_id",
            get(guild_tag_detail).delete(guild_tag_delete),
        )
        .route("/dashboard/guild/:guild_id/highlights", get(guild_highlights))
        .route(
            "/dashboard/guild/:guild_id/highlights/:user_id",
            delete(guild_highlight_delete),
        )
        .route("/dashboard/guild/:guild_id/emoji-stats", get(guild_emoji_stats))
        .route("/dashboard/guild/:guild_id/commands", get(guild_commands))
        .route("/dashboard/guild/:guild_id/commands/toggle", post(guild_command_toggle))
        .route("/dashboard/guild/:guild_id/plonks", post(guild_plonk_manage))
        .route("/dashboard/guild/:guild_id/lockdowns/lock", post(guild_lockdown_lock))
        .route(
            "/dashboard/guild/:guild_id/lockdowns/unlock",
            post(guild_lockdown_unlock),
        )
        .route(
            "/dashboard/guild/:guild_id/moderation/ignore",
            post(guild_moderation_ignore),
        )
        .route(
            "/dashboard/guild/:guild_id/audit-log-flags",
            patch(guild_audit_log_flags),
        )
        .route("/dashboard/guild/:guild_id/stats", get(guild_stats))
        // Phase 5: Audit log, bulk actions, activity, export
        .route("/dashboard/guild/:guild_id/audit-log", get(guild_audit_log))
        .route("/dashboard/guild/:guild_id/audit-log.json", get(guild_audit_log_json))
        .route("/dashboard/guild/:guild_id/audit-log/recent", get(guild_cases_recent))
        .route("/dashboard/guild/:guild_id/cases", post(guild_case_create))
        .route(
            "/dashboard/guild/:guild_id/cases/:case_index",
            patch(guild_case_update).delete(guild_case_delete),
        )
        .route(
            "/dashboard/guild/:guild_id/members/bulk-action",
            post(guild_bulk_action),
        )
        .route(
            "/dashboard/guild/:guild_id/members/:user_id/activity",
            get(guild_member_activity),
        )
        .route(
            "/dashboard/guild/:guild_id/export/leaderboard.csv",
            get(guild_export_leaderboard),
        )
        .route("/dashboard/guild/:guild_id/export/cases.csv", get(guild_export_cases))
}
