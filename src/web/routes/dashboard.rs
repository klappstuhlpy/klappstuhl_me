use crate::{
    flash::{Flasher, Flashes},
    models::Account,
    percy::{Channel, GuildInfo, PercyClient, Role, UserGuild},
    AppState,
};
use askama::Template;
use axum::{
    extract::{Path, State},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
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
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/no_discord.html")]
struct NoDiscordTemplate {
    account: Option<Account>,
    flashes: Flashes,
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

    let guilds = percy.get_user_guilds(&discord_id).await.unwrap_or_default();
    let guild_id_str = guild_id.to_string();
    if !guilds.iter().any(|g| g.id == guild_id_str) {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    let channels = percy.get_guild_channels(guild_id).await.unwrap_or_default();
    let roles = percy.get_guild_roles(guild_id).await.unwrap_or_default();

    GuildTemplate {
        account: Some(account),
        flashes,
        guild,
        channels,
        roles,
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
    Path(guild_id): Path<u64>,
    Form(form): Form<ConfigForm>,
) -> Response {
    let Some(percy) = get_percy_client(&state) else {
        return Redirect::to("/").into_response();
    };

    let Some(discord_id) = get_discord_id(&state, account.id).await else {
        return Redirect::to("/percy/dashboard").into_response();
    };

    let guilds = percy.get_user_guilds(&discord_id).await.unwrap_or_default();
    let guild_id_str = guild_id.to_string();
    if !guilds.iter().any(|g| g.id == guild_id_str) {
        return flasher.add("Access denied.").bail("/percy/dashboard");
    }

    let patch = build_patch(&form.section, &form.fields);
    let redirect_url = format!("/percy/dashboard/guild/{guild_id}");

    match percy.patch_guild_config(guild_id, &patch).await {
        Ok(()) => flasher.add("Settings saved.").bail(&redirect_url),
        Err(e) => {
            tracing::error!(error = %e, "Failed to update guild config");
            flasher.add("Failed to save settings.").bail(&redirect_url)
        }
    }
}

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
                patch.insert(
                    "audit_log_channel_id".into(),
                    parse_id_or_null(v),
                );
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

// -- Router ------------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/percy/dashboard", get(guild_list))
        .route("/percy/dashboard/guild/:guild_id", get(guild_detail))
        .route("/percy/dashboard/guild/:guild_id/config", post(guild_config_update))
}
