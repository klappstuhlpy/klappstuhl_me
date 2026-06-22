//! Request handlers for the Percy dashboard routes.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Form, Json,
};
use serde::Deserialize;
use std::collections::HashMap;

use crate::{
    flash::{Flasher, Flashes},
    models::Account,
    percy::*,
    AppState,
};

use super::templates::*;
use super::{
    build_general_invite_url, build_invite_url, cached_channels, cached_legal_doc, cached_roles, check_guild_access,
    check_guild_membership, get_admin_guilds, get_discord_id, get_percy_client, json_or_flash,
};

/// Canonical sources for Percy's public legal docs, fetched live and rendered.
const PRIVACY_POLICY_URL: &str = "https://raw.githubusercontent.com/klappstuhlpy/Percy/master/PRIVACY_POLICY.md";
const TERMS_OF_SERVICE_URL: &str = "https://raw.githubusercontent.com/klappstuhlpy/Percy/master/TERMS_OF_SERVICE.md";

/// `GET /privacy-policy` — Percy's Privacy Policy.
pub(super) async fn percy_privacy(
    State(state): State<AppState>,
    account: Option<Account>,
    flashes: Flashes,
) -> Response {
    render_legal_doc(&state, account, flashes, "Privacy Policy", PRIVACY_POLICY_URL).await
}

/// `GET /terms-of-service` — Percy's Terms of Service.
pub(super) async fn percy_terms(State(state): State<AppState>, account: Option<Account>, flashes: Flashes) -> Response {
    render_legal_doc(&state, account, flashes, "Terms of Service", TERMS_OF_SERVICE_URL).await
}

/// Shared rendering for the public legal pages: fetch (cached) the canonical
/// markdown, render to HTML, and wrap it in the site layout. A fetch failure
/// surfaces as 502 — by design there is no embedded fallback copy.
async fn render_legal_doc(
    state: &AppState,
    account: Option<Account>,
    flashes: Flashes,
    title: &'static str,
    url: &'static str,
) -> Response {
    match cached_legal_doc(state, url).await {
        Ok(content) => PercyLegalTemplate {
            account,
            flashes,
            title,
            content: (*content).clone(),
        }
        .into_response(),
        Err(e) => {
            tracing::warn!(error = %e, url, "failed to fetch Percy legal doc");
            (
                StatusCode::BAD_GATEWAY,
                "Could not load this document right now. Please try again shortly.",
            )
                .into_response()
        }
    }
}

pub(super) async fn guild_list(State(state): State<AppState>, account: Option<Account>, flashes: Flashes) -> Response {
    // Logged-out visitors get a welcome/landing page that introduces the bot
    // and links to login/signup (which redirect back here afterwards).
    let Some(account) = account else {
        return PercyLandingTemplate { account: None, flashes }.into_response();
    };

    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };

    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return NoDiscordTemplate {
            account: Some(account),
            flashes,
        }
        .into_response();
    };

    // Percy knows the guilds it *shares* with the user (each tagged manageable);
    // the stored OAuth guild list adds servers the user manages that Percy isn't
    // in yet. Fetch both concurrently.
    let (mutual, admin_guilds) = tokio::join!(
        percy.get_user_guilds(&discord_id),
        get_admin_guilds(&state, &discord_id),
    );
    let mutual = mutual.unwrap_or_default();

    // Guilds Percy is already in (any membership) — used to exclude them from the
    // "Add Percy" bucket.
    let present: std::collections::HashSet<&str> = mutual.iter().map(|g| g.id.as_str()).collect();

    let add_percy: Vec<AddPercyGuild> = admin_guilds
        .into_iter()
        .filter(|g| !present.contains(g.id.as_str()))
        .map(|g| AddPercyGuild {
            invite_url: build_invite_url(&state, g.id.parse().unwrap_or(0)),
            icon_url: g
                .icon
                .as_deref()
                .map(|hash| format!("https://cdn.discordapp.com/icons/{}/{}.png", g.id, hash)),
            name: g.name,
            owner: g.owner,
        })
        .collect();

    let (managed, public): (Vec<UserGuild>, Vec<UserGuild>) = mutual.into_iter().partition(|g| g.manageable);

    GuildsTemplate {
        account: Some(account),
        flashes,
        managed,
        public,
        add_percy,
        invite_url: build_general_invite_url(&state),
    }
    .into_response()
}

pub(super) async fn guild_detail(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };

    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    // Fan out the independent reads concurrently; `join!` (not `try_join!`) so a
    // single degraded sub-resource doesn't abort the others — each is handled below.
    let (guild, channels, roles, sentinel, lockdowns, status_feed, bot_profile) = tokio::join!(
        percy.get_guild(guild_id),
        cached_channels(&percy, guild_id),
        cached_roles(&percy, guild_id),
        percy.get_sentinel(guild_id),
        percy.get_lockdowns(guild_id),
        percy.get_status_feed(guild_id),
        percy.get_custom_bot(guild_id),
    );

    let guild = match guild {
        Ok(g) => g,
        Err(PercyError::NotFound) => {
            let invite_url = build_invite_url(&state, guild_id);
            return GuildNotFoundTemplate {
                account: Some(account),
                flashes,
                guild_id,
                invite_url,
            }
            .into_response();
        }
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };

    // The guild itself loaded, but if the role/channel pickers failed to fetch we
    // render a warning rather than silently showing empty dropdowns (saving with
    // empty pickers could clobber config).
    let degraded = channels.is_err() || roles.is_err();
    let channels = channels.unwrap_or_default();
    let roles = roles.unwrap_or_default();
    let sentinel = sentinel.ok().flatten();
    let lockdowns = lockdowns.unwrap_or(LockdownsResponse { entries: Vec::new() });
    let status_feed = status_feed.unwrap_or(StatusFeedInfo {
        subscribed: false,
        channel: None,
    });
    let bot_profile = bot_profile.unwrap_or_default();

    GuildTemplate {
        account: Some(account),
        flashes,
        guild,
        channels,
        roles,
        sentinel,
        lockdowns,
        status_feed,
        bot_profile,
        degraded,
    }
    .into_response()
}

