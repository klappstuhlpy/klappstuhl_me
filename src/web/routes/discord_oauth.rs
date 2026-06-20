use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

use crate::{
    auth::hash_password,
    flash::Flasher,
    headers::ClientIp,
    models::{is_valid_username, Account},
    percy::PercyClient,
    ratelimit::RateLimit,
    token::Token,
    AppState,
};
use axum::{
    extract::{Query, State},
    http::{
        header::{CACHE_CONTROL, SET_COOKIE},
        HeaderValue, StatusCode,
    },
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use quick_cache::sync::Cache;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Signed state payload passed through the OAuth2 flow to prevent CSRF.
#[derive(Serialize, Deserialize)]
struct OAuthState {
    /// If the user is logged in when initiating, their account ID (linking mode).
    account_id: Option<i64>,
    /// Post-login redirect target carried from the login/signup page.
    #[serde(default)]
    next: Option<String>,
    /// Expiry as unix timestamp.
    exp: i64,
}

/// Query string carrying a post-auth redirect target (`?next=/some/path`).
#[derive(Deserialize)]
struct NextQuery {
    #[serde(default)]
    next: Option<String>,
}

/// Discord's token exchange response.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[allow(dead_code)]
    token_type: String,
}

/// Discord's /users/@me response (only the fields we need).
///
/// The avatar hash is intentionally *not* captured: the site resolves the
/// avatar live from the bot (`GET /account/discord/avatar`) so it never goes
/// stale, and nothing about the avatar is persisted.
#[derive(Deserialize)]
struct DiscordUser {
    id: String,
    username: String,
}

/// Query params on the OAuth2 callback.
#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// One entry of Discord's `/users/@me/guilds` response (only the fields we keep).
#[derive(Deserialize)]
struct DiscordPartialGuild {
    id: String,
    name: String,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    owner: bool,
    /// The user's permission bitfield in the guild. Discord returns this as a
    /// *string* on modern API versions but a *number* on older/unversioned ones,
    /// so we accept either — a type mismatch here would otherwise fail the whole
    /// array and silently store no guilds.
    #[serde(default, deserialize_with = "de_permissions")]
    permissions: u64,
}

/// Deserialize a permission bitfield from either a JSON string or number.
fn de_permissions<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(s) => s.parse::<u64>().map_err(Error::custom),
        serde_json::Value::Number(n) => Ok(n.as_u64().unwrap_or(0)),
        _ => Ok(0),
    }
}

/// Discord permission bits we treat as "can manage this server".
const PERM_ADMINISTRATOR: u64 = 1 << 3;
const PERM_MANAGE_GUILD: u64 = 1 << 5;

// -- Handlers ----------------------------------------------------------------

/// `GET /auth/discord` — Initiate Discord OAuth2 flow.
async fn discord_login(
    State(state): State<AppState>,
    account: Option<Account>,
    Query(query): Query<NextQuery>,
) -> Response {
    let config = state.config();
    let cfg = &config.discord;
    if !cfg.enabled() {
        return Redirect::to("/login").into_response();
    }

    let key = &state.config().secret_key;
    let oauth_state = OAuthState {
        account_id: account.map(|a| a.id),
        next: crate::utils::safe_next(query.next.as_deref()),
        exp: (OffsetDateTime::now_utc() + time::Duration::minutes(10)).unix_timestamp(),
    };
    let signed = match key.sign(&oauth_state) {
        Ok(s) => s,
        Err(_) => return Redirect::to("/login").into_response(),
    };

    let client_id = cfg.client_id.as_deref().unwrap();
    // In dev this resolves to http://localhost:<port>/auth/discord/callback so
    // local testing works without the public domain; in prod it's the configured
    // redirect_uri. Both must be registered in the Discord developer portal.
    let Some(redirect_uri) = config.discord_redirect_uri() else {
        return Redirect::to("/login").into_response();
    };
    let encoded_redirect: String = redirect_uri
        .bytes()
        .flat_map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![b as char]
            }
            _ => format!("%{b:02X}").chars().collect(),
        })
        .collect();
    let url = format!(
        "https://discord.com/api/oauth2/authorize\
         ?client_id={client_id}\
         &redirect_uri={encoded_redirect}\
         &response_type=code\
         &scope=identify%20guilds\
         &state={signed}",
    );
    Redirect::to(&url).into_response()
}

