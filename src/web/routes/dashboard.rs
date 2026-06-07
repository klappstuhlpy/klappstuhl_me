use crate::{
    flash::{Flasher, Flashes},
    models::Account,
    percy::{
        BotStats, Channel, CommandsResponse, GatekeeperInfo, GiveawaysResponse, GuildInfo,
        GuildStats, LeaderboardResponse, LevelingConfig, Member, PercyClient, PollsResponse, Role,
        TagsResponse, UserGuild,
    },
    AppState,
};
use askama::Template;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Json, Router,
};
use serde::Deserialize;
use std::collections::HashMap;

// -- Templates ---------------------------------------------------------------

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/guilds.html")]
struct GuildsTemplate {
    account: Option<Account>,
    flashes: Flashes,
    guilds: Vec<UserGuild>,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/guild.html")]
struct GuildTemplate {
    account: Option<Account>,
    flashes: Flashes,
    guild: GuildInfo,
    channels: Vec<Channel>,
    roles: Vec<Role>,
    gatekeeper: Option<GatekeeperInfo>,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/no_discord.html")]
struct NoDiscordTemplate {
    account: Option<Account>,
    flashes: Flashes,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/guild_not_found.html")]
struct GuildNotFoundTemplate {
    account: Option<Account>,
    flashes: Flashes,
    guild_id: u64,
    invite_url: String,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/members.html")]
struct MembersTemplate {
    account: Option<Account>,
    flashes: Flashes,
    guild_id: u64,
    guild_name: String,
    members: Vec<Member>,
    roles: Vec<Role>,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/leveling.html")]
struct LevelingTemplate {
    account: Option<Account>,
    flashes: Flashes,
    guild_id: u64,
    guild_name: String,
    config: LevelingConfig,
    leaderboard: LeaderboardResponse,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/polls.html")]
struct PollsTemplate {
    account: Option<Account>,
    flashes: Flashes,
    guild_id: u64,
    guild_name: String,
    polls: PollsResponse,
    active_count: usize,
    ended_count: usize,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/giveaways.html")]
struct GiveawaysTemplate {
    account: Option<Account>,
    flashes: Flashes,
    guild_id: u64,
    guild_name: String,
    giveaways: GiveawaysResponse,
    active_count: usize,
    ended_count: usize,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/tags.html")]
struct TagsTemplate {
    account: Option<Account>,
    flashes: Flashes,
    guild_id: u64,
    guild_name: String,
    tags: TagsResponse,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/commands.html")]
struct CommandsTemplate {
    account: Option<Account>,
    flashes: Flashes,
    guild_id: u64,
    guild_name: String,
    commands: CommandsResponse,
    channels: Vec<Channel>,
    disabled_count: usize,
    partial_count: usize,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/stats.html")]
struct StatsTemplate {
    account: Option<Account>,
    flashes: Flashes,
    guild_id: u64,
    guild_name: String,
    stats: GuildStats,
    bot_stats: BotStats,
}

// -- Handlers ----------------------------------------------------------------

async fn guild_list(State(state): State<AppState>, account: Account, flashes: Flashes) -> Response {
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

    let guilds = match percy.get_user_guilds(&discord_id).await {
        Ok(g) => g,
        Err(_) => Vec::new(),
    };

    GuildsTemplate {
        account: Some(account),
        flashes,
        guilds,
    }
    .into_response()
}

async fn guild_detail(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };

    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/percy/dashboard").into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(crate::percy::PercyError::NotFound) => {
            let invite_url = build_invite_url(&state, guild_id);
            return GuildNotFoundTemplate {
                account: Some(account),
                flashes,
                guild_id,
                invite_url,
            }
            .into_response();
        }
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let channels = percy.get_guild_channels(guild_id).await.unwrap_or_default();
    let roles = percy.get_guild_roles(guild_id).await.unwrap_or_default();
    let gatekeeper = percy.get_gatekeeper(guild_id).await.ok().flatten();

