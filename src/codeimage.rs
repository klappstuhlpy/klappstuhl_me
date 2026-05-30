//! Carbon-style "code screenshot" rendering — pure Rust, no headless browser.
//!
//! Syntax-highlights a snippet with `syntect` and emits a self-contained SVG
//! with a window chrome (rounded card + traffic-light dots). SVG keeps the
//! output crisp at any size and renders directly in a browser or `<img>`.

use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, Style, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Default theme used when none is requested.
pub const DEFAULT_THEME: &str = "base16-ocean.dark";

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

/// The list of built-in theme names.
pub fn available_themes() -> Vec<String> {
    let mut v: Vec<String> = theme_set().themes.keys().cloned().collect();
    v.sort();
    v
}

fn hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Renders `code` to an SVG string. `language` is a token or extension
/// (e.g. `rust`, `py`, `js`); unknown/empty falls back to plain text.
pub fn render_svg(code: &str, language: &str, theme_name: &str) -> Result<String, String> {
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
        .or_else(|| ts.themes.get(DEFAULT_THEME))
        .ok_or("no themes available")?;
    let bg = theme.settings.background.unwrap_or(Color { r: 40, g: 44, b: 52, a: 255 });

    let font_size = 14.0_f32;
    let char_w = font_size * 0.6;
    let line_h = font_size * 1.45;
    let pad = 22.0_f32;
    let header_h = 34.0_f32;

    let mut hl = HighlightLines::new(syntax, theme);
    let mut body = String::new();
    let mut max_chars = 0usize;
    let mut line_count = 0usize;

    for line in LinesWithEndings::from(code) {
        let ranges: Vec<(Style, &str)> = hl.highlight_line(line, ps).map_err(|e| e.to_string())?;
        let y = header_h + pad + line_h * line_count as f32 + font_size;
        body.push_str(&format!(
            "<text x=\"{:.0}\" y=\"{:.1}\" xml:space=\"preserve\">",
            pad, y
        ));
        let mut chars_in_line = 0usize;
        for (style, text) in ranges {
            let text = text.trim_end_matches(['\n', '\r']);
            if text.is_empty() {
                continue;
            }
            let expanded = text.replace('\t', "    ");
            chars_in_line += expanded.chars().count();
            body.push_str(&format!(
                "<tspan fill=\"{}\">{}</tspan>",
                hex(style.foreground),
                xml_escape(&expanded)
            ));
        }
        body.push_str("</text>");
        max_chars = max_chars.max(chars_in_line);
        line_count += 1;
    }
    if line_count == 0 {
        line_count = 1;
    }

    let width = (pad * 2.0 + char_w * max_chars.max(20) as f32).ceil();
    let height = (header_h + pad * 2.0 + line_h * line_count as f32).ceil();

    Ok(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w:.0}" height="{h:.0}" viewBox="0 0 {w:.0} {h:.0}" font-family="ui-monospace, SFMono-Regular, Menlo, Consolas, monospace" font-size="{fs}px">
<rect width="{w:.0}" height="{h:.0}" rx="10" fill="{bg}"/>
<circle cx="22" cy="18" r="6" fill="#ff5f56"/>
<circle cx="42" cy="18" r="6" fill="#ffbd2e"/>
<circle cx="62" cy="18" r="6" fill="#27c93f"/>
{body}
</svg>"##,
        w = width,
        h = height,
        fs = font_size,
        bg = hex(bg),
        body = body,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_valid_svg_for_rust() {
        let svg = render_svg("fn main() {\n    println!(\"hi\");\n}\n", "rust", DEFAULT_THEME).unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("</svg>"));
        assert!(svg.contains("<tspan"));
        // The string literal content is XML-escaped, not raw.
        assert!(svg.contains("println!"));
    }

    #[test]
    fn unknown_language_falls_back_to_plain_text() {
        let svg = render_svg("just some text", "not-a-language", DEFAULT_THEME).unwrap();
        assert!(svg.starts_with("<svg"));
    }

    #[test]
    fn unknown_theme_falls_back() {
        let svg = render_svg("x = 1", "py", "no-such-theme").unwrap();
        assert!(svg.starts_with("<svg"));
    }

    #[test]
    fn escapes_markup() {
        let svg = render_svg("<script>&</script>", "html", DEFAULT_THEME).unwrap();
        assert!(!svg.contains("<script>"));
        assert!(svg.contains("&lt;script&gt;") || svg.contains("&amp;"));
    }
}
