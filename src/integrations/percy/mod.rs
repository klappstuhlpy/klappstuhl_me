//! Percy bot internal API client.
//!
//! Proxies dashboard requests to Percy's aiohttp internal API, which owns all
//! guild config mutations and cache invalidation. The client authenticates with
//! a pre-shared bearer token configured in [`PercyConfig`].

//!
//! Response models live in [`types`].

use reqwest::Client;

use crate::config::PercyConfig;

mod types;
pub use types::*;

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
    pub async fn patch_guild_config(&self, guild_id: u64, patch: &serde_json::Value) -> Result<(), PercyError> {
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
    pub async fn get_guild_members(&self, guild_id: u64, limit: u32, after: u64) -> Result<Vec<Member>, PercyError> {
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
    pub async fn patch_gatekeeper(&self, guild_id: u64, patch: &serde_json::Value) -> Result<(), PercyError> {
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

    /// Send the gatekeeper verification message to a channel and return the message_id.
    pub async fn send_gatekeeper_message(
        &self,
        guild_id: u64,
        channel_id: u64,
        title: &str,
        content: &str,
    ) -> Result<u64, PercyError> {
        let body = serde_json::json!({
            "channel_id": channel_id,
            "guild_id": guild_id,
            "title": title,
            "content": content,
        });
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/gatekeeper/message")))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        let data: serde_json::Value = resp.json().await?;
        let message_id = data["message_id"].as_u64().unwrap_or(0);
        Ok(message_id)
    }

    /// Enable or disable the gatekeeper.
    pub async fn toggle_gatekeeper(&self, guild_id: u64, enabled: bool) -> Result<(), PercyError> {
        let body = serde_json::json!({"enabled": enabled});
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/gatekeeper/toggle")))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
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

    /// Fetch the daily cumulative-XP history for the leveling chart.
    pub async fn get_leveling_xp_history(&self, guild_id: u64, days: u32) -> Result<XpHistoryResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/leveling/xp-history")))
            .bearer_auth(&self.token)
            .query(&[("days", days.to_string())])
            .send()
            .await?;
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Fetch an aggregated member profile (identity, leveling, moderation history, notes).
    pub async fn get_member_detail(&self, guild_id: u64, user_id: &str) -> Result<MemberDetail, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/members/{user_id}/detail")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Fetch a user's avatar history (base64 images + timestamps).
    pub async fn get_member_avatars(&self, guild_id: u64, user_id: &str, limit: u32) -> Result<AvatarHistoryResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/members/{user_id}/avatars")))
            .query(&[("limit", limit.to_string())])
            .bearer_auth(&self.token)
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
    pub async fn patch_poll(&self, guild_id: u64, poll_id: i64, patch: &serde_json::Value) -> Result<(), PercyError> {
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
    pub async fn toggle_command(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
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
    pub async fn manage_plonk(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
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

    // -- Autoresponders -------------------------------------------------------

    /// Fetch autoresponders for a guild.
    pub async fn get_autoresponders(&self, guild_id: u64) -> Result<AutorespondersResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/autoresponders")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(resp.json().await?)
    }

    /// Create an autoresponder.
    pub async fn create_autoresponder(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/autoresponders")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Delete an autoresponder by trigger.
    pub async fn delete_autoresponder(&self, guild_id: u64, trigger: &str) -> Result<(), PercyError> {
        let resp = self
            .client
            .delete(self.url(&format!("/api/internal/guilds/{guild_id}/autoresponders/{trigger}")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Patch an autoresponder by trigger.
    pub async fn patch_autoresponder(
        &self,
        guild_id: u64,
        trigger: &str,
        body: &serde_json::Value,
    ) -> Result<(), PercyError> {
        let resp = self
            .client
            .patch(self.url(&format!("/api/internal/guilds/{guild_id}/autoresponders/{trigger}")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    // -- Economy --------------------------------------------------------------

    /// Fetch economy info (shop items + lottery) for a guild.
    pub async fn get_economy(&self, guild_id: u64) -> Result<EconomyInfo, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/economy")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(resp.json().await?)
    }

    /// Create a shop item.
    pub async fn create_economy_item(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/economy/items")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Delete a shop item by name.
    pub async fn delete_economy_item(&self, guild_id: u64, name: &str) -> Result<(), PercyError> {
        let resp = self
            .client
            .delete(self.url(&format!("/api/internal/guilds/{guild_id}/economy/items/{name}")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Fetch economy balances (leaderboard).
    pub async fn get_economy_balances(&self, guild_id: u64, limit: u32) -> Result<BalancesResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/economy/balances")))
            .bearer_auth(&self.token)
            .query(&[("limit", limit.to_string())])
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(resp.json().await?)
    }

    /// Patch a user's economy balance.
    pub async fn patch_economy_balance(
        &self,
        guild_id: u64,
        user_id: &str,
        body: &serde_json::Value,
    ) -> Result<(), PercyError> {
        let resp = self
            .client
            .patch(self.url(&format!("/api/internal/guilds/{guild_id}/economy/balances/{user_id}")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Create a lottery.
    pub async fn create_lottery(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/economy/lottery")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Delete (cancel) the active lottery.
    pub async fn delete_lottery(&self, guild_id: u64) -> Result<(), PercyError> {
        let resp = self
            .client
            .delete(self.url(&format!("/api/internal/guilds/{guild_id}/economy/lottery")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    // -- Comics ---------------------------------------------------------------

    /// Fetch comic feeds for a guild.
    pub async fn get_comics(&self, guild_id: u64) -> Result<ComicsResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/comics")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(resp.json().await?)
    }

    /// Create a comic feed.
    pub async fn create_comic(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/comics")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Patch a comic feed by brand.
    pub async fn patch_comic(&self, guild_id: u64, brand: &str, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .patch(self.url(&format!("/api/internal/guilds/{guild_id}/comics/{brand}")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Delete a comic feed by brand.
    pub async fn delete_comic(&self, guild_id: u64, brand: &str) -> Result<(), PercyError> {
        let resp = self
            .client
            .delete(self.url(&format!("/api/internal/guilds/{guild_id}/comics/{brand}")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Trigger a manual push for a comic feed.
    pub async fn push_comic(&self, guild_id: u64, brand: &str) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/comics/{brand}/push")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    // -- Temp Channels --------------------------------------------------------

    /// Fetch temp channel entries for a guild.
    pub async fn get_temp_channels(&self, guild_id: u64) -> Result<TempChannelsResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/temp-channels")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(resp.json().await?)
    }

    /// Create a temp channel config.
    pub async fn create_temp_channel(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/temp-channels")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Patch a temp channel config.
    pub async fn patch_temp_channel(
        &self,
        guild_id: u64,
        channel_id: u64,
        body: &serde_json::Value,
    ) -> Result<(), PercyError> {
        let resp = self
            .client
            .patch(self.url(&format!("/api/internal/guilds/{guild_id}/temp-channels/{channel_id}")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Delete a temp channel config.
    pub async fn delete_temp_channel(&self, guild_id: u64, channel_id: u64) -> Result<(), PercyError> {
        let resp = self
            .client
            .delete(self.url(&format!("/api/internal/guilds/{guild_id}/temp-channels/{channel_id}")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    // -- Status Feed ----------------------------------------------------------

    /// Fetch status feed info for a guild.
    pub async fn get_status_feed(&self, guild_id: u64) -> Result<StatusFeedInfo, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/status-feed")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(resp.json().await?)
    }

    /// Subscribe to or update status feed.
    pub async fn post_status_feed(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/status-feed")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Unsubscribe from status feed.
    pub async fn delete_status_feed(&self, guild_id: u64) -> Result<(), PercyError> {
        let resp = self
            .client
            .delete(self.url(&format!("/api/internal/guilds/{guild_id}/status-feed")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    // -- Lockdowns ------------------------------------------------------------

    /// Fetch locked-down channels for a guild.
    pub async fn get_lockdowns(&self, guild_id: u64) -> Result<LockdownsResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/lockdowns")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(resp.json().await?)
    }

    /// Lock channels (apply lockdown overwrites).
    pub async fn lock_channels(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/lockdowns/lock")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Unlock channels.
    pub async fn unlock_channels(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/lockdowns/unlock")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Add or remove a moderation ignored entity (safe automod entity).
    pub async fn manage_moderation_ignore(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/moderation/ignore")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    // -- Highlights -----------------------------------------------------------

    /// Fetch highlights for a guild.
    pub async fn get_highlights(&self, guild_id: u64) -> Result<HighlightsResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/highlights")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(resp.json().await?)
    }

    /// Delete a user's highlights.
    pub async fn delete_highlight(&self, guild_id: u64, user_id: &str) -> Result<(), PercyError> {
        let resp = self
            .client
            .delete(self.url(&format!("/api/internal/guilds/{guild_id}/highlights/{user_id}")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    // -- Emoji Stats ----------------------------------------------------------

    /// Fetch emoji usage stats for a guild.
    pub async fn get_emoji_stats(&self, guild_id: u64, limit: u32) -> Result<EmojiStatsResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/emoji-stats")))
            .bearer_auth(&self.token)
            .query(&[("limit", limit.to_string())])
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(resp.json().await?)
    }

    // -- Leveling (extended) --------------------------------------------------

    /// Patch leveling configuration.
    pub async fn patch_leveling_config(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .patch(self.url(&format!("/api/internal/guilds/{guild_id}/leveling/config")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Manage leveling role rewards.
    pub async fn post_leveling_roles(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/leveling/roles")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Create the preset of milestone level-reward roles (levels 5-100).
    pub async fn create_leveling_role_preset(&self, guild_id: u64) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/leveling/roles/preset")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Manage leveling XP multipliers.
    pub async fn post_leveling_multipliers(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/leveling/multipliers")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    /// Manage leveling blacklist (channels/roles excluded from XP).
    pub async fn post_leveling_blacklist(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/leveling/blacklist")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(())
    }

    // -- Audit log (moderation cases) -----------------------------------------

    /// Fetch paginated, filterable moderation cases.
    pub async fn get_cases(
        &self,
        guild_id: u64,
        limit: u32,
        offset: u32,
        action: Option<&str>,
        moderator_id: Option<&str>,
        target_id: Option<&str>,
        after: Option<&str>,
        before: Option<&str>,
    ) -> Result<CasesResponse, PercyError> {
        let mut req = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/cases")))
            .bearer_auth(&self.token)
            .query(&[("limit", limit.to_string()), ("offset", offset.to_string())]);
        if let Some(a) = action {
            req = req.query(&[("action", a)]);
        }
        if let Some(m) = moderator_id {
            req = req.query(&[("moderator_id", m)]);
        }
        if let Some(t) = target_id {
            req = req.query(&[("target_id", t)]);
        }
        if let Some(a) = after {
            req = req.query(&[("after", a)]);
        }
        if let Some(b) = before {
            req = req.query(&[("before", b)]);
        }
        let resp = req.send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Fetch cases created since a given timestamp (for live notifications).
    pub async fn get_recent_cases(&self, guild_id: u64, since: &str) -> Result<RecentCasesResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/cases/recent")))
            .bearer_auth(&self.token)
            .query(&[("since", since)])
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }

    /// Perform a bulk moderation action on multiple members.
    pub async fn bulk_member_action(&self, guild_id: u64, body: &serde_json::Value) -> Result<BulkActionResponse, PercyError> {
        let resp = self
            .client
            .post(self.url(&format!("/api/internal/guilds/{guild_id}/members/bulk-action")))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if resp.status().is_client_error() || resp.status().is_server_error() {
            let text = resp.text().await.unwrap_or_default();
            return Err(PercyError::Api(text));
        }
        Ok(resp.json().await?)
    }

    /// Fetch per-day activity data for a member (heatmap).
    pub async fn get_member_activity(&self, guild_id: u64, user_id: &str, days: u32) -> Result<ActivityResponse, PercyError> {
        let resp = self
            .client
            .get(self.url(&format!("/api/internal/guilds/{guild_id}/members/{user_id}/activity")))
            .bearer_auth(&self.token)
            .query(&[("days", days.to_string())])
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        resp.error_for_status_ref().map_err(PercyError::Http)?;
        Ok(resp.json().await?)
    }
}

// -- Error type --------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PercyError {
    #[error("guild not found")]
    NotFound,
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Api(String),
}
