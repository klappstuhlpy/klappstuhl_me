//! Percy bot internal API client.
//!
//! Proxies dashboard requests to Percy's aiohttp internal API, which owns all
//! guild config mutations and cache invalidation. The client authenticates with
//! a pre-shared bearer token configured in [`PercyConfig`].
//!
//! Response models live in [`types`].
//!
//! Every endpoint method is a one-liner over three shared helpers: [`PercyClient::send_into`]
//! (deserialize the JSON body), [`PercyClient::send_unit`] (discard the body), and the
//! associated [`PercyClient::check`] (uniform status → [`PercyError`] mapping). `404` maps to
//! [`PercyError::NotFound`]; every other 4xx/5xx maps to [`PercyError::Api`] carrying Percy's
//! response body, so upstream validation errors reach the dashboard.

use reqwest::{Client, RequestBuilder, Response};
use serde::de::DeserializeOwned;

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

    // -- Shared request plumbing ---------------------------------------------

    /// Maps Percy's HTTP status into a [`PercyError`], returning the response for
    /// the caller to deserialize on success. `404` → [`PercyError::NotFound`]; any
    /// other 4xx/5xx → [`PercyError::Api`] carrying the response body so Percy's
    /// error text surfaces to the dashboard.
    async fn check(resp: Response) -> Result<Response, PercyError> {
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(PercyError::NotFound);
        }
        if status.is_client_error() || status.is_server_error() {
            return Err(PercyError::Api(resp.text().await.unwrap_or_default()));
        }
        Ok(resp)
    }

    /// Applies auth, sends `req`, and discards the body. For mutations / fire-and-forget.
    async fn send_unit(&self, req: RequestBuilder) -> Result<(), PercyError> {
        Self::check(req.bearer_auth(&self.token).send().await?).await?;
        Ok(())
    }

    /// Applies auth, sends `req`, and deserializes the JSON body into `T`.
    async fn send_into<T: DeserializeOwned>(&self, req: RequestBuilder) -> Result<T, PercyError> {
        let resp = Self::check(req.bearer_auth(&self.token).send().await?).await?;
        Ok(resp.json().await?)
    }

    // -- Guild config --------------------------------------------------------

    /// Fetch full guild config + metadata from Percy.
    pub async fn get_guild(&self, guild_id: u64) -> Result<GuildInfo, PercyError> {
        self.send_into(self.client.get(self.url(&format!("/api/v1/guilds/{guild_id}"))))
            .await
    }

    /// Patch guild config fields.
    pub async fn patch_guild_config(&self, guild_id: u64, patch: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/config")))
                .json(patch),
        )
        .await
    }

    /// Apply multiple config mutations atomically in one request.
    pub async fn batch_guild_config(
        &self,
        guild_id: u64,
        operations: &[BatchOperation],
    ) -> Result<BatchResponse, PercyError> {
        self.send_into(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/batch")))
                .json(&serde_json::json!({"operations": operations})),
        )
        .await
    }

    /// Update audit log event flags.
    pub async fn patch_audit_log_flags(
        &self,
        guild_id: u64,
        flags: &std::collections::HashMap<String, bool>,
    ) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/audit-log-flags")))
                .json(flags),
        )
        .await
    }

    /// Fetch guild roles.
    pub async fn get_guild_roles(&self, guild_id: u64) -> Result<Vec<Role>, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/roles"))),
        )
        .await
    }

    /// Fetch guild channels.
    pub async fn get_guild_channels(&self, guild_id: u64) -> Result<Vec<Channel>, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/channels"))),
        )
        .await
    }

    /// Fetch guilds the given Discord user can manage (has Manage Server or Admin).
    pub async fn get_user_guilds(&self, discord_user_id: &str) -> Result<Vec<UserGuild>, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/users/{discord_user_id}/guilds"))),
        )
        .await
    }

    /// Fetch a user's *current* avatar URL straight from the bot. Resolved live so
    /// it never goes stale; the dashboard does not persist avatars.
    pub async fn get_user_avatar(&self, discord_user_id: &str) -> Result<UserAvatar, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/users/{discord_user_id}/avatar"))),
        )
        .await
    }

    // -- Members -------------------------------------------------------------

    /// Fetch guild members (paginated, optionally filtered by search term).
    pub async fn get_guild_members(
        &self,
        guild_id: u64,
        limit: u32,
        after: u64,
        search: Option<&str>,
    ) -> Result<MembersResponse, PercyError> {
        let mut query = vec![("limit", limit.to_string()), ("after", after.to_string())];
        if let Some(s) = search {
            if !s.is_empty() {
                query.push(("search", s.to_string()));
            }
        }
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/members")))
                .query(&query),
        )
        .await
    }

    /// Perform a moderation action on a guild member.
    pub async fn member_action(
        &self,
        guild_id: u64,
        user_id: &str,
        action: &str,
        reason: Option<&str>,
        moderator_id: Option<&str>,
    ) -> Result<(), PercyError> {
        let mut body = serde_json::json!({"action": action});
        if let Some(r) = reason {
            body["reason"] = serde_json::Value::String(r.to_string());
        }
        if let Some(m) = moderator_id {
            body["moderator_id"] = serde_json::Value::String(m.to_string());
        }
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/members/{user_id}/action")))
                .json(&body),
        )
        .await
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
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/members/{user_id}/roles")))
                .json(&body),
        )
        .await
    }

    /// Fetch an aggregated member profile (identity, leveling, moderation history).
    pub async fn get_member_detail(&self, guild_id: u64, user_id: &str) -> Result<MemberDetail, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/members/{user_id}/detail"))),
        )
        .await
    }

    /// Fetch a member's own profile (leveling, economy, command stats — no mod history).
    pub async fn get_member_self(&self, guild_id: u64, user_id: &str) -> Result<MemberSelf, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/members/{user_id}/self"))),
        )
        .await
    }

    pub async fn get_user_settings(&self, discord_id: &str) -> Result<UserSettings, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/users/{discord_id}/settings"))),
        )
        .await
    }

    pub async fn patch_user_settings(
        &self,
        discord_id: &str,
        patch: &UserSettingsPatch,
    ) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/users/{discord_id}/settings")))
                .json(patch),
        )
        .await
    }

    /// Fetch a user's avatar history (base64 images + timestamps).
    pub async fn get_member_avatars(
        &self,
        guild_id: u64,
        user_id: &str,
        limit: u32,
    ) -> Result<AvatarHistoryResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/members/{user_id}/avatars")))
                .query(&[("limit", limit.to_string())]),
        )
        .await
    }

    // -- Sentinel ----------------------------------------------------------

    /// Fetch sentinel configuration for a guild.
    pub async fn get_sentinel(&self, guild_id: u64) -> Result<Option<SentinelInfo>, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/sentinel"))),
        )
        .await
    }

    /// Patch sentinel configuration.
    pub async fn patch_sentinel(&self, guild_id: u64, patch: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/sentinel")))
                .json(patch),
        )
        .await
    }

    /// Send the sentinel verification message to a channel and return the message_id.
    pub async fn send_sentinel_message(
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
        let data: serde_json::Value = self
            .send_into(
                self.client
                    .post(self.url(&format!("/api/v1/guilds/{guild_id}/sentinel/message")))
                    .json(&body),
            )
            .await?;
        Ok(data["message_id"].as_u64().unwrap_or(0))
    }

    /// Enable or disable the sentinel.
    pub async fn toggle_sentinel(&self, guild_id: u64, enabled: bool) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/sentinel/toggle")))
                .json(&serde_json::json!({"enabled": enabled})),
        )
        .await
    }

    // -- Leveling ------------------------------------------------------------

    /// Fetch leveling config for a guild.
    pub async fn get_leveling_config(&self, guild_id: u64) -> Result<LevelingConfig, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/leveling/config"))),
        )
        .await
    }

    /// Fetch the leveling leaderboard.
    pub async fn get_leveling_leaderboard(&self, guild_id: u64, limit: u32) -> Result<LeaderboardResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/leveling/leaderboard")))
                .query(&[("limit", limit.to_string())]),
        )
        .await
    }

    /// Fetch the daily cumulative-XP history for the leveling chart.
    pub async fn get_leveling_xp_history(&self, guild_id: u64, days: u32) -> Result<XpHistoryResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/leveling/xp-history")))
                .query(&[("days", days.to_string())]),
        )
        .await
    }

    /// Update a user's level/xp.
    pub async fn patch_leveling_user(
        &self,
        guild_id: u64,
        user_id: &str,
        patch: &serde_json::Value,
    ) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/leveling/users/{user_id}")))
                .json(patch),
        )
        .await
    }

    /// Patch leveling configuration.
    pub async fn patch_leveling_config(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/leveling/config")))
                .json(body),
        )
        .await
    }

    /// Manage leveling role rewards.
    pub async fn post_leveling_roles(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/leveling/roles")))
                .json(body),
        )
        .await
    }

    /// Create the preset of milestone level-reward roles (levels 5-100).
    pub async fn create_leveling_role_preset(&self, guild_id: u64) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/leveling/roles/preset"))),
        )
        .await
    }

    /// Manage leveling XP multipliers.
    pub async fn post_leveling_multipliers(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/leveling/multipliers")))
                .json(body),
        )
        .await
    }

    /// Manage leveling blacklist (channels/roles excluded from XP).
    pub async fn post_leveling_blacklist(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/leveling/blacklist")))
                .json(body),
        )
        .await
    }

    // -- Polls ---------------------------------------------------------------

    /// Fetch polls for a guild.
    pub async fn get_polls(&self, guild_id: u64) -> Result<PollsResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/polls"))),
        )
        .await
    }

    /// Create a new poll. Surfaces Percy's validation error text via [`PercyError::Api`].
    pub async fn create_poll(&self, guild_id: u64, body: &serde_json::Value) -> Result<serde_json::Value, PercyError> {
        self.send_into(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/polls")))
                .json(body),
        )
        .await
    }

    /// Edit a poll.
    pub async fn patch_poll(&self, guild_id: u64, poll_id: i64, patch: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/polls/{poll_id}")))
                .json(patch),
        )
        .await
    }

    /// End a running poll.
    pub async fn end_poll(&self, guild_id: u64, poll_id: i64) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/polls/{poll_id}/end"))),
        )
        .await
    }

    // -- Giveaways / Tags ----------------------------------------------------

    /// Fetch giveaways for a guild.
    pub async fn get_giveaways(&self, guild_id: u64) -> Result<GiveawaysResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/giveaways"))),
        )
        .await
    }

    /// Fetch tags for a guild.
    pub async fn get_tags(&self, guild_id: u64) -> Result<TagsResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/tags"))),
        )
        .await
    }

    // -- Commands ------------------------------------------------------------

    /// Fetch commands and plonk list for a guild.
    pub async fn get_commands(&self, guild_id: u64) -> Result<CommandsResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/commands"))),
        )
        .await
    }

    /// Toggle a command (enable/disable).
    pub async fn toggle_command(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/commands/toggle")))
                .json(body),
        )
        .await
    }

    /// Manage plonks (add/remove ignored entities).
    pub async fn manage_plonk(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/plonks")))
                .json(body),
        )
        .await
    }

    // -- Stats ---------------------------------------------------------------

    /// Fetch guild stats.
    pub async fn get_guild_stats(&self, guild_id: u64) -> Result<GuildStats, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/stats"))),
        )
        .await
    }

    /// Fetch bot-wide stats.
    pub async fn get_bot_stats(&self) -> Result<BotStats, PercyError> {
        self.send_into(self.client.get(self.url("/api/v1/bot/stats")))
            .await
    }

    /// Fetch bot changelog (git log grouped by version tags).
    pub async fn get_changelog(&self) -> Result<ChangelogResponse, PercyError> {
        self.send_into(self.client.get(self.url("/api/v1/bot/changelog")))
            .await
    }

    /// Composite: fetch guild overview (config summary + stats + bot stats + feature flags)
    /// in a single round-trip.
    pub async fn get_guild_overview(&self, guild_id: u64) -> Result<GuildOverview, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/overview"))),
        )
        .await
    }

    /// Fetch all bot commands (public, no guild context).
    pub async fn get_bot_commands(&self) -> Result<Vec<CommandInfo>, PercyError> {
        let resp: PublicCommandsResponse =
            self.send_into(self.client.get(self.url("/api/v1/commands/public"))).await?;
        Ok(resp.commands)
    }

    // -- Autoresponders ------------------------------------------------------

    /// Fetch autoresponders for a guild.
    pub async fn get_autoresponders(&self, guild_id: u64) -> Result<AutorespondersResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/autoresponders"))),
        )
        .await
    }

    /// Create an autoresponder.
    pub async fn create_autoresponder(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/autoresponders")))
                .json(body),
        )
        .await
    }

    /// Delete an autoresponder by trigger.
    pub async fn delete_autoresponder(&self, guild_id: u64, trigger: &str) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .delete(self.url(&format!("/api/v1/guilds/{guild_id}/autoresponders/{trigger}"))),
        )
        .await
    }

    /// Patch an autoresponder by trigger.
    pub async fn patch_autoresponder(
        &self,
        guild_id: u64,
        trigger: &str,
        body: &serde_json::Value,
    ) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/autoresponders/{trigger}")))
                .json(body),
        )
        .await
    }

    // -- Economy -------------------------------------------------------------

    /// Fetch economy info (shop items + lottery) for a guild.
    pub async fn get_economy(&self, guild_id: u64) -> Result<EconomyInfo, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/economy"))),
        )
        .await
    }

    /// Create a shop item.
    pub async fn create_economy_item(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/economy/items")))
                .json(body),
        )
        .await
    }

    /// Delete a shop item by name.
    pub async fn delete_economy_item(&self, guild_id: u64, name: &str) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .delete(self.url(&format!("/api/v1/guilds/{guild_id}/economy/items/{name}"))),
        )
        .await
    }

    /// Fetch economy balances (leaderboard).
    pub async fn get_economy_balances(&self, guild_id: u64, limit: u32) -> Result<BalancesResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/economy/balances")))
                .query(&[("limit", limit.to_string())]),
        )
        .await
    }

    /// Patch a user's economy balance.
    pub async fn patch_economy_balance(
        &self,
        guild_id: u64,
        user_id: &str,
        body: &serde_json::Value,
    ) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/economy/balances/{user_id}")))
                .json(body),
        )
        .await
    }

    /// Create a lottery.
    pub async fn create_lottery(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/economy/lottery")))
                .json(body),
        )
        .await
    }

    /// Delete (cancel) the active lottery.
    pub async fn delete_lottery(&self, guild_id: u64) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .delete(self.url(&format!("/api/v1/guilds/{guild_id}/economy/lottery"))),
        )
        .await
    }

    // -- Music ---------------------------------------------------------------

    /// Fetch music/equalizer state for a guild.
    pub async fn get_music(&self, guild_id: u64) -> Result<MusicInfo, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/music"))),
        )
        .await
    }

    /// Fetch time-synced lyrics for the guild's currently-playing track.
    pub async fn get_music_lyrics(&self, guild_id: u64) -> Result<MusicLyrics, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/music/lyrics"))),
        )
        .await
    }

    /// Apply equalizer settings (preset or custom bands).
    pub async fn post_music_equalizer(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/music/equalizer")))
                .json(body),
        )
        .await
    }

    /// Apply a filter action (nightcore, 8d, lowpass, reset).
    pub async fn post_music_filters(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/music/filters")))
                .json(body),
        )
        .await
    }

    /// Set up the music panel (create channel + send panel message).
    pub async fn post_music_setup(
        &self,
        guild_id: u64,
        body: &serde_json::Value,
    ) -> Result<MusicSetupResponse, PercyError> {
        self.send_into(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/music/setup")))
                .json(body),
        )
        .await
    }

    /// Reset the music configuration (delete channel, clear config).
    pub async fn post_music_reset(&self, guild_id: u64) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/music/reset")))
                .json(&serde_json::json!({})),
        )
        .await
    }

    /// Enable or disable the 24/7 always-on player for a guild.
    pub async fn post_music_247(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/music/247")))
                .json(body),
        )
        .await
    }

    /// Update the DJ mode setting for a guild's music player.
    pub async fn patch_music_dj_mode(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/music/dj-mode")))
                .json(body),
        )
        .await
    }

    /// Control the live player (pause/resume/skip/stop) on behalf of a dashboard
    /// viewer. Percy enforces voice-presence and DJ-mode rules and surfaces a 403
    /// (mapped to [`PercyError::Api`]) when the viewer isn't permitted.
    pub async fn music_control(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/music/control")))
                .json(body),
        )
        .await
    }

    // -- Comics --------------------------------------------------------------

    /// Fetch comic feeds for a guild.
    pub async fn get_comics(&self, guild_id: u64) -> Result<ComicsResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/comics"))),
        )
        .await
    }

    /// Create a comic feed.
    pub async fn create_comic(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/comics")))
                .json(body),
        )
        .await
    }

    /// Patch a comic feed by brand.
    pub async fn patch_comic(&self, guild_id: u64, brand: &str, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/comics/{brand}")))
                .json(body),
        )
        .await
    }

    /// Delete a comic feed by brand.
    pub async fn delete_comic(&self, guild_id: u64, brand: &str) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .delete(self.url(&format!("/api/v1/guilds/{guild_id}/comics/{brand}"))),
        )
        .await
    }

    /// Trigger a manual push for a comic feed.
    pub async fn push_comic(&self, guild_id: u64, brand: &str) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/comics/{brand}/push"))),
        )
        .await
    }

    // -- Temp Channels -------------------------------------------------------

    /// Fetch temp channel entries for a guild.
    pub async fn get_temp_channels(&self, guild_id: u64) -> Result<TempChannelsResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/temp-channels"))),
        )
        .await
    }

    /// Create a temp channel config.
    pub async fn create_temp_channel(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/temp-channels")))
                .json(body),
        )
        .await
    }

    /// Patch a temp channel config.
    pub async fn patch_temp_channel(
        &self,
        guild_id: u64,
        channel_id: u64,
        body: &serde_json::Value,
    ) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/temp-channels/{channel_id}")))
                .json(body),
        )
        .await
    }

    /// Delete a temp channel config.
    pub async fn delete_temp_channel(&self, guild_id: u64, channel_id: u64) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .delete(self.url(&format!("/api/v1/guilds/{guild_id}/temp-channels/{channel_id}"))),
        )
        .await
    }

    // -- Status Feed ---------------------------------------------------------

    /// Fetch status feed info for a guild.
    pub async fn get_status_feed(&self, guild_id: u64) -> Result<StatusFeedInfo, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/status-feed"))),
        )
        .await
    }

    /// Subscribe to or update status feed.
    pub async fn post_status_feed(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/status-feed")))
                .json(body),
        )
        .await
    }

    /// Unsubscribe from status feed.
    pub async fn delete_status_feed(&self, guild_id: u64) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .delete(self.url(&format!("/api/v1/guilds/{guild_id}/status-feed"))),
        )
        .await
    }

    // -- Lockdowns -----------------------------------------------------------

    /// Fetch locked-down channels for a guild.
    pub async fn get_lockdowns(&self, guild_id: u64) -> Result<LockdownsResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/lockdowns"))),
        )
        .await
    }

    /// Lock channels (apply lockdown overwrites).
    pub async fn lock_channels(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/lockdowns/lock")))
                .json(body),
        )
        .await
    }

    /// Unlock channels.
    pub async fn unlock_channels(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/lockdowns/unlock")))
                .json(body),
        )
        .await
    }

    /// Add or remove a moderation ignored entity (safe automod entity).
    pub async fn manage_moderation_ignore(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/moderation/ignore")))
                .json(body),
        )
        .await
    }

    // -- Highlights ----------------------------------------------------------

    /// Fetch highlights for a guild.
    pub async fn get_highlights(&self, guild_id: u64) -> Result<HighlightsResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/highlights"))),
        )
        .await
    }

    /// Delete a user's highlights.
    pub async fn delete_highlight(&self, guild_id: u64, user_id: &str) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .delete(self.url(&format!("/api/v1/guilds/{guild_id}/highlights/{user_id}"))),
        )
        .await
    }

    // -- Emoji Stats ---------------------------------------------------------

    /// Fetch emoji usage stats for a guild.
    pub async fn get_emoji_stats(&self, guild_id: u64, limit: u32) -> Result<EmojiStatsResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/emoji-stats")))
                .query(&[("limit", limit.to_string())]),
        )
        .await
    }

    // -- Audit log (moderation cases) ----------------------------------------

    /// Fetch paginated, filterable moderation cases.
    #[allow(clippy::too_many_arguments)]
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
            .get(self.url(&format!("/api/v1/guilds/{guild_id}/cases")))
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
        self.send_into(req).await
    }

    /// Fetch cases created since a given timestamp (for live notifications).
    pub async fn get_recent_cases(&self, guild_id: u64, since: &str) -> Result<RecentCasesResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/cases/recent")))
                .query(&[("since", since)]),
        )
        .await
    }

    /// Manually open a moderation case (records + announces it; does not perform the action).
    pub async fn create_case(
        &self,
        guild_id: u64,
        action: &str,
        target_id: &str,
        moderator_id: Option<&str>,
        reason: Option<&str>,
    ) -> Result<CaseActionResponse, PercyError> {
        let mut body = serde_json::json!({"action": action, "target_id": target_id});
        if let Some(m) = moderator_id {
            body["moderator_id"] = serde_json::Value::String(m.to_string());
        }
        if let Some(r) = reason {
            body["reason"] = serde_json::Value::String(r.to_string());
        }
        self.send_into(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/cases")))
                .json(&body),
        )
        .await
    }

    /// Update a case's reason (also syncs the modlog channel post).
    pub async fn update_case_reason(
        &self,
        guild_id: u64,
        case_index: u64,
        reason: &str,
    ) -> Result<CaseActionResponse, PercyError> {
        self.send_into(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/cases/{case_index}")))
                .json(&serde_json::json!({"reason": reason})),
        )
        .await
    }

    /// Close (delete) a case, removing its modlog channel post.
    pub async fn delete_case(&self, guild_id: u64, case_index: u64) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .delete(self.url(&format!("/api/v1/guilds/{guild_id}/cases/{case_index}"))),
        )
        .await
    }

    /// Perform a bulk moderation action on multiple members.
    pub async fn bulk_member_action(
        &self,
        guild_id: u64,
        body: &serde_json::Value,
    ) -> Result<BulkActionResponse, PercyError> {
        self.send_into(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/members/bulk-action")))
                .json(body),
        )
        .await
    }

    /// Fetch per-day activity data for a member (heatmap).
    pub async fn get_member_activity(
        &self,
        guild_id: u64,
        user_id: &str,
        days: u32,
    ) -> Result<ActivityResponse, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/members/{user_id}/activity")))
                .query(&[("days", days.to_string())]),
        )
        .await
    }

    // -- Custom Bot Profile ---------------------------------------------------

    /// Fetch the bot's per-guild profile customization.
    pub async fn get_custom_bot(&self, guild_id: u64) -> Result<CustomBotProfile, PercyError> {
        self.send_into(
            self.client
                .get(self.url(&format!("/api/v1/guilds/{guild_id}/custom-bot"))),
        )
        .await
    }

    /// Update the bot's per-guild profile customization.
    pub async fn patch_custom_bot(&self, guild_id: u64, body: &serde_json::Value) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .patch(self.url(&format!("/api/v1/guilds/{guild_id}/custom-bot")))
                .json(body),
        )
        .await
    }

    /// Reset the bot's per-guild profile to defaults.
    pub async fn reset_custom_bot(&self, guild_id: u64) -> Result<(), PercyError> {
        self.send_unit(
            self.client
                .post(self.url(&format!("/api/v1/guilds/{guild_id}/custom-bot/reset")))
                .json(&serde_json::json!({})),
        )
        .await
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
