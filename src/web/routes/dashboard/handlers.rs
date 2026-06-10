//! Request handlers for the Percy dashboard routes.

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
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
    build_general_invite_url, build_invite_url, check_guild_access, get_discord_id, get_percy_client, json_or_flash,
};

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

    let guilds = match percy.get_user_guilds(&discord_id).await {
        Ok(g) => g,
        Err(_) => Vec::new(),
    };

    GuildsTemplate {
        account: Some(account),
        flashes,
        guilds,
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
    let lockdowns = percy
        .get_lockdowns(guild_id)
        .await
        .unwrap_or(LockdownsResponse { entries: Vec::new() });
    let status_feed = percy.get_status_feed(guild_id).await.unwrap_or(StatusFeedInfo {
        subscribed: false,
        channel: None,
    });

    GuildTemplate {
        account: Some(account),
        flashes,
        guild,
        channels,
        roles,
        gatekeeper,
        lockdowns,
        status_feed,
    }
    .into_response()
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
        return Redirect::to("/percy/dashboard").into_response();
    };

    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(&headers, &flasher, false, "Access denied.", "/percy/dashboard");
    }

    let patch = build_patch(&form.section, &form.fields);
    let redirect_url = format!("/percy/dashboard/guild/{guild_id}");

    match percy.patch_guild_config(guild_id, &patch).await {
        Ok(()) => {
            if form.section == "flags" {
                // Auto-enable/disable gatekeeper when its flag is toggled
                let gk_enabled = form.fields.contains_key("gatekeeper");
                if let Err(e) = percy.toggle_gatekeeper(guild_id, gk_enabled).await {
                    tracing::warn!(error = %e, "Gatekeeper toggle after flag change");
                }

                // Clear associated config fields when flags are turned off
                let mut clear = serde_json::Map::new();
                if !form.fields.contains_key("audit_log") {
                    clear.insert("audit_log_channel_id".into(), serde_json::Value::Null);
                }
                if !form.fields.contains_key("alerts") {
                    clear.insert("alert_channel_id".into(), serde_json::Value::Null);
                }
                if !form.fields.contains_key("raid") {
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

pub(super) async fn guild_gatekeeper_update(
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

    let redirect_url = format!("/percy/dashboard/guild/{guild_id}");

    let starter_title = form.get("starter_title").filter(|s| !s.is_empty());
    let starter_content = form.get("starter_content").filter(|s| !s.is_empty());
    let channel_id = form.get("channel_id").and_then(|s| s.parse::<u64>().ok());
    let sent_starter = starter_title.is_some() && starter_content.is_some() && channel_id.is_some();

    if let (Some(title), Some(content), Some(ch_id)) = (starter_title, starter_content, channel_id) {
        match percy.send_gatekeeper_message(guild_id, ch_id, title, content).await {
            Ok(_message_id) => {}
            Err(e) => {
                tracing::error!(error = %e, "Failed to send gatekeeper starter message");
                let msg = format!("Failed to send verification message: {e}");
                return json_or_flash(&headers, &flasher, false, &msg, &redirect_url);
            }
        }
    }

    let mut patch = build_gatekeeper_patch(&form);
    // channel_id was already set by send_gatekeeper_message
    if sent_starter {
        if let Some(obj) = patch.as_object_mut() {
            obj.remove("channel_id");
        }
    }
    if !patch.as_object().map_or(true, |m| m.is_empty()) {
        if let Err(e) = percy.patch_gatekeeper(guild_id, &patch).await {
            tracing::error!(error = %e, "Failed to update gatekeeper config");
            return json_or_flash(
                &headers,
                &flasher,
                false,
                "Failed to save gatekeeper settings.",
                &redirect_url,
            );
        }
    }

    // Try to auto-enable if all required fields are now set
    if let Err(e) = percy.toggle_gatekeeper(guild_id, true).await {
        tracing::debug!(error = %e, "Gatekeeper not ready to enable (expected if setup incomplete)");
    }

    json_or_flash(&headers, &flasher, true, "Gatekeeper settings saved.", &redirect_url)
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
        .member_action(guild_id, &user_id, &body.action, body.reason.as_deref())
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let config = percy.get_leveling_config(guild_id).await.unwrap_or_default();
    let leaderboard = percy
        .get_leveling_leaderboard(guild_id, 25)
        .await
        .unwrap_or(LeaderboardResponse {
            entries: Vec::new(),
            total: 0,
        });

    let roles = percy.get_guild_roles(guild_id).await.unwrap_or_default();
    let channels = percy.get_guild_channels(guild_id).await.unwrap_or_default();

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
    }
    .into_response()
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let polls = percy
        .get_polls(guild_id)
        .await
        .unwrap_or(PollsResponse { polls: Vec::new() });
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let giveaways = percy
        .get_giveaways(guild_id)
        .await
        .unwrap_or(GiveawaysResponse { giveaways: Vec::new() });
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }
    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };
    let data = percy
        .get_autoresponders(guild_id)
        .await
        .unwrap_or(AutorespondersResponse {
            entries: Vec::new(),
            total: 0,
        });
    AutorespondersTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(&headers, &flasher, false, "Access denied.", "/percy/dashboard");
    }
    let redirect_url = format!("/percy/dashboard/guild/{guild_id}/autoresponders");
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(&headers, &flasher, false, "Access denied.", "/percy/dashboard");
    }
    let redirect_url = format!("/percy/dashboard/guild/{guild_id}/leveling");
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }
    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };
    let economy = percy.get_economy(guild_id).await.unwrap_or(EconomyInfo {
        items: Vec::new(),
        lottery: None,
    });
    let balances = percy
        .get_economy_balances(guild_id, 25)
        .await
        .unwrap_or(BalancesResponse { entries: Vec::new() });
    let channels = percy.get_guild_channels(guild_id).await.unwrap_or_default();
    EconomyTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
        economy,
        balances,
        channels,
    }
    .into_response()
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }
    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };
    let data = percy
        .get_comics(guild_id)
        .await
        .unwrap_or(ComicsResponse { feeds: Vec::new() });
    let channels = percy.get_guild_channels(guild_id).await.unwrap_or_default();
    let roles = percy.get_guild_roles(guild_id).await.unwrap_or_default();
    ComicsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }
    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };
    let data = percy
        .get_temp_channels(guild_id)
        .await
        .unwrap_or(TempChannelsResponse { entries: Vec::new() });
    let channels = percy.get_guild_channels(guild_id).await.unwrap_or_default();
    TempChannelsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }
    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };
    let data = percy
        .get_highlights(guild_id)
        .await
        .unwrap_or(HighlightsResponse { entries: Vec::new() });
    HighlightsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Redirect::to("/percy/dashboard").into_response();
    }
    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };
    let data = percy.get_emoji_stats(guild_id, 50).await.unwrap_or(EmojiStatsResponse {
        total_uses: 0,
        distinct_emojis: 0,
        entries: Vec::new(),
    });
    EmojiStatsTemplate {
        account: Some(account),
        flashes,
        guild_id,
        guild_name: guild.name,
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return json_or_flash(&headers, &flasher, false, "Access denied.", "/percy/dashboard");
    }
    let redirect_url = format!("/percy/dashboard/guild/{guild_id}/comics");
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.delete_lottery(guild_id).await {
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
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
        return Redirect::to("/percy/dashboard").into_response();
    };
    if !check_guild_access(&percy, &discord_id, guild_id).await {
        return Json(serde_json::json!({"ok": false, "error": "Access denied"})).into_response();
    }
    match percy.delete_highlight(guild_id, &user_id).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e.to_string()})).into_response(),
    }
}
