//! The public changelog at `/changelog`.
//!
//! The repo-root `CHANGELOG.md` is the single source: it is embedded into the
//! binary at compile time and parsed here into structured releases, so the page
//! and the file GitHub renders can never drift apart. The format contract the
//! parser enforces is documented in `.claude/CHANGELOG_GUIDE.md`; [`tests`]
//! validates the real file against it, so a malformed changelog fails
//! `cargo test` rather than rendering garbage in production.
//!
//! `## [Unreleased]` is parsed (to validate its shape) but never rendered —
//! entries become public only once they ship in a release.

use crate::models::Account;
use askama::Template;
use axum::{routing::get, Router};
use std::sync::OnceLock;

use crate::AppState;

/// The changelog source, embedded at compile time.
const SOURCE: &str = include_str!("../../CHANGELOG.md");

/// The six categories a release may use, in the order they must appear.
/// `(heading, css-slug)`.
const CATEGORIES: [(&str, &str); 6] = [
    ("Added", "added"),
    ("Changed", "changed"),
    ("Deprecated", "deprecated"),
    ("Removed", "removed"),
    ("Fixed", "fixed"),
    ("Security", "security"),
];

/// One category block within a release.
#[derive(Debug)]
pub struct Section {
    /// The category name as written (`Added`, `Fixed`, …).
    pub name: &'static str,
    /// Lowercased category, used as a CSS class hook.
    pub slug: &'static str,
    /// Entries, each already rendered from inline markdown to HTML.
    pub entries: Vec<String>,
}

/// One released version.
#[derive(Debug)]
pub struct Release {
    /// `1.4.2`
    pub version: String,
    /// ISO date as written in the file (`2026-07-14`), used for `<time datetime>`.
    pub date: String,
    /// Human-readable date for display (`14 July 2026`).
    pub date_label: String,
    pub sections: Vec<Section>,
}

impl Release {
    /// `(major, minor, patch)`, used to assert the file is ordered newest-first.
    fn semver(&self) -> (u64, u64, u64) {
        parse_semver(&self.version).expect("version was validated at parse time")
    }
}

#[derive(Debug)]
struct Parsed {
    /// Sections under `## [Unreleased]`. Parsed for validation, never rendered.
    unreleased: Vec<Section>,
    /// Released versions, newest first (as written in the file).
    releases: Vec<Release>,
}

fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.split('.');
    let out = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(out)
}

/// Render one bullet's inline markdown (`code`, **bold**, links) to HTML.
/// The source is our own compile-time-embedded file, not user input, so raw
/// HTML passthrough is fine. Strips the wrapping `<p>` pulldown-cmark adds.
fn render_inline(text: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};

    let mut out = String::new();
    html::push_html(&mut out, Parser::new_ext(text, Options::empty()));
    let trimmed = out.trim();
    trimmed
        .strip_prefix("<p>")
        .and_then(|s| s.strip_suffix("</p>"))
        .unwrap_or(trimmed)
        .to_string()
}

/// `2026-07-14` → `14 July 2026`. The date is validated as a real calendar date,
/// so a typo like `2026-13-01` fails the test rather than shipping.
fn date_label(iso: &str) -> Option<String> {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    let date = time::Date::parse(iso, &format).ok()?;
    Some(format!("{} {} {}", date.day(), date.month(), date.year()))
}