/// `GET /dashboard/guild/:guild_id/overview` — read-only public overview
/// for members who can't manage the guild. Access requires only that the viewer
/// shares the guild with Percy; everything shown is already visible to members
/// in Discord.
pub(super) async fn guild_overview(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_membership(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    let (guild, stats, bot_stats, music, leaderboard, polls, giveaways, economy) = tokio::join!(
        percy.get_guild(guild_id),
        percy.get_guild_stats(guild_id),
        percy.get_bot_stats(),
        percy.get_music(guild_id),
        percy.get_leveling_leaderboard(guild_id, 10),
        percy.get_polls(guild_id),
        percy.get_giveaways(guild_id),
        percy.get_economy(guild_id),
    );

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };
    let stats = match stats {
        Ok(s) => s,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };

    let bot_stats = bot_stats.unwrap_or(BotStats {
        guild_count: 0,
        user_count: 0,
        channel_count: 0,
        total_commands_used: 0,
        cog_count: 0,
        command_count: 0,
        latency_ms: 0.0,
        uptime_seconds: 0.0,
    });
    let music = music.unwrap_or(MusicInfo {
        active: false,
        equalizer: vec![0.0; 15],
        filters: MusicFiltersState {
            nightcore: false,
            eight_d: false,
            lowpass: None,
        },
        presets: vec![],
        now_playing: None,
        queue: Vec::new(),
        history: Vec::new(),
        channel: None,
        channel_name: None,
        setup: None,
        always_on: Default::default(),
        listeners: Vec::new(),
    });
    let can_control_music = music.active && music.listeners.contains(&discord_id);
    let leaderboard = leaderboard.unwrap_or(LeaderboardResponse {
        entries: Vec::new(),
        total: 0,
    });
    let active_polls = polls
        .map(|p| p.polls.into_iter().filter(|p| !p.ended).collect())
        .unwrap_or_default();
    let active_giveaways = giveaways
        .map(|g| g.giveaways.into_iter().filter(|g| !g.ended).collect())
        .unwrap_or_default();
    let economy = economy.unwrap_or(EconomyInfo {
        items: Vec::new(),
        lottery: None,
    });

    OverviewTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        guild_icon: guild.icon_url,
        member_count: guild.member_count,
        stats,
        bot_stats,
        music,
        can_control_music,
        leaderboard,
        active_polls,
        active_giveaways,
        economy,
    }
    .into_response()
}

