//! Askama view templates and view-model row structs for the Percy dashboard.

use askama::Template;

use crate::{flash::Flashes, models::Account, percy::*};

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/guilds.html")]
pub(crate) struct GuildsTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    /// Servers the user can manage and Percy is in — full admin dashboard cards.
    pub(crate) managed: Vec<UserGuild>,
    /// Servers the user is in (but can't manage) where Percy is present —
    /// read-only public overview cards.
    pub(crate) public: Vec<UserGuild>,
    /// Servers the user can manage but Percy is *not* in yet — greyed "Add Percy"
    /// cards that link to the bot invite (captured from Discord OAuth).
    pub(crate) add_percy: Vec<AddPercyGuild>,
    /// Invite URL (no guild pre-selected) for the "Invite Percy" empty-state button.
    pub(crate) invite_url: String,
}

/// A greyed "Add Percy" card: a server the user administrates but Percy isn't in.
pub(crate) struct AddPercyGuild {
    pub(crate) name: String,
    pub(crate) icon_url: Option<String>,
    pub(crate) owner: bool,
    /// Bot invite URL with this guild pre-selected.
    pub(crate) invite_url: String,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/guild.html")]
pub(crate) struct GuildTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild: GuildInfo,
    pub(crate) channels: Vec<Channel>,
    pub(crate) roles: Vec<Role>,
    pub(crate) sentinel: Option<SentinelInfo>,
    pub(crate) lockdowns: LockdownsResponse,
    pub(crate) status_feed: StatusFeedInfo,
    pub(crate) bot_profile: CustomBotProfile,
    /// True when the guild loaded but a sub-resource (roles/channels) failed to
    /// fetch — the page renders, but with empty pickers, so saving could clobber
    /// config. Drives a warning banner instead of a silently hollow page.
    pub(crate) degraded: bool,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/no_discord.html")]
pub(crate) struct NoDiscordTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
}

/// Logged-out welcome/landing page shown at `/dashboard` to visitors who
/// haven't signed in yet. Introduces the bot and points to login/signup (which
/// redirect back to the dashboard afterwards).
#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/landing.html")]
pub(crate) struct PercyLandingTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
}

/// A public Percy legal document (Privacy Policy / Terms of Service). `content`
/// is pre-rendered, trusted HTML produced from the canonical GitHub markdown.
#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/legal.html")]
pub(crate) struct PercyLegalTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) title: &'static str,
    pub(crate) content: String,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/guild_not_found.html")]
pub(crate) struct GuildNotFoundTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) invite_url: String,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/members.html")]
pub(crate) struct MembersTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) members: Vec<Member>,
    pub(crate) roles: Vec<Role>,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/leveling.html")]
pub(crate) struct LevelingTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) config: LevelingConfig,
    pub(crate) leaderboard: LeaderboardResponse,
    pub(crate) roles: Vec<Role>,
    pub(crate) text_channels: Vec<Channel>,
    pub(crate) all_channels: Vec<Channel>,
    pub(crate) level_roles: Vec<LevelRoleRow>,
    pub(crate) multiplier_roles: Vec<MultiplierRow>,
    pub(crate) multiplier_channels: Vec<MultiplierRow>,
    pub(crate) blacklisted_roles: Vec<BlacklistRow>,
    pub(crate) blacklisted_channels: Vec<BlacklistRow>,
    pub(crate) blacklisted_users: Vec<BlacklistRow>,
    pub(crate) special_messages: Vec<SpecialMsgRow>,
    /// Pre-serialized JSON array of `{day,total_xp,gainers}` for the uPlot chart.
    pub(crate) xp_history_json: String,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/user.html")]
pub(crate) struct UserLookupTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) member: MemberDetail,
}

/// Read-only public server overview shown to members who can't manage the guild.
/// Surfaces the same information a member could already see in Discord: server
/// stats, top leveling ranks, active polls/giveaways, the shop, the live music
/// player, and a short "powered by Percy" blurb.
#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/overview.html")]
pub(crate) struct OverviewTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) guild_icon: Option<String>,
    pub(crate) member_count: Option<u32>,
    pub(crate) stats: GuildStats,
    pub(crate) bot_stats: BotStats,
    pub(crate) music: MusicInfo,
    /// Whether the viewer currently shares the bot's voice channel (initial
    /// state; the live poll refreshes it). Gates the playback controls.
    pub(crate) can_control_music: bool,
    pub(crate) leaderboard: LeaderboardResponse,
    pub(crate) active_polls: Vec<PollInfo>,
    pub(crate) active_giveaways: Vec<GiveawayInfo>,
    pub(crate) economy: EconomyInfo,
}