    GuildTemplate {
        account: Some(account),
        flashes,
        guild,
        channels,
        roles,
        gatekeeper,
    }
    .into_response()
}

#[derive(Deserialize)]
struct ConfigForm {
    #[serde(rename = "_section")]
    section: String,
    #[serde(flatten)]
    fields: HashMap<String, String>,
}

async fn guild_config_update(
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
        return Redirect::to("/percy/dashboard").into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(&headers, &flasher, false, "Access denied.", "/percy/dashboard");
    }

    let patch = build_patch(&form.section, &form.fields);
    let redirect_url = format!("/percy/dashboard/guild/{guild_id}");

    match percy.patch_guild_config(guild_id, &patch).await {
        Ok(()) => json_or_flash(&headers, &flasher, true, "Settings saved.", &redirect_url),
        Err(e) => {
            tracing::error!(error = %e, "Failed to update guild config");
            json_or_flash(&headers, &flasher, false, "Failed to save settings.", &redirect_url)
        }
    }
}

async fn guild_gatekeeper_update(
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
        return Redirect::to("/percy/dashboard").into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(&headers, &flasher, false, "Access denied.", "/percy/dashboard");
    }

    let patch = build_gatekeeper_patch(&form);
    let redirect_url = format!("/percy/dashboard/guild/{guild_id}");

    match percy.patch_gatekeeper(guild_id, &patch).await {
        Ok(()) => json_or_flash(&headers, &flasher, true, "Gatekeeper settings saved.", &redirect_url),
        Err(e) => {
            tracing::error!(error = %e, "Failed to update gatekeeper config");
            json_or_flash(&headers, &flasher, false, "Failed to save gatekeeper settings.", &redirect_url)
        }
    }
}

async fn guild_members(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };

    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/percy/dashboard").into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let members = percy.get_guild_members(guild_id, 100, 0).await.unwrap_or_default();
    let roles = percy.get_guild_roles(guild_id).await.unwrap_or_default();

    MembersTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        members,
        roles,
    }
    .into_response()
}

#[derive(Deserialize)]
struct MembersQuery {
    #[serde(default = "default_limit")]
    limit: u32,
    #[serde(default)]
    after: u64,
}

fn default_limit() -> u32 {
    100
}

async fn guild_members_json(
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
struct MemberActionBody {
    action: String,
    reason: Option<String>,
}

async fn guild_member_action(
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

    match percy.member_action(guild_id, &user_id, &body.action, body.reason.as_deref()).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})).into_response(),
    }
}

#[derive(Deserialize)]
struct MemberRolesBody {
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

async fn guild_member_roles(
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

async fn guild_leveling(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let config = percy.get_leveling_config(guild_id).await.unwrap_or(LevelingConfig {
        enabled: false,
        configured: false,
        level_up_channel_id: None,
        level_up_message: None,
        stack_roles: false,
        voice_enabled: false,
        xp_rate: 1.0,
    });
    let leaderboard = percy.get_leveling_leaderboard(guild_id, 25).await.unwrap_or(LeaderboardResponse {
        entries: Vec::new(),
        total: 0,
    });

    LevelingTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        config,
        leaderboard,
    }
    .into_response()
}

async fn guild_polls(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let polls = percy.get_polls(guild_id).await.unwrap_or(PollsResponse { polls: Vec::new() });
    let active_count = polls.polls.iter().filter(|p| !p.ended).count();
    let ended_count = polls.polls.iter().filter(|p| p.ended).count();

    PollsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        polls,
        active_count,
        ended_count,
    }
    .into_response()
}

async fn guild_poll_edit(
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

async fn guild_giveaways(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let giveaways = percy.get_giveaways(guild_id).await.unwrap_or(GiveawaysResponse { giveaways: Vec::new() });
    let active_count = giveaways.giveaways.iter().filter(|g| !g.ended).count();
    let ended_count = giveaways.giveaways.iter().filter(|g| g.ended).count();

    GiveawaysTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        giveaways,
        active_count,
        ended_count,
    }
    .into_response()
}

async fn guild_tags(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let tags = percy.get_tags(guild_id).await.unwrap_or(TagsResponse {
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
        tags,
    }
    .into_response()
}

async fn guild_commands(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let commands = percy.get_commands(guild_id).await.unwrap_or(CommandsResponse {
        commands: Vec::new(),
        plonks: Vec::new(),
    });
    let channels = percy.get_guild_channels(guild_id).await.unwrap_or_default();
    let disabled_count = commands.commands.iter().filter(|c| c.globally_disabled).count();
    let partial_count = commands.commands.iter().filter(|c| !c.globally_disabled && !c.disabled_in.is_empty()).count();

    CommandsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        commands,
        channels,
        disabled_count,
        partial_count,
    }
    .into_response()
}

async fn guild_command_toggle(
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

async fn guild_plonk_manage(
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

async fn guild_stats(
    State(state): State<AppState>,
    account: Account,
    flashes: Flashes,
    Path(guild_id): Path<u64>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };
    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let stats = match percy.get_guild_stats(guild_id).await {
        Ok(s) => s,
        Err(_) => return Redirect::to(&format!("/percy/dashboard/guild/{guild_id}")).into_response(),
    };

    let bot_stats = percy.get_bot_stats().await.unwrap_or(BotStats {
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
        stats,
        bot_stats,
    }
    .into_response()
}

async fn guild_leveling_update(
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
                "gatekeeper": fields.contains_key("gatekeeper"),
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

fn build_gatekeeper_patch(fields: &HashMap<String, String>) -> serde_json::Value {
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

// -- Helpers -----------------------------------------------------------------

fn get_percy_client(state: &AppState) -> Option<PercyClient> {
    PercyClient::new(state.client.clone(), &state.config().percy)
}

async fn get_discord_id(state: &AppState, account_id: i64) -> Option<String> {
    state
        .database()
        .call(move |conn| {
            conn.prepare_cached("SELECT discord_user_id FROM user_discord_links WHERE account_id = ?")
                .and_then(|mut stmt| stmt.query_row([account_id], |row| row.get(0)))
        })
        .await
        .ok()
}

async fn check_guild_access(percy: &PercyClient, discord_id: &str, guild_id: u64) -> bool {
    let guilds = percy.get_user_guilds(discord_id).await.unwrap_or_default();
    let guild_id_str = guild_id.to_string();
    guilds.iter().any(|g| g.id == guild_id_str)
}

fn build_invite_url(state: &AppState, guild_id: u64) -> String {
    let client_id = state
        .config()
        .percy
        .bot_client_id
        .as_deref()
        .or(state.config().discord.client_id.as_deref())
        .unwrap_or("MISSING_CLIENT_ID");
    format!(
        "https://discord.com/oauth2/authorize?client_id={client_id}&scope=bot+applications.commands&permissions=8&guild_id={guild_id}"
    )
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
        .route("/percy/dashboard", get(guild_list))
        .route("/percy/dashboard/guild/:guild_id", get(guild_detail))
        .route("/percy/dashboard/guild/:guild_id/config", post(guild_config_update))
        .route("/percy/dashboard/guild/:guild_id/gatekeeper", post(guild_gatekeeper_update))
        .route("/percy/dashboard/guild/:guild_id/members", get(guild_members))
        .route("/percy/dashboard/guild/:guild_id/members.json", get(guild_members_json))
        .route("/percy/dashboard/guild/:guild_id/members/:user_id/action", post(guild_member_action))
        .route("/percy/dashboard/guild/:guild_id/members/:user_id/roles", post(guild_member_roles))
        // New sections
        .route("/percy/dashboard/guild/:guild_id/leveling", get(guild_leveling))
        .route("/percy/dashboard/guild/:guild_id/leveling/users/:user_id", post(guild_leveling_update))
        .route("/percy/dashboard/guild/:guild_id/polls", get(guild_polls))
        .route("/percy/dashboard/guild/:guild_id/polls/:poll_id", post(guild_poll_edit))
        .route("/percy/dashboard/guild/:guild_id/giveaways", get(guild_giveaways))
        .route("/percy/dashboard/guild/:guild_id/tags", get(guild_tags))
        .route("/percy/dashboard/guild/:guild_id/commands", get(guild_commands))
        .route("/percy/dashboard/guild/:guild_id/commands/toggle", post(guild_command_toggle))
        .route("/percy/dashboard/guild/:guild_id/plonks", post(guild_plonk_manage))
        .route("/percy/dashboard/guild/:guild_id/stats", get(guild_stats))
}
