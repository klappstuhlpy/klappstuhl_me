use crate::{
    flash::Flashes,
    models::Account,
    percy::{GuildInfo, PercyClient, PercyError, UserGuild},
    AppState,
};
use askama::Template;
use axum::{
    extract::{Path, State},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Router,
};

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

    // Verify the user actually has access to this guild.
    let guilds = percy.get_user_guilds(&discord_id).await.unwrap_or_default();
    let guild_id_str = guild_id.to_string();
    if !guilds.iter().any(|g| g.id == guild_id_str) {
        return Redirect::to("/percy/dashboard").into_response();
    }

    let guild = match percy.get_guild(guild_id).await {
        Ok(g) => g,
        Err(PercyError::NotFound) => return Redirect::to("/percy/dashboard").into_response(),
        Err(_) => return Redirect::to("/percy/dashboard").into_response(),
    };

    GuildTemplate {
        account: Some(account),
        flashes,
        guild,
    }
    .into_response()
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
}
