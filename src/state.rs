use quick_cache::sync::Cache;
use std::{sync::Arc, time::Duration};
use tokio::sync::{broadcast, RwLockReadGuard};
use crate::{auth::hash_password, boxed_params, cached::TimedCachedValue, cloudflare::Cloudflare, database::Table, docker::DockerClient, firewall::Backend as FirewallBackend, geoip::GeoIp, logging::RequestLogger, models::{Account, ImageEntry, Session}, token::MAX_TOKEN_AGE, Config, Database};
use crate::models::ImageFile;

/// One live event pushed over WebSocket subscribers.
///
/// `topic` is one of "metrics", "audit", "secrets" — clients say which
/// topics they care about when they connect, and the `/ws` handler
/// filters accordingly.  The `data` field is whatever JSON payload the
/// producer chose to ship (typically the same JSON the matching HTTP
/// endpoint would return).
#[derive(Debug, Clone, serde::Serialize)]
pub struct LiveEvent {
    pub topic: &'static str,
    pub data: serde_json::Value,
}

/// A processed media result kept around for a shareable short link.
#[derive(Debug, Clone)]
pub struct ProcessedMedia {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

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
    geoip: GeoIp,
    cloudflare: Option<Cloudflare>,
    /// Broadcast hub for live updates pushed to /ws subscribers. Each
    /// LiveEvent carries its own topic tag; the WS handler decides
    /// whether to forward it based on the client's subscriptions.
    /// 64 buffer slots — slow clients get RecvError::Lagged rather than
    /// blocking producers.
    live_tx: broadcast::Sender<LiveEvent>,
    /// Docker introspection client. `None` when Docker socket is unavailable.
    docker: Option<Arc<DockerClient>>,
    /// Firewall backend (nftables / ufw / iptables). `None` when none of
    /// the supported binaries is available at startup.
    firewall_backend: Option<FirewallBackend>,
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
    pub async fn new(mut config: Config, database: Database) -> Self {
        let incorrect_default_password_hash =
            hash_password("incorrect-default-password").expect("could not hash default password");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .expect("could not build HTTP client");

        let requests = RequestLogger::new().expect("could not build request logger");

        // Resolve a GeoIP database path. Tries (in order):
        //   1. `config.geoip_db_path`              (explicit override)
        //   2. `<data>/geoip/GeoLite2-City.mmdb`   (subdirectory layout — Docker)
        //   3. `<data>/GeoLite2-City.mmdb`         (flat layout — bare-metal Windows/macOS)
        //   4. `<config>/geoip/GeoLite2-City.mmdb` (subdirectory in config dir)
        //   5. `<config>/GeoLite2-City.mmdb`       (flat in config dir, in case the
        //                                          user dropped it next to config.json)
        // The first one that actually exists on disk wins. If none exist, we log
        // every path we tried so the user can see where to put the file.
        let geoip_was_unset = config.geoip_db_path.is_none();
        let geoip_path = {
            let mut candidates: Vec<std::path::PathBuf> = Vec::new();
            if let Some(p) = config.geoip_db_path.clone() {
                candidates.push(p);
            }
            if let Some(data_dir) = crate::database::directory()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_owned()))
            {
                candidates.push(data_dir.join("geoip").join("GeoLite2-City.mmdb"));
                candidates.push(data_dir.join("GeoLite2-City.mmdb"));
            }
            if let Some(config_dir) = crate::Config::path()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_owned()))
            {
                candidates.push(config_dir.join("geoip").join("GeoLite2-City.mmdb"));
                candidates.push(config_dir.join("GeoLite2-City.mmdb"));
            }

            let found = candidates.iter().find(|p| p.exists()).cloned();
            if found.is_none() {
                tracing::info!(
                    "GeoIP database not found. Checked: {}",
                    candidates
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            found
        };

        // Persist the discovered path back to config.json so subsequent
        // start-ups skip the candidate scan and log a clear, stable path.
        // We only persist when the user hadn't set one explicitly — that
        // way an explicit override is never silently overwritten.
        if geoip_was_unset {
            if let Some(p) = geoip_path.as_ref() {
                config.geoip_db_path = Some(p.clone());
                match config.save() {
                    Ok(()) => tracing::info!(
                        path = %p.display(),
                        "saved auto-detected GeoIP path to config"
                    ),
                    Err(e) => tracing::warn!(
                        error = %e,
                        "could not persist auto-detected GeoIP path to config"
                    ),
                }
            }
        }

        let geoip = GeoIp::open(geoip_path.as_deref());

        // Cloudflare client (only if both token and zone are configured).
        let cloudflare = match (
            config.cloudflare_api_token.as_ref(),
            config.cloudflare_zone_id.as_ref(),
        ) {
            (Some(token), Some(zone)) if !token.is_empty() && !zone.is_empty() => {
                Some(Cloudflare::new(client.clone(), token.clone(), zone.clone()))
            }
            _ => None,
        };

        let (live_tx, _) = broadcast::channel(64);
        let docker = DockerClient::connect();
        let firewall_backend = {
            let override_kind = config
                .firewall_backend
                .as_deref()
                .filter(|s| !s.is_empty());
            let backend = FirewallBackend::detect(override_kind).await;
            tracing::info!(backend = %backend.kind.label(), "firewall backend detected");
            if matches!(backend.kind, crate::firewall::BackendKind::Disabled) {
                None
            } else {
                Some(backend)
            }
        };

        Self {
            inner: Arc::new(InnerState {
                config,
                database,
                cached_images: TimedCachedValue::new(Duration::from_secs(60 * 30)),
                cached_image_files: TimedCachedValue::new(Duration::from_secs(60 * 30)),
                cached_users: Cache::new(1000),
                valid_sessions: Cache::new(1000),
                processed_media: Cache::new(512),
                geoip,
                cloudflare,
                live_tx,
                docker,
                firewall_backend,
            }),
            client,
            requests,
            incorrect_default_password_hash,
        }
    }

    pub fn geoip(&self) -> &GeoIp {
        &self.inner.geoip
    }

    pub fn cloudflare(&self) -> Option<&Cloudflare> {
        self.inner.cloudflare.as_ref()
    }

    /// Returns the Docker introspection client, if available.
    pub fn docker(&self) -> Option<&Arc<DockerClient>> {
        self.inner.docker.as_ref()
    }

    /// Stores a processed media blob for sharing and returns its short id.
    /// The id is URL-safe and used by the public `/m/:id` view route.
    pub fn store_media(&self, bytes: Vec<u8>, content_type: impl Into<String>) -> String {
        let id = nanoid::nanoid!(12);
        self.inner.processed_media.insert(
            id.clone(),
            ProcessedMedia { bytes, content_type: content_type.into() },
        );
        id
    }

    /// Looks up a previously shared media blob by id.
    pub fn get_media(&self, id: &str) -> Option<ProcessedMedia> {
        self.inner.processed_media.get(id)
    }

    /// Returns the configured firewall backend, if any.  `None` means
    /// neither `nft`, `ufw`, nor `iptables` was found at startup, in
    /// which case rule edits still persist to the DB but no kernel
    /// changes are applied.
    pub fn firewall_backend(&self) -> Option<&FirewallBackend> {
        self.inner.firewall_backend.as_ref()
    }

    /// Returns a fresh receiver for live events. Each WS connection
    /// subscribes once and filters by topic in user space.
    pub fn live_subscribe(&self) -> broadcast::Receiver<LiveEvent> {
        self.inner.live_tx.subscribe()
    }

    /// Publish a live event. Never fails — when nobody is subscribed,
    /// broadcast::Sender::send returns Err that we silently drop.
    pub fn live_publish(&self, topic: &'static str, data: serde_json::Value) {
        let _ = self.inner.live_tx.send(LiveEvent { topic, data });
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

    /// Returns true if at least one alert sink (Discord, ntfy, or generic
    /// webhook) is configured. Callers use this to skip building alert
    /// payloads when nothing would consume them.
    pub fn has_any_alert_sink(&self) -> bool {
        let cfg = self.config();
        cfg.webhook.is_some() || cfg.ntfy_url.is_some() || cfg.alert_webhook_url.is_some()
    }

    /// Fans an alert out to every configured sink (Discord, ntfy, generic
    /// webhook). The payload is the Discord webhook shape; a neutral
    /// notification is derived from it for the non-Discord sinks.
    ///
    /// All deliveries happen in the background — failures are not surfaced.
    pub fn send_alert<T: serde::Serialize + Send + 'static>(&self, payload: T) {
        let Ok(value) = serde_json::to_value(&payload) else {
            return;
        };
        let cfg = self.config();

        if let Some(wh) = cfg.webhook.clone() {
            let client = self.client.clone();
            let v = value.clone();
            tokio::spawn(async move { wh.prepare(v).send(&client).await });
        }

        if cfg.ntfy_url.is_none() && cfg.alert_webhook_url.is_none() {
            return;
        }
        let note = crate::alerts::AlertNotification::from_discord_value(&value);
        if let Some(url) = cfg.ntfy_url.clone() {
            let client = self.client.clone();
            let note = note.clone();
            tokio::spawn(async move { crate::alerts::send_ntfy(&client, &url, &note).await });
        }
        if let Some(url) = cfg.alert_webhook_url.clone() {
            let client = self.client.clone();
            let note = note.clone();
            tokio::spawn(async move { crate::alerts::send_webhook(&client, &url, &note).await });
        }
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
            Err(guard) => match self.database().get_by_id::<Account>(id).await.ok().flatten() {
                Some(account) => {
                    let _ = guard.insert(account.clone());
                    Some(account)
                }
                None => None,
            },
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
                let query = r#"
                    SELECT account.id AS id, account.name AS name, account.password AS password,
                           account.flags AS flags, account.totp_secret AS totp_secret,
                           account.totp_enabled AS totp_enabled,
                           session.api_key AS api_key, session.created_at AS created_at
                    FROM account INNER JOIN session ON session.account_id = account.id
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
                                created_at: row.get("created_at")?,
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

    pub async fn generate_api_key(
        &self,
        id: i64,
        scopes: &[crate::models::Scope],
    ) -> anyhow::Result<String> {
        let mut token = crate::token::Token::new(id)?;
        token.api_key = true;
        let key = token.base64();
        // Persist scopes as comma-separated scope string. Empty string
        // (no scopes selected) is treated as legacy/full at lookup time
        // — see Session::has_scope.
        let scopes_str = scopes
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(",");
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
            .all("SELECT id, size, X'' AS image_data, mimetype, uploader_id, uploaded_at FROM images ORDER BY id ASC", [])
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
            });
        }

        let _ = self.inner.cached_image_files.set(image_files).await;
        self.inner.cached_images.set(files).await
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