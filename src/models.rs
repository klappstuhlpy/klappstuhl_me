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
    /// The representable image bytes.
    pub(crate) image_data: Vec<u8>,
    /// The file's size in bytes.
    pub(crate) size: u64,
    /// The date the the file was uploaded, in UTC, as an RFC3339 string.
    #[serde(with = "time::serde::rfc3339")]
    pub(crate) uploaded_at: OffsetDateTime,
    /// The id of the uploader
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) uploader_id: Option<i64>,
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
    /// The timestamp when the image was uploaded.
    #[serde(with = "time::serde::rfc3339")]
    pub uploaded_at: OffsetDateTime,
    /// The id of the uploader
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uploader_id: Option<i64>,
}

impl ImageEntry {
    /// Returns a temporary ImageEntry suitable for editing.
    pub fn temporary(id: String) -> Self {
        Self {
            id,
            mimetype: Default::default(),
            image_data: Default::default(),
            uploaded_at: OffsetDateTime::now_utc(),
            uploader_id: Default::default()
        }
    }

    /// Returns data safe for embedding into the frontend
    pub fn data(&self) -> ImageEntryData {
        ImageEntryData {
            id: &self.id,
            mimetype: &self.mimetype,
            image_data: &self.image_data,
            uploaded_at: &self.uploaded_at,
            uploader_id: self.uploader_id
        }
    }

    pub fn ext(&self) -> String {
        let _ext = self.mimetype.split("/");
        if let Some(ext) = _ext.last() {
            ext.to_string()
        } else {
            "png".to_string()
        }
    }
}

impl Table for ImageEntry {
    const NAME: &'static str = "images";

    const COLUMNS: &'static [&'static str] = &[
        "id",
        "mimetype",
        "image_data",
        "uploaded_at",
        "uploader_id"
    ];

    type Id = String;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            mimetype: row.get("mimetype")?,
            image_data: row.get("image_data")?,
            uploaded_at: row.get("uploaded_at")?,
            uploader_id: row.get("uploader_id")?
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
}

impl Table for Account {
    const NAME: &'static str = "account";

    const COLUMNS: &'static [&'static str] = &["id", "name", "password", "flags"];

    type Id = i64;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            password: row.get("password")?,
            flags: row.get("flags")?,
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
}

impl Table for Session {
    const NAME: &'static str = "session";

    const COLUMNS: &'static [&'static str] = &["id", "account_id", "created_at", "description", "api_key"];

    type Id = String;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            account_id: row.get("account_id")?,
            created_at: row.get("created_at")?,
            description: row.get("description")?,
            api_key: row.get("api_key")?,
        })
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
}