use crate::models::ImageFile;
use crate::{
    auth::hash_password,
    boxed_params,
    cached::TimedCachedValue,
    database::Table,
    logging::RequestLogger,
    models::{Account, ImageEntry, Session},
    token::MAX_TOKEN_AGE,
    Config, Database,
};
use quick_cache::sync::Cache;
use std::{sync::Arc, time::Duration};
use tokio::sync::RwLockReadGuard;

/// A processed media result kept around for a shareable short link.
#[derive(Debug, Clone)]
pub struct ProcessedMedia {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

/// Username of the dedicated, non-personal account that owns every per-guild
/// image-gallery key. Auto-created on first provision (see
/// [`AppState::ensure_gallery_service_account`]); it has no usable password.
const GALLERY_SERVICE_ACCOUNT: &str = "percy-service";

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct SessionInfo {
    pub id: i64,
    pub api_key: bool,
    pub created_at: time::OffsetDateTime,
    /// Comma-separated granted scopes for API keys (empty = legacy full
    /// access / browser session). Carried so API-header auth can enforce
    /// per-token permissions without a second DB lookup.
    pub scopes: String,
}

impl From<Session> for SessionInfo {
    fn from(value: Session) -> Self {
        Self {
            id: value.account_id,
            api_key: value.api_key,
            created_at: value.created_at,
            scopes: value.scopes,
        }
    }
}

impl SessionInfo {
    /// Returns `true` if the session is expired
    pub fn is_expired(&self) -> bool {
        !self.api_key && time::OffsetDateTime::now_utc() > (self.created_at + MAX_TOKEN_AGE)
    }
}

/// A cached, downscaled gallery thumbnail. `bytes` is reference-counted so the
/// cache and every in-flight response share one allocation.
#[derive(Clone)]
pub struct Thumbnail {
    pub bytes: Arc<Vec<u8>>,
    pub content_type: &'static str,
}

struct InnerState {
    config: Config,
    database: Database,
    cached_images: TimedCachedValue<Vec<ImageEntry>>,
    cached_image_files: TimedCachedValue<Vec<ImageFile>>,
    cached_users: Cache<i64, Account>,
    valid_sessions: Cache<String, SessionInfo>,
    /// Bounded LRU of processed media (from /api/convert, /api/image/:op) that
    /// callers asked to share via a short link. Capacity-bounded rather than
    /// TTL'd — old entries are evicted as new ones arrive.
    processed_media: Cache<String, ProcessedMedia>,
    /// Bounded LRU of generated gallery thumbnails, keyed by image id. Image
    /// bytes are immutable per id (ids are random and never reused), so entries
    /// never go stale; they're simply evicted under capacity pressure.
    thumbnails: Cache<String, Thumbnail>,
}

/// Global application state for the axum Router.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<InnerState>,
    pub client: reqwest::Client,
    pub requests: RequestLogger,
    pub incorrect_default_password_hash: String,
}

impl AppState {
    pub async fn new(config: Config, database: Database) -> Self {
        let incorrect_default_password_hash =
            hash_password("incorrect-default-password").expect("could not hash default password");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .expect("could not build HTTP client");

        let requests = RequestLogger::new().expect("could not build request logger");

        Self {
            inner: Arc::new(InnerState {
                config,
                database,
                cached_images: TimedCachedValue::new(Duration::from_secs(60 * 30)),
                cached_image_files: TimedCachedValue::new(Duration::from_secs(60 * 30)),
                cached_users: Cache::new(1000),
                valid_sessions: Cache::new(1000),
                processed_media: Cache::new(512),
                thumbnails: Cache::new(1024),
            }),
            client,
            requests,
            incorrect_default_password_hash,
        }
    }

