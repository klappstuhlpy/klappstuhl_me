//! Chart rendering endpoint (`POST /api/v1/render/chart`).
//!
//! A pure-Rust SVG chart renderer (no headless browser, no JS): line, area,
//! bar, scatter, pie, and donut charts from a JSON spec, styled after the
//! site's terminal aesthetic. Both themes use categorical palettes that were
//! validated for colorblind-adjacent separation and contrast against the
//! chart surface — series colors are assigned in this fixed order and never
//! cycled, which is why a request is capped at [`MAX_SERIES`] series.
//!
//! Lives under the `render` tag alongside the code screenshot and QR
//! renderers and is gated by the same `images:read` scope.

use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::fmt::Write as _;
use utoipa::ToSchema;

use super::auth::ApiToken;
use super::utils::{ApiJson, RateLimitResponse};
use crate::{error::ApiError, headers::ClientIp, models::Scope, AppState};

/// The palette size; series beyond this have no validated color, so the
/// request is rejected (fold extra categories into an "Other" series).
const MAX_SERIES: usize = 7;
/// Per-series point cap.
const MAX_POINTS_PER_SERIES: usize = 2000;
/// Whole-request point cap.
const MAX_TOTAL_POINTS: usize = 8000;
/// Longest accepted series/category label (longer ones are refused, not cut).
const MAX_LABEL_CHARS: usize = 48;
/// Longest accepted title.
const MAX_TITLE_CHARS: usize = 120;

const MIN_W: u32 = 320;
const MAX_W: u32 = 1600;
const DEFAULT_W: u32 = 860;
const MIN_H: u32 = 240;
const MAX_H: u32 = 1000;
const DEFAULT_H: u32 = 480;

const FONT: &str = "'JetBrains Mono','Cascadia Code',ui-monospace,Consolas,monospace";

// ─── Request types ──────────────────────────────────────────────────────────────

/// The chart form. Pick by the data's job: `line`/`area` for change over time,
/// `bar` for magnitude comparison, `scatter` for correlation, `pie`/`donut`
/// for a part-to-whole split (single series, ≤ 7 slices).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChartKind {
    Line,
    Area,
    Bar,
    Scatter,
    Pie,
    Donut,
}

/// Color theme. `dark` (the default) matches the site; `light` is stepped for
/// a light surface, not an automatic inversion.
#[derive(Debug, Clone, Copy, Default, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChartTheme {
    #[default]
    Dark,
    Light,
}

/// One data point: a bare y-value (x becomes the point's index / the matching
/// `labels` entry) or an explicit `[x, y]` pair (line and scatter only).
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum DataPoint {
    /// A bare y-value.
    Y(f64),
    /// An `[x, y]` pair.
    Xy([f64; 2]),
}

/// One plotted series.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ChartSeries {
    /// Series name, shown in the legend (and as the pie's slice-label fallback).
    pub label: String,
    /// The points: numbers (`[3, 1, 4]`) or `[x, y]` pairs (`[[0, 3], [2, 1]]`).
    pub data: Vec<DataPoint>,
}

/// Body of a chart render request.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ChartRequest {
    /// The chart form.
    pub kind: ChartKind,
    /// Optional title drawn above the plot.
    #[serde(default)]
    pub title: Option<String>,
    /// The series to plot (1–7). Pie/donut take exactly one.
    pub series: Vec<ChartSeries>,
    /// Category labels along the x-axis (or the slice labels for pie/donut).
    /// Missing entries fall back to the point index.
    #[serde(default)]
    pub labels: Vec<String>,
    /// `dark` (default) or `light`.
    #[serde(default)]
    pub theme: ChartTheme,
    /// Image width in px (clamped to 320..=1600, default 860).
    #[serde(default)]
    pub width: Option<u32>,
    /// Image height in px (clamped to 240..=1000, default 480).
    #[serde(default)]
    pub height: Option<u32>,
    /// Optional y-axis caption.
    #[serde(default)]
    pub y_label: Option<String>,
    /// Optional x-axis caption.
    #[serde(default)]
    pub x_label: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ChartQuery {
    #[serde(default)]
    share: Option<bool>,
}

// ─── Theme ──────────────────────────────────────────────────────────────────────

