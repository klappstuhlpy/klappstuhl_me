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
}

impl MemberDetail {
    pub fn top_role_color_hex(&self) -> String {
        format!("#{:06x}", self.top_role_color)
    }
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
}

// -- Music types -------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct NowPlaying {
    pub title: String,
    pub author: String,
    pub duration: u64,
    pub position: u64,
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
    pub channel_id: String,
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
    pub channel: Option<String>,
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
pub struct TempChannelEntry {
    pub channel_id: String,
    pub channel_name: String,
    pub format: String,
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
