//! Percy bot internal API client.
//!
//! Proxies dashboard requests to Percy's aiohttp internal API, which owns all
//! guild config mutations and cache invalidation. The client authenticates with
//! a pre-shared bearer token configured in [`PercyConfig`].

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::config::PercyConfig;

/// A typed client for Percy's internal API.
#[derive(Clone)]
pub struct PercyClient {
    client: Client,
    base_url: String,
    token: String,
}

impl PercyClient {
    /// Creates a new client from the Percy config block. Returns `None` if the
    /// config is not fully set.
    pub fn new(client: Client, config: &PercyConfig) -> Option<Self> {
        if !config.enabled() {
            return None;
        }
        Some(Self {
            client,
            base_url: config.api_url.clone().unwrap().trim_end_matches('/').to_string(),
            token: config.api_token.clone().unwrap(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// Fetch full guild config + metadata from Percy.
    pub async fn get_guild(&self, guild_id: u64) -> Result<GuildInfo, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Patch guild config fields.
    pub async fn patch_guild_config(
        &self,
        guild_id: u64,
        patch: &serde_json::Value,
    ) -> Result<(), PercyError> {
        let resp = self
            .client
            .patch(self.url(&format!("/api/internal/guilds/{guild_id}/config")))
            .bearer_auth(&self.token)
            .json(patch)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(())
    }

    /// Fetch guild roles.
    pub async fn get_guild_roles(&self, guild_id: u64) -> Result<Vec<Role>, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/roles")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Fetch guild channels.
    pub async fn get_guild_channels(&self, guild_id: u64) -> Result<Vec<Channel>, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/channels")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Fetch guilds the given Discord user can manage (has Manage Server or Admin).
    pub async fn get_user_guilds(&self, discord_user_id: &str) -> Result<Vec<UserGuild>, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/users/{discord_user_id}/guilds")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Fetch guild members (paginated).
    pub async fn get_guild_members(
        &self,
        guild_id: u64,
        limit: u32,
        after: u64,
    ) -> Result<Vec<Member>, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/members")))
            .bearer_auth(&self.token)
            .query(&[("limit", limit.to_string()), ("after", after.to_string())])
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }
}

// -- Response types ----------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct UserGuild {
    pub id: String,
    pub name: String,
    pub icon_url: Option<String>,
    pub member_count: Option<u32>,
    pub owner: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GuildInfo {
    pub id: u64,
    pub name: String,
    pub icon_url: Option<String>,
    pub member_count: Option<u32>,
    pub flags: GuildFlags,
    pub audit_log_channel_id: Option<u64>,
    pub poll_channel_id: Option<u64>,
    pub poll_ping_role_id: Option<u64>,
    pub poll_reason_channel_id: Option<u64>,
    pub mention_count: Option<u32>,
    pub mute_role_id: Option<u64>,
    pub alert_channel_id: Option<u64>,
    pub music_panel_channel_id: Option<u64>,
    pub use_music_panel: bool,
    pub prefixes: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GuildFlags {
    pub audit_log: bool,
    pub raid: bool,
    pub alerts: bool,
    pub gatekeeper: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub color: u32,
    pub position: i32,
    pub permissions: u64,
    pub mentionable: bool,
    pub managed: bool,
    pub hoist: bool,
    pub icon_url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub channel_type: String,
    pub position: i32,
    pub category_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub avatar_url: String,
    pub joined_at: Option<String>,
    pub roles: Vec<String>,
    pub bot: bool,
}

// -- Error type --------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PercyError {
    #[error("guild not found")]
    NotFound,
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}
