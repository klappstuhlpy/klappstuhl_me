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
/// Accepts same-origin **absolute paths** (e.g. `/dashboard`) and full URLs
/// whose host is a subdomain of a trusted domain (e.g. `http://percy.localhost:9510/dashboard`
/// when `localhost` is the cookie domain). The trusted-subdomain allowance lets
/// cross-subdomain login flows redirect back to the originating subdomain after
/// the OAuth callback lands on the apex.
///
/// Rejects protocol-relative (`//evil.com`) and absolute URLs to unknown hosts
/// so a crafted `?next=` cannot turn login into an open redirect.
pub fn safe_next(next: Option<&str>) -> Option<String> {
    safe_next_for_domain(next, None)
}

/// Like [`safe_next`] but with an explicit trusted domain (the cookie domain).
/// Full URLs whose host is `*.<trusted_domain>` (or the domain itself, with any port)
/// are accepted in addition to bare paths.
pub fn safe_next_for_domain(next: Option<&str>, trusted_domain: Option<&str>) -> Option<String> {
    let n = next?.trim();
    if n.is_empty() {
        return None;
    }
    // Reject backslashes: some browsers fold `/\evil.com` into `//evil.com`.
    if n.contains('\\') {
        return None;
    }
    // Bare path — always safe.
    if n.starts_with('/') && !n.starts_with("//") {
        return Some(n.to_string());
    }
    // Full URL — only allow if the host is a trusted (sub)domain.
    if let Some(domain) = trusted_domain {
        if let Some(url) = n.strip_prefix("http://").or_else(|| n.strip_prefix("https://")) {
            let host_and_path = url.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((url, "/".to_string()));
            let host = host_and_path.0.split(':').next().unwrap_or(host_and_path.0);
            if host == domain || host.ends_with(&format!(".{domain}")) {
                return Some(n.to_string());
            }
        }
    }
    None
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
    use super::{safe_next, safe_next_for_domain};

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

    #[test]
    fn safe_next_for_domain_accepts_trusted_subdomains() {
        let d = Some("localhost");
        assert_eq!(
            safe_next_for_domain(Some("http://percy.localhost:9510/dashboard"), d).as_deref(),
            Some("http://percy.localhost:9510/dashboard")
        );
        assert_eq!(
            safe_next_for_domain(Some("https://percy.klappstuhl.me/dashboard"), Some("klappstuhl.me")).as_deref(),
            Some("https://percy.klappstuhl.me/dashboard")
        );
        // Bare paths still work
        assert_eq!(safe_next_for_domain(Some("/dashboard"), d).as_deref(), Some("/dashboard"));
    }

    #[test]
    fn safe_next_for_domain_rejects_untrusted_hosts() {
        let d = Some("localhost");
        assert_eq!(safe_next_for_domain(Some("https://evil.com/dashboard"), d), None);
        assert_eq!(safe_next_for_domain(Some("http://notlocalhost:9510/x"), d), None);
        assert_eq!(safe_next_for_domain(Some("http://evillocalhost:9510/x"), d), None);
    }
}