/// `GET /auth/discord/callback` — Handle OAuth2 callback from Discord.
async fn discord_callback(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    flasher: Flasher,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if let Some(error) = &query.error {
        tracing::warn!(error, "Discord OAuth2 denied by user");
        return flasher.add("Discord login was cancelled.").bail("/login");
    }

    let (Some(code), Some(state_param)) = (&query.code, &query.state) else {
        return flasher.add("Invalid OAuth2 callback parameters.").bail("/login");
    };

    let key = &state.config().secret_key;
    let oauth_state: OAuthState = match key.verify(state_param) {
        Some(s) => s,
        None => return flasher.add("Invalid or expired OAuth2 state.").bail("/login"),
    };

    if OffsetDateTime::now_utc().unix_timestamp() > oauth_state.exp {
        return flasher.add("OAuth2 session expired. Please try again.").bail("/login");
    }

    let config = state.config();
    let cfg = &config.discord;
    if !cfg.enabled() {
        return flasher.add("Discord login is not configured.").bail("/login");
    }

    // Must be byte-identical to the redirect_uri sent in the authorize step.
    let Some(redirect_uri) = config.discord_redirect_uri() else {
        return flasher.add("Discord login is not configured.").bail("/login");
    };

    // Exchange code for access token.
    let token_res = state
        .client
        .post("https://discord.com/api/oauth2/token")
        .form(&[
            ("client_id", cfg.client_id.as_deref().unwrap()),
            ("client_secret", cfg.client_secret.as_deref().unwrap()),
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send()
        .await;

    let token_resp: TokenResponse = match token_res {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, "Failed to parse Discord token response");
                return flasher.add("Failed to complete Discord login.").bail("/login");
            }
        },
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::error!(%status, %body, "Discord token exchange failed");
            return flasher.add("Failed to complete Discord login.").bail("/login");
        }
        Err(e) => {
            tracing::error!(error = %e, "Discord token exchange request failed");
            return flasher.add("Failed to connect to Discord.").bail("/login");
        }
    };

    // Fetch user identity.
    let user_res = state
        .client
        .get("https://discord.com/api/users/@me")
        .header("Authorization", format!("Bearer {}", token_resp.access_token))
        .send()
        .await;

    let discord_user: DiscordUser = match user_res {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(u) => u,
            Err(e) => {
                tracing::error!(error = %e, "Failed to parse Discord user response");
                return flasher.add("Failed to fetch Discord identity.").bail("/login");
            }
        },
        _ => {
            return flasher.add("Failed to fetch Discord identity.").bail("/login");
        }
    };

    // Capture the user's manageable guilds (scope `guilds` is always requested)
    // so the dashboard can offer to add Percy to servers it isn't in yet. Best
    // effort — a failure here must not block login.
    store_admin_guilds(&state, &token_resp.access_token, &discord_user.id).await;

    // Dispatch based on whether the user was logged in when they started the flow.
    if let Some(account_id) = oauth_state.account_id {
        link_discord(&state, &flasher, client_ip, account_id, &discord_user).await
    } else {
        let target = crate::utils::safe_next(oauth_state.next.as_deref()).unwrap_or_else(|| "/".to_string());
        login_or_create(&state, &flasher, client_ip, &discord_user, &target).await
    }
}

/// Link Discord to an existing (already logged-in) account.
async fn link_discord(
    state: &AppState,
    flasher: &Flasher,
    client_ip: Option<std::net::IpAddr>,
    account_id: i64,
    user: &DiscordUser,
) -> Response {
    let discord_id = user.id.clone();
    let username = user.username.clone();

    let result: rusqlite::Result<()> = state
        .database()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO user_discord_links (account_id, discord_user_id, discord_username) VALUES (?, ?, ?)",
                rusqlite::params![account_id, discord_id, username],
            )?;
            Ok(())
        })
        .await;

    match result {
        Ok(()) => {
            // The account may be cached without its Discord link; refresh it so
            // the header picks up the avatar immediately.
            state.invalidate_account_cache(account_id);
            state
                .audit("auth.discord.link")
                .actor_label(format!("id:{account_id}"))
                .target(&user.id)
                .ip_opt(client_ip)
                .fire();
            flasher.add("Discord account linked successfully.").bail("/account")
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("UNIQUE constraint") {
                flasher
                    .add("This Discord account is already linked to another user.")
                    .bail("/account")
            } else {
                tracing::error!(error = %e, "Failed to link Discord account");
                flasher.add("Failed to link Discord account.").bail("/account")
            }
        }
    }
}

