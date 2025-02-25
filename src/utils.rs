use std::{path::PathBuf, str::FromStr};
use nanoid::nanoid;
use percent_encoding::{AsciiSet, CONTROLS};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};

/// The maximum amount of bytes an upload can have, in bytes.
pub const MAX_UPLOAD_SIZE: u64 = 1024 * 1024 * 16;
pub const MAX_BODY_SIZE: usize = MAX_UPLOAD_SIZE as usize;

pub const FRAGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'[')
    .add(b']')
    .add(b'`')
    .add(b'#')
    .add(b';')
    .add(b'%');

/// This is mainly for use in forms.
///
/// Since forms always receive strings, this uses FromStr for the internal type.
pub fn generic_empty_string_is_none<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: std::error::Error,
{
    let opt = Option::<String>::deserialize(de)?;
    let opt = opt.as_deref();
    match opt {
        None | Some("") => Ok(None),
        Some(s) => s.parse::<T>().map(Some).map_err(serde::de::Error::custom),
    }
}

pub fn empty_string_is_none<'de, D: Deserializer<'de>>(de: D) -> Result<Option<String>, D::Error> {
    let opt: Option<String> = Option::deserialize(de)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

pub fn inner_json<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let s = crate::borrowed::MaybeBorrowedString::deserialize(de)?;
    serde_json::from_str(&s).map_err(serde::de::Error::custom)
}

pub fn is_over_length<T: AsRef<str>>(opt: &Option<T>, length: usize) -> bool {
    opt.as_ref().map(|x| x.as_ref().len() >= length).unwrap_or_default()
}

/// Generate a random id for the image
/// The id is 10 characters long and contains only lowercase and uppercase letters
pub fn get_new_image_id() -> String {
    let chars: [char; 52] = [
        'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r',
        's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J',
        'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
    ];
    nanoid!(5, &chars)
}

pub fn join_iter<T: ToString>(sep: impl AsRef<str>, mut iter: impl Iterator<Item = T>) -> String {
    let mut buffer = String::new();
    if let Some(item) = iter.next() {
        buffer.push_str(&item.to_string());
    }
    for item in iter {
        buffer.push_str(sep.as_ref());
        buffer.push_str(&item.to_string());
    }
    buffer
}

/// Returns the directory where logs are stored
pub fn logs_directory() -> PathBuf {
    dirs::state_dir()
        .map(|p| p.join(crate::PROGRAM_NAME))
        .unwrap_or_else(|| PathBuf::from("./logs"))
}

/// This is mainly used for serde defaults
pub const fn default_true() -> bool {
    true
}

pub const fn is_false(s: &bool) -> bool {
    !*s
}

pub mod base64_bytes {
    use base64::{prelude::BASE64_STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::borrowed::MaybeBorrowedString;

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        let b64 = BASE64_STANDARD.encode(bytes);
        serializer.serialize_str(&b64)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let s = MaybeBorrowedString::deserialize(deserializer)?;
        BASE64_STANDARD.decode(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}

/// Utility type to differentiate between explicit null and missing values.
///
/// This still requires using `#[serde(default)]` or `#[serde(skip_serialization_if = "Patch::is_missing")]`
/// but this allows for easier differentiation than the double `Option` approach.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Patch<T> {
    Missing,
    Null,
    Value(T),
}

impl<T> Patch<T> {
    /// Returns `true` if the patch is [`Missing`].
    ///
    /// [`Missing`]: Patch::Missing
    #[must_use]
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    /// Returns `true` if the patch is [`Null`].
    ///
    /// [`Null`]: Patch::Null
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Returns the double option for this type.
    ///
    /// - `None` is missing
    /// - `Some(None)` is `null`
    /// - `Some(Some(T))` is a given value
    #[must_use]
    pub fn to_option(self) -> Option<Option<T>> {
        match self {
            Patch::Missing => None,
            Patch::Null => Some(None),
            Patch::Value(value) => Some(Some(value)),
        }
    }
}

impl<T> Default for Patch<T> {
    fn default() -> Self {
        Self::Missing
    }
}

impl<T> From<Option<T>> for Patch<T> {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(v) => Self::Value(v),
            None => Self::Null,
        }
    }
}

impl<T> Serialize for Patch<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            // There's unfortunately no way to tell the serializer to not serialize a variant
            Patch::Missing => serializer.serialize_none(),
            Patch::Null => serializer.serialize_none(),
            Patch::Value(value) => serializer.serialize_some(value),
        }
    }
}

impl<'de, T> Deserialize<'de> for Patch<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::deserialize(deserializer).map(Into::into)
    }
}