-- Revises: 24
-- Reason: Drop the two Percy tables that the percy-dashboard now owns.
--
-- The dashboard is a standalone app on percy.<domain> with its own `dashboard.db`
-- (DASHBOARD_DECOUPLING_PLAN §6.1, Phase 6). Both of these are re-declared in its
-- `sql/0.sql` and are keyed by the Discord user/guild id — never by this site's
-- `account.id` — which is exactly why they could move without a rewrite.
--
-- Both are caches of Discord/Percy data that the dashboard refills on the user's
-- next login, so dropping the stale copies here loses nothing recoverable.
--
-- What deliberately does NOT move: `user_discord_links`. It backs *this* site's
-- Discord login and is FK-joined into session resolution (see 20.sql), so it
-- stays account-bound and stays here.

-- Guilds the signed-in user can manage, captured from the OAuth /users/@me/guilds
-- response (15.sql). The dashboard captures this at its own login now.
DROP TABLE IF EXISTS user_discord_admin_guilds;

-- Vanity slugs for public leaderboard pages (16.sql). Purely guild-keyed.
DROP TABLE IF EXISTS percy_leaderboard_vanity;
