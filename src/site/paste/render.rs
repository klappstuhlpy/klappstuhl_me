//! Turning a paste body into something a browser can show: syntax highlighting
//! with per-line anchors, and **sanitised** markdown.
//!
//! ## Why the markdown parser here is not the changelog's
//!
//! `pulldown-cmark` passes raw HTML blocks through untouched. That is fine for
//! the changelog and the API description, which we write. A *paste* is written by
//! a stranger, so the same configuration would be a stored-XSS hole: `<script>`
//! in a markdown paste would execute on `klappstuhl.me`, on the origin that holds
//! the session cookie.
//!
//! [`markdown`] therefore drops every `Event::Html` / `Event::InlineHtml` before
//! serialising, and rewrites any link/image destination that isn't plainly safe
//! (`javascript:`, `data:`, …) to `#`. Do not "simplify" this back to the
//! changelog's parser configuration.

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, ThemeSet};
use syntect::html::{styled_line_to_highlighted_html, IncludeBackground};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use std::sync::OnceLock;

/// A highlighted paste body, one entry per source line.
pub struct Highlighted {
    /// Per-line HTML, so the template can wrap each in its own anchor row.
    pub lines: Vec<String>,
    /// The theme's background colour, as hex.
    pub background: String,
    /// The theme's default foreground, as hex.
    pub foreground: String,
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

fn hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// Highlights `code` line by line.
///
/// Unlike `codeimage::render_html` (which returns one opaque `<pre>` blob), this
/// returns the lines separately — that is what lets the viewer give every line an
/// `id="L12"` anchor, a clickable gutter, and range selection. An unknown or
/// empty language falls back to plain text; an unknown theme to the default.
pub fn highlight(code: &str, language: &str, theme_name: &str) -> Highlighted {
    let ps = syntaxes();
    let ts = theme_set();

    let lang = language.trim();
    let syntax = ps
        .find_syntax_by_token(lang)
        .or_else(|| ps.find_syntax_by_extension(lang))
        .unwrap_or_else(|| ps.find_syntax_plain_text());
    let theme = ts
        .themes
        .get(theme_name)
        .or_else(|| ts.themes.get(crate::codeimage::DEFAULT_THEME))
        .expect("syntect ships with the default theme");

    let background = hex(theme.settings.background.unwrap_or(Color {
        r: 40,
        g: 44,
        b: 52,
        a: 255,
    }));
    let foreground = hex(theme.settings.foreground.unwrap_or(Color {
        r: 220,
        g: 223,
        b: 228,
        a: 255,
    }));

    let mut hl = HighlightLines::new(syntax, theme);
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(code) {
        let html = hl
            .highlight_line(line, ps)
            .map(|ranges| styled_line_to_highlighted_html(&ranges, IncludeBackground::No).unwrap_or_default())
            // A highlighter failure must not lose the line — fall back to the
            // escaped source, which is what the reader actually came for.
            .unwrap_or_else(|_| escape_html(line));
        // syntect keeps the line terminator *inside* the last span
        // (`…():\n</span>`), so trimming the string's tail misses it. A single
        // line has no interior newline, so dropping every `\n`/`\r` removes
        // exactly the terminator — which otherwise renders as a blank line
        // (each viewer row, and every join in the editor's overlay).
        lines.push(html.replace(['\n', '\r'], ""));
    }
    if lines.is_empty() {
        lines.push(String::new());
    }

    Highlighted {
        lines,
        background,
        foreground,
    }
}

/// HTML-escapes a text node.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Renders a markdown paste to HTML with raw HTML stripped and unsafe link
/// destinations neutralised. See the module docs for why this exists separately.
pub fn markdown(source: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);

    let events = Parser::new_ext(source, options).filter_map(|event| match event {
        // The whole point: a stranger's raw HTML never reaches the page.
        Event::Html(_) | Event::InlineHtml(_) => None,
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Link {
            link_type,
            dest_url: sanitize_url(dest_url),
            title,
            id,
        })),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Some(Event::Start(Tag::Image {
            link_type,
            dest_url: sanitize_url(dest_url),
            title,
            id,
        })),
        other => Some(other),
    });

    let mut html = String::with_capacity(source.len() * 3 / 2);
    pulldown_cmark::html::push_html(&mut html, events);
    html
}

