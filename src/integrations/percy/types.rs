//! Typed response models for Percy's internal API.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize)]
pub struct UserGuild {
    pub id: String,
    pub name: String,
    pub icon_url: Option<String>,
    pub member_count: Option<u32>,
    pub owner: bool,
    /// True when the user has Administrator or Manage Server in this guild. The
    /// dashboard shows manageable guilds as full admin cards and the rest as
    /// read-only public overviews. Defaults to `false` for backward compat with
    /// an older Percy that only returned manageable guilds.
    #[serde(default)]
    pub manageable: bool,
}

/// A user's *current* avatar, resolved live from the bot (never persisted, so it
/// can't go stale).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserAvatar {
    pub avatar_url: String,
    #[serde(default)]
    pub username: String,
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
    #[serde(default)]
    pub ignored_entities: Vec<IgnoredEntity>,
    pub mute_role: Option<RoleRef>,
    pub alert_channel: Option<ChannelRef>,
    #[serde(default)]
    pub audit_log_flags: std::collections::BTreeMap<String, bool>,
    pub music_panel_channel: Option<ChannelRef>,
    pub use_music_panel: bool,
    pub prefixes: Vec<String>,
    #[serde(default)]
    pub is_new_config: bool,
    #[serde(default)]
    pub ai: AiConfig,
}

/// Per-guild AI feature flags plus per-channel overrides (the AI-native rewrite).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AiConfig {
    #[serde(default)]
    pub flags: AiFlags,
    #[serde(default)]
    pub overrides: Vec<AiChannelOverride>,
}

/// The AI feature toggles. Mirrors Percy's `GuildConfig.AIFlags`; each field defaults off.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AiFlags {
    #[serde(default)]
    pub assistant: bool,
    #[serde(default)]
    pub router: bool,
    #[serde(default)]
    pub moderation: bool,
    #[serde(default)]
    pub sentinel: bool,
    #[serde(default)]
    pub music: bool,
    #[serde(default)]
    pub polls: bool,
    #[serde(default)]
    pub giveaways: bool,
    #[serde(default)]
    pub tags: bool,
    #[serde(default)]
    pub reminders: bool,
}

/// A per-channel override: `controlled` marks which features the channel overrides,
/// and `enabled` holds their on/off value for those controlled features.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AiChannelOverride {
    #[serde(default)]
    pub channel: Option<ChannelRef>,
    pub channel_id: String,
    #[serde(default)]
    pub controlled: AiFlags,
    #[serde(default)]
    pub enabled: AiFlags,
}

/// One controlled feature in an override's summary (for the dashboard table).
pub struct AiFeatureState {
    pub label: &'static str,
    pub on: bool,
}

impl AiChannelOverride {
    /// The features this channel actually overrides, with their on/off value, for display.
    pub fn summary(&self) -> Vec<AiFeatureState> {
        let pairs = [
            ("Assistant", self.controlled.assistant, self.enabled.assistant),
            ("Router", self.controlled.router, self.enabled.router),
            ("Moderation", self.controlled.moderation, self.enabled.moderation),
            ("Sentinel", self.controlled.sentinel, self.enabled.sentinel),
            ("Music", self.controlled.music, self.enabled.music),
            ("Polls", self.controlled.polls, self.enabled.polls),
            ("Giveaways", self.controlled.giveaways, self.enabled.giveaways),
            ("Tags", self.controlled.tags, self.enabled.tags),
            ("Reminders", self.controlled.reminders, self.enabled.reminders),
        ];
        pairs
            .into_iter()
            .filter(|&(_, controlled, _)| controlled)
            .map(|(label, _, on)| AiFeatureState { label, on })
            .collect()
    }
}