/// Public leaderboard page — accessible by anyone, no login required.
#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/leaderboard.html")]
pub(crate) struct LeaderboardTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) guild_icon: Option<String>,
    pub(crate) leaderboard: LeaderboardResponse,
    pub(crate) balances: BalancesResponse,
    /// The vanity slug claimed for this guild, if any.
    pub(crate) vanity: Option<String>,
    /// Whether the current viewer can manage vanity settings (guild admin).
    pub(crate) can_manage: bool,
}

/// A resolved level-reward row (role granted at a level threshold).
pub(crate) struct LevelRoleRow {
    pub(crate) role_id: String,
    pub(crate) role_name: String,
    pub(crate) color_hex: String,
    pub(crate) level: i64,
}

/// A resolved XP-multiplier row for a role or channel.
pub(crate) struct MultiplierRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) multiplier: f64,
}

/// A resolved blacklist entry (role/channel/user excluded from XP).
pub(crate) struct BlacklistRow {
    pub(crate) id: String,
    pub(crate) name: String,
}

/// A custom level-up message keyed by level.
pub(crate) struct SpecialMsgRow {
    pub(crate) level: String,
    pub(crate) message: String,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/polls.html")]
pub(crate) struct PollsTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) guild: GuildInfo,
    pub(crate) channels: Vec<Channel>,
    pub(crate) roles: Vec<Role>,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) polls: PollsResponse,
    pub(crate) active_count: usize,
    pub(crate) ended_count: usize,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/giveaways.html")]
pub(crate) struct GiveawaysTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) giveaways: GiveawaysResponse,
    pub(crate) active_count: usize,
    pub(crate) ended_count: usize,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/tags.html")]
pub(crate) struct TagsTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) tags: TagsResponse,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/commands.html")]
pub(crate) struct CommandsTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) commands: CommandsResponse,
    pub(crate) channels: Vec<Channel>,
    pub(crate) disabled_count: usize,
    pub(crate) partial_count: usize,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/stats.html")]
pub(crate) struct StatsTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) stats: GuildStats,
    pub(crate) bot_stats: BotStats,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/autoresponders.html")]
pub(crate) struct AutorespondersTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) data: AutorespondersResponse,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/economy.html")]
pub(crate) struct EconomyTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) economy: EconomyInfo,
    pub(crate) balances: BalancesResponse,
    pub(crate) channels: Vec<Channel>,
    pub(crate) roles: Vec<Role>,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/music.html")]
pub(crate) struct MusicTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) music: MusicInfo,
    pub(crate) guild: GuildInfo,
    pub(crate) channels: Vec<Channel>,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/comics.html")]
pub(crate) struct ComicsTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) data: ComicsResponse,
    pub(crate) channels: Vec<Channel>,
    pub(crate) roles: Vec<Role>,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/temp_channels.html")]
pub(crate) struct TempChannelsTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) data: TempChannelsResponse,
    pub(crate) channels: Vec<Channel>,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/highlights.html")]
pub(crate) struct HighlightsTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) data: HighlightsResponse,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/emoji_stats.html")]
pub(crate) struct EmojiStatsTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) data: EmojiStatsResponse,
}

#[allow(dead_code)]
#[derive(Template)]
#[template(path = "percy/audit_log.html")]
pub(crate) struct AuditLogTemplate {
    pub(crate) account: Option<Account>,
    pub(crate) flashes: Flashes,
    pub(crate) guild_id: u64,
    pub(crate) guild_name: String,
    pub(crate) nav_active: &'static str,
    pub(crate) page_title: &'static str,
    pub(crate) cases: CasesResponse,
    pub(crate) filter_action: String,
    pub(crate) filter_moderator: String,
    pub(crate) filter_after: String,
    pub(crate) filter_before: String,
}