/// Parse `CHANGELOG.md` into releases, enforcing the format contract. The error
/// is a human-readable message naming the offending line.
fn parse(source: &str) -> Result<Parsed, String> {
    let mut parsed = Parsed {
        unreleased: Vec::new(),
        releases: Vec::new(),
    };
    // The release currently being filled, and — for `[Unreleased]` — a flag, since
    // its sections land in a different bucket.
    let mut current: Option<Release> = None;
    let mut in_unreleased = false;
    let mut sections: Vec<Section> = Vec::new();
    let mut h1_count = 0usize;
    let mut seen_first_heading = false;

    // Close the release/unreleased block being filled and bank its sections.
    macro_rules! flush {
        () => {
            if let Some(mut release) = current.take() {
                release.sections = std::mem::take(&mut sections);
                parsed.releases.push(release);
            } else if std::mem::take(&mut in_unreleased) {
                parsed.unreleased = std::mem::take(&mut sections);
            }
        };
    }

    for (number, raw) in source.lines().enumerate() {
        let line = raw.trim_end();
        let numbered = |msg: String| format!("CHANGELOG.md line {}: {msg}", number + 1);

        if let Some(rest) = line.strip_prefix("# ") {
            if seen_first_heading {
                return Err(numbered(format!("unexpected second H1 heading `# {rest}`")));
            }
            h1_count += 1;
            continue;
        }

        if let Some(rest) = line.strip_prefix("## ") {
            flush!();
            seen_first_heading = true;

            if rest == "[Unreleased]" {
                if !parsed.releases.is_empty() {
                    return Err(numbered("`## [Unreleased]` must come before every release".into()));
                }
                in_unreleased = true;
                continue;
            }

            // `## [X.Y.Z] - YYYY-MM-DD`
            let (version, date) = rest
                .strip_prefix('[')
                .and_then(|r| r.split_once("] - "))
                .ok_or_else(|| {
                    numbered(format!(
                        "malformed release heading `## {rest}`, expected `## [X.Y.Z] - YYYY-MM-DD`"
                    ))
                })?;

            if parse_semver(version).is_none() {
                return Err(numbered(format!("`{version}` is not a valid X.Y.Z version")));
            }
            let date_label =
                date_label(date).ok_or_else(|| numbered(format!("`{date}` is not a valid YYYY-MM-DD date")))?;

            current = Some(Release {
                version: version.to_string(),
                date: date.to_string(),
                date_label,
                sections: Vec::new(),
            });
            continue;
        }

        if let Some(name) = line.strip_prefix("### ") {
            if current.is_none() && !in_unreleased {
                return Err(numbered(format!("category `### {name}` sits outside a release")));
            }
            let (name, slug) = CATEGORIES
                .iter()
                .find(|(candidate, _)| *candidate == name)
                .ok_or_else(|| {
                    numbered(format!(
                        "unknown category `### {name}`; allowed: {}",
                        CATEGORIES.map(|(c, _)| c).join(", ")
                    ))
                })?;

            // Categories must be unique within a release and appear in canonical order.
            let position = |needle: &str| CATEGORIES.iter().position(|(c, _)| *c == needle).unwrap();
            if let Some(previous) = sections.last() {
                if position(previous.name) >= position(name) {
                    return Err(numbered(format!(
                        "category `### {name}` is out of order (or duplicated) — it must come after `### {}`",
                        previous.name
                    )));
                }
            }

            sections.push(Section {
                name,
                slug,
                entries: Vec::new(),
            });
            continue;
        }

        if let Some(entry) = line.strip_prefix("- ") {
            let section = sections
                .last_mut()
                .ok_or_else(|| numbered("bullet sits outside a category heading".into()))?;
            section.entries.push(render_inline(entry.trim()));
            continue;
        }

        // Everything else is only allowed before the first `##` (the intro prose)
        // or as a trailing link-reference definition / blank line.
        let ignorable = line.trim().is_empty() || line.starts_with('[') || !seen_first_heading;
        if !ignorable {
            return Err(numbered(format!("unexpected content inside a release: `{line}`")));
        }
    }
    flush!();

    if h1_count != 1 {
        return Err(format!("CHANGELOG.md must have exactly one `# ` H1, found {h1_count}"));
    }
    if parsed.releases.is_empty() {
        return Err("CHANGELOG.md contains no releases".into());
    }

    // Newest first, strictly descending — the page renders them in file order.
    for pair in parsed.releases.windows(2) {
        if pair[0].semver() <= pair[1].semver() {
            return Err(format!(
                "releases must be ordered newest-first and strictly descending, but {} is listed above {}",
                pair[0].version, pair[1].version
            ));
        }
    }

    Ok(parsed)
}

/// The parsed releases, newest first. Parsed once on first access; the inline
/// test guarantees the embedded file parses, so this cannot panic in production.
fn releases() -> &'static [Release] {
    static RELEASES: OnceLock<Vec<Release>> = OnceLock::new();
    RELEASES.get_or_init(|| {
        parse(SOURCE)
            .expect("CHANGELOG.md is validated by changelog::tests")
            .releases
    })
}