/// Login via an existing Discord link, or create a new account.
async fn login_or_create(
    state: &AppState,
    flasher: &Flasher,
    client_ip: Option<std::net::IpAddr>,
    user: &DiscordUser,
    target: &str,
) -> Response {
    let discord_id = user.id.clone();

    // Check for existing link.
    let existing: Option<i64> = state
        .database()
        .call(move |conn| {
            let mut stmt =
                conn.prepare_cached("SELECT account_id FROM user_discord_links WHERE discord_user_id = ?")?;
            stmt.query_row([&discord_id], |row| row.get(0)).optional()
        })
        .await
        .ok()
        .flatten();

    if let Some(account_id) = existing {
        // Existing linked account — check TOTP, then create session.
        let account: Option<Account> = state
            .database()
            .get("SELECT * FROM account WHERE id = ?", [account_id])
            .await
            .ok()
            .flatten();

        let Some(account) = account else {
            return flasher.add("Linked account no longer exists.").bail("/login");
        };

        // Discord OAuth is treated as a sufficiently strong factor on its own:
        // verifying ownership of the linked Discord account stands in for the
        // password+TOTP path, so we skip the TOTP challenge here even when the
        // account has 2FA enabled for username/password logins.
        create_session_response(state, &account, client_ip, "auth.discord.login", target).await
    } else {
        // No existing link — create a new account.
        let username = sanitize_username(&user.username);
        let discord_id_for_insert = user.id.clone();
        let discord_username_for_insert = user.username.clone();

        // Generate a sentinel password hash that can never be matched.
        let mut random_bytes = [0u8; 64];
        if getrandom::getrandom(&mut random_bytes).is_err() {
            return flasher.add("Internal error during account creation.").bail("/login");
        }
        let sentinel_password = base64::Engine::encode(&base64::prelude::BASE64_URL_SAFE_NO_PAD, random_bytes);
        let password_hash = match hash_password(&sentinel_password) {
            Ok(h) => h,
            Err(_) => return flasher.add("Internal error during account creation.").bail("/login"),
        };

        let username_clone = username.clone();
        let tx_result: rusqlite::Result<i64> = state
            .database()
            .call(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "INSERT INTO account(name, password) VALUES (?, ?)",
                    rusqlite::params![username_clone, password_hash],
                )?;
                let new_id = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO user_discord_links (account_id, discord_user_id, discord_username) VALUES (?, ?, ?)",
                    rusqlite::params![new_id, discord_id_for_insert, discord_username_for_insert],
                )?;
                tx.commit()?;
                Ok(new_id)
            })
            .await;

        match tx_result {
            Ok(new_id) => {
                state
                    .audit("auth.discord.signup")
                    .actor_label(&username)
                    .target(&user.id)
                    .ip_opt(client_ip)
                    .fire();

                let account = Account {
                    id: new_id,
                    name: username,
                    password: String::new(),
                    flags: Default::default(),
                    totp_secret: None,
                    totp_enabled: false,
                    discord_id: Some(user.id.clone()),
                };
                create_session_response(state, &account, client_ip, "auth.discord.login", target).await
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("UNIQUE constraint failed: account.name") {
                    // Username collision — try with a random suffix.
                    let suffixed = append_random_suffix(&username);
                    let discord_id2 = user.id.clone();
                    let discord_username2 = user.username.clone();
                    let password_hash2 = {
                        let mut rb = [0u8; 64];
                        let _ = getrandom::getrandom(&mut rb);
                        let s = base64::Engine::encode(&base64::prelude::BASE64_URL_SAFE_NO_PAD, rb);
                        match hash_password(&s) {
                            Ok(h) => h,
                            Err(_) => return flasher.add("Internal error.").bail("/login"),
                        }
                    };
                    let suffixed_clone = suffixed.clone();
                    let retry: rusqlite::Result<i64> = state
                        .database()
                        .call(move |conn| {
                            let tx = conn.transaction()?;
                            tx.execute(
                                "INSERT INTO account(name, password) VALUES (?, ?)",
                                rusqlite::params![suffixed_clone, password_hash2],
                            )?;
                            let new_id = tx.last_insert_rowid();
                            tx.execute(
                                "INSERT INTO user_discord_links (account_id, discord_user_id, discord_username) VALUES (?, ?, ?)",
                                rusqlite::params![new_id, discord_id2, discord_username2],
                            )?;
                            tx.commit()?;
                            Ok(new_id)
                        })
                        .await;

                    match retry {
                        Ok(new_id) => {
                            state
                                .audit("auth.discord.signup")
                                .actor_label(&suffixed)
                                .target(&user.id)
                                .ip_opt(client_ip)
                                .fire();
                            let account = Account {
                                id: new_id,
                                name: suffixed,
                                password: String::new(),
                                flags: Default::default(),
                                totp_secret: None,
                                totp_enabled: false,
                                discord_id: Some(user.id.clone()),
                            };
                            create_session_response(state, &account, client_ip, "auth.discord.login", target).await
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to create Discord account (retry)");
                            flasher
                                .add("Failed to create account. Please try again.")
                                .bail("/login")
                        }
                    }
                } else if msg.contains("UNIQUE constraint failed: user_discord_links") {
                    flasher
                        .add("This Discord account is already linked to another user.")
                        .bail("/login")
                } else {
                    tracing::error!(error = %e, "Failed to create Discord account");
                    flasher.add("Failed to create account.").bail("/login")
                }
            }
        }
    }
}