impl AiConfig {
    /// The per-channel overrides serialized as JSON, for the editor's `window.AI_OVERRIDES`.
    pub fn overrides_json(&self) -> String {
        serde_json::to_string(&self.overrides).unwrap_or_else(|_| "[]".to_string())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IgnoredEntity {
    pub id: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GuildFlags {
    pub audit_log: bool,
    pub raid: bool,
    pub alerts: bool,
    pub sentinel: bool,
    #[serde(default)]
    pub mentions: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
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
pub struct MembersResponse {
    pub members: Vec<Member>,
    pub total: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SentinelInfo {
    pub channel: Option<ChannelRef>,
    pub role: Option<RoleRef>,
    pub message: Option<u64>,
    pub starter_role: Option<RoleRef>,
    pub bypass_action: String,
    pub rate: Option<String>,
    pub started_at: Option<String>,
    pub member_count: u32,
    pub needs_setup: bool,
}

// -- Leveling types ----------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct LevelingConfig {
    pub enabled: bool,
    #[serde(default)]
    pub configured: bool,
    /// 0 = don't send, 1 = source channel, 2 = DM, else channel id.
    #[serde(default = "default_level_up_channel")]
    pub level_up_channel: i64,
    pub level_up_message: Option<String>,
    /// Map of level (as string) -> custom message.
    #[serde(default)]
    pub special_level_up_messages: HashMap<String, String>,
    #[serde(default)]
    pub blacklisted_roles: Vec<i64>,
    #[serde(default)]
    pub blacklisted_channels: Vec<i64>,
    #[serde(default)]
    pub blacklisted_users: Vec<i64>,
    /// Map of role id (as string) -> level threshold.
    #[serde(default)]
    pub level_roles: HashMap<String, i64>,
    /// Map of role id (as string) -> XP multiplier.
    #[serde(default)]
    pub multiplier_roles: HashMap<String, f64>,
    /// Map of channel id (as string) -> XP multiplier.
    #[serde(default)]
    pub multiplier_channels: HashMap<String, f64>,
    #[serde(default)]
    pub role_stack: bool,
    #[serde(default)]
    pub voice_enabled: bool,
    #[serde(default)]
    pub delete_after_leave: bool,
    #[serde(default = "default_factor")]
    pub factor: f64,
    #[serde(default = "default_base")]
    pub base: i64,
    #[serde(default = "default_min_gain")]
    pub min_gain: i64,
    #[serde(default = "default_max_gain")]
    pub max_gain: i64,
    #[serde(default = "default_cooldown_per")]
    pub cooldown_per: i64,
}

fn default_factor() -> f64 {
    1.0
}
fn default_level_up_channel() -> i64 {
    1
}
fn default_base() -> i64 {
    100
}
fn default_min_gain() -> i64 {
    8
}
fn default_max_gain() -> i64 {
    15
}
fn default_cooldown_per() -> i64 {
    40
}

impl Default for LevelingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            configured: false,
            level_up_channel: default_level_up_channel(),
            level_up_message: None,
            special_level_up_messages: HashMap::new(),
            blacklisted_roles: Vec::new(),
            blacklisted_channels: Vec::new(),
            blacklisted_users: Vec::new(),
            level_roles: HashMap::new(),
            multiplier_roles: HashMap::new(),
            multiplier_channels: HashMap::new(),
            role_stack: false,
            voice_enabled: false,
            delete_after_leave: false,
            factor: default_factor(),
            base: default_base(),
            min_gain: default_min_gain(),
            max_gain: default_max_gain(),
            cooldown_per: default_cooldown_per(),
        }
    }
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

/// One day of the cumulative-XP time series for a guild.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct XpHistoryPoint {
    /// ISO date (YYYY-MM-DD).
    pub day: String,
    pub total_xp: i64,
    pub gainers: i64,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct XpHistoryResponse {
    #[serde(default)]
    pub points: Vec<XpHistoryPoint>,
    #[serde(default)]
    pub days: u32,
}

// -- Member detail (user lookup) ---------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct MemberRoleBadge {
    pub id: String,
    pub name: String,
    pub color: u32,
}

impl MemberRoleBadge {
    pub fn color_hex(&self) -> String {
        format!("#{:06x}", self.color)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MemberLeveling {
    pub level: i64,
    pub xp: i64,
    pub messages: i64,
    pub rank: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MemberCase {
    pub case_index: i64,
    pub action: String,
    pub reason: Option<String>,
    pub moderator_id: Option<String>,
    pub moderator_name: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MemberCommandStats {
    #[serde(default)]
    pub total_commands: i64,
    pub first_command_at: Option<String>,
    #[serde(default)]
    pub top_commands: Vec<CommandUsageEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CommandUsageEntry {
    pub command: String,
    pub uses: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NameHistoryEntry {
    pub name: String,
    pub changed_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MemberNameHistory {
    #[serde(default)]
    pub usernames: Vec<NameHistoryEntry>,
    #[serde(default)]
    pub nicknames: Vec<NameHistoryEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MemberDetail {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub avatar_url: String,
    pub joined_at: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub roles: Vec<MemberRoleBadge>,
    pub bot: bool,
    #[serde(default)]
    pub in_guild: bool,
    pub leveling: Option<MemberLeveling>,
    #[serde(default)]
    pub cases: Vec<MemberCase>,
    #[serde(default)]
    pub case_count: u32,
    #[serde(default)]
    pub warning_count: u32,
    // Extended user info
    pub top_role: Option<String>,
    #[serde(default)]
    pub top_role_color: u32,
    pub join_position: Option<u32>,
    #[serde(default)]
    pub member_count: u32,
    #[serde(default)]
    pub mutual_guilds: u32,
    #[serde(default)]
    pub permissions: u64,
    pub boosting_since: Option<String>,
    #[serde(default)]
    pub public_flags: Vec<String>,
    pub status: Option<String>,
    pub command_stats: Option<MemberCommandStats>,
    pub last_seen: Option<String>,
    pub names: Option<MemberNameHistory>,
    #[serde(default)]
    pub avatar_count: u32,
    #[serde(default)]
    pub owned_tags: Vec<OwnedTag>,
}

impl MemberDetail {
    pub fn top_role_color_hex(&self) -> String {
        format!("#{:06x}", self.top_role_color)
    }
}

/// Personal profile for a non-admin member (leveling + economy + command stats).
#[derive(Debug, Deserialize, Serialize)]
pub struct MemberSelf {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub avatar_url: String,
    pub joined_at: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub roles: Vec<MemberRoleBadge>,
    pub top_role: Option<String>,
    #[serde(default)]
    pub top_role_color: u32,
    pub join_position: Option<u32>,
    #[serde(default)]
    pub member_count: u32,
    pub boosting_since: Option<String>,
    pub leveling: Option<MemberLeveling>,
    pub economy: Option<MemberEconomy>,
    pub command_stats: Option<MemberCommandStats>,
    #[serde(default)]
    pub owned_tags: Vec<OwnedTag>,
}

impl MemberSelf {
    pub fn top_role_color_hex(&self) -> String {
        format!("#{:06x}", self.top_role_color)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemberEconomy {
    #[serde(default)]
    pub cash: i64,
    #[serde(default)]
    pub bank: i64,
    #[serde(default)]
    pub total: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserSettings {
    pub timezone: Option<String>,
    #[serde(default = "default_true")]
    pub track_presence: bool,
    #[serde(default = "default_true")]
    pub track_history: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct UserSettingsPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_presence: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_history: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AvatarEntry {
    pub image: String,
    pub changed_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AvatarHistoryResponse {
    pub avatars: Vec<AvatarEntry>,
    #[serde(default)]
    pub total: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PresenceEntry {
    pub status: Option<String>,
    pub status_before: Option<String>,
    pub changed_at: Option<String>,
}

/// Consent-tracked history for a user (names, avatars, presence) — the payload
/// behind the personal dashboard's "Your History" view.
#[derive(Debug, Deserialize, Serialize)]
pub struct UserHistory {
    #[serde(default)]
    pub usernames: Vec<NameHistoryEntry>,
    #[serde(default)]
    pub nicknames: Vec<NameHistoryEntry>,
    #[serde(default)]
    pub avatars: Vec<AvatarEntry>,
    #[serde(default)]
    pub presence: Vec<PresenceEntry>,
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
    #[serde(default)]
    pub total: u32,
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
    #[serde(default)]
    pub total: u32,
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

/// Full content of a single tag, fetched on demand for the markdown preview.
#[derive(Debug, Deserialize, Serialize)]
pub struct TagDetail {
    pub id: i64,
    pub name: String,
    pub content: String,
    pub owner_id: Option<String>,
    pub owner_name: Option<String>,
    #[serde(default)]
    pub uses: u32,
    pub created_at: Option<String>,
}

/// One `(name, content)` row in a tag export/import payload.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TagExportRow {
    pub name: String,
    pub content: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TagExport {
    pub tags: Vec<TagExportRow>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TagImportFailure {
    pub name: String,
    pub error: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TagImportResult {
    #[serde(default)]
    pub created: u32,
    #[serde(default)]
    pub skipped: u32,
    #[serde(default)]
    pub failed: Vec<TagImportFailure>,
}

/// A tag owned by a user, shown on their profile page.
#[derive(Debug, Deserialize, Serialize)]
pub struct OwnedTag {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub uses: u32,
}

// -- Commands types ----------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct CommandInfo {
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub cog: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub disabled_in: Vec<String>,
    #[serde(default)]
    pub globally_disabled: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PublicCommandsResponse {
    pub commands: Vec<CommandInfo>,
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
    #[serde(default)]
    pub version: Option<String>,
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

// -- Batch operations --------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct BatchOperation {
    #[serde(rename = "type")]
    pub op_type: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct BatchResponse {
    pub ok: bool,
    pub results: Vec<BatchResult>,
}

#[derive(Debug, Deserialize)]
pub struct BatchResult {
    #[serde(rename = "type")]
    pub op_type: String,
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
}

// -- Changelog ---------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct ChangelogResponse {
    pub entries: Vec<ChangelogEntry>,
    #[serde(default)]
    pub current_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChangelogEntry {
    pub version: String,
    pub date: String,
    pub changes: Vec<String>,
}

// -- Composite endpoint types ------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct GuildOverviewGuild {
    pub id: u64,
    pub name: String,
    pub icon_url: Option<String>,
    pub member_count: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GuildOverviewStats {
    pub online_count: u32,
    pub bot_count: u32,
    pub channel_count: u32,
    pub role_count: u32,
    pub emoji_count: u32,
    pub boost_count: u32,
    pub boost_tier: u32,
    pub total_commands: u64,
    #[serde(default)]
    pub recent_cases: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GuildOverviewBot {
    #[serde(default)]
    pub version: Option<String>,
    pub guild_count: u32,
    pub user_count: u32,
    pub command_count: u32,
    pub latency_ms: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GuildOverviewFeatures {
    #[serde(default)]
    pub leveling: bool,
    #[serde(default)]
    pub economy: bool,
    #[serde(default)]
    pub music: bool,
    #[serde(default)]
    pub sentinel: bool,
    #[serde(default)]
    pub audit_log: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GuildOverview {
    pub guild: GuildOverviewGuild,
    pub stats: GuildOverviewStats,
    pub bot: GuildOverviewBot,
    pub features: GuildOverviewFeatures,
}

// -- Autoresponders types ----------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct AutoresponderEntry {
    pub id: u64,
    pub trigger: String,
    pub response: String,
    pub match_type: String,
    pub ignore_case: bool,
    pub enabled: bool,
    pub uses: u64,
    pub created_by: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AutorespondersResponse {
    pub entries: Vec<AutoresponderEntry>,
    pub total: u32,
}

// -- Economy types -----------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct ShopItem {
    pub id: u64,
    pub name: String,
    pub description: Option<String>,
    pub price: u64,
    #[serde(default)]
    pub effect: Option<String>,
    #[serde(default)]
    pub effect_value: Option<i64>,
    #[serde(default)]
    pub duration_minutes: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LotteryInfo {
    pub ticket_price: u64,
    pub jackpot: u64,
    pub channel_id: String,
    pub ends_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EconomyInfo {
    pub items: Vec<ShopItem>,
    pub lottery: Option<LotteryInfo>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BalanceEntry {
    pub user_id: String,
    pub username: String,
    pub avatar_url: Option<String>,
    pub cash: i64,
    pub bank: i64,
    pub total: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BalancesResponse {
    pub entries: Vec<BalanceEntry>,
    #[serde(default)]
    pub total: u32,
}

// -- Music types -------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TrackLink {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TrackRequester {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NowPlaying {
    pub title: String,
    pub author: String,
    pub duration: u64,
    pub position: u64,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub artwork: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub is_stream: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub volume: u32,
    /// 0 = off, 1 = loop track, 2 = loop queue.
    /// Percy sends/expects the JSON key `loop`; rename so it both deserialises
    /// from Percy and re-serialises as `loop` for the dashboard player JS.
    #[serde(default, rename = "loop")]
    pub loop_mode: u8,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default)]
    pub recommended: bool,
    #[serde(default)]
    pub album: Option<TrackLink>,
    #[serde(default)]
    pub playlist: Option<TrackLink>,
    #[serde(default)]
    pub artist_url: Option<String>,
    #[serde(default)]
    pub requester: Option<TrackRequester>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QueueTrack {
    pub title: String,
    pub author: String,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub artwork: Option<String>,
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub is_stream: bool,
    #[serde(default)]
    pub requester: Option<TrackRequester>,
    /// True for wavelink autoplay recommendations queued after the manual queue.
    #[serde(default)]
    pub autoplay: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LyricLine {
    pub time: u64,
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MusicLyrics {
    #[serde(default)]
    pub has_synced: bool,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub lines: Vec<LyricLine>,
    #[serde(default)]
    pub plain: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MusicFiltersState {
    #[serde(default)]
    pub nightcore: bool,
    #[serde(rename = "8d", default)]
    pub eight_d: bool,
    #[serde(default)]
    pub lowpass: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MusicSetup {
    /// The dedicated panel channel, if one is configured. `None` means the panel
    /// (when enabled) is created temporarily where playback starts.
    #[serde(default)]
    pub channel_id: Option<String>,
    pub message_id: Option<String>,
    pub use_panel: bool,
    #[serde(default)]
    pub dj_mode: u8,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct AlwaysOnState {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MusicInfo {
    pub active: bool,
    pub equalizer: Vec<f64>,
    pub filters: MusicFiltersState,
    pub presets: Vec<String>,
    #[serde(default)]
    pub now_playing: Option<NowPlaying>,
    #[serde(default)]
    pub queue: Vec<QueueTrack>,
    /// Recently played tracks (most-recent-first) for the History tab.
    #[serde(default)]
    pub history: Vec<QueueTrack>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub channel_name: Option<String>,
    #[serde(default)]
    pub setup: Option<MusicSetup>,
    #[serde(default)]
    pub always_on: AlwaysOnState,
    /// Discord IDs of the (non-bot) members currently sharing the bot's voice
    /// channel. The public overview uses this to decide whether a viewer may
    /// control playback; Percy re-verifies on every control request.
    #[serde(default)]
    pub listeners: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct MusicSetupResponse {
    pub ok: bool,
    pub channel_id: String,
    pub channel_name: String,
}

// -- Comics types ------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct ComicFeedEntry {
    pub id: u64,
    pub brand: String,
    pub channel_id: String,
    pub format: String,
    pub day: u8,
    pub ping: Option<String>,
    pub pin: bool,
    pub next_pull: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ComicsResponse {
    pub feeds: Vec<ComicFeedEntry>,
}

// -- Temp Channels types -----------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct ActiveTempChannel {
    pub channel_id: String,
    pub channel_name: String,
    pub user_count: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TempChannelEntry {
    pub channel_id: String,
    pub channel_name: String,
    pub format: String,
    /// Live spawned channels for this hub (resolved by shared category). Empty
    /// when nobody is using the hub. `#[serde(default)]` keeps it backward-safe.
    #[serde(default)]
    pub active_channels: Vec<ActiveTempChannel>,
    #[serde(default)]
    pub total_users: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TempChannelsResponse {
    pub entries: Vec<TempChannelEntry>,
}

// -- Status Feed types -------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct StatusFeedInfo {
    pub subscribed: bool,
    pub channel: Option<ChannelRef>,
}

// -- Lockdowns types ---------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct LockdownEntry {
    pub channel_id: String,
    pub channel_name: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LockdownsResponse {
    pub entries: Vec<LockdownEntry>,
}

// -- Highlights types --------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct HighlightEntry {
    pub user_id: String,
    pub username: String,
    pub triggers: Vec<String>,
    pub blocked_count: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HighlightsResponse {
    pub entries: Vec<HighlightEntry>,
    #[serde(default)]
    pub total: u32,
}

// -- Emoji Stats types -------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct EmojiStatEntry {
    pub emoji_id: String,
    pub emoji_name: String,
    pub emoji_url: Option<String>,
    pub total: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct EmojiStatsResponse {
    pub total_uses: u64,
    pub distinct_emojis: u64,
    pub entries: Vec<EmojiStatEntry>,
    #[serde(default)]
    pub total: u32,
}

// -- Audit log (cases) types -------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CaseEntry {
    pub case_index: i64,
    pub action: String,
    pub target_id: String,
    pub target_name: String,
    pub moderator_id: Option<String>,
    pub moderator_name: Option<String>,
    pub reason: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CasesResponse {
    pub cases: Vec<CaseEntry>,
    pub total: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RecentCasesResponse {
    pub cases: Vec<CaseEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CaseActionResponse {
    pub ok: bool,
    pub case: CaseEntry,
}

// -- Activity heatmap types --------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct ActivityDay {
    pub day: String,
    pub count: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ActivityResponse {
    pub activity: Vec<ActivityDay>,
    pub days: u32,
}

// -- Bulk action types -------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct BulkActionFailure {
    pub user_id: String,
    pub error: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BulkActionResponse {
    pub ok: bool,
    pub successes: u32,
    pub failures: Vec<BulkActionFailure>,
}

// -- Custom Bot Profile types ------------------------------------------------

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct CustomBotProfile {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub banner_url: Option<String>,
    #[serde(default)]
    pub about_me: Option<String>,
    #[serde(default)]
    pub accent_color: Option<String>,
}