/// Resolved theme colors. The categorical `series` orders are load-bearing:
/// each was chosen (per surface) to maximize the minimum adjacent-pair
/// colorblind ΔE and validated ≥ 3:1 contrast on the dark surface — do not
/// reorder or extend without re-validating.
struct Theme {
    surface: &'static str,
    ink: &'static str,
    ink_secondary: &'static str,
    ink_muted: &'static str,
    grid: &'static str,
    baseline: &'static str,
    series: [&'static str; MAX_SERIES],
}

impl ChartTheme {
    fn resolve(self) -> Theme {
        match self {
            ChartTheme::Dark => Theme {
                surface: "#0e0e10",
                ink: "#fafafa",
                ink_secondary: "#a1a1aa",
                ink_muted: "#71717a",
                grid: "#26262a",
                baseline: "#3a3a3e",
                series: [
                    "#d4714f", "#3987e5", "#199e70", "#c98500", "#d55181", "#9085e9", "#008300",
                ],
            },
            ChartTheme::Light => Theme {
                surface: "#fffefb",
                ink: "#1f1e1d",
                ink_secondary: "#52514e",
                ink_muted: "#6b6a66",
                grid: "#e7e3da",
                baseline: "#d8d4cc",
                series: [
                    "#d97757", "#2a78d6", "#1baf7a", "#4a3aa7", "#eda100", "#e87ba4", "#008300",
                ],
            },
        }
    }
}

// ─── Endpoint ───────────────────────────────────────────────────────────────────

/// Render a chart
///
/// Renders a line, area, bar, scatter, pie, or donut chart from a JSON spec as
/// an SVG image — no client-side charting library needed.
///
/// The result is returned as `image/svg+xml`, or — with `?share=true` — as
/// JSON `{id, url, content_type}` carrying a short `/m/:id` link to the stored
/// SVG. Series colors come from a fixed, colorblind-validated palette, so at
/// most 7 series (or pie slices) are accepted — fold the rest into an "Other"
/// category.
#[utoipa::path(
    post,
    path = "/render/chart",
    request_body = ChartRequest,
    params(("share" = Option<bool>, Query, description = "Return JSON with a stored short link instead of the raw SVG.")),
    responses(
        (status = 200, description = "The rendered SVG", content_type = "image/svg+xml", body = String),
        (status = 400, description = "Invalid chart spec (bad series/labels/values)", body = ApiError),
        (status = 401, description = "User is unauthenticated", body = ApiError),
        (status = 429, response = RateLimitResponse),
    ),
    security(("api_key" = ["images:read"])),
    tag = "render"
)]
pub async fn render_chart(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    auth: ApiToken,
    Query(query): Query<ChartQuery>,
    ApiJson(req): ApiJson<ChartRequest>,
) -> Result<Response, ApiError> {
    let account = auth.require_account(&state, Scope::ImagesRead).await?;

    let svg = render_svg(&req).map_err(|(field, msg)| ApiError::validation(field, msg))?;

    state.audit("api.render.chart").actor(&account).ip_opt(client_ip).fire();

    if query.share.unwrap_or(false) {
        let id = state.store_media(svg.into_bytes(), "image/svg+xml");
        let url = state.config().url_to(format!("/m/{id}"));
        return Ok(ApiJson(serde_json::json!({
            "id": id,
            "url": url,
            "content_type": "image/svg+xml",
        }))
        .into_response());
    }

    Ok(([(header::CONTENT_TYPE, "image/svg+xml".to_string())], svg).into_response())
}

// ─── Validation + normalisation ─────────────────────────────────────────────────