    /// An `AppState` for tests, over the `database` you hand it.
    ///
    /// [`AppState::new`] is unusable from a test: it probes the filesystem for a
    /// GeoIP database, *writes back to `config.json`*, opens `requests.db`, dials
    /// the Docker socket and shells out to detect a firewall backend. This does
    /// none of that — every optional integration is off, the request logger
    /// discards, and the config is a throwaway with a random secret key.
    ///
    /// Pair it with an in-memory database, which must be single-connection: each
    /// `:memory:` connection is its *own* database, so a pool of 10 would hand
    /// each query a different empty one.
    ///
    /// ```ignore
    /// let db = Database::file(":memory:")
    ///     .connections(1)
    ///     .with_init(crate::migrations::migrate)
    ///     .open()
    ///     .await?;
    /// let state = AppState::for_tests(db).await;
    /// ```
    #[cfg(test)]
    pub async fn for_tests(database: Database) -> Self {
        // `Config` has no `Default` (its `secret_key` is required), but every
        // other field is `#[serde(default)]` — so the key is the whole config.
        let secret_key = crate::key::SecretKey::random().expect("random secret key");
        let config: Config = serde_json::from_value(serde_json::json!({ "secret_key": secret_key.hex() }))
            .expect("a config from just a secret key");
        Self {
            inner: Arc::new(InnerState {
                config,
                database,
                cached_images: TimedCachedValue::new(Duration::from_secs(60 * 30)),
                cached_image_files: TimedCachedValue::new(Duration::from_secs(60 * 30)),
                cached_users: Cache::new(1000),
                valid_sessions: Cache::new(1000),
                processed_media: Cache::new(512),
                thumbnails: Cache::new(1024),
            }),
            client: reqwest::Client::new(),
            requests: RequestLogger::null(),
            incorrect_default_password_hash: hash_password("incorrect-default-password")
                .expect("could not hash default password"),
        }
    }

    /// Stores a processed media blob for sharing and returns its short id.
    /// The id is URL-safe and used by the public `/m/:id` view route.
    pub fn store_media(&self, bytes: Vec<u8>, content_type: impl Into<String>) -> String {
        let id = nanoid::nanoid!(12);
        self.inner.processed_media.insert(
            id.clone(),
            ProcessedMedia {
                bytes,
                content_type: content_type.into(),
            },
        );
        id
    }

    /// Looks up a previously shared media blob by id.
    pub fn get_media(&self, id: &str) -> Option<ProcessedMedia> {
        self.inner.processed_media.get(id)
    }