/// `GET /dashboard/guild/:guild_id/overview/music` — live now-playing
/// state for the public overview's auto-refreshing music panel. Membership-gated.
pub(super) async fn guild_overview_music_status(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false})).into_response();
    };
    if !check_guild_membership(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.get_music(guild_id).await {
        Ok(music) => {
            let can_control = music.active && music.listeners.contains(&discord_id);
            Json(serde_json::json!({
                "ok": true,
                "active": music.active,
                "channel": music.channel,
                "channel_name": music.channel_name,
                "now_playing": music.now_playing,
                "queue": music.queue,
                "history": music.history,
                "can_control": can_control,
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

/// `GET /dashboard/guild/:guild_id/overview/music/lyrics` — synced lyrics
/// for the public overview's player. Membership-gated.
pub(super) async fn guild_overview_music_lyrics(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false})).into_response();
    };
    if !check_guild_membership(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    lyrics_json(&percy, guild_id).await
}

/// Shared helper: fetch lyrics and wrap them with an `ok` flag for the player JS.
async fn lyrics_json(percy: &PercyClient, guild_id: u64) -> Response {
    match percy.get_music_lyrics(guild_id).await {
        Ok(l) => Json(serde_json::json!({
            "ok": true,
            "has_synced": l.has_synced,
            "title": l.title,
            "source": l.source,
            "lines": l.lines,
            "plain": l.plain,
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

/// `POST /dashboard/guild/:guild_id/overview/music/control` — drive the
/// shared live player from the public overview (pause/resume/skip/stop, plus
/// volume/seek/jump/move). The viewer's Discord id is taken from the session
/// (never the request body); the full body is forwarded so action arguments
/// (`value`, `position`, …) survive. Percy enforces voice-presence and DJ-mode.
pub(super) async fn guild_overview_music_control(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(mut body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false, "error": "Not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false, "error": "Not authenticated"})).into_response();
    };
    if !check_guild_membership(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    // Inject the authenticated identity; never trust a client-supplied user_id.
    if let Some(obj) = body.as_object_mut() {
        obj.insert("user_id".into(), serde_json::Value::String(discord_id));
    }
    match percy.music_control(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct ConfigForm {
    #[serde(rename = "_section")]
    section: String,
    #[serde(flatten)]
    fields: HashMap<String, String>,
}

pub(super) async fn guild_config_update(
    State(state): State<AppState>,
    account: Account,
    flasher: Flasher,
    headers: HeaderMap,
    Path(guild_id): Path<u64>,
    Form(form): Form<ConfigForm>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };

    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(&headers, &flasher, false, "Access denied.", "/dashboard");
    }

    let patch = build_patch(&form.section, &form.fields);
    let redirect_url = format!("/dashboard/guild/{guild_id}");

    match percy.patch_guild_config(guild_id, &patch).await {
        Ok(()) => {
            if form.section == "flags" {
                // Auto-enable/disable sentinel when its flag is toggled
                let gk_enabled = form.fields.contains_key("sentinel");
                if let Err(e) = percy.toggle_sentinel(guild_id, gk_enabled).await {
                    tracing::warn!(error = %e, "Sentinel toggle after flag change");
                }

                // Clear associated config fields when flags are turned off
                let mut clear = serde_json::Map::new();
                if !form.fields.contains_key("audit_log") {
                    clear.insert("audit_log_channel_id".into(), serde_json::Value::Null);
                }
                if !form.fields.contains_key("alerts") {
                    clear.insert("alert_channel_id".into(), serde_json::Value::Null);
                }
                if !form.fields.contains_key("mentions") {
                    clear.insert("mention_count".into(), serde_json::Value::Null);
                }
                if !clear.is_empty() {
                    if let Err(e) = percy
                        .patch_guild_config(guild_id, &serde_json::Value::Object(clear))
                        .await
                    {
                        tracing::warn!(error = %e, "Failed to clear config fields on flag disable");
                    }
                }
            }
            json_or_flash(&headers, &flasher, true, "Settings saved.", &redirect_url)
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to update guild config");
            json_or_flash(&headers, &flasher, false, "Failed to save settings.", &redirect_url)
        }
    }
}

pub(super) async fn guild_sentinel_update(
    State(state): State<AppState>,
    account: Account,
    flasher: Flasher,
    headers: HeaderMap,
    Path(guild_id): Path<u64>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };

    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(&headers, &flasher, false, "Access denied.", "/dashboard");
    }

    let redirect_url = format!("/dashboard/guild/{guild_id}");

    let starter_title = form.get("starter_title").filter(|s| !s.is_empty());
    let starter_content = form.get("starter_content").filter(|s| !s.is_empty());
    let channel_id = form.get("channel_id").and_then(|s| s.parse::<u64>().ok());
    let sent_starter = starter_title.is_some() && starter_content.is_some() && channel_id.is_some();

    if let (Some(title), Some(content), Some(ch_id)) = (starter_title, starter_content, channel_id) {
        match percy.send_sentinel_message(guild_id, ch_id, title, content).await {
            Ok(_message_id) => {}
            Err(e) => {
                tracing::error!(error = %e, "Failed to send sentinel starter message");
                let msg = format!("Failed to send verification message: {e}");
                return json_or_flash(&headers, &flasher, false, &msg, &redirect_url);
            }
        }
    }

    let mut patch = build_sentinel_patch(&form);
    // channel_id was already set by send_sentinel_message
    if sent_starter {
        if let Some(obj) = patch.as_object_mut() {
            obj.remove("channel_id");
        }
    }
    if !patch.as_object().map_or(true, |m| m.is_empty()) {
        if let Err(e) = percy.patch_sentinel(guild_id, &patch).await {
            tracing::error!(error = %e, "Failed to update sentinel config");
            return json_or_flash(
                &headers,
                &flasher,
                false,
                "Failed to save sentinel settings.",
                &redirect_url,
            );
        }
    }

    // Try to auto-enable if all required fields are now set
    if let Err(e) = percy.toggle_sentinel(guild_id, true).await {
        tracing::debug!(error = %e, "Sentinel not ready to enable (expected if setup incomplete)");
    }

    json_or_flash(&headers, &flasher, true, "Sentinel settings saved.", &redirect_url)
}

pub(super) async fn guild_members(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };

    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    let (guild, members, roles) = tokio::join!(
        percy.get_guild(guild_id),
        percy.get_guild_members(guild_id, 100, 0),
        cached_roles(&percy, guild_id),
    );

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };

    let members = members.unwrap_or_default();
    let roles = roles.unwrap_or_default();

    MembersTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "members",
        page_title: "Members",
        members,
        roles,
    }
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct MembersQuery {
    #[serde(default = "default_limit")]
    limit: u32,
    #[serde(default)]
    after: u64,
}

fn default_limit() -> u32 {
    100
}

pub(super) async fn guild_members_json(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Query(query): Query<MembersQuery>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };

    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    let members = percy
        .get_guild_members(guild_id, query.limit.min(100), query.after)
        .await
        .unwrap_or_default();

    Json(serde_json::json!({"members": members})).into_response()
}

#[derive(Deserialize)]
pub(super) struct MemberActionBody {
    action: String,
    reason: Option<String>,
}

pub(super) async fn guild_member_action(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, user_id)): Path<(u64, String)>,
    Json(body): Json<MemberActionBody>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };

    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy
        .member_action(
            guild_id,
            &user_id,
            &body.action,
            body.reason.as_deref(),
            Some(&discord_id),
        )
        .await
    {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct MemberRolesBody {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

pub(super) async fn guild_member_roles(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, user_id)): Path<(u64, String)>,
    Json(body): Json<MemberRolesBody>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };

    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.member_roles(guild_id, &user_id, &body.add, &body.remove).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

// -- New section handlers ----------------------------------------------------

/// Format a Discord role color (`u32`) as a CSS hex string; 0 (no color) maps
/// to Discord's default role gray.
fn color_hex(color: u32) -> String {
    if color == 0 {
        "#99aab5".to_string()
    } else {
        format!("#{color:06x}")
    }
}

pub(super) async fn guild_leveling(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    // Fan out the six independent reads concurrently.
    let (guild, config, leaderboard, xp_history, roles, channels) = tokio::join!(
        percy.get_guild(guild_id),
        percy.get_leveling_config(guild_id),
        percy.get_leveling_leaderboard(guild_id, 25),
        percy.get_leveling_xp_history(guild_id, 30),
        cached_roles(&percy, guild_id),
        cached_channels(&percy, guild_id),
    );

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };

    let config = config.unwrap_or_default();
    let leaderboard = leaderboard.unwrap_or(LeaderboardResponse {
        entries: Vec::new(),
        total: 0,
    });

    // Daily cumulative-XP snapshots for the chart; serialize for the inline uPlot script.
    let xp_history = xp_history.unwrap_or_default();
    let xp_history_json = serde_json::to_string(&xp_history.points).unwrap_or_else(|_| "[]".to_string());

    let roles = roles.unwrap_or_default();
    let channels = channels.unwrap_or_default();

    // Resolve Discord IDs to display names from the fetched role/channel lists.
    let role_name = |id: &str| -> String {
        roles
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| format!("Role {id}"))
    };
    let channel_name = |id: &str| -> String {
        channels
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| format!("Channel {id}"))
    };

    // Level-reward roles, sorted by their level threshold (resolve color too).
    let mut level_roles: Vec<LevelRoleRow> = config
        .level_roles
        .iter()
        .map(|(rid, lvl)| {
            let role = roles.iter().find(|r| &r.id == rid);
            LevelRoleRow {
                role_id: rid.clone(),
                role_name: role.map(|r| r.name.clone()).unwrap_or_else(|| format!("Role {rid}")),
                color_hex: role
                    .map(|r| color_hex(r.color))
                    .unwrap_or_else(|| "#99aab5".to_string()),
                level: *lvl,
            }
        })
        .collect();
    level_roles.sort_by_key(|r| r.level);

    // XP multipliers for roles and channels, sorted by display name.
    let mut multiplier_roles: Vec<MultiplierRow> = config
        .multiplier_roles
        .iter()
        .map(|(rid, m)| MultiplierRow {
            id: rid.clone(),
            name: role_name(rid),
            multiplier: *m,
        })
        .collect();
    multiplier_roles.sort_by(|a, b| a.name.cmp(&b.name));
    let mut multiplier_channels: Vec<MultiplierRow> = config
        .multiplier_channels
        .iter()
        .map(|(cid, m)| MultiplierRow {
            id: cid.clone(),
            name: channel_name(cid),
            multiplier: *m,
        })
        .collect();
    multiplier_channels.sort_by(|a, b| a.name.cmp(&b.name));

    // Blacklists (roles/channels resolve to names; users only have an ID here).
    let blacklisted_roles: Vec<BlacklistRow> = config
        .blacklisted_roles
        .iter()
        .map(|id| BlacklistRow {
            id: id.to_string(),
            name: role_name(&id.to_string()),
        })
        .collect();
    let blacklisted_channels: Vec<BlacklistRow> = config
        .blacklisted_channels
        .iter()
        .map(|id| BlacklistRow {
            id: id.to_string(),
            name: channel_name(&id.to_string()),
        })
        .collect();
    let blacklisted_users: Vec<BlacklistRow> = config
        .blacklisted_users
        .iter()
        .map(|id| BlacklistRow {
            id: id.to_string(),
            name: format!("User {id}"),
        })
        .collect();

    // Custom per-level messages, sorted by numeric level.
    let mut special_messages: Vec<SpecialMsgRow> = config
        .special_level_up_messages
        .iter()
        .map(|(lvl, msg)| SpecialMsgRow {
            level: lvl.clone(),
            message: msg.clone(),
        })
        .collect();
    special_messages.sort_by_key(|r| r.level.parse::<i64>().unwrap_or(0));

    // Text channels for the level-up message destination; all channels for multiplier/blacklist pickers.
    let text_channels: Vec<Channel> = channels
        .iter()
        .filter(|c| c.channel_type == "text" || c.channel_type == "news")
        .cloned()
        .collect();
    let all_channels = channels.clone();

    LevelingTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "leveling",
        page_title: "Leveling",
        config,
        leaderboard,
        roles,
        text_channels,
        all_channels,
        level_roles,
        multiplier_roles,
        multiplier_channels,
        blacklisted_roles,
        blacklisted_channels,
        blacklisted_users,
        special_messages,
        xp_history_json,
    }
    .into_response()
}