type RenderError = (&'static str, String);

fn err(field: &'static str, msg: impl Into<String>) -> RenderError {
    (field, msg.into())
}

/// A series normalised to explicit (x, y) points.
struct NormSeries {
    label: String,
    points: Vec<(f64, f64)>,
}

/// Validates the request and normalises every series to (x, y) pairs. Bare
/// y-values get their index as x. Explicit pairs are only meaningful where x
/// is a continuous axis (line/area/scatter).
fn normalize(req: &ChartRequest) -> Result<Vec<NormSeries>, RenderError> {
    if req.series.is_empty() {
        return Err(err("series", "at least one series is required"));
    }
    if req.series.len() > MAX_SERIES {
        return Err(err(
            "series",
            format!("at most {MAX_SERIES} series are supported — fold the rest into an \"Other\" category"),
        ));
    }
    if matches!(req.kind, ChartKind::Pie | ChartKind::Donut) && req.series.len() != 1 {
        return Err(err("series", "pie/donut charts take exactly one series"));
    }
    if let Some(title) = &req.title {
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(err("title", format!("title too long ({MAX_TITLE_CHARS} chars max)")));
        }
    }
    for label in req.labels.iter().chain(req.series.iter().map(|s| &s.label)) {
        if label.chars().count() > MAX_LABEL_CHARS {
            return Err(err("labels", format!("label too long ({MAX_LABEL_CHARS} chars max)")));
        }
    }

    let mut total = 0usize;
    let mut out = Vec::with_capacity(req.series.len());
    let pairs_ok = matches!(req.kind, ChartKind::Line | ChartKind::Area | ChartKind::Scatter);

    for series in &req.series {
        if series.data.is_empty() {
            return Err(err("series", format!("series `{}` has no data", series.label)));
        }
        if series.data.len() > MAX_POINTS_PER_SERIES {
            return Err(err(
                "series",
                format!("series `{}` exceeds {MAX_POINTS_PER_SERIES} points", series.label),
            ));
        }
        total += series.data.len();
        if total > MAX_TOTAL_POINTS {
            return Err(err(
                "series",
                format!("too many points overall ({MAX_TOTAL_POINTS} max)"),
            ));
        }

        let mut points = Vec::with_capacity(series.data.len());
        for (i, point) in series.data.iter().enumerate() {
            let (x, y) = match point {
                DataPoint::Y(y) => (i as f64, *y),
                DataPoint::Xy([x, y]) => {
                    if !pairs_ok {
                        return Err(err(
                            "series",
                            "`[x, y]` pairs are only supported for line, area, and scatter charts",
                        ));
                    }
                    (*x, *y)
                }
            };
            if !x.is_finite() || !y.is_finite() {
                return Err(err("series", "data values must be finite numbers"));
            }
            points.push((x, y));
        }
        out.push(NormSeries {
            label: series.label.clone(),
            points,
        });
    }

    if matches!(req.kind, ChartKind::Pie | ChartKind::Donut) {
        let slices = &out[0].points;
        if slices.len() > MAX_SERIES {
            return Err(err(
                "series",
                format!("at most {MAX_SERIES} slices are supported — fold the rest into an \"Other\" slice"),
            ));
        }
        if slices.iter().any(|(_, v)| *v < 0.0) {
            return Err(err("series", "pie/donut values must be non-negative"));
        }
        if slices.iter().map(|(_, v)| v).sum::<f64>() <= 0.0 {
            return Err(err("series", "pie/donut values must sum to more than zero"));
        }
    }

    Ok(out)
}

// ─── Scale helpers ──────────────────────────────────────────────────────────────

/// Rounds a raw step up to the nearest 1/2/5 × 10ⁿ.
fn nice_step(raw: f64) -> f64 {
    let mag = 10f64.powf(raw.abs().log10().floor());
    let norm = raw / mag;
    let n = if norm <= 1.0 {
        1.0
    } else if norm <= 2.0 {
        2.0
    } else if norm <= 5.0 {
        5.0
    } else {
        10.0
    };
    n * mag
}

/// Evenly spaced "nice" tick values covering [min, max].
fn nice_ticks(min: f64, max: f64, target: usize) -> Vec<f64> {
    let (min, max) = if (max - min).abs() < f64::EPSILON {
        // Degenerate domain (single value): pad it so a scale still exists.
        let pad = if min == 0.0 { 1.0 } else { min.abs() * 0.5 };
        (min - pad, max + pad)
    } else {
        (min, max)
    };
    let step = nice_step((max - min) / target.max(2) as f64);
    let start = (min / step).floor() * step;
    let mut ticks = Vec::new();
    let mut i = 0u32;
    loop {
        let t = start + step * i as f64;
        // Snap float accumulation noise to the step grid.
        let t = (t / step).round() * step;
        ticks.push(t);
        if t >= max || ticks.len() > 12 {
            break;
        }
        i += 1;
    }
    ticks
}