    /// Start an audit-log entry. Call `.actor(…).target(…).ip_opt(…).fire()`
    /// to record it (fire-and-forget — the response is never delayed).
    pub fn audit(&self, action: &'static str) -> crate::audit::AuditBuilder<'_> {
        crate::audit::AuditBuilder::new(self, action)
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn database(&self) -> &Database {
        &self.inner.database
    }

    /// Sends an alert to the configured Discord webhook (if any).
    /// Delivery happens in the background — failures are not surfaced.
    pub fn send_alert<T: serde::Serialize + Send + 'static>(&self, payload: T) {
        let Some(wh) = self.config().alerts.discord_webhook_url.clone() else {
            return;
        };
        let Ok(value) = serde_json::to_value(&payload) else {
            return;
        };
        let client = self.client.clone();
        tokio::spawn(async move { wh.prepare(value).send(&client).await });
    }

    pub fn cached_images(&self) -> &TimedCachedValue<Vec<ImageEntry>> {
        &self.inner.cached_images
    }

    pub fn cached_image_files(&self) -> &TimedCachedValue<Vec<ImageFile>> {
        &self.inner.cached_image_files
    }

    pub async fn invalidate_image_caches(&self) {
        self.inner.cached_images.invalidate().await;
        self.inner.cached_image_files.invalidate().await;
    }

    pub async fn get_account(&self, id: i64) -> Option<Account> {
        match self.inner.cached_users.get_value_or_guard_async(&id).await {
            Ok(acc) => Some(acc),
            Err(guard) => {
                // LEFT JOIN the Discord link so the cached account carries the
                // linked Discord id (used by the site header, which resolves the
                // avatar via GET /account/discord/avatar from the stored hash).
                // Keep this in sync with the inline query in `get_session_account`
                // so both cache writers store the same shape.
                let query = r#"
                    SELECT account.id AS id, account.name AS name, account.password AS password,
                           account.flags AS flags, account.created_at AS created_at,
                           account.totp_secret AS totp_secret,
                           account.totp_enabled AS totp_enabled,
                           dl.discord_user_id AS discord_id
                    FROM account
                    LEFT JOIN user_discord_links dl ON dl.account_id = account.id
                    WHERE account.id = ?
                "#;
                match self
                    .database()
                    .get_row(query, (id,), |row| Account::from_row(row))
                    .await
                    .ok()
                {
                    Some(account) => {
                        let _ = guard.insert(account.clone());
                        Some(account)
                    }
                    None => None,
                }
            }
        }
    }

    pub fn invalidate_account_cache(&self, id: i64) {
        self.inner.cached_users.remove(&id);
    }

    pub fn clear_account_cache(&self) {
        self.inner.cached_users.clear();
    }

    pub fn clear_session_cache(&self) {
        self.inner.valid_sessions.clear();
    }

    /// Returns if the session is valid (i.e. in the database or cache).
    pub async fn is_session_valid(&self, session: &str) -> Option<SessionInfo> {
        match self.inner.valid_sessions.get_value_or_guard_async(session).await {
            Ok(info) => {
                if info.is_expired() {
                    self.invalidate_session(session).await;
                    None
                } else {
                    Some(info)
                }
            }
            Err(guard) => match self
                .database()
                .get_by_id::<Session>(session.to_owned())
                .await
                .ok()
                .flatten()
            {
                Some(info) => {
                    let info = SessionInfo::from(info);
                    if info.is_expired() {
                        self.invalidate_session(session).await;
                        None
                    } else {
                        let _ = guard.insert(info.clone());
                        Some(info)
                    }
                }
                None => None,
            },
        }
    }

    /// Returns the account associated with the session and account ID if they're valid.
    ///
    /// This is merely an optimisation to avoid doing multiple database lookups.
    pub async fn get_session_account(&self, session: &str, id: i64, api_key: bool) -> Option<Account> {
        match self.inner.valid_sessions.get_value_or_guard_async(session).await {
            Ok(info) => {
                if info.is_expired() {
                    self.invalidate_session(session).await;
                    return None;
                }
                let account = self.get_account(info.id).await;
                if account.is_none() {
                    self.inner.valid_sessions.remove(session);
                }
                account
            }
            Err(guard) => {
                // `created_at` is the *account's* (Account::from_row reads that name);
                // the session's own timestamp is aliased apart so the two don't collide.
                let query = r#"
                    SELECT account.id AS id, account.name AS name, account.password AS password,
                           account.flags AS flags, account.created_at AS created_at,
                           account.totp_secret AS totp_secret,
                           account.totp_enabled AS totp_enabled,
                           dl.discord_user_id AS discord_id,
                           session.api_key AS api_key, session.created_at AS session_created_at
                    FROM account INNER JOIN session ON session.account_id = account.id
                    LEFT JOIN user_discord_links dl ON dl.account_id = account.id
                    WHERE session.id = ? AND session.account_id = ? AND session.api_key = ?
                "#;
                match self
                    .database()
                    .get_row(
                        query,
                        (session.to_owned(), id, api_key),
                        |row| -> rusqlite::Result<(Account, SessionInfo)> {
                            let account = Account::from_row(row)?;
                            let info = SessionInfo {
                                id: account.id,
                                api_key: row.get("api_key")?,
                                created_at: row.get("session_created_at")?,
                                // Browser-session path: scopes are irrelevant
                                // (cookie sessions bypass scope checks).
                                scopes: String::new(),
                            };
                            Ok((account, info))
                        },
                    )
                    .await
                    .ok()
                {
                    Some((account, info)) => {
                        if info.is_expired() {
                            self.invalidate_session(session).await;
                            None
                        } else {
                            let _ = guard.insert(info);
                            self.inner.cached_users.insert(account.id, account.clone());
                            Some(account)
                        }
                    }
                    None => None,
                }
            }
        }
    }

    /// Invalidate the given session
    ///
    /// This can invalidate API tokens as well.
    pub async fn invalidate_session(&self, session: &str) -> bool {
        self.inner.valid_sessions.remove(session);
        self.database()
            .execute("DELETE FROM session WHERE id = ?", (session.to_owned(),))
            .await
            .is_ok()
    }

    /// Saves the session given by the token to the database
    pub async fn save_session(&self, token: &crate::token::Token, description: Option<String>) {
        let query =
            "INSERT INTO session(id, account_id, description, api_key) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING";
        let _ = self
            .database()
            .execute(query, (token.base64(), token.id, description, token.api_key))
            .await;
    }

    pub async fn invalidate_api_keys(&self, id: i64) {
        let sessions: Vec<Session> = self
            .database()
            .all(
                "DELETE FROM session WHERE account_id = ? AND api_key != 0 RETURNING *",
                [id],
            )
            .await
            .unwrap_or_default();

        for session in sessions {
            self.inner.valid_sessions.remove(&session.id);
        }
    }

    pub async fn generate_api_key(&self, id: i64, scopes: &[crate::models::Scope]) -> anyhow::Result<String> {
        let mut token = crate::token::Token::new(id)?;
        token.api_key = true;
        let key = token.base64();
        // Persist scopes as comma-separated scope string. Empty string
        // (no scopes selected) is treated as legacy/full at lookup time
        // — see Session::has_scope.
        let scopes_str = scopes.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",");
        self.database()
            .execute(
                "INSERT INTO session(id, account_id, description, api_key, scopes)
                 VALUES (?, ?, 'API Key', 1, ?)",
                (key.clone(), id, scopes_str),
            )
            .await?;
        Ok(key)
    }