/// Allows only destinations that cannot execute script. Anything else — most of
/// all `javascript:` and `data:` — collapses to `#`. Scheme-relative (`//host`)
/// and same-page (`#frag`) and site-relative (`/path`) links are fine.
fn sanitize_url(url: CowStr<'_>) -> CowStr<'static> {
    let trimmed = url.trim();
    let safe = match trimmed.split_once(':') {
        // No scheme at all: a relative/anchor link. Safe.
        None => true,
        Some((scheme, _)) => {
            // A "scheme" that contains a `/`, `?` or `#` before the colon isn't a
            // scheme (e.g. `/a:b`, `?x=1:2`) — it's a relative URL.
            if scheme.contains(['/', '?', '#']) {
                true
            } else {
                matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https" | "mailto")
            }
        }
    };
    if safe {
        CowStr::from(trimmed.to_string())
    } else {
        CowStr::from("#")
    }
}

/// Whether a paste's language means "render this as markdown".
pub fn is_markdown(language: Option<&str>) -> bool {
    matches!(language, Some("md") | Some("markdown"))
}

/// Guesses a syntect language token from a filename's extension. Used by the
/// editor's drag-and-drop and by `POST /p`'s `filename` hint.
pub fn language_from_filename(name: &str) -> Option<String> {
    let ext = name.rsplit_once('.')?.1.trim().to_ascii_lowercase();
    (!ext.is_empty()).then_some(ext)
}

/// A human-friendly download filename for a paste (`<id>.<ext>`).
pub fn download_name(id: &str, language: Option<&str>) -> String {
    match language {
        Some(lang) if !lang.is_empty() => format!("{id}.{lang}"),
        _ => format!("{id}.txt"),
    }
}

/// Language tokens for which a real brand logo is vendored under
/// `static/img/lang/<token>.svg`. Any token not in here falls back, in the
/// picker, to a tinted monospace badge. Keep this in step with the files that
/// actually exist on disk — a token listed here whose file is missing just shows
/// a broken image until the browser's `onerror` swaps in the badge.
pub const ICON_TOKENS: &[&str] = &[
    "rs",
    "py",
    "js",
    "ts",
    "html",
    "css",
    "go",
    "java",
    "kt",
    "rb",
    "php",
    "lua",
    "c",
    "cpp",
    "cs",
    "sh",
    "md",
    "swift",
    "scala",
    "hs",
    "r",
    "pl",
    "dart",
    "ex",
    "clj",
    "dockerfile",
    "vim",
    "ps1",
    "m",
    "groovy",
    "jl",
    "ml",
    "erl",
    "fs",
    "graphql",
    "coffee",
    "tex",
    "vue",
    "scss",
    "nim",
    "zig",
    "json",
    "yaml",
];

/// The languages shown *first* in the editor's picker, in this order — the
/// popular ones, most carrying a vendored logo. Everything else syntect knows is
/// appended after these (alphabetically) by [`picker_languages`], so the list is
/// exhaustive without burying Rust under `AppleScript`.
const FEATURED: &[(&str, &str)] = &[
    ("rs", "Rust"),
    ("py", "Python"),
    ("js", "JavaScript"),
    ("ts", "TypeScript"),
    ("go", "Go"),
    ("java", "Java"),
    ("c", "C"),
    ("cpp", "C++"),
    ("cs", "C#"),
    ("html", "HTML"),
    ("css", "CSS"),
    ("scss", "SCSS"),
    ("php", "PHP"),
    ("rb", "Ruby"),
    ("swift", "Swift"),
    ("kt", "Kotlin"),
    ("scala", "Scala"),
    ("dart", "Dart"),
    ("lua", "Lua"),
    ("pl", "Perl"),
    ("r", "R"),
    ("hs", "Haskell"),
    ("clj", "Clojure"),
    ("ex", "Elixir"),
    ("erl", "Erlang"),
    ("fs", "F#"),
    ("ml", "OCaml"),
    ("jl", "Julia"),
    ("nim", "Nim"),
    ("zig", "Zig"),
    ("coffee", "CoffeeScript"),
    ("groovy", "Groovy"),
    ("m", "Objective-C"),
    ("vue", "Vue"),
    ("graphql", "GraphQL"),
    ("sql", "SQL"),
    ("json", "JSON"),
    ("yaml", "YAML"),
    ("toml", "TOML"),
    ("xml", "XML"),
    ("md", "Markdown"),
    ("tex", "LaTeX"),
    ("sh", "Shell"),
    ("ps1", "PowerShell"),
    ("dockerfile", "Dockerfile"),
    ("vim", "Vim"),
    ("diff", "Diff"),
];

/// One row of the editor's language picker.
pub struct PickerLang {
    /// The syntect token stored on the paste (`rs`, `py`, …). Empty means "Auto".
    pub token: String,
    /// The human label shown in the list (`Rust`).
    pub name: String,
    /// Whether a vendored brand logo exists for [`token`](Self::token); when it
    /// doesn't, the frontend draws a tinted badge instead.
    pub icon: bool,
}

/// The complete language list for the editor picker: the [`FEATURED`] shortlist
/// first, then every *other* syntax syntect ships, sorted by name and de-duped by
/// token and by name. Built once and cached — syntect's set is static.
pub fn picker_languages() -> &'static [PickerLang] {
    static LIST: OnceLock<Vec<PickerLang>> = OnceLock::new();
    LIST.get_or_init(|| {
        use std::collections::HashSet;

        let mut out: Vec<PickerLang> = Vec::new();
        let mut seen_tokens: HashSet<String> = HashSet::new();
        let mut seen_names: HashSet<String> = HashSet::new();

        for (token, name) in FEATURED {
            out.push(PickerLang {
                token: (*token).to_string(),
                name: (*name).to_string(),
                icon: ICON_TOKENS.contains(token),
            });
            seen_tokens.insert((*token).to_string());
            seen_names.insert(name.to_ascii_lowercase());
        }

        let mut tail: Vec<PickerLang> = Vec::new();
        for syntax in syntaxes().syntaxes() {
            // Skip the plain-text and hidden helper syntaxes; the picker's own
            // "Auto" entry already covers "no highlighting".
            if syntax.name.is_empty() || syntax.name.starts_with('_') || syntax.name == "Plain Text" {
                continue;
            }
            let token = match syntax.file_extensions.first() {
                Some(ext) if !ext.is_empty() => ext.clone(),
                _ => continue,
            };
            if seen_tokens.contains(&token) || seen_names.contains(&syntax.name.to_ascii_lowercase()) {
                continue;
            }
            seen_tokens.insert(token.clone());
            seen_names.insert(syntax.name.to_ascii_lowercase());
            tail.push(PickerLang {
                icon: ICON_TOKENS.contains(&token.as_str()),
                token,
                name: syntax.name.clone(),
            });
        }
        tail.sort_by_key(|l| l.name.to_ascii_lowercase());
        out.extend(tail);
        out
    })
}