/// `POST /account/discord/unlink` — Remove Discord link from the current account.
async fn discord_unlink(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    flasher: Flasher,
    account: Account,
) -> Response {
    let account_id = account.id;
    let result: rusqlite::Result<usize> = state
        .database()
        .call(move |conn| conn.execute("DELETE FROM user_discord_links WHERE account_id = ?", [account_id]))
        .await;

    match result {
        Ok(1..) => {
            state.invalidate_account_cache(account_id);
            state
                .audit("auth.discord.unlink")
                .actor(&account)
                .ip_opt(client_ip)
                .fire();
            flasher.add("Discord account unlinked.").bail("/account")
        }
        _ => flasher.add("No Discord account was linked.").bail("/account"),
    }
}

/// Fetches the user's guild list from Discord and replaces their stored set of
/// *manageable* guilds (admin or Manage Server). Drives the dashboard's "Add
/// Percy" section, which lists servers the user administrates that Percy is not
/// in yet. Entirely best-effort: any failure is logged and swallowed so it can
/// never block the login/link flow.
async fn store_admin_guilds(state: &AppState, access_token: &str, discord_id: &str) {
    let res = state
        .client
        .get("https://discord.com/api/v10/users/@me/guilds")
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .await;

    let guilds: Vec<DiscordPartialGuild> = match res {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(g) => g,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to parse Discord guilds response");
                return;
            }
        },
        Ok(resp) => {
            tracing::warn!(status = %resp.status(), "Discord guilds fetch returned non-success");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "Discord guilds fetch failed");
            return;
        }
    };

    // Keep only guilds the user can manage; that's all the "Add Percy" flow needs.
    let manageable: Vec<DiscordPartialGuild> = guilds
        .into_iter()
        .filter(|g| g.owner || g.permissions & (PERM_ADMINISTRATOR | PERM_MANAGE_GUILD) != 0)
        .collect();

    let discord_id = discord_id.to_string();
    let result: rusqlite::Result<()> = state
        .database()
        .call(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM user_discord_admin_guilds WHERE discord_user_id = ?",
                [&discord_id],
            )?;
            {
                let mut stmt = tx.prepare_cached(
                    "INSERT INTO user_discord_admin_guilds (discord_user_id, guild_id, name, icon, owner) \
                     VALUES (?, ?, ?, ?, ?)",
                )?;
                for g in &manageable {
                    stmt.execute(rusqlite::params![discord_id, g.id, g.name, g.icon, g.owner as i64])?;
                }
            }
            tx.commit()
        })
        .await;

    if let Err(e) = result {
        tracing::warn!(error = %e, "Failed to store user admin guilds");
    }
}

// -- Helpers -----------------------------------------------------------------