/// Short human tick format: `1500000` → `1.5M`, `0.25` → `0.25`.
fn fmt_num(v: f64) -> String {
    let a = v.abs();
    let scaled = if a >= 1e9 {
        Some((v / 1e9, "B"))
    } else if a >= 1e6 {
        Some((v / 1e6, "M"))
    } else if a >= 1e4 {
        Some((v / 1e3, "k"))
    } else {
        None
    };
    match scaled {
        Some((s, suffix)) => {
            let mut t = format!("{s:.1}");
            if t.ends_with(".0") {
                t.truncate(t.len() - 2);
            }
            format!("{t}{suffix}")
        }
        None => {
            if v.fract().abs() < 1e-9 {
                format!("{}", v.round() as i64)
            } else {
                let mut t = format!("{v:.2}");
                while t.ends_with('0') {
                    t.truncate(t.len() - 1);
                }
                if t.ends_with('.') {
                    t.truncate(t.len() - 1);
                }
                t
            }
        }
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Rough text width at the 11px mono size (used for layout, not truncation).
fn text_w(s: &str, font_px: f64) -> f64 {
    s.chars().count() as f64 * font_px * 0.62
}

// ─── Rendering ──────────────────────────────────────────────────────────────────

/// Pure render: chart spec → SVG document. Returns `(field, message)` on
/// invalid specs so the handler can shape a validation error.
fn render_svg(req: &ChartRequest) -> Result<String, RenderError> {
    let series = normalize(req)?;
    let theme = req.theme.resolve();
    let w = req.width.unwrap_or(DEFAULT_W).clamp(MIN_W, MAX_W) as f64;
    let h = req.height.unwrap_or(DEFAULT_H).clamp(MIN_H, MAX_H) as f64;

    let mut svg = String::with_capacity(16 * 1024);
    let _ = write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="{FONT}">"#
    );
    let _ = write!(svg, r#"<rect width="{w}" height="{h}" fill="{}"/>"#, theme.surface);

    // Header: title + legend. The legend is always drawn for ≥ 2 series (and
    // for every pie/donut, where slices are the categories) — it doubles as
    // the visible-label relief for low-contrast light-theme slots.
    let mut cursor_y = 16.0;
    if let Some(title) = req.title.as_deref().filter(|t| !t.trim().is_empty()) {
        cursor_y += 8.0;
        let _ = write!(
            svg,
            r#"<text x="20" y="{cursor_y}" font-size="15" font-weight="600" fill="{}">{}</text>"#,
            theme.ink,
            xml_escape(title.trim())
        );
        cursor_y += 14.0;
    }

    let is_pie = matches!(req.kind, ChartKind::Pie | ChartKind::Donut);
    let legend_entries: Vec<(String, &str)> = if is_pie {
        series[0]
            .points
            .iter()
            .enumerate()
            .map(|(i, _)| (category_label(&req.labels, i), theme.series[i % MAX_SERIES]))
            .collect()
    } else if series.len() >= 2 {
        series
            .iter()
            .enumerate()
            .map(|(i, s)| (s.label.clone(), theme.series[i]))
            .collect()
    } else {
        Vec::new()
    };

    if !legend_entries.is_empty() {
        let mut x = 20.0;
        let mut y = cursor_y + 12.0;
        for (label, color) in &legend_entries {
            let item_w = 14.0 + text_w(label, 11.0) + 18.0;
            if x + item_w > w - 16.0 && x > 20.0 {
                x = 20.0;
                y += 18.0;
            }
            let _ = write!(
                svg,
                r#"<rect x="{x}" y="{}" width="10" height="10" rx="2" fill="{color}"/>"#,
                y - 9.0
            );
            let _ = write!(
                svg,
                r#"<text x="{}" y="{y}" font-size="11" fill="{}">{}</text>"#,
                x + 14.0,
                theme.ink_secondary,
                xml_escape(label)
            );
            x += item_w;
        }
        cursor_y = y + 8.0;
    } else {
        cursor_y += 6.0;
    }

    if is_pie {
        render_pie(&mut svg, req, &series[0], &theme, w, h, cursor_y);
    } else {
        render_xy(&mut svg, req, &series, &theme, w, h, cursor_y)?;
    }

    svg.push_str("</svg>");
    Ok(svg)
}

/// The label for category slot `i`: the caller-provided label or the index.
fn category_label(labels: &[String], i: usize) -> String {
    labels.get(i).cloned().unwrap_or_else(|| i.to_string())
}

/// Shared cartesian renderer for line/area/bar/scatter.
#[allow(clippy::too_many_arguments)]
fn render_xy(
    svg: &mut String,
    req: &ChartRequest,
    series: &[NormSeries],
    theme: &Theme,
    w: f64,
    h: f64,
    top: f64,
) -> Result<(), RenderError> {
    let is_bar = req.kind == ChartKind::Bar;
    let categorical = is_bar
        || series
            .iter()
            .all(|s| s.points.iter().enumerate().all(|(i, p)| p.0 == i as f64));
    let n_slots = series.iter().map(|s| s.points.len()).max().unwrap_or(0);

    // Y domain. Bars and areas are anchored to a zero baseline.
    let mut y_min = f64::INFINITY;
    let mut y_max = f64::NEG_INFINITY;
    for p in series.iter().flat_map(|s| &s.points) {
        y_min = y_min.min(p.1);
        y_max = y_max.max(p.1);
    }
    if matches!(req.kind, ChartKind::Bar | ChartKind::Area) {
        y_min = y_min.min(0.0);
        y_max = y_max.max(0.0);
    }
    let y_ticks = nice_ticks(y_min, y_max, 5);
    let (y_lo, y_hi) = (y_ticks[0], *y_ticks.last().unwrap());

    // X domain (continuous charts).
    let mut x_min = f64::INFINITY;
    let mut x_max = f64::NEG_INFINITY;
    for p in series.iter().flat_map(|s| &s.points) {
        x_min = x_min.min(p.0);
        x_max = x_max.max(p.0);
    }
    if (x_max - x_min).abs() < f64::EPSILON {
        x_min -= 0.5;
        x_max += 0.5;
    }

    // Margins. The left gutter fits the widest y tick; the right gutter fits
    // the line-end direct labels when they're drawn.
    let y_tick_labels: Vec<String> = y_ticks.iter().map(|t| fmt_num(*t)).collect();
    let left = 20.0
        + y_tick_labels.iter().map(|t| text_w(t, 11.0)).fold(0.0, f64::max)
        + 10.0
        + if req.y_label.is_some() { 18.0 } else { 0.0 };
    let end_labels = matches!(req.kind, ChartKind::Line | ChartKind::Area) && series.len() >= 2 && series.len() <= 4;
    let right = if end_labels {
        16.0 + series.iter().map(|s| text_w(&s.label, 11.0)).fold(0.0, f64::max) + 8.0
    } else {
        20.0
    };
    let bottom = h - 24.0 - if req.x_label.is_some() { 18.0 } else { 0.0 };
    let plot_x = left;
    let plot_w = (w - right - left).max(40.0);
    let plot_y = top + 6.0;
    let plot_h = (bottom - 16.0 - plot_y).max(40.0);

    let sx = |x: f64| plot_x + (x - x_min) / (x_max - x_min) * plot_w;
    let sy = |y: f64| plot_y + plot_h - (y - y_lo) / (y_hi - y_lo) * plot_h;

    // Grid + y ticks (recessive hairlines; the zero line gets the baseline tone).
    for (tick, label) in y_ticks.iter().zip(&y_tick_labels) {
        let y = sy(*tick);
        let color = if tick.abs() < f64::EPSILON {
            theme.baseline
        } else {
            theme.grid
        };
        let _ = write!(
            svg,
            r#"<line x1="{plot_x}" y1="{y:.2}" x2="{:.2}" y2="{y:.2}" stroke="{color}" stroke-width="1"/>"#,
            plot_x + plot_w
        );
        let _ = write!(
            svg,
            r#"<text x="{:.2}" y="{:.2}" font-size="11" fill="{}" text-anchor="end">{label}</text>"#,
            plot_x - 8.0,
            y + 4.0,
            theme.ink_muted
        );
    }
    // Bottom axis border when zero isn't inside the domain.
    if y_lo > 0.0 || y_hi < 0.0 {
        let _ = write!(
            svg,
            r#"<line x1="{plot_x}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="{}" stroke-width="1"/>"#,
            plot_y + plot_h,
            plot_x + plot_w,
            plot_y + plot_h,
            theme.baseline
        );
    }

    // X labels.
    if categorical {
        // Thin category labels to whatever fits without collisions.
        let max_label_w = (0..n_slots)
            .map(|i| text_w(&category_label(&req.labels, i), 11.0))
            .fold(0.0, f64::max)
            + 10.0;
        let step = ((n_slots as f64 * max_label_w) / plot_w).ceil().max(1.0) as usize;
        let band = plot_w / n_slots.max(1) as f64;
        for i in (0..n_slots).step_by(step) {
            let cx = if is_bar {
                plot_x + band * (i as f64 + 0.5)
            } else {
                sx(i as f64)
            };
            let _ = write!(
                svg,
                r#"<text x="{cx:.2}" y="{:.2}" font-size="11" fill="{}" text-anchor="middle">{}</text>"#,
                plot_y + plot_h + 16.0,
                theme.ink_muted,
                xml_escape(&category_label(&req.labels, i))
            );
        }
    } else {
        for tick in nice_ticks(x_min, x_max, 6) {
            if tick < x_min - f64::EPSILON || tick > x_max + f64::EPSILON {
                continue;
            }
            let _ = write!(
                svg,
                r#"<text x="{:.2}" y="{:.2}" font-size="11" fill="{}" text-anchor="middle">{}</text>"#,
                sx(tick),
                plot_y + plot_h + 16.0,
                theme.ink_muted,
                fmt_num(tick)
            );
        }
    }

    // Axis captions.
    if let Some(label) = req.x_label.as_deref() {
        let _ = write!(
            svg,
            r#"<text x="{:.2}" y="{:.2}" font-size="11" fill="{}" text-anchor="middle">{}</text>"#,
            plot_x + plot_w / 2.0,
            h - 10.0,
            theme.ink_secondary,
            xml_escape(label)
        );
    }
    if let Some(label) = req.y_label.as_deref() {
        let cy = plot_y + plot_h / 2.0;
        let _ = write!(
            svg,
            r#"<text x="14" y="{cy:.2}" font-size="11" fill="{}" text-anchor="middle" transform="rotate(-90 14 {cy:.2})">{}</text>"#,
            theme.ink_secondary,
            xml_escape(label)
        );
    }

    // Marks.
    match req.kind {
        ChartKind::Bar => {
            let zero_y = sy(y_lo.max(0.0).min(y_hi));
            let band = plot_w / n_slots.max(1) as f64;
            let group_pad = (band * 0.18).max(2.0);
            let bar_w = ((band - group_pad * 2.0 - 2.0 * (series.len() as f64 - 1.0)) / series.len() as f64).max(1.0);
            for (si, s) in series.iter().enumerate() {
                for (i, p) in s.points.iter().enumerate() {
                    let x = plot_x + band * i as f64 + group_pad + (bar_w + 2.0) * si as f64;
                    let y = sy(p.1);
                    let (top_y, bar_h) = if y <= zero_y {
                        (y, zero_y - y)
                    } else {
                        (zero_y, y - zero_y)
                    };
                    let r = 3.0f64.min(bar_w / 2.0).min(bar_h);
                    // Rounded at the data end only, anchored flat at the baseline.
                    let path = if p.1 >= 0.0 {
                        format!(
                            "M{x:.2} {b:.2} V{ty:.2} q0 -{r:.2} {r:.2} -{r:.2} h{iw:.2} q{r:.2} 0 {r:.2} {r:.2} V{b:.2} Z",
                            b = top_y + bar_h,
                            ty = top_y + r,
                            iw = bar_w - 2.0 * r
                        )
                    } else {
                        format!(
                            "M{x:.2} {t:.2} V{by:.2} q0 {r:.2} {r:.2} {r:.2} h{iw:.2} q{r:.2} 0 {r:.2} -{r:.2} V{t:.2} Z",
                            t = top_y,
                            by = top_y + bar_h - r,
                            iw = bar_w - 2.0 * r
                        )
                    };
                    let _ = write!(svg, r#"<path d="{path}" fill="{}"/>"#, theme.series[si]);
                }
            }
        }
        ChartKind::Line | ChartKind::Area => {
            for (si, s) in series.iter().enumerate() {
                let mut points = s.points.clone();
                points.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut d = String::new();
                for (i, p) in points.iter().enumerate() {
                    let _ = write!(d, "{}{:.2} {:.2}", if i == 0 { "M" } else { "L" }, sx(p.0), sy(p.1));
                }
                if req.kind == ChartKind::Area {
                    let base = sy(y_lo.max(0.0).min(y_hi));
                    let mut fill = d.clone();
                    let _ = write!(
                        fill,
                        "L{:.2} {base:.2}L{:.2} {base:.2}Z",
                        sx(points.last().unwrap().0),
                        sx(points[0].0)
                    );
                    let _ = write!(
                        svg,
                        r#"<path d="{fill}" fill="{}" fill-opacity="0.18"/>"#,
                        theme.series[si]
                    );
                }
                let _ = write!(
                    svg,
                    r#"<path d="{d}" fill="none" stroke="{}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>"#,
                    theme.series[si]
                );
                // Small point markers when the series is sparse enough to read them.
                if points.len() <= 30 {
                    for p in &points {
                        let _ = write!(
                            svg,
                            r#"<circle cx="{:.2}" cy="{:.2}" r="3" fill="{}" stroke="{}" stroke-width="1.5"/>"#,
                            sx(p.0),
                            sy(p.1),
                            theme.series[si],
                            theme.surface
                        );
                    }
                }
            }
            if end_labels {
                // Direct labels at the line ends, nudged apart vertically.
                let mut ends: Vec<(f64, String)> = series
                    .iter()
                    .map(|s| {
                        let last = s.points.iter().max_by(|a, b| a.0.total_cmp(&b.0)).unwrap();
                        (sy(last.1), s.label.clone())
                    })
                    .collect();
                ends.sort_by(|a, b| a.0.total_cmp(&b.0));
                for i in 1..ends.len() {
                    if ends[i].0 - ends[i - 1].0 < 13.0 {
                        ends[i].0 = ends[i - 1].0 + 13.0;
                    }
                }
                for (y, label) in ends {
                    let _ = write!(
                        svg,
                        r#"<text x="{:.2}" y="{:.2}" font-size="11" fill="{}">{}</text>"#,
                        plot_x + plot_w + 6.0,
                        y + 4.0,
                        theme.ink_secondary,
                        xml_escape(&label)
                    );
                }
            }
        }
        ChartKind::Scatter => {
            for (si, s) in series.iter().enumerate() {
                for p in &s.points {
                    let _ = write!(
                        svg,
                        r#"<circle cx="{:.2}" cy="{:.2}" r="4" fill="{}" stroke="{}" stroke-width="2"/>"#,
                        sx(p.0),
                        sy(p.1),
                        theme.series[si],
                        theme.surface
                    );
                }
            }
        }
        ChartKind::Pie | ChartKind::Donut => unreachable!("pie is rendered by render_pie"),
    }

    Ok(())
}

/// Pie/donut renderer: slices with a 2px surface gap and direct labels
/// (`name pct%`) for slices big enough to carry one.
fn render_pie(svg: &mut String, req: &ChartRequest, series: &NormSeries, theme: &Theme, w: f64, h: f64, top: f64) {
    let total: f64 = series.points.iter().map(|(_, v)| v).sum();
    let cx = w / 2.0;
    let cy = top + (h - top) / 2.0;
    let r = ((h - top) / 2.0 - 34.0).min(w / 2.0 - 110.0).max(40.0);
    let inner = if req.kind == ChartKind::Donut { r * 0.55 } else { 0.0 };

    let mut angle = -std::f64::consts::FRAC_PI_2; // start at 12 o'clock
    for (i, (_, value)) in series.points.iter().enumerate() {
        let frac = value / total;
        if frac <= 0.0 {
            continue;
        }
        let sweep = frac * std::f64::consts::TAU;
        let (a0, a1) = (angle, angle + sweep);
        angle = a1;

        let (x0, y0) = (cx + r * a0.cos(), cy + r * a0.sin());
        let (x1, y1) = (cx + r * a1.cos(), cy + r * a1.sin());
        let large = i32::from(sweep > std::f64::consts::PI);
        let path = if inner > 0.0 {
            let (ix0, iy0) = (cx + inner * a1.cos(), cy + inner * a1.sin());
            let (ix1, iy1) = (cx + inner * a0.cos(), cy + inner * a0.sin());
            format!(
                "M{x0:.2} {y0:.2}A{r:.2} {r:.2} 0 {large} 1 {x1:.2} {y1:.2}L{ix0:.2} {iy0:.2}A{inner:.2} {inner:.2} 0 {large} 0 {ix1:.2} {iy1:.2}Z"
            )
        } else if frac >= 1.0 - 1e-9 {
            // A single 100% slice: a full circle (an arc with identical
            // endpoints renders as nothing).
            format!(
                "M{cx:.2} {t:.2}A{r:.2} {r:.2} 0 1 1 {cx:.2} {b:.2}A{r:.2} {r:.2} 0 1 1 {cx:.2} {t:.2}Z",
                t = cy - r,
                b = cy + r
            )
        } else {
            format!("M{cx:.2} {cy:.2}L{x0:.2} {y0:.2}A{r:.2} {r:.2} 0 {large} 1 {x1:.2} {y1:.2}Z")
        };
        let _ = write!(
            svg,
            r#"<path d="{path}" fill="{}" stroke="{}" stroke-width="2"/>"#,
            theme.series[i % MAX_SERIES],
            theme.surface
        );

        // Direct label outside the slice; tiny slices rely on the legend.
        if frac >= 0.04 {
            let mid = (a0 + a1) / 2.0;
            let (lx, ly) = (cx + (r + 14.0) * mid.cos(), cy + (r + 14.0) * mid.sin());
            let anchor = if mid.cos() < -0.2 {
                "end"
            } else if mid.cos() > 0.2 {
                "start"
            } else {
                "middle"
            };
            let _ = write!(
                svg,
                r#"<text x="{lx:.2}" y="{ly:.2}" font-size="11" fill="{}" text-anchor="{anchor}">{} {:.0}%</text>"#,
                theme.ink_secondary,
                xml_escape(&category_label(&req.labels, i)),
                frac * 100.0
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(kind: ChartKind, series: Vec<ChartSeries>) -> ChartRequest {
        ChartRequest {
            kind,
            title: Some("Test chart".into()),
            series,
            labels: vec![],
            theme: ChartTheme::Dark,
            width: None,
            height: None,
            y_label: None,
            x_label: None,
        }
    }

    fn ys(label: &str, values: &[f64]) -> ChartSeries {
        ChartSeries {
            label: label.into(),
            data: values.iter().map(|v| DataPoint::Y(*v)).collect(),
        }
    }

    #[test]
    fn nice_ticks_cover_domain() {
        let ticks = nice_ticks(0.0, 97.0, 5);
        assert_eq!(ticks[0], 0.0);
        assert!(*ticks.last().unwrap() >= 97.0);
        // Degenerate single-value domains still produce a usable scale.
        let flat = nice_ticks(5.0, 5.0, 5);
        assert!(flat.len() >= 2);
        assert!(flat[0] <= 5.0 && *flat.last().unwrap() >= 5.0);
    }

    #[test]
    fn fmt_num_shortens() {
        assert_eq!(fmt_num(1_500_000.0), "1.5M");
        assert_eq!(fmt_num(2_000_000_000.0), "2B");
        assert_eq!(fmt_num(12_000.0), "12k");
        assert_eq!(fmt_num(5000.0), "5000");
        assert_eq!(fmt_num(0.25), "0.25");
        assert_eq!(fmt_num(-3.0), "-3");
    }

    #[test]
    fn renders_line_with_legend_and_end_labels() {
        let req = base(
            ChartKind::Line,
            vec![ys("cpu", &[1.0, 4.0, 2.0]), ys("ram", &[2.0, 3.0, 5.0])],
        );
        let svg = render_svg(&req).unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("Test chart"));
        // Legend + line-end direct labels both name the series.
        assert!(svg.matches("cpu").count() >= 2);
        assert_eq!(svg.matches(r#"stroke-width="2" stroke-linejoin"#).count(), 2);
    }

    #[test]
    fn renders_bars_including_negatives() {
        let req = base(ChartKind::Bar, vec![ys("delta", &[3.0, -2.0, 5.0])]);
        let svg = render_svg(&req).unwrap();
        // One rounded-end bar path per value.
        assert_eq!(svg.matches("<path").count(), 3);
    }

    #[test]
    fn renders_pie_with_percentages() {
        let mut req = base(ChartKind::Pie, vec![ys("split", &[3.0, 1.0])]);
        req.labels = vec!["a".into(), "b".into()];
        let svg = render_svg(&req).unwrap();
        assert!(svg.contains("a 75%"));
        assert!(svg.contains("b 25%"));
        // Donut variant renders ring paths.
        req.kind = ChartKind::Donut;
        assert!(render_svg(&req).is_ok());
    }

    #[test]
    fn scatter_takes_xy_pairs_but_bar_does_not() {
        let pair_series = ChartSeries {
            label: "points".into(),
            data: vec![DataPoint::Xy([1.0, 2.0]), DataPoint::Xy([3.0, 4.0])],
        };
        let req = base(ChartKind::Scatter, vec![pair_series]);
        assert!(render_svg(&req).is_ok());

        let pair_series = ChartSeries {
            label: "points".into(),
            data: vec![DataPoint::Xy([1.0, 2.0])],
        };
        let req = base(ChartKind::Bar, vec![pair_series]);
        assert!(render_svg(&req).is_err());
    }

    #[test]
    fn rejects_invalid_specs() {
        // No series.
        assert!(render_svg(&base(ChartKind::Line, vec![])).is_err());
        // Too many series for the validated palette.
        let many = (0..8).map(|i| ys(&format!("s{i}"), &[1.0])).collect();
        assert!(render_svg(&base(ChartKind::Line, many)).is_err());
        // Pie with more than one series / negative values.
        assert!(render_svg(&base(ChartKind::Pie, vec![ys("a", &[1.0]), ys("b", &[1.0])])).is_err());
        assert!(render_svg(&base(ChartKind::Pie, vec![ys("a", &[1.0, -1.0])])).is_err());
        // Empty series data.
        assert!(render_svg(&base(ChartKind::Line, vec![ys("empty", &[])])).is_err());
    }

    #[test]
    fn many_categories_thin_their_labels() {
        let values: Vec<f64> = (0..500).map(|i| (i % 17) as f64).collect();
        let mut req = base(ChartKind::Line, vec![ys("noise", &values)]);
        req.labels = (0..500).map(|i| format!("cat-{i}")).collect();
        let svg = render_svg(&req).unwrap();
        // Labels were thinned, not rendered 500 times.
        assert!(svg.matches("cat-").count() < 60);
    }

    #[test]
    fn user_text_is_escaped() {
        // Two series so the legend renders the series labels.
        let mut req = base(
            ChartKind::Line,
            vec![ys("<script>", &[1.0, 2.0]), ys("ok", &[2.0, 1.0])],
        );
        req.title = Some("a <b> & 'c'".into());
        let svg = render_svg(&req).unwrap();
        assert!(!svg.contains("<script>"));
        assert!(svg.contains("&lt;script&gt;"));
        assert!(svg.contains("a &lt;b&gt; &amp; &apos;c&apos;"));
    }

    #[test]
    fn single_full_slice_renders_a_circle() {
        let req = base(ChartKind::Pie, vec![ys("all", &[5.0])]);
        let svg = render_svg(&req).unwrap();
        assert!(svg.contains("A")); // arc commands present
        assert!(svg.contains("100%"));
    }
}
