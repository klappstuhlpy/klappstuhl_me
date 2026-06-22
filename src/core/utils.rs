use nanoid::nanoid;
use serde::{Deserialize, Deserializer};
use std::path::PathBuf;

/// The maximum amount of bytes an upload can have, in bytes.
pub const MAX_UPLOAD_SIZE: u64 = 1024 * 1024 * 16;
pub const MAX_BODY_SIZE: usize = MAX_UPLOAD_SIZE as usize;

pub fn empty_string_is_none<'de, D: Deserializer<'de>>(de: D) -> Result<Option<String>, D::Error> {
    let opt: Option<String> = Option::deserialize(de)?;
    Ok(opt.filter(|s| !s.is_empty()))
}

/// Generate a random id for the image
/// The id is 10 characters long and contains only lowercase and uppercase letters
pub fn get_new_image_id() -> String {
    let chars: [char; 52] = [
        'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v',
        'w', 'x', 'y', 'z', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R',
        'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
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

/// Validate a post-authentication redirect target.
///
/// Only accepts same-origin **absolute paths** (e.g. `/dashboard`).
/// Rejects protocol-relative (`//evil.com`) and absolute URLs (`https://…`)
/// so a crafted `?next=` cannot turn login into an open redirect. Returns the
/// trimmed path when safe, otherwise `None`.
pub fn safe_next(next: Option<&str>) -> Option<String> {
    let n = next?.trim();
    // Reject backslashes too: some browsers fold `/\evil.com` into the
    // protocol-relative `//evil.com`, which would be an open redirect.
    if !n.is_empty() && n.starts_with('/') && !n.starts_with("//") && !n.contains("://") && !n.contains('\\') {
        Some(n.to_string())
    } else {
        None
    }
}

/// Percent-encode a string for use as a URL query-parameter value
/// (RFC 3986 unreserved characters pass through unchanged).
pub fn urlencode(s: &str) -> String {
    s.bytes()
        .flat_map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => vec![b as char],
            _ => format!("%{b:02X}").chars().collect(),
        })
        .collect()
}

/// Returns the directory where logs are stored.
pub fn logs_directory() -> PathBuf {
    dirs::state_dir()
        .map(|p| p.join(crate::PROGRAM_NAME))
        .unwrap_or_else(|| PathBuf::from("./logs"))
}

#[cfg(test)]
mod tests {
    use super::safe_next;

    #[test]
    fn safe_next_accepts_internal_paths() {
        assert_eq!(safe_next(Some("/dashboard")).as_deref(), Some("/dashboard"));
        assert_eq!(safe_next(Some("/a?b=c#d")).as_deref(), Some("/a?b=c#d"));
    }

    #[test]
    fn safe_next_rejects_open_redirects() {
        for bad in [
            "//evil.com",
            "https://evil.com",
            "/\\evil.com",
            "javascript:alert(1)",
            "",
            "  ",
            "relative",
        ] {
            assert_eq!(safe_next(Some(bad)), None, "should reject {bad:?}");
        }
        assert_eq!(safe_next(None), None);
    }
}
