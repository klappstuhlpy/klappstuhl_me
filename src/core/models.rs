use rusqlite::{
    types::{FromSql, FromSqlResult, ToSqlOutput, ValueRef},
    ToSql,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::{database::Table, key::SecretKey, token::Token};

/// Represents an image file.
#[derive(Debug, Serialize, PartialEq, Eq, Clone, ToSchema)]
pub struct ImageFile {
    /// The file's download URL.
    pub(crate) url: String,
    /// The ID of the image.
    pub(crate) id: String,
    /// The mime type of the image.
    #[schema(example = "image/png")]
    pub(crate) mimetype: String,
    /// The representable image bytes. Not serialized — loaded lazily via /gallery/raw/:id.
    #[serde(skip_serializing)]
    pub(crate) image_data: Vec<u8>,
    /// The file's size in bytes.
    pub(crate) size: u64,
    /// The date the the file was uploaded, in UTC, as an RFC3339 string.
    #[serde(with = "time::serde::rfc3339")]
    pub(crate) uploaded_at: OffsetDateTime,
    /// The id of the uploader
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) uploader_id: Option<i64>,
    /// Optional expiry timestamp (RFC3339). `None` = never expires.
    #[serde(
        with = "time::serde::rfc3339::option",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) expires_at: Option<OffsetDateTime>,
    /// The uploader's original filename, if recorded. `None` for legacy rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) original_name: Option<String>,
    /// Number of times the image's landing page has been viewed.
    #[serde(default)]
    pub(crate) views: i64,
}

impl ImageFile {
    /// A human-friendly download filename. The recorded original name when
    /// present (path components stripped), otherwise the canonical `id`
    /// (which already carries the extension). Always a bare filename.
    pub fn download_name(&self) -> String {
        match self.original_name.as_deref() {
            Some(name) => {
                let bare = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
                if bare.is_empty() {
                    self.id.clone()
                } else {
                    bare.to_string()
                }
            }
            None => self.id.clone(),
        }
    }
}

/// An entry that represents a saved image.
#[derive(Debug, Serialize, PartialEq, Eq, Clone, ToSchema)]
#[schema(as = ImageEntry)]
pub struct ImageEntry {
    /// The ID of the image.
    pub id: String,
    /// The mime type of the image.
    #[schema(example = "image/png")]
    pub mimetype: String,
    /// The representable image bytes.
    pub image_data: Vec<u8>,
    /// The file's size in bytes.
    pub size: u64,
    /// The timestamp when the image was uploaded.
    #[serde(with = "time::serde::rfc3339")]
    pub uploaded_at: OffsetDateTime,
    /// The id of the uploader
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uploader_id: Option<i64>,
    /// Optional expiry timestamp. `None` = never expires.
    #[serde(
        with = "time::serde::rfc3339::option",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_at: Option<OffsetDateTime>,
    /// The uploader's original filename, if recorded. `None` for legacy rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_name: Option<String>,
    /// Number of times this image's landing page has been viewed.
    #[serde(default)]
    pub views: i64,
}

impl ImageEntry {
    /// Returns a temporary ImageEntry suitable for editing.
    pub fn temporary(id: String) -> Self {
        Self {
            id,
            mimetype: Default::default(),
            size: Default::default(),
            image_data: Default::default(),
            uploaded_at: OffsetDateTime::now_utc(),
            uploader_id: Default::default(),
            expires_at: None,
            original_name: None,
            views: 0,
        }
    }

    /// Whether this image's expiry has passed.
    pub fn is_expired(&self) -> bool {
        self.expires_at.map(|e| OffsetDateTime::now_utc() > e).unwrap_or(false)
    }

    /// Returns data safe for embedding into the frontend
    pub fn data(&self) -> ImageEntryData<'_> {
        ImageEntryData {
            id: &self.id,
            mimetype: &self.mimetype,
            size: &self.size,
            image_data: &self.image_data,
            uploaded_at: &self.uploaded_at,
            uploader_id: self.uploader_id,
        }
    }

    /// Returns the file extension derived from the MIME type (e.g. `"png"` from `"image/png"`).
    pub fn ext(&self) -> String {
        self.mimetype.split('/').last().unwrap_or("png").to_string()
    }

    /// A human-friendly download filename: the recorded original name when
    /// present, otherwise the canonical `{id}.{ext}`. The result is always a
    /// bare filename (no path separators) so it is safe in a
    /// `Content-Disposition` header or as a ZIP entry name.
    pub fn download_name(&self) -> String {
        match self.original_name.as_deref() {
            Some(name) => {
                let bare = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
                if bare.is_empty() {
                    format!("{}.{}", self.id, self.ext())
                } else {
                    bare.to_string()
                }
            }
            None => format!("{}.{}", self.id, self.ext()),
        }
    }
}

impl Table for ImageEntry {
    const NAME: &'static str = "images";

    const COLUMNS: &'static [&'static str] = &["id", "mimetype", "image_data", "uploaded_at", "uploader_id"];

    type Id = String;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            mimetype: row.get("mimetype")?,
            size: row.get("size")?,
            image_data: row.get("image_data")?,
            uploaded_at: row.get("uploaded_at")?,
            uploader_id: row.get("uploader_id")?,
            // Tolerant: queries that don't SELECT this column (e.g. the
            // metadata-only cache load) yield None rather than erroring.
            expires_at: row.get::<_, Option<OffsetDateTime>>("expires_at").unwrap_or(None),
            original_name: row.get::<_, Option<String>>("original_name").unwrap_or(None),
            views: row.get::<_, i64>("views").unwrap_or(0),
        })
    }
}

/// A user-created short link, served from the `r.` subdomain
/// (`r.<domain>/<code>` → `target_url`).
#[derive(Debug, Clone, Serialize)]
pub struct ShortLink {
    /// Auto-increment primary key.
    pub id: i64,
    /// The short code / custom alias that appears in the URL (unique).
    pub code: String,
    /// The destination the short link redirects to.
    pub target_url: String,
    /// Owner account id.
    pub account_id: i64,
    /// Number of times the link has been resolved.
    pub clicks: i64,
    /// When the link was created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the link was last edited.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl Table for ShortLink {
    const NAME: &'static str = "short_link";

    const COLUMNS: &'static [&'static str] = &[
        "id",
        "code",
        "target_url",
        "account_id",
        "clicks",
        "created_at",
        "updated_at",
    ];

    type Id = i64;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            code: row.get("code")?,
            target_url: row.get("target_url")?,
            account_id: row.get("account_id")?,
            clicks: row.get("clicks")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Serialize, PartialEq, Eq, Clone, ToSchema)]
pub struct ResolvedImageData {
    /// encoded image bytes
    pub bytes: String,
    /// The image's content type.
    pub content_type: String,
}

/// Data that is passed around from the server to the frontend JavaScript
#[derive(Debug, Clone, Serialize)]
pub struct ImageEntryData<'a> {
    /// The ID of the entry.
    pub id: &'a str,
    /// The mime type of the image.
    pub mimetype: &'a str,
    /// The file's size in bytes.
    pub size: &'a u64,
    /// The image_data
    pub image_data: &'a Vec<u8>,
    /// The timestamp when the image was uploaded.
    #[serde(with = "time::serde::timestamp")]
    pub uploaded_at: &'a OffsetDateTime,
    /// The id of the uploader
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uploader_id: Option<i64>,
}

#[derive(Deserialize, Serialize, Default, PartialEq, Eq, Clone, Copy)]
pub struct AccountFlags(u32);

impl FromSql for AccountFlags {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let value = u32::column_result(value)?;
        Ok(Self(value))
    }
}

impl ToSql for AccountFlags {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.0.into())
    }
}

impl AccountFlags {
    const ADMIN: u32 = 1 << 0;

    pub const fn new() -> Self {
        Self(0)
    }

    #[inline]
    fn has_flag(&self, val: u32) -> bool {
        (self.0 & val) == val
    }

    #[inline]
    fn toggle_flag(&mut self, val: u32, toggle: bool) {
        if toggle {
            self.0 |= val;
        } else {
            self.0 &= !val;
        }
    }

    pub fn is_admin(&self) -> bool {
        self.has_flag(Self::ADMIN)
    }

    pub fn set_admin(&mut self, toggle: bool) {
        self.toggle_flag(Self::ADMIN, toggle)
    }
}

impl std::fmt::Debug for AccountFlags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountFlags")
            .field("value", &self.0)
            .field("admin", &self.is_admin())
            .finish()
    }
}