async fn create_session_response(
    state: &AppState,
    account: &Account,
    client_ip: Option<std::net::IpAddr>,
    audit_action: &'static str,
    target: &str,
) -> Response {
    let Ok(token) = Token::new(account.id) else {
        return Redirect::to("/login").into_response();
    };
    let key = &state.config().secret_key;
    let cookie = token.to_cookie(key);
    state.save_session(&token, Some("Discord OAuth".to_string())).await;
    state.audit(audit_action).actor(account).ip_opt(client_ip).fire();

    let mut response = Redirect::to(target).into_response();
    if let Ok(val) = HeaderValue::from_str(&cookie.to_string()) {
        response.headers_mut().insert(SET_COOKIE, val);
    }
    response
}

/// Sanitize a Discord username into a valid local username [a-z0-9._-], 3-32 chars.
fn sanitize_username(discord_name: &str) -> String {
    let sanitized: String = discord_name
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '.' || *c == '_' || *c == '-')
        .take(32)
        .collect();

    if sanitized.len() >= 3 && is_valid_username(&sanitized) {
        sanitized
    } else {
        // Pad short usernames or fall back to a generated name.
        let padded = format!("{sanitized:_<3}");
        if is_valid_username(&padded) {
            padded[..padded.len().min(32)].to_string()
        } else {
            let mut buf = [0u8; 4];
            let _ = getrandom::getrandom(&mut buf);
            format!("user-{}", u32::from_le_bytes(buf) % 100_000_000)
        }
    }
}

/// Append a random 4-digit suffix to handle username collisions.
fn append_random_suffix(base: &str) -> String {
    let mut buf = [0u8; 2];
    let _ = getrandom::getrandom(&mut buf);
    let suffix = u16::from_le_bytes(buf) % 10000;
    let candidate = format!("{base}-{suffix:04}");
    // Truncate to 32 chars if needed.
    candidate[..candidate.len().min(32)].to_string()
}

// -- Live Discord avatar -----------------------------------------------------

/// How long a resolved avatar URL is reused before re-checking the bot. Short
/// enough that a profile-picture change shows up quickly; long enough that the
/// header doesn't hit Percy on every page load.
const AVATAR_TTL: Duration = Duration::from_secs(600);

/// `discord_user_id -> (resolved_at, avatar_url)`.
fn avatar_cache() -> &'static Cache<String, (Instant, String)> {
    static CACHE: OnceLock<Cache<String, (Instant, String)>> = OnceLock::new();
    CACHE.get_or_init(|| Cache::new(2000))
}

/// Discord's default embed avatar for a user id, used when the bot can't be
/// reached or the user is unknown so the header always renders something.
fn default_discord_avatar(discord_id: &str) -> String {
    let idx = discord_id.parse::<u64>().map(|n| (n >> 22) % 6).unwrap_or(0);
    format!("https://cdn.discordapp.com/embed/avatars/{idx}.png")
}

fn avatar_redirect(url: &str) -> Response {
    let mut resp = Redirect::temporary(url).into_response();
    // Let the browser cache the image briefly too, matching the server-side TTL.
    resp.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("private, max-age=600"));
    resp
}

/// `GET /account/discord/avatar` — 302-redirects to the linked user's *current*
/// Discord avatar, resolved live from the bot and never persisted (so it can't go
/// stale). The site header points its `<img>` here.
async fn discord_avatar(State(state): State<AppState>, account: Account) -> Response {
    let Some(discord_id) = account.discord_id.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if let Some((ts, url)) = avatar_cache().get(&discord_id) {
        if ts.elapsed() < AVATAR_TTL {
            return avatar_redirect(&url);
        }
    }

    if let Some(percy) = PercyClient::new(state.percy_client.clone(), &state.config().percy) {
        if let Ok(avatar) = percy.get_user_avatar(&discord_id).await {
            avatar_cache().insert(discord_id, (Instant::now(), avatar.avatar_url.clone()));
            return avatar_redirect(&avatar.avatar_url);
        }
    }

    // Bot unreachable or unconfigured — fall back to the default avatar. Not
    // cached, so the next request retries the bot.
    avatar_redirect(&default_discord_avatar(&discord_id))
}

// -- Router ------------------------------------------------------------------

use rusqlite::OptionalExtension;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/discord", get(discord_login))
        .route(
            "/auth/discord/callback",
            get(discord_callback).layer(RateLimit::default().quota(10, 60.0).build()),
        )
        .route("/account/discord/unlink", post(discord_unlink))
        .route("/account/discord/avatar", get(discord_avatar))
}
