//! Implements an audit log trail for editor actions

use rusqlite::{types::FromSql, ToSql};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{database::Table};

/*
    It's important to note that the data in here should be backwards compatible.
*/

/// Inner data for a file operation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOperation {
    /// The name of the file.
    pub name: String,
    /// Whether the file failed to be moved over.
    pub failed: bool,
}

impl FileOperation {
    /// Creates a new file operation
    pub fn placeholder() -> Self {
        Self {
            name: String::new(),
            failed: false,
        }
    }
}

/// Audit log data for a file upload operation
///
/// For this data, `image_id` and `account_id` are only null if the data is deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upload {
    pub files: Vec<FileOperation>,
    pub api: bool,
}

/// Audit log data for an entry delete operation
///
/// For this data, `image_id` is null and `account_id` is null the data is deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteImage {
    pub file: FileOperation,
    pub api: bool
}

/// Audit log data for a file delete operation
///
/// For this data and `account_id` are only null if the data is deleted.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteFiles {
    pub files: Vec<FileOperation>,
}

impl DeleteFiles {
    pub fn add_file(&mut self, name: String, failed: bool) {
        self.files.push(FileOperation { name, failed });
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum AuditLogData {
    Upload(Upload),
    DeleteImage(DeleteImage),
    DeleteFiles(DeleteFiles),
}

impl From<DeleteImage> for AuditLogData {
    fn from(v: DeleteImage) -> Self {
        Self::DeleteImage(v)
    }
}

impl From<DeleteFiles> for AuditLogData {
    fn from(v: DeleteFiles) -> Self {
        Self::DeleteFiles(v)
    }
}

impl From<Upload> for AuditLogData {
    fn from(v: Upload) -> Self {
        Self::Upload(v)
    }
}

impl FromSql for AuditLogData {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        serde_json::from_str(value.as_str()?).map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))
    }
}

impl ToSql for AuditLogData {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        let as_str = serde_json::to_string(self).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Ok(rusqlite::types::ToSqlOutput::Owned(as_str.into()))
    }
}

/// An audit log entry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditLogEntry {
    /// The ID of the entry. This is represented as a datetime with milliseconds precision
    /// in the database.
    pub id: i64,
    /// The account ID that created this entry.
    pub account_id: Option<i64>,
    /// The actual data for this audit log entry.
    pub data: AuditLogData,
}

fn datetime_to_ms(dt: OffsetDateTime) -> i64 {
    let ts = dt.unix_timestamp_nanos() / 1_000_000;
    ts as i64
}

impl AuditLogEntry {
    /// Creates a new audit log entry
    pub fn new<T>(data: T) -> Self
    where
        T: Into<AuditLogData>,
    {
        Self {
            id: datetime_to_ms(OffsetDateTime::now_utc()),
            account_id: None,
            data: data.into(),
        }
    }

    pub fn full<T>(data: T, account_id: i64) -> Self
    where
        T: Into<AuditLogData>,
    {
        Self {
            id: datetime_to_ms(OffsetDateTime::now_utc()),
            account_id: Some(account_id),
            data: data.into(),
        }
    }

    pub fn with_account(mut self, account_id: i64) -> Self {
        self.account_id = Some(account_id);
        self
    }

    pub fn created_at(&self) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp_nanos(self.id as i128 * 1_000_000).unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }
}

impl Table for AuditLogEntry {
    const NAME: &'static str = "audit_log";
    const COLUMNS: &'static [&'static str] = &["id", "account_id", "data"];
    type Id = i64;

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            account_id: row.get("account_id")?,
            data: row.get("data")?,
        })
    }
}