/// A registered account.
///
/// This server implements a rather simple authentication scheme.
/// Passwords are hashed using Argon2. No emails are stored.
///
/// Authentication is also done using [`crate::token::Token`] instead of
/// maintaining a session database.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Account {
    /// The account ID.
    pub id: i64,
    /// The username of the account.
    ///
    /// Usernames are all lowercase, and can only contain [a-z0-9._\-] characters.
    pub name: String,
    /// The Argon hashed password.
    pub password: String,
    /// The account flags associated with this account.
    pub flags: AccountFlags,
    /// Encrypted TOTP shared secret (base64 nonce‖ciphertext), or `None` when
    /// 2FA has never been set up. See [`crate::totp`].
    pub totp_secret: Option<String>,
    /// Whether TOTP two-factor authentication is currently active. Only ever
    /// set after the user verifies a code during enrollment.
    pub totp_enabled: bool,
    /// The linked Discord user ID, when this account has connected a Discord
    /// account. Not a column on `account` — it is populated by a LEFT JOIN on
    /// `user_discord_links` in the session/account loaders, and defaults to
    /// `None` for the many queries that select only the base account columns.
    pub discord_id: Option<String>,
}

impl Account {
    /// Whether the account has verified, active 2FA.
    pub fn has_totp(&self) -> bool {
        self.totp_enabled && self.totp_secret.is_some()
    }

    /// Whether a Discord account is currently linked to this account.
    ///
    /// The linked user's avatar is **not** stored here — the site header resolves
    /// it live from the bot via `GET /account/discord/avatar`, so it can't go
    /// stale. Only the link's existence (and id) is persisted.
    pub fn has_discord(&self) -> bool {
        self.discord_id.is_some()
    }

    /// Whether the account has a real, usable password set.
    ///
    /// Accounts created through Discord OAuth store [`crate::auth::NO_PASSWORD_SENTINEL`]
    /// instead of an Argon2 hash (they sign in via Discord). For those, changing
    /// the password is really *setting* one and must not require a current
    /// password the user never had.
    pub fn has_password(&self) -> bool {
        !self.password.is_empty() && self.password != crate::auth::NO_PASSWORD_SENTINEL
    }
}

impl Table for Account {
    const NAME: &'static str = "account";

    const COLUMNS: &'static [&'static str] = &["id", "name", "password", "flags", "totp_secret", "totp_enabled"];

    type Id = i64;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            password: row.get("password")?,
            flags: row.get("flags")?,
            // Tolerant of result sets that don't select these columns (e.g. the
            // explicit-column session JOIN): a missing column yields the
            // default rather than an error.
            totp_secret: row.get::<_, Option<String>>("totp_secret").unwrap_or(None),
            totp_enabled: row.get::<_, bool>("totp_enabled").unwrap_or(false),
            // Populated only when the loader JOINs `user_discord_links`;
            // tolerant of result sets that don't select this column.
            discord_id: row.get::<_, Option<String>>("discord_id").unwrap_or(None),
        })
    }
}

/// A trait for getting some information out of the account.
///
/// This works with `Option<Account>` as well. It's basically
/// just a cleaner way of doing `map` followed by `unwrap_or_default`.
pub trait AccountCheck {
    fn flags(&self) -> AccountFlags;
}

impl AccountCheck for Account {
    fn flags(&self) -> AccountFlags {
        self.flags
    }
}

impl AccountCheck for Option<Account> {
    fn flags(&self) -> AccountFlags {
        self.as_ref().map(|t| t.flags).unwrap_or_default()
    }
}

/// Returns `true` if the username satisfies length and character constraints (3–32 chars, `[a-z0-9._-]`).
pub fn is_valid_username(s: &str) -> bool {
    s.len() >= 3
        && s.len() <= 32
        && s.as_bytes()
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'.' || *c == b'_' || *c == b'-')
}

/// An authentication session.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Session {
    /// The session ID.
    pub id: String,
    /// The account ID.
    pub account_id: i64,
    /// When the session was created
    pub created_at: OffsetDateTime,
    /// The description associated with this session
    pub description: Option<String>,
    /// Whether the session is an API key.
    pub api_key: bool,
    /// Comma-separated list of granted scopes for API keys. Empty for
    /// browser sessions or legacy (pre-scopes) API tokens — see
    /// [`Session::scope_set`] for parsing.
    pub scopes: String,
}

impl Table for Session {
    const NAME: &'static str = "session";

    const COLUMNS: &'static [&'static str] = &["id", "account_id", "created_at", "description", "api_key", "scopes"];

    type Id = String;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            account_id: row.get("account_id")?,
            created_at: row.get("created_at")?,
            description: row.get("description")?,
            api_key: row.get("api_key")?,
            // Fall back to "" on databases that haven't been ALTER'd yet
            // (defensive — the idempotent ALTER in main.rs handles this).
            scopes: row
                .get::<_, Option<String>>("scopes")
                .unwrap_or(None)
                .unwrap_or_default(),
        })
    }
}

/// Granular permissions that can be attached to an API token.
///
/// Browser sessions always behave as if all scopes are granted (the
/// authorisation check is bypassed when `Session::api_key == false`).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Read-only access to image/file resources via the API.
    ImagesRead,
    /// Upload + delete images via the API.
    ImagesWrite,
    /// Upload / list / delete images in a Discord guild's shared gallery via the
    /// `/guilds/:id/images` endpoints. Held by trusted service keys (Percy);
    /// the caller is responsible for authorising the acting guild.
    GuildImages,
    /// Read-only access to admin dashboard JSON endpoints
    /// (metrics, security, secrets, audit).
    AdminRead,
    /// Mutate admin state (service actions, secret status).
    AdminWrite,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::ImagesRead => "images:read",
            Scope::ImagesWrite => "images:write",
            Scope::GuildImages => "images:guild",
            Scope::AdminRead => "admin:read",
            Scope::AdminWrite => "admin:write",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "images:read" => Some(Scope::ImagesRead),
            "images:write" => Some(Scope::ImagesWrite),
            "images:guild" => Some(Scope::GuildImages),
            "admin:read" => Some(Scope::AdminRead),
            "admin:write" => Some(Scope::AdminWrite),
            _ => None,
        }
    }

    /// Every defined scope, in display order (used for the checkbox UI).
    pub fn all() -> &'static [Scope] {
        &[
            Scope::ImagesRead,
            Scope::ImagesWrite,
            Scope::GuildImages,
            Scope::AdminRead,
            Scope::AdminWrite,
        ]
    }

    /// Scopes a normal (non-admin) account may **not** attach to a personal API
    /// key. The `admin:*` scopes are operator-only; `images:guild` is minted
    /// exclusively for per-guild service keys (see the dashboard integration)
    /// and is never handed to end users. Enforced in `generate_api_key` and
    /// hidden from the personal-key UI unless the account is an admin.
    pub fn requires_admin(self) -> bool {
        matches!(self, Scope::GuildImages | Scope::AdminRead | Scope::AdminWrite)
    }
}

impl Session {
    /// A human readable label used for the user.
    pub fn label(&self) -> &str {
        self.description.as_deref().unwrap_or("No description")
    }

    pub fn signed(&self, key: &SecretKey) -> Option<String> {
        Token::from_base64(&self.id).map(|t| t.signed(key))
    }

    /// Parses the comma-separated `scopes` column into a typed set.
    pub fn scope_set(&self) -> std::collections::HashSet<Scope> {
        self.scopes
            .split(',')
            .filter_map(|s| Scope::from_str(s.trim()))
            .collect()
    }

    /// Returns `true` if this session is allowed to perform actions
    /// requiring `needed`.  Browser sessions bypass scope checks;
    /// legacy API keys (with an empty scopes string) also bypass for
    /// backwards-compatibility.
    pub fn has_scope(&self, needed: Scope) -> bool {
        if !self.api_key {
            return true; // browser-cookie session, full access
        }
        if self.scopes.is_empty() {
            return true; // legacy API key, pre-scopes
        }
        self.scope_set().contains(&needed)
    }
}

#[cfg(test)]
mod scope_tests {
    use super::Scope;

    #[test]
    fn every_scope_round_trips_through_its_wire_string() {
        for scope in Scope::all() {
            assert_eq!(Scope::from_str(scope.as_str()), Some(*scope));
        }
    }

    #[test]
    fn guild_images_scope_has_stable_wire_name() {
        assert_eq!(Scope::GuildImages.as_str(), "images:guild");
        assert_eq!(Scope::from_str("images:guild"), Some(Scope::GuildImages));
    }

    #[test]
    fn privileged_scopes_require_admin() {
        // Operator/internal-only scopes a normal user must never self-grant.
        assert!(Scope::AdminRead.requires_admin());
        assert!(Scope::AdminWrite.requires_admin());
        assert!(Scope::GuildImages.requires_admin());
        // The everyday image scopes stay available to everyone.
        assert!(!Scope::ImagesRead.requires_admin());
        assert!(!Scope::ImagesWrite.requires_admin());
    }
}