/// Aggregated profile page for a single user: identity, leveling, moderation
/// history (warnings/cases), and note count.
pub(super) async fn guild_user_lookup(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path((guild_id, user_id)): Path<(u64, String)>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    let (guild, member) = tokio::join!(percy.get_guild(guild_id), percy.get_member_detail(guild_id, &user_id),);

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };

    let member = match member {
        Ok(m) => m,
        Err(_) => {
            return Redirect::to(&format!("/dashboard/guild/{guild_id}/members")).into_response();
        }
    };

    UserLookupTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        member,
    }
    .into_response()
}

pub(super) async fn guild_member_avatars(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, user_id)): Path<(u64, String)>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "percy unavailable"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "unauthorized"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "forbidden"})).into_response();
    }

    match percy.get_member_avatars(guild_id, &user_id, 20).await {
        Ok(data) => Json(serde_json::json!(data)).into_response(),
        Err(_) => Json(serde_json::json!({"avatars": [], "total": 0})).into_response(),
    }
}

pub(super) async fn guild_polls(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    let (guild, channels, roles, polls) = tokio::join!(
        percy.get_guild(guild_id),
        cached_channels(&percy, guild_id),
        cached_roles(&percy, guild_id),
        percy.get_polls(guild_id),
    );

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };

    let channels = channels.unwrap_or_default();
    let roles = roles.unwrap_or_default();

    let polls = polls.unwrap_or(PollsResponse { polls: Vec::new() });
    let active_count = polls.polls.iter().filter(|p| !p.ended).count();
    let ended_count = polls.polls.iter().filter(|p| p.ended).count();

    PollsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name.clone(),
        guild,
        channels,
        roles,
        nav_active: "polls",
        page_title: "Polls",
        polls,
        active_count,
        ended_count,
    }
    .into_response()
}

pub(super) async fn guild_poll_edit(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, poll_id)): Path<(u64, i64)>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.patch_poll(guild_id, poll_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_poll_end(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, poll_id)): Path<(u64, i64)>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.end_poll(guild_id, poll_id).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_poll_create(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.create_poll(guild_id, &body).await {
        Ok(data) => Json(serde_json::json!({"ok": true, "id": data.get("id")})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

/// Stores an uploaded poll banner in the image host and returns its public URL.
///
/// The poll-create flow only deals in URLs (Percy renders the banner from a URL,
/// exactly like the `/polls create` command uses an attachment's CDN URL), so a
/// file upload is turned into a hosted URL here before the poll is created.
pub(super) async fn guild_poll_image_upload(
    State(state): State<AppState>,
    account: Account,
    crate::headers::ClientIp(client_ip): crate::headers::ClientIp,
    Path(guild_id): Path<u64>,
    multipart: axum::extract::Multipart,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match crate::routes::image::raw_upload_file(state, account, client_ip, multipart, true, None).await {
        Ok(result) if !result.links.is_empty() => {
            // `raw_upload_file` returns the gallery *page* URL (`/gallery/<id>.<ext>`),
            // which is HTML. Discord can only embed a direct image, so hand back the
            // raw bytes URL (`/gallery/raw/<id>.<ext>`) for the poll banner.
            let raw_url = result.links[0].replacen("/gallery/", "/gallery/raw/", 1);
            Json(serde_json::json!({"ok": true, "url": raw_url})).into_response()
        }
        Ok(result) => {
            let msg = if result.infected > 0 {
                "Image blocked: it failed a malware scan."
            } else {
                "Image upload failed."
            };
            Json(serde_json::json!({"error": msg})).into_response()
        }
        Err(e) => Json(serde_json::json!({"error": e.error})).into_response(),
    }
}

pub(super) async fn guild_giveaways(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    let (guild, giveaways) = tokio::join!(percy.get_guild(guild_id), percy.get_giveaways(guild_id));

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };

    let giveaways = giveaways.unwrap_or(GiveawaysResponse { giveaways: Vec::new() });
    let active_count = giveaways.giveaways.iter().filter(|g| !g.ended).count();
    let ended_count = giveaways.giveaways.iter().filter(|g| g.ended).count();

    GiveawaysTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "giveaways",
        page_title: "Giveaways",
        giveaways,
        active_count,
        ended_count,
    }
    .into_response()
}

pub(super) async fn guild_tags(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    let (guild, tags) = tokio::join!(percy.get_guild(guild_id), percy.get_tags(guild_id));

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };

    let tags = tags.unwrap_or(TagsResponse {
        total: 0,
        total_uses: 0,
        tags: Vec::new(),
        top_creators: Vec::new(),
    });

    TagsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "tags",
        page_title: "Tags",
        tags,
    }
    .into_response()
}

pub(super) async fn guild_commands(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    let (guild, commands, channels) = tokio::join!(
        percy.get_guild(guild_id),
        percy.get_commands(guild_id),
        cached_channels(&percy, guild_id),
    );

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };

    let commands = commands.unwrap_or(CommandsResponse {
        commands: Vec::new(),
        plonks: Vec::new(),
    });
    let channels = channels.unwrap_or_default();
    let disabled_count = commands.commands.iter().filter(|c| c.globally_disabled).count();
    let partial_count = commands
        .commands
        .iter()
        .filter(|c| !c.globally_disabled && !c.disabled_in.is_empty())
        .count();

    CommandsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "commands",
        page_title: "Commands",
        commands,
        channels,
        disabled_count,
        partial_count,
    }
    .into_response()
}