/// Best-effort language detection for a paste saved in **Auto** mode.
///
/// Three passes, cheapest first: the title's extension if syntect knows it, then
/// a first-line probe (shebangs, `<?php`, XML declarations, modelines), then a
/// keyword-scoring [`heuristic_language`] over the whole body — which is what
/// catches a bare `async def test():` that carries no shebang. Returns `None`
/// when nothing scores high enough, so prose falls back to plain text.
pub fn detect_language(code: &str, title: Option<&str>) -> Option<String> {
    let ps = syntaxes();
    if let Some(title) = title {
        if let Some(ext) = language_from_filename(title) {
            if ps.find_syntax_by_extension(&ext).is_some() {
                return Some(ext);
            }
        }
    }
    let first = code.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if let Some(syntax) = ps.find_syntax_by_first_line(first) {
        if let Some(ext) = syntax.file_extensions.first().filter(|e| !e.is_empty()) {
            return Some(ext.clone());
        }
    }
    heuristic_language(code)
}

/// Guesses a language from distinctive keywords in the body. Every rule is a set
/// of `(needle, weight)` pairs matched (lowercased) against the source; the
/// highest-scoring language wins if it clears a small threshold, which keeps
/// prose and ambiguous snippets as plain text. This is a heuristic, not a parser
/// — it only has to be right often enough to spare people the picker.
fn heuristic_language(code: &str) -> Option<String> {
    // Only the leading chunk matters and lowercasing the whole of a large paste
    // is wasted work — the signal is dense near the top.
    let head: String = code.chars().take(4096).collect::<String>().to_ascii_lowercase();

    #[rustfmt::skip]
    const RULES: &[(&str, &[(&str, i32)])] = &[
        ("py",   &[("async def", 4), ("def ", 2), ("elif", 3), ("import ", 1), ("print(", 2), ("__init__", 4), ("if __name__", 5), ("self.", 2), ("lambda ", 2)]),
        ("rs",   &[("fn ", 2), ("let mut", 4), ("println!", 4), ("impl ", 3), ("pub fn", 4), ("-> ", 1), ("use std", 3), (".unwrap()", 2), ("match ", 1), ("#[derive", 4)]),
        ("ts",   &[("interface ", 4), (": string", 3), (": number", 3), ("import type", 4), ("export const", 2), (": boolean", 3)]),
        ("js",   &[("console.log", 4), ("function ", 2), ("const ", 1), ("=>", 1), ("require(", 3), ("document.", 3), ("window.", 3), ("export default", 2)]),
        ("go",   &[("package main", 5), ("fmt.", 4), (":=", 2), ("func ", 1), ("import (", 3), ("interface{", 3)]),
        ("java", &[("public class", 4), ("system.out", 4), ("public static void main", 6), ("import java", 4)]),
        ("cs",   &[("using system", 5), ("console.writeline", 5), ("namespace ", 3), ("public void", 2)]),
        ("cpp",  &[("#include", 2), ("std::", 4), ("cout", 3), ("using namespace", 4), ("int main", 2), ("template<", 3)]),
        ("c",    &[("#include <", 2), ("printf(", 3), ("int main", 2), ("void ", 1), ("malloc(", 3)]),
        ("rb",   &[("puts ", 3), ("elsif", 4), ("attr_", 3), ("require '", 2), (".each do", 3), ("end\n", 1)]),
        ("php",  &[("<?php", 6), ("echo ", 2), ("$this->", 4), ("function ", 1)]),
        ("html", &[("<!doctype", 5), ("<html", 5), ("<div", 2), ("</", 1), ("<span", 2)]),
        ("sql",  &[("select ", 2), ("from ", 1), ("insert into", 4), ("create table", 4), ("where ", 1)]),
        ("sh",   &[("#!/bin", 4), ("fi\n", 2), ("then", 1), ("esac", 4), ("echo $", 2)]),
        ("css",  &[("@media", 4), ("px;", 1), ("margin:", 2), ("padding:", 2), ("color:", 1)]),
        ("yaml", &[("---\n", 3), ("- name:", 4), ("steps:", 2)]),
    ];

    let mut best: (&str, i32) = ("", 0);
    for (token, needles) in RULES {
        let score: i32 = needles.iter().filter(|(n, _)| head.contains(n)).map(|(_, w)| *w).sum();
        if score > best.1 {
            best = (token, score);
        }
    }
    if best.1 >= 3 {
        return Some(best.0.to_string());
    }

    // A `{…}`/`[…]` document with quoted keys is almost always JSON — but only if
    // nothing above matched more strongly (a JS object literal would have).
    let trimmed = code.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('[')) && code.contains('"') && code.contains(':') {
        return Some("json".to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_returns_one_entry_per_line() {
        let out = highlight(
            "fn main() {\n    println!(\"hi\");\n}\n",
            "rs",
            crate::codeimage::DEFAULT_THEME,
        );
        assert_eq!(out.lines.len(), 3);
        assert!(out.background.starts_with('#'));
        // Lines carry markup, not raw source.
        assert!(out.lines[0].contains("<span"));
        // No line may carry an embedded newline — syntect hides the terminator
        // inside the last span, and if it survives it renders as a blank line
        // between every row (viewer) and every join (editor overlay).
        assert!(
            out.lines.iter().all(|l| !l.contains('\n')),
            "a highlighted line kept its terminator: {:?}",
            out.lines
        );
    }

    #[test]
    fn highlight_never_returns_zero_lines() {
        assert_eq!(highlight("", "", crate::codeimage::DEFAULT_THEME).lines.len(), 1);
    }

    /// The reason this module exists. A markdown paste is a stranger's input; if
    /// raw HTML survived rendering, a `<script>` would run on our origin.
    #[test]
    fn markdown_strips_raw_html() {
        let html = markdown("# hi\n\n<script>alert(1)</script>\n\nnormal <b>text</b>\n");
        assert!(!html.contains("<script"));
        assert!(!html.contains("alert(1)"), "the script body leaked: {html}");
        assert!(!html.contains("<b>"), "inline HTML leaked: {html}");
        // The legitimate markdown still renders.
        assert!(html.contains("<h1>hi</h1>"));
    }

    #[test]
    fn markdown_neutralises_javascript_urls() {
        let html = markdown("[click](javascript:alert(1))\n\n![x](data:text/html;base64,PHN2Zz4=)\n");
        assert!(!html.contains("javascript:"), "{html}");
        assert!(!html.contains("data:"), "{html}");
        assert!(html.contains("href=\"#\""));
    }

    #[test]
    fn markdown_keeps_ordinary_links() {
        let html = markdown("[a](https://example.com) [b](/local) [c](#frag) [d](mailto:x@y.z)");
        assert!(html.contains("href=\"https://example.com\""));
        assert!(html.contains("href=\"/local\""));
        assert!(html.contains("href=\"#frag\""));
        assert!(html.contains("href=\"mailto:x@y.z\""));
    }

    #[test]
    fn markdown_escapes_text_nodes() {
        let html = markdown("a < b & c");
        assert!(html.contains("&lt;"));
        assert!(html.contains("&amp;"));
    }

    #[test]
    fn language_and_download_names() {
        assert_eq!(language_from_filename("main.rs").as_deref(), Some("rs"));
        assert_eq!(language_from_filename("Makefile"), None);
        assert_eq!(download_name("abc", Some("rs")), "abc.rs");
        assert_eq!(download_name("abc", None), "abc.txt");
    }

    #[test]
    fn picker_is_featured_first_then_exhaustive() {
        let langs = picker_languages();
        // The featured shortlist leads, so Rust is first — not `AppleScript`.
        assert_eq!(langs[0].token, "rs");
        assert!(
            langs.iter().any(|l| l.token == "rs" && l.icon),
            "rs should carry a logo"
        );
        // The featured shortlist plus syntect's tail makes a long list — well
        // beyond the old hand-curated two dozen.
        assert!(langs.len() > 60, "expected the full syntect set, got {}", langs.len());
        // Every vendored icon token that appears is flagged.
        for l in langs {
            assert_eq!(
                l.icon,
                ICON_TOKENS.contains(&l.token.as_str()),
                "icon flag wrong for {}",
                l.token
            );
        }
        // "Auto" is a template concern (empty token) — never a list row.
        assert!(langs.iter().all(|l| !l.token.is_empty()));
    }

    #[test]
    fn detect_language_uses_title_then_shebang_then_heuristics() {
        // Title extension wins when syntect knows it.
        assert_eq!(detect_language("print('hi')", Some("hello.py")).as_deref(), Some("py"));
        // Otherwise a shebang is enough.
        assert_eq!(
            detect_language("#!/usr/bin/env python3\nprint(1)", None).as_deref(),
            Some("py")
        );
        // The regression that prompted this: bare Python with no shebang.
        assert_eq!(
            detect_language("async def test():\n    return \"HI\"", None).as_deref(),
            Some("py")
        );
        // A few other bare snippets the heuristic should catch.
        assert_eq!(
            detect_language("fn main() { println!(\"hi\"); }", None).as_deref(),
            Some("rs")
        );
        assert_eq!(
            detect_language("SELECT * FROM users WHERE id = 1;", None).as_deref(),
            Some("sql")
        );
        assert_eq!(
            detect_language("{\n  \"a\": 1,\n  \"b\": [2, 3]\n}", None).as_deref(),
            Some("json")
        );
        // Prose with no signal stays undecided → caller renders plain text.
        assert_eq!(detect_language("just some words here", None), None);
    }
}