    pub async fn get_api_key(&self, id: i64) -> Option<String> {
        self.database()
            .get_row(
                "SELECT id FROM session WHERE account_id = ? AND api_key = 1",
                [id],
                |row| row.get("id"),
            )
            .await
            .ok()
    }

    /// Get-or-create the dedicated, non-personal service account that owns every
    /// per-guild image-gallery key, returning its id.
    ///
    /// The account has a sentinel password that can never verify against Argon2,
    /// so it can't be logged into — it exists solely to be the uploader/audit
    /// actor for guild gallery keys. This keeps the bot from ever needing a
    /// personal/all-access key.
    pub async fn ensure_gallery_service_account(&self) -> anyhow::Result<i64> {
        if let Ok(id) = self
            .database()
            .get_row(
                "SELECT id FROM account WHERE name = ?",
                (GALLERY_SERVICE_ACCOUNT,),
                |row| row.get::<_, i64>(0),
            )
            .await
        {
            return Ok(id);
        }

        let id = self
            .database()
            .call(|conn| {
                // ON CONFLICT DO NOTHING makes this safe under a race (two guilds
                // provisioning at once); we re-select the id unconditionally after.
                conn.execute(
                    "INSERT INTO account(name, password) VALUES (?, ?) ON CONFLICT(name) DO NOTHING",
                    rusqlite::params![GALLERY_SERVICE_ACCOUNT, "!percy-service:no-login!"],
                )?;
                conn.query_row(
                    "SELECT id FROM account WHERE name = ?",
                    rusqlite::params![GALLERY_SERVICE_ACCOUNT],
                    |row| row.get::<_, i64>(0),
                )
            })
            .await?;
        Ok(id)
    }