#[derive(Template)]
#[template(path = "changelog.html")]
struct ChangelogTemplate {
    account: Option<Account>,
    releases: &'static [Release],
    /// The version this binary is running, i.e. the newest *deployed* release.
    current: &'static str,
}

async fn page(account: Option<Account>) -> ChangelogTemplate {
    ChangelogTemplate {
        account,
        releases: releases(),
        current: crate::VERSION,
    }
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/changelog", get(page))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real, embedded changelog must satisfy the format contract. This is the
    /// check that makes a malformed changelog a build failure rather than a
    /// broken public page.
    #[test]
    fn embedded_changelog_parses() {
        let parsed = parse(SOURCE).expect("the real CHANGELOG.md must parse");
        assert!(!parsed.releases.is_empty());

        for release in &parsed.releases {
            assert!(
                !release.sections.is_empty(),
                "release {} has no categories",
                release.version
            );
            for section in &release.sections {
                assert!(
                    !section.entries.is_empty(),
                    "release {} has an empty `### {}` — omit empty categories",
                    release.version,
                    section.name
                );
            }
        }
    }

    /// The running binary must not be older than the newest release documented:
    /// a release is written down only once its version is cut in `Cargo.toml`.
    #[test]
    fn newest_release_is_not_ahead_of_the_binary() {
        let newest = releases().first().expect("at least one release");
        let current = parse_semver(crate::VERSION).expect("Cargo.toml version is semver");
        assert!(
            newest.semver() <= current,
            "CHANGELOG.md documents {} but Cargo.toml is at {} — bump the version, or move the \
             entries back under `## [Unreleased]`",
            newest.version,
            crate::VERSION,
        );
    }

    #[test]
    fn rejects_a_malformed_release_heading() {
        let err = parse("# Changelog\n\n## [1.0] - 2026-01-01\n\n### Added\n\n- Thing\n").unwrap_err();
        assert!(err.contains("not a valid X.Y.Z version"), "{err}");
    }

    #[test]
    fn rejects_an_impossible_date() {
        let err = parse("# Changelog\n\n## [1.0.0] - 2026-13-01\n\n### Added\n\n- Thing\n").unwrap_err();
        assert!(err.contains("not a valid YYYY-MM-DD date"), "{err}");
    }

    #[test]
    fn rejects_an_unknown_category() {
        let err = parse("# Changelog\n\n## [1.0.0] - 2026-01-01\n\n### Improved\n\n- Thing\n").unwrap_err();
        assert!(err.contains("unknown category"), "{err}");
    }

    #[test]
    fn rejects_out_of_order_categories() {
        let source = "# Changelog\n\n## [1.0.0] - 2026-01-01\n\n### Fixed\n\n- A\n\n### Added\n\n- B\n";
        let err = parse(source).unwrap_err();
        assert!(err.contains("out of order"), "{err}");
    }

    #[test]
    fn rejects_ascending_releases() {
        let source = "# Changelog\n\n## [1.0.0] - 2026-01-01\n\n### Added\n\n- A\n\n\
                      ## [2.0.0] - 2026-02-01\n\n### Added\n\n- B\n";
        let err = parse(source).unwrap_err();
        assert!(err.contains("strictly descending"), "{err}");
    }

    #[test]
    fn unreleased_is_parsed_but_kept_out_of_the_releases() {
        let source = "# Changelog\n\nIntro prose.\n\n## [Unreleased]\n\n### Added\n\n- Soon\n\n\
                      ## [1.0.0] - 2026-01-01\n\n### Added\n\n- Shipped\n";
        let parsed = parse(source).unwrap();
        assert_eq!(parsed.releases.len(), 1);
        assert_eq!(parsed.releases[0].version, "1.0.0");
        assert_eq!(parsed.unreleased[0].entries, vec!["Soon"]);
    }

    #[test]
    fn renders_inline_markdown_in_entries() {
        let source = "# Changelog\n\n## [1.0.0] - 2026-01-01\n\n### Added\n\n- A `code` and **bold** entry\n";
        let parsed = parse(source).unwrap();
        assert_eq!(
            parsed.releases[0].sections[0].entries[0],
            "A <code>code</code> and <strong>bold</strong> entry"
        );
    }

    #[test]
    fn formats_the_date_for_display() {
        assert_eq!(date_label("2026-07-14").unwrap(), "14 July 2026");
    }
}
