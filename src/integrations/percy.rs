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

    /// Perform a moderation action on a guild member.
    pub async fn member_action(
        &self,
        guild_id: u64,
        user_id: &str,
        action: &str,
        reason: Option<&str>,
    ) -> Result<(), PercyError> {
        let mut body = serde_json::json!({"action": action});
        if let Some(r) = reason {
            body["reason"] = serde_json::Value::String(r.to_string());
        }
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/members/{user_id}/action")))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(())
    }

    /// Update a member's roles.
    pub async fn member_roles(
        &self,
        guild_id: u64,
        user_id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<(), PercyError> {
        let body = serde_json::json!({"add": add, "remove": remove});
        let resp = self
            .client
            .patch(self.url(&format!("/api/internal/guilds/{guild_id}/members/{user_id}/roles")))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(())
    }

    /// Fetch gatekeeper configuration for a guild.
    pub async fn get_gatekeeper(&self, guild_id: u64) -> Result<Option<GatekeeperInfo>, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/gatekeeper")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Patch gatekeeper configuration.
    pub async fn patch_gatekeeper(
        &self,
        guild_id: u64,
        patch: &serde_json::Value,
    ) -> Result<(), PercyError> {
        let resp = self
            .client
            .patch(self.url(&format!("/api/internal/guilds/{guild_id}/gatekeeper")))
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

    /// Fetch leveling config for a guild.
    pub async fn get_leveling_config(&self, guild_id: u64) -> Result<LevelingConfig, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/leveling/config")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Fetch the leveling leaderboard.
    pub async fn get_leveling_leaderboard(&self, guild_id: u64, limit: u32) -> Result<LeaderboardResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/leveling/leaderboard")))
            .bearer_auth(&self.token)
            .query(&[("limit", limit.to_string())])
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Update a user's level/xp.
    pub async fn patch_leveling_user(
        &self,
        guild_id: u64,
        user_id: &str,
        patch: &serde_json::Value,
    ) -> Result<(), PercyError> {
        let resp = self
            .client
            .patch(self.url(&format!("/api/internal/guilds/{guild_id}/leveling/users/{user_id}")))
            .bearer_auth(&self.token)
            .json(patch)
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(())
    }

    /// Fetch polls for a guild.
    pub async fn get_polls(&self, guild_id: u64) -> Result<PollsResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/polls")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Edit a poll.
    pub async fn patch_poll(
        &self,
        guild_id: u64,
        poll_id: i64,
        patch: &serde_json::Value,
    ) -> Result<(), PercyError> {
        let resp = self
            .client
            .patch(self.url(&format!("/api/internal/guilds/{guild_id}/polls/{poll_id}")))
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

    /// Fetch giveaways for a guild.
    pub async fn get_giveaways(&self, guild_id: u64) -> Result<GiveawaysResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/giveaways")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Fetch tags for a guild.
    pub async fn get_tags(&self, guild_id: u64) -> Result<TagsResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/tags")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Fetch commands and plonk list for a guild.
    pub async fn get_commands(&self, guild_id: u64) -> Result<CommandsResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/commands")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Toggle a command (enable/disable).
    pub async fn toggle_command(
        &self,
        guild_id: u64,
        body: &serde_json::Value,
    ) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/commands/toggle")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(())
    }

    /// Manage plonks (add/remove ignored entities).
    pub async fn manage_plonk(
        &self,
        guild_id: u64,
        body: &serde_json::Value,
    ) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/plonks")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(())
    }

    /// Fetch guild stats.
    pub async fn get_guild_stats(&self, guild_id: u64) -> Result<GuildStats, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/stats")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Fetch bot-wide stats.
    pub async fn get_bot_stats(&self) -> Result<BotStats, PercyError> {
        let resp = self
            .client
            .get(self.url("/api/internal/bot/stats"))
            .bearer_auth(&self.token)
            .send()
            .await?;
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChannelRef {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub channel_type: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RoleRef {
    pub id: String,
    pub name: String,
    pub color: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GuildInfo {
    pub id: u64,
    pub name: String,
    pub icon_url: Option<String>,
    pub member_count: Option<u32>,
    pub flags: GuildFlags,
    pub audit_log_channel: Option<ChannelRef>,
    #[serde(default)]
    pub mod_log_channel: Option<ChannelRef>,
    #[serde(default)]
    pub message_log_channel: Option<ChannelRef>,
    #[serde(default)]
    pub voice_log_channel: Option<ChannelRef>,
    pub poll_channel: Option<ChannelRef>,
    pub poll_ping_role: Option<RoleRef>,
    pub poll_reason_channel: Option<ChannelRef>,
    pub mention_count: Option<u32>,
    pub mute_role: Option<RoleRef>,
    pub alert_channel: Option<ChannelRef>,
    pub music_panel_channel: Option<ChannelRef>,
    pub use_music_panel: bool,
    pub prefixes: Vec<String>,
    #[serde(default)]
    pub is_new_config: bool,
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

#[derive(Debug, Deserialize, Serialize)]
pub struct GatekeeperInfo {
    pub channel: Option<ChannelRef>,
    pub role: Option<RoleRef>,
    pub starter_role: Option<RoleRef>,
    pub bypass_action: String,
    pub rate: Option<String>,
    pub started_at: Option<String>,
    pub member_count: u32,
}

// -- Leveling types ----------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct LevelingConfig {
    pub enabled: bool,
    #[serde(default)]
    pub configured: bool,
    pub level_up_channel_id: Option<String>,
    pub level_up_message: Option<String>,
    #[serde(default)]
    pub stack_roles: bool,
    #[serde(default)]
    pub voice_enabled: bool,
    #[serde(default = "default_xp_rate")]
    pub xp_rate: f64,
}

fn default_xp_rate() -> f64 {
    1.0
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LeaderboardEntry {
    pub user_id: String,
    pub username: String,
    pub avatar_url: Option<String>,
    pub level: u32,
    pub xp: u64,
    pub total_xp: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LeaderboardResponse {
    pub entries: Vec<LeaderboardEntry>,
    pub total: u32,
}

// -- Polls types -------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct PollInfo {
    pub id: i64,
    pub channel_id: String,
    pub message_id: String,
    pub question: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub image_url: String,
    #[serde(default)]
    pub color: String,
    pub published: Option<String>,
    pub expires: Option<String>,
    #[serde(default)]
    pub ended: bool,
    #[serde(default)]
    pub total_votes: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PollsResponse {
    pub polls: Vec<PollInfo>,
}

// -- Giveaways types ---------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct GiveawayInfo {
    pub id: i64,
    pub channel_id: String,
    pub message_id: String,
    pub author_id: String,
    pub title: String,
    pub description: Option<String>,
    pub winners_count: u32,
    pub entries: u32,
    #[serde(default)]
    pub ended: bool,
    pub ends_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GiveawaysResponse {
    pub giveaways: Vec<GiveawayInfo>,
}

// -- Tags types --------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct TagInfo {
    pub id: i64,
    pub name: String,
    pub owner_id: Option<String>,
    pub owner_name: Option<String>,
    pub uses: u32,
    pub created_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TagCreator {
    pub user_id: Option<String>,
    pub username: String,
    pub count: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TagsResponse {
    pub total: u32,
    pub total_uses: u64,
    pub tags: Vec<TagInfo>,
    pub top_creators: Vec<TagCreator>,
}

// -- Commands types ----------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct CommandInfo {
    pub name: String,
    pub category: String,
    pub description: String,
    #[serde(default)]
    pub disabled_in: Vec<String>,
    pub globally_disabled: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PlonkEntry {
    pub entity_id: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CommandsResponse {
    pub commands: Vec<CommandInfo>,
    pub plonks: Vec<PlonkEntry>,
}

// -- Stats types -------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct CommandUsage {
    pub command: String,
    pub uses: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GuildStats {
    pub member_count: Option<u32>,
    pub online_count: u32,
    pub bot_count: u32,
    pub human_count: u32,
    pub channel_count: u32,
    pub role_count: u32,
    pub emoji_count: u32,
    pub boost_count: u32,
    pub boost_tier: u32,
    pub total_commands: u64,
    pub top_commands: Vec<CommandUsage>,
    pub created_at: String,
    pub owner_id: String,
    pub owner_name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BotStats {
    pub guild_count: u32,
    pub user_count: u32,
    pub channel_count: u32,
    pub total_commands_used: u64,
    pub cog_count: u32,
    pub command_count: u32,
    pub latency_ms: f64,
    #[serde(default)]
    pub uptime_seconds: f64,
}

// -- Error type --------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PercyError {
    #[error("guild not found")]
    NotFound,
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}