    /// Get-or-create the `images:guild`-scoped API key for a Discord guild.
    ///
    /// Returns the stored key when it's still a live session (so a revoked key —
    /// its `session` row deleted — is transparently replaced), otherwise mints a
    /// fresh one under the service account and persists the guild → key mapping in
    /// `guild_api_key`. This is what lets the bot provision a narrow, per-guild
    /// key on demand instead of holding a personal key.
    pub async fn ensure_guild_api_key(&self, guild_id: &str) -> anyhow::Result<String> {
        let gid = guild_id.to_string();

        if let Ok(token) = self
            .database()
            .get_row(
                "SELECT gak.token FROM guild_api_key gak \
                 JOIN session s ON s.id = gak.token \
                 WHERE gak.guild_id = ?",
                (gid.clone(),),
                |row| row.get::<_, String>(0),
            )
            .await
        {
            return Ok(token);
        }

        let account_id = self.ensure_gallery_service_account().await?;
        let token = self
            .generate_api_key(account_id, &[crate::models::Scope::GuildImages])
            .await?;

        let stored = token.clone();
        self.database()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO guild_api_key(guild_id, token, account_id) VALUES (?, ?, ?) \
                     ON CONFLICT(guild_id) DO UPDATE SET \
                         token = excluded.token, account_id = excluded.account_id, \
                         created_at = CURRENT_TIMESTAMP",
                    rusqlite::params![gid, stored, account_id],
                )?;
                Ok::<_, rusqlite::Error>(())
            })
            .await?;

        Ok(token)
    }

    /// Invalidate all sessions used by the account.
    ///
    /// This does *not* invalidate API tokens.
    pub async fn invalidate_account_sessions(&self, id: i64) {
        let sessions: Vec<Session> = self
            .database()
            .all(
                "DELETE FROM session WHERE account_id = ? AND api_key = 0 RETURNING *",
                [id],
            )
            .await
            .unwrap_or_default();

        for session in sessions {
            self.inner.valid_sessions.remove(&session.id);
        }
    }

    pub async fn resolve_images(&self) -> RwLockReadGuard<'_, Vec<ImageEntry>> {
        let reader = self.inner.cached_images.get().await;
        if let Some(lock) = reader {
            return lock;
        }

        // Cache miss
        let files: Vec<ImageEntry> = self
            .database()
            .all(
                "SELECT id, size, X'' AS image_data, mimetype, uploader_id, uploaded_at, expires_at, original_name, views FROM images ORDER BY id ASC",
                [],
            )
            .await
            .unwrap();

        let mut image_files = Vec::new();
        for file in files.clone() {
            let entry = file;
            let filename = format!("{}.{}", entry.id, entry.ext());
            let url = format!("/gallery/{filename}");
            image_files.push(ImageFile {
                url,
                id: filename,
                mimetype: entry.mimetype,
                size: entry.size,
                image_data: entry.image_data,
                uploaded_at: entry.uploaded_at,
                uploader_id: entry.uploader_id,
                expires_at: entry.expires_at,
                original_name: entry.original_name,
                views: entry.views,
            });
        }

        let _ = self.inner.cached_image_files.set(image_files).await;
        self.inner.cached_images.set(files).await
    }

    /// Atomically bumps an image's view counter and returns the new total.
    ///
    /// Best-effort: a database error yields `None` and the caller falls back to
    /// the (possibly stale) cached count. The metadata cache is intentionally
    /// *not* invalidated here — view counts are approximate and invalidating on
    /// every view would reload every image from disk constantly. The gallery's
    /// cached count catches up on the next natural invalidation.
    pub async fn increment_image_views(&self, id: &str) -> Option<i64> {
        let id = id.to_string();
        self.database()
            .get_row(
                "UPDATE images SET views = views + 1 WHERE id = ? RETURNING views",
                boxed_params![id],
                |row| row.get::<_, i64>(0),
            )
            .await
            .ok()
    }

    /// Returns a cached thumbnail for `id`, generating it from `bytes` on a
    /// cache miss. Returns `None` when the bytes can't be decoded (e.g. AVIF),
    /// signalling the caller to fall back to the original image.
    ///
    /// Generation (decode + resize + encode) runs on the blocking pool so it
    /// never stalls the async runtime.
    pub async fn thumbnail_for(&self, id: &str, bytes: &[u8]) -> Option<Thumbnail> {
        if let Some(thumb) = self.inner.thumbnails.get(id) {
            return Some(thumb);
        }
        let owned = bytes.to_vec();
        let (data, content_type) = tokio::task::spawn_blocking(move || crate::thumbnail::generate(&owned))
            .await
            .ok()
            .flatten()?;
        let thumb = Thumbnail {
            bytes: Arc::new(data),
            content_type,
        };
        self.inner.thumbnails.insert(id.to_string(), thumb.clone());
        Some(thumb)
    }

    pub async fn resolve_image_data_for(&self, id: &str) -> Option<Vec<u8>> {
        let id = id.to_string(); // owned

        self.database()
            .get_row(
                "SELECT image_data FROM images WHERE id = ?",
                boxed_params![id], // <--- use boxed_params! to satisfy Send + 'static
                |row| row.get::<_, Vec<u8>>(0),
            )
            .await
            .ok()
    }

    pub async fn resolve_image_files(&self) -> RwLockReadGuard<'_, Vec<ImageFile>> {
        if let Some(lock) = self.inner.cached_image_files.get().await {
            tracing::debug!("Cache hit for image files");
            return lock;
        }
        tracing::debug!("Cache miss, reloading images from DB");

        // Cache miss
        let _ = self.resolve_images().await;
        self.inner.cached_image_files.get().await.unwrap()
    }

    /// Gets the image by the given ID.
    ///
    /// If not found in cache then it calls the database.
    /// This incurs the cost of one clone regardless of the case.
    ///
    /// All errors are coerced into None.
    pub async fn get_image(&self, id: String) -> Option<ImageEntry> {
        if let Some(guard) = self.cached_images().get().await {
            if let Some(found_ref) = guard.iter().find(|x| x.id == id) {
                // Clone the entry first
                let mut entry = found_ref.clone();

                // Now we can drop the lock safely
                drop(guard);

                // If image_data is missing, fetch it from DB
                if entry.image_data.is_empty() {
                    if let Some(data) = self.resolve_image_data_for(&id).await {
                        entry.image_data = data;
                        return Some(entry);
                    } else {
                        return None;
                    }
                }

                return Some(entry);
            }
        }

        self.database().get_by_id(id).await.ok().flatten()
    }
}