pub(super) async fn guild_command_toggle(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.toggle_command(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_plonk_manage(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.manage_plonk(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_lockdown_lock(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.lock_channels(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_lockdown_unlock(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.unlock_channels(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_moderation_ignore(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.manage_moderation_ignore(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_audit_log_flags(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<std::collections::HashMap<String, bool>>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false, "error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false, "error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "access denied"})).into_response();
    }
    match percy.patch_audit_log_flags(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_stats(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    let (guild, stats, bot_stats) = tokio::join!(
        percy.get_guild(guild_id),
        percy.get_guild_stats(guild_id),
        percy.get_bot_stats(),
    );

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };

    let stats = match stats {
        Ok(s) => s,
        Err(_) => return Redirect::to(&format!("/dashboard/guild/{guild_id}")).into_response(),
    };

    let bot_stats = bot_stats.unwrap_or(BotStats {
        guild_count: 0,
        user_count: 0,
        channel_count: 0,
        total_commands_used: 0,
        cog_count: 0,
        command_count: 0,
        latency_ms: 0.0,
        uptime_seconds: 0.0,
    });

    StatsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "stats",
        page_title: "Stats",
        stats,
        bot_stats,
    }
    .into_response()
}

pub(super) async fn guild_leveling_update(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, user_id)): Path<(u64, String)>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.patch_leveling_user(guild_id, &user_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

// -- Patch builders ----------------------------------------------------------

fn build_patch(section: &str, fields: &HashMap<String, String>) -> serde_json::Value {
    match section {
        "flags" => {
            let flags = serde_json::json!({
                "audit_log": fields.contains_key("audit_log"),
                "raid": fields.contains_key("raid"),
                "alerts": fields.contains_key("alerts"),
                "sentinel": fields.contains_key("sentinel"),
                "mentions": fields.contains_key("mentions"),
            });
            serde_json::json!({ "flags": flags })
        }
        "moderation" => {
            let mut patch = serde_json::Map::new();
            if let Some(v) = fields.get("audit_log_channel_id") {
                patch.insert("audit_log_channel_id".into(), parse_id_or_null(v));
            }
            if let Some(v) = fields.get("alert_channel_id") {
                patch.insert("alert_channel_id".into(), parse_id_or_null(v));
            }
            if let Some(v) = fields.get("mute_role_id") {
                patch.insert("mute_role_id".into(), parse_id_or_null(v));
            }
            if let Some(v) = fields.get("mention_count") {
                let n: Option<u32> = v.parse().ok().filter(|&n| n > 0);
                patch.insert(
                    "mention_count".into(),
                    n.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null),
                );
            }
            if let Some(v) = fields.get("mod_log_channel_id") {
                patch.insert("mod_log_channel_id".into(), parse_id_or_null(v));
            }
            if let Some(v) = fields.get("message_log_channel_id") {
                patch.insert("message_log_channel_id".into(), parse_id_or_null(v));
            }
            if let Some(v) = fields.get("voice_log_channel_id") {
                patch.insert("voice_log_channel_id".into(), parse_id_or_null(v));
            }
            serde_json::Value::Object(patch)
        }
        "polls" => {
            let mut patch = serde_json::Map::new();
            if let Some(v) = fields.get("poll_channel_id") {
                patch.insert("poll_channel_id".into(), parse_id_or_null(v));
            }
            if let Some(v) = fields.get("poll_reason_channel_id") {
                patch.insert("poll_reason_channel_id".into(), parse_id_or_null(v));
            }
            if let Some(v) = fields.get("poll_ping_role_id") {
                patch.insert("poll_ping_role_id".into(), parse_id_or_null(v));
            }
            serde_json::Value::Object(patch)
        }
        "music" => {
            let mut patch = serde_json::Map::new();
            patch.insert(
                "use_music_panel".into(),
                serde_json::Value::Bool(fields.contains_key("use_music_panel")),
            );
            if let Some(v) = fields.get("music_panel_channel_id") {
                patch.insert("music_panel_channel_id".into(), parse_id_or_null(v));
            }
            serde_json::Value::Object(patch)
        }
        "prefixes" => {
            let prefixes: Vec<&str> = fields
                .get("prefixes")
                .map(|s| s.split(',').map(str::trim).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default();
            serde_json::json!({ "prefixes": prefixes })
        }
        _ => serde_json::json!({}),
    }
}

fn build_sentinel_patch(fields: &HashMap<String, String>) -> serde_json::Value {
    let mut patch = serde_json::Map::new();
    if let Some(v) = fields.get("channel_id") {
        patch.insert("channel_id".into(), parse_id_or_null(v));
    }
    if let Some(v) = fields.get("role_id") {
        patch.insert("role_id".into(), parse_id_or_null(v));
    }
    if let Some(v) = fields.get("starter_role_id") {
        patch.insert("starter_role_id".into(), parse_id_or_null(v));
    }
    if let Some(v) = fields.get("bypass_action") {
        if v == "ban" || v == "kick" {
            patch.insert("bypass_action".into(), serde_json::Value::String(v.clone()));
        }
    }
    if let Some(v) = fields.get("rate") {
        if v.is_empty() {
            patch.insert("rate".into(), serde_json::Value::Null);
        } else {
            patch.insert("rate".into(), serde_json::Value::String(v.clone()));
        }
    }
    serde_json::Value::Object(patch)
}

fn parse_id_or_null(s: &str) -> serde_json::Value {
    if s.is_empty() {
        serde_json::Value::Null
    } else {
        s.parse::<u64>()
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null)
    }
}

// -- New Feature Handlers ----------------------------------------------------

pub(super) async fn guild_autoresponders(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }
    let (guild, data) = tokio::join!(percy.get_guild(guild_id), percy.get_autoresponders(guild_id));

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };
    let data = data.unwrap_or(AutorespondersResponse {
        entries: Vec::new(),
        total: 0,
    });
    AutorespondersTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "autoresponders",
        page_title: "Autoresponders",
        data,
    }
    .into_response()
}

pub(super) async fn guild_autoresponders_action(
    State(state): State<AppState>,
    account: Account,
    headers: HeaderMap,
    flasher: Flasher,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(&headers, &flasher, false, "Access denied.", "/dashboard");
    }
    let redirect_url = format!("/dashboard/guild/{guild_id}/autoresponders");
    match percy.create_autoresponder(guild_id, &body).await {
        Ok(()) => json_or_flash(&headers, &flasher, true, "Autoresponder created.", &redirect_url),
        Err(e) => {
            let msg = format!("Failed: {e}");
            json_or_flash(&headers, &flasher, false, &msg, &redirect_url)
        }
    }
}

pub(super) async fn guild_leveling_config_update(
    State(state): State<AppState>,
    account: Account,
    headers: HeaderMap,
    flasher: Flasher,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(&headers, &flasher, false, "Access denied.", "/dashboard");
    }
    let redirect_url = format!("/dashboard/guild/{guild_id}/leveling");
    match percy.patch_leveling_config(guild_id, &body).await {
        Ok(()) => json_or_flash(&headers, &flasher, true, "Configuration saved.", &redirect_url),
        Err(e) => {
            let msg = format!("Failed: {e}");
            json_or_flash(&headers, &flasher, false, &msg, &redirect_url)
        }
    }
}

pub(super) async fn guild_leveling_roles(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.post_leveling_roles(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_leveling_roles_preset(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.create_leveling_role_preset(guild_id).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_leveling_multipliers(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.post_leveling_multipliers(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_leveling_blacklist(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.post_leveling_blacklist(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_economy(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }
    let (guild, economy, balances, channels, roles) = tokio::join!(
        percy.get_guild(guild_id),
        percy.get_economy(guild_id),
        percy.get_economy_balances(guild_id, 25),
        cached_channels(&percy, guild_id),
        cached_roles(&percy, guild_id),
    );
    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };
    let economy = economy.unwrap_or(EconomyInfo {
        items: Vec::new(),
        lottery: None,
    });
    let balances = balances.unwrap_or(BalancesResponse { entries: Vec::new() });
    let channels = channels.unwrap_or_default();
    let roles = roles.unwrap_or_default();
    EconomyTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "economy",
        page_title: "Economy",
        economy,
        balances,
        channels,
        roles,
    }
    .into_response()
}

pub(super) async fn guild_music(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }
    let (guild, music, channels) = tokio::join!(
        percy.get_guild(guild_id),
        percy.get_music(guild_id),
        cached_channels(&percy, guild_id),
    );
    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };
    let music = music.unwrap_or(MusicInfo {
        active: false,
        equalizer: vec![0.0; 15],
        filters: MusicFiltersState {
            nightcore: false,
            eight_d: false,
            lowpass: None,
        },
        presets: vec![],
        now_playing: None,
        queue: Vec::new(),
        history: Vec::new(),
        channel: None,
        channel_name: None,
        setup: None,
        always_on: Default::default(),
        listeners: Vec::new(),
    });
    let channels = channels.unwrap_or_default();
    MusicTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name.clone(),
        nav_active: "music",
        page_title: "Music",
        music,
        guild,
        channels,
    }
    .into_response()
}

pub(super) async fn guild_music_equalizer(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.post_music_equalizer(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_music_filters(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.post_music_filters(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_music_status(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.get_music(guild_id).await {
        Ok(music) => Json(serde_json::json!({
            "ok": true,
            "active": music.active,
            "channel": music.channel,
            "channel_name": music.channel_name,
            "now_playing": music.now_playing,
            "queue": music.queue,
            "history": music.history,
            // Dashboard admins (Manage Server) can always drive the player.
            "can_control": true,
            "equalizer": music.equalizer,
            "filters": music.filters,
            "setup": music.setup,
            "always_on": music.always_on,
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

/// `POST /dashboard/guild/:guild_id/music/control` — drive the live player
/// from the admin music page. The viewer's Discord id comes from the session;
/// Percy enforces DJ-mode rules (admins/Manage-Server always pass).
pub(super) async fn guild_music_control(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(mut body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false, "error": "Not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false, "error": "Not authenticated"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    // Inject the authenticated identity; never trust a client-supplied user_id.
    if let Some(obj) = body.as_object_mut() {
        obj.insert("user_id".into(), serde_json::Value::String(discord_id));
    }
    match percy.music_control(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

/// `GET /dashboard/guild/:guild_id/music/lyrics` — synced lyrics for the
/// admin music page player.
pub(super) async fn guild_music_lyrics(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    lyrics_json(&percy, guild_id).await
}

pub(super) async fn guild_music_247(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false, "error": "Not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false, "error": "Not authenticated"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.post_music_247(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_music_dj_mode(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false, "error": "Not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false, "error": "Not authenticated"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.patch_music_dj_mode(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_music_setup(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false, "error": "Not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false, "error": "Not authenticated"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.post_music_setup(guild_id, &body).await {
        Ok(resp) => Json(serde_json::json!({
            "ok": true,
            "channel_id": resp.channel_id,
            "channel_name": resp.channel_name,
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_music_reset(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false, "error": "Not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false, "error": "Not authenticated"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.post_music_reset(guild_id).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_comics(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }
    let (guild, data, channels, roles) = tokio::join!(
        percy.get_guild(guild_id),
        percy.get_comics(guild_id),
        cached_channels(&percy, guild_id),
        cached_roles(&percy, guild_id),
    );

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };
    let data = data.unwrap_or(ComicsResponse { feeds: Vec::new() });
    let channels = channels.unwrap_or_default();
    let roles = roles.unwrap_or_default();
    ComicsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "comics",
        page_title: "Comics",
        data,
        channels,
        roles,
    }
    .into_response()
}

pub(super) async fn guild_temp_channels(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }
    let (guild, data, channels) = tokio::join!(
        percy.get_guild(guild_id),
        percy.get_temp_channels(guild_id),
        cached_channels(&percy, guild_id),
    );

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };
    let data = data.unwrap_or(TempChannelsResponse { entries: Vec::new() });
    let channels = channels.unwrap_or_default();
    TempChannelsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "temp-channels",
        page_title: "Temp Channels",
        data,
        channels,
    }
    .into_response()
}

pub(super) async fn guild_highlights(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }
    let (guild, data) = tokio::join!(percy.get_guild(guild_id), percy.get_highlights(guild_id));

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };
    let data = data.unwrap_or(HighlightsResponse { entries: Vec::new() });
    HighlightsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "highlights",
        page_title: "Highlights",
        data,
    }
    .into_response()
}

pub(super) async fn guild_emoji_stats(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }
    let (guild, data) = tokio::join!(percy.get_guild(guild_id), percy.get_emoji_stats(guild_id, 50));

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };
    let data = data.unwrap_or(EmojiStatsResponse {
        total_uses: 0,
        distinct_emojis: 0,
        entries: Vec::new(),
    });
    EmojiStatsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "emoji-stats",
        page_title: "Emoji Stats",
        data,
    }
    .into_response()
}

pub(super) async fn guild_comics_push(
    State(state): State<AppState>,
    account: Account,
    headers: HeaderMap,
    flasher: Flasher,
    Path((guild_id, brand)): Path<(u64, String)>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(&headers, &flasher, false, "Access denied.", "/dashboard");
    }
    let redirect_url = format!("/dashboard/guild/{guild_id}/comics");
    match percy.push_comic(guild_id, &brand).await {
        Ok(()) => json_or_flash(&headers, &flasher, true, "Feed pushed.", &redirect_url),
        Err(e) => {
            let msg = format!("Failed: {e}");
            json_or_flash(&headers, &flasher, false, &msg, &redirect_url)
        }
    }
}

// -- CRUD proxy handlers (PATCH/DELETE for sub-resources) ---------------------

pub(super) async fn guild_autoresponder_patch(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, trigger)): Path<(u64, String)>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.patch_autoresponder(guild_id, &trigger, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_autoresponder_delete(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, trigger)): Path<(u64, String)>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.delete_autoresponder(guild_id, &trigger).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_economy_item_delete(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, name)): Path<(u64, String)>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.delete_economy_item(guild_id, &name).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_economy_items_create(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.create_economy_item(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_economy_lottery_create(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.create_lottery(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_economy_lottery_delete(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.delete_lottery(guild_id).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

/// Adjust a single member's economy balance (`{ "cash": <i64>, "bank": <i64> }`,
/// either field optional — forwarded as-is to Percy).
pub(super) async fn guild_economy_balance_update(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, user_id)): Path<(u64, String)>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.patch_economy_balance(guild_id, &user_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

/// Subscribe the guild to (or move) the bot status feed (`{ "channel_id": "<id>" }`).
pub(super) async fn guild_status_feed_subscribe(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.post_status_feed(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

/// Unsubscribe the guild from the bot status feed.
pub(super) async fn guild_status_feed_unsubscribe(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.delete_status_feed(guild_id).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_comic_patch(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, brand)): Path<(u64, String)>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.patch_comic(guild_id, &brand, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_comic_delete(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, brand)): Path<(u64, String)>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.delete_comic(guild_id, &brand).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_comic_create(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.create_comic(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_temp_channel_create(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.create_temp_channel(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_temp_channel_patch(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, channel_id)): Path<(u64, u64)>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.patch_temp_channel(guild_id, channel_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_temp_channel_delete(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, channel_id)): Path<(u64, u64)>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.delete_temp_channel(guild_id, channel_id).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_highlight_delete(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, user_id)): Path<(u64, String)>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.delete_highlight(guild_id, &user_id).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

// -- Phase 5: Audit log, bulk actions, activity, export, notifications --------

#[derive(Deserialize)]
pub(super) struct CasesQuery {
    #[serde(default)]
    action: String,
    #[serde(default)]
    moderator_id: String,
    #[serde(default)]
    after: String,
    #[serde(default)]
    before: String,
    #[serde(default = "default_cases_limit")]
    limit: u32,
    #[serde(default)]
    offset: u32,
}

fn default_cases_limit() -> u32 {
    50
}

pub(super) async fn guild_audit_log(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
    Query(q): Query<CasesQuery>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    let action = if q.action.is_empty() {
        None
    } else {
        Some(q.action.as_str())
    };
    let moderator_id = if q.moderator_id.is_empty() {
        None
    } else {
        Some(q.moderator_id.as_str())
    };
    let after = if q.after.is_empty() {
        None
    } else {
        Some(q.after.as_str())
    };
    let before = if q.before.is_empty() {
        None
    } else {
        Some(q.before.as_str())
    };

    let (guild, cases) = tokio::join!(
        percy.get_guild(guild_id),
        percy.get_cases(guild_id, q.limit, q.offset, action, moderator_id, None, after, before),
    );

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return Redirect::to("/dashboard").into_response(),
    };

    let cases = cases.unwrap_or(CasesResponse {
        cases: Vec::new(),
        total: 0,
    });

    AuditLogTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        nav_active: "audit-log",
        page_title: "Audit Log",
        cases,
        filter_action: q.action,
        filter_moderator: q.moderator_id,
        filter_after: q.after,
        filter_before: q.before,
    }
    .into_response()
}

pub(super) async fn guild_audit_log_json(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Query(q): Query<CasesQuery>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    let action = if q.action.is_empty() {
        None
    } else {
        Some(q.action.as_str())
    };
    let moderator_id = if q.moderator_id.is_empty() {
        None
    } else {
        Some(q.moderator_id.as_str())
    };
    let after = if q.after.is_empty() {
        None
    } else {
        Some(q.after.as_str())
    };
    let before = if q.before.is_empty() {
        None
    } else {
        Some(q.before.as_str())
    };

    match percy
        .get_cases(guild_id, q.limit, q.offset, action, moderator_id, None, after, before)
        .await
    {
        Ok(data) => Json(serde_json::json!(data)).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_bulk_action(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(mut body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    body["moderator_id"] = serde_json::Value::String(discord_id.clone());
    match percy.bulk_member_action(guild_id, &body).await {
        Ok(resp) => Json(serde_json::json!(resp)).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_member_activity(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, user_id)): Path<(u64, String)>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.get_member_activity(guild_id, &user_id, 365).await {
        Ok(data) => Json(serde_json::json!(data)).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_export_leaderboard(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    let leaderboard = percy
        .get_leveling_leaderboard(guild_id, 500)
        .await
        .unwrap_or(LeaderboardResponse {
            entries: Vec::new(),
            total: 0,
        });

    let mut csv = String::from("Rank,Username,Level,XP,Total XP\n");
    for (i, entry) in leaderboard.entries.iter().enumerate() {
        csv.push_str(&format!(
            "{},{},{},{},{}\n",
            i + 1,
            entry.username.replace(',', " "),
            entry.level,
            entry.xp,
            entry.total_xp,
        ));
    }

    (
        [
            ("Content-Type", "text/csv; charset=utf-8"),
            ("Content-Disposition", "attachment; filename=\"leaderboard.csv\""),
        ],
        csv,
    )
        .into_response()
}

pub(super) async fn guild_export_cases(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/dashboard").into_response();
    }

    let cases = percy
        .get_cases(guild_id, 100, 0, None, None, None, None, None)
        .await
        .unwrap_or(CasesResponse {
            cases: Vec::new(),
            total: 0,
        });

    let mut csv = String::from("Case #,Action,Target,Moderator,Reason,Date\n");
    for case in &cases.cases {
        csv.push_str(&format!(
            "{},{},{},{},{},{}\n",
            case.case_index,
            case.action,
            case.target_name.replace(',', " "),
            case.moderator_name.as_deref().unwrap_or("Unknown").replace(',', " "),
            case.reason
                .as_deref()
                .unwrap_or("")
                .replace(',', " ")
                .replace('\n', " "),
            case.created_at.as_deref().unwrap_or(""),
        ));
    }

    (
        [
            ("Content-Type", "text/csv; charset=utf-8"),
            ("Content-Disposition", "attachment; filename=\"moderation_history.csv\""),
        ],
        csv,
    )
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct CaseCreateBody {
    action: String,
    target_id: String,
    reason: Option<String>,
}

pub(super) async fn guild_case_create(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<CaseCreateBody>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy
        .create_case(
            guild_id,
            &body.action,
            &body.target_id,
            Some(&discord_id),
            body.reason.as_deref(),
        )
        .await
    {
        Ok(resp) => Json(serde_json::json!(resp)).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct CaseUpdateBody {
    reason: String,
}

pub(super) async fn guild_case_update(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, case_index)): Path<(u64, u64)>,
    Json(body): Json<CaseUpdateBody>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.update_case_reason(guild_id, case_index, &body.reason).await {
        Ok(resp) => Json(serde_json::json!(resp)).into_response(),
        Err(crate::percy::PercyError::NotFound) => Json(serde_json::json!({"error": "case not found"})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_case_delete(
    State(state): State<AppState>,
    account: Account,
    Path((guild_id, case_index)): Path<(u64, u64)>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    match percy.delete_case(guild_id, case_index).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(crate::percy::PercyError::NotFound) => Json(serde_json::json!({"error": "case not found"})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

pub(super) async fn guild_cases_recent(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"error": "not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"error": "no discord link"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"error": "access denied"})).into_response();
    }

    let since = params.get("since").map(|s| s.as_str()).unwrap_or("");
    if since.is_empty() {
        return Json(serde_json::json!({"error": "since parameter required"})).into_response();
    }

    match percy.get_recent_cases(guild_id, since).await {
        Ok(data) => Json(serde_json::json!(data)).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

// -- Public Leaderboard -------------------------------------------------------

/// Resolve a vanity slug or guild ID to a guild_id for the public leaderboard.
async fn resolve_leaderboard_target(state: &AppState, target: &str) -> Option<u64> {
    if let Ok(id) = target.parse::<u64>() {
        return Some(id);
    }
    let slug = target.to_lowercase();
    state
        .database()
        .call(move |conn| -> rusqlite::Result<Option<u64>> {
            conn.prepare_cached("SELECT guild_id FROM percy_leaderboard_vanity WHERE slug = ?")
                .and_then(|mut stmt| {
                    stmt.query_row([&slug], |row| row.get::<_, String>(0))
                        .map(|id| id.parse::<u64>().ok())
                })
        })
        .await
        .ok()
        .flatten()
}

/// Get the vanity slug for a guild, if one exists.
async fn get_vanity_for_guild(state: &AppState, guild_id: u64) -> Option<String> {
    let gid = guild_id.to_string();
    state
        .database()
        .call(move |conn| -> rusqlite::Result<String> {
            conn.prepare_cached("SELECT slug FROM percy_leaderboard_vanity WHERE guild_id = ?")
                .and_then(|mut stmt| stmt.query_row([&gid], |row| row.get(0)))
        })
        .await
        .ok()
}

/// `GET /lb/:target` — public leaderboard page (no login required).
pub(super) async fn public_leaderboard(
    State(state): State<AppState>,
    account: Option<Account>,
    flashes: Flashes,
    Path(target): Path<String>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/dashboard").into_response();
    };
    let Some(guild_id) = resolve_leaderboard_target(&state, &target).await else {
        return (StatusCode::NOT_FOUND, "Leaderboard not found").into_response();
    };

    let (guild, leaderboard, balances) = tokio::join!(
        percy.get_guild(guild_id),
        percy.get_leveling_leaderboard(guild_id, 100),
        percy.get_economy_balances(guild_id, 100),
    );

    let guild = match guild {
        Ok(g) => g,
        Err(_) => return (StatusCode::NOT_FOUND, "Guild not found").into_response(),
    };

    let leaderboard = leaderboard.unwrap_or(LeaderboardResponse {
        entries: Vec::new(),
        total: 0,
    });
    let balances = balances.unwrap_or(BalancesResponse { entries: Vec::new() });
    let vanity = get_vanity_for_guild(&state, guild_id).await;

    let can_manage = if let Some(ref acc) = account {
        if let Some(discord_id) = get_discord_id(&state, acc.id).await {
            check_guild_access(&percy, &discord_id, guild_id).await
        } else {
            false
        }
    } else {
        false
    };

    LeaderboardTemplate {
        account,
        flashes,
        guild_id,
        guild_name: guild.name,
        guild_icon: guild.icon_url,
        leaderboard,
        balances,
        vanity,
        can_manage,
    }
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct VanityClaimBody {
    slug: String,
}

/// `POST /lb/:guild_id/vanity` — claim or update a vanity slug.
pub(super) async fn public_leaderboard_vanity_claim(
    State(state): State<AppState>,
    account: Account,
    headers: HeaderMap,
    flasher: Flasher,
    Path(guild_id): Path<u64>,
    Json(body): Json<VanityClaimBody>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return json_or_flash(&headers, &flasher, false, "Not configured", &format!("/lb/{guild_id}"));
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return json_or_flash(
            &headers,
            &flasher,
            false,
            "No Discord account linked",
            &format!("/lb/{guild_id}"),
        );
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(
            &headers,
            &flasher,
            false,
            "You don't have permission to manage this server",
            &format!("/lb/{guild_id}"),
        );
    }

    let slug = body.slug.trim().to_lowercase();
    if slug.is_empty() || slug.len() > 32 {
        return json_or_flash(
            &headers,
            &flasher,
            false,
            "Vanity URL must be 1-32 characters",
            &format!("/lb/{guild_id}"),
        );
    }
    if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return json_or_flash(
            &headers,
            &flasher,
            false,
            "Vanity URL can only contain letters, numbers, hyphens, and underscores",
            &format!("/lb/{guild_id}"),
        );
    }
    if slug.parse::<u64>().is_ok() {
        return json_or_flash(
            &headers,
            &flasher,
            false,
            "Vanity URL cannot be a number",
            &format!("/lb/{guild_id}"),
        );
    }

    let gid = guild_id.to_string();
    let discord_id_clone = discord_id.clone();
    let slug_clone = slug.clone();
    let result = state
        .database()
        .call(move |conn| -> rusqlite::Result<Result<(), &'static str>> {
            let existing: Option<String> = conn
                .prepare_cached("SELECT guild_id FROM percy_leaderboard_vanity WHERE slug = ?")
                .and_then(|mut stmt| stmt.query_row([&slug_clone], |row| row.get(0)))
                .ok();

            if let Some(ref owner_guild) = existing {
                if owner_guild != &gid {
                    return Ok(Err("This vanity URL is already taken"));
                }
            }

            conn.execute(
                "INSERT INTO percy_leaderboard_vanity (slug, guild_id, claimed_by) VALUES (?, ?, ?) \
                 ON CONFLICT(guild_id) DO UPDATE SET slug = excluded.slug",
                rusqlite::params![slug_clone, gid, discord_id_clone],
            )?;
            Ok(Ok(()))
        })
        .await;

    match result {
        Ok(Ok(())) => json_or_flash(
            &headers,
            &flasher,
            true,
            &format!("Vanity URL set to /lb/{slug}"),
            &format!("/lb/{guild_id}"),
        ),
        Ok(Err(msg)) => json_or_flash(&headers, &flasher, false, msg, &format!("/lb/{guild_id}")),
        Err(_) => json_or_flash(&headers, &flasher, false, "Database error", &format!("/lb/{guild_id}")),
    }
}

/// `DELETE /lb/:guild_id/vanity` — remove a vanity slug.
pub(super) async fn public_leaderboard_vanity_delete(
    State(state): State<AppState>,
    account: Account,
    headers: HeaderMap,
    flasher: Flasher,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return json_or_flash(&headers, &flasher, false, "Not configured", &format!("/lb/{guild_id}"));
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return json_or_flash(
            &headers,
            &flasher,
            false,
            "No Discord account linked",
            &format!("/lb/{guild_id}"),
        );
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(
            &headers,
            &flasher,
            false,
            "You don't have permission to manage this server",
            &format!("/lb/{guild_id}"),
        );
    }

    let gid = guild_id.to_string();
    let _ = state
        .database()
        .call(move |conn| conn.execute("DELETE FROM percy_leaderboard_vanity WHERE guild_id = ?", [&gid]))
        .await;

    json_or_flash(
        &headers,
        &flasher,
        true,
        "Vanity URL removed",
        &format!("/lb/{guild_id}"),
    )
}

// -- Custom Bot Profile -------------------------------------------------------

/// `PATCH /dashboard/guild/:guild_id/custom-bot` — update bot profile.
pub(super) async fn guild_custom_bot_update(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false, "error": "Not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false, "error": "Not authenticated"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }

    match percy.patch_custom_bot(guild_id, &body).await {
        Ok(()) => Json(serde_json::json!({"ok": true, "message": "Bot profile updated"})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}

/// `POST /dashboard/guild/:guild_id/custom-bot/reset` — reset to defaults.
pub(super) async fn guild_custom_bot_reset(
    State(state): State<AppState>,
    account: Account,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Json(serde_json::json!({"ok": false, "error": "Not configured"})).into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Json(serde_json::json!({"ok": false, "error": "Not authenticated"})).into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }

    match percy.reset_custom_bot(guild_id).await {
        Ok(()) => Json(serde_json::json!({"ok": true, "message": "Bot profile reset to defaults"})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}
