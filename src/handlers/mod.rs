//! HTTP handlers + shared server-render helpers.
//!
//! `health` is the unauthenticated liveness probe; `status` carries the PUBLIC status page
//! and JSON API; `admin` carries the SSO-gated operator dashboard + incident posting.
//!
//! The shared design tokens / CSS are embedded (via `include_str!`) and inlined into every
//! page, matching the HOLDFAST enterprise brand (the same look as the Keystone login UI):
//! brand gradient, indigo accent, status pills, cards, app-bar.

pub mod admin;
pub mod health;
pub mod status;

/// Embedded design system, inlined into each rendered page's `<style>`.
pub const APP_CSS: &str = include_str!("../../static/app.css");

/// The HOLDFAST shield glyph (small, for the app-bar brand lockup).
pub const SHIELD_SVG: &str = r##"<svg viewBox="0 0 48 48" fill="none" xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="hf-shield-sm" x1="8" y1="4" x2="40" y2="44" gradientUnits="userSpaceOnUse"><stop stop-color="#818CF8"/><stop offset="1" stop-color="#4F46E5"/></linearGradient></defs><path d="M24 4 8 9.5V22c0 11 7 17.4 16 21.5C33 39.4 40 33 40 22V9.5L24 4Z" fill="url(#hf-shield-sm)"/><rect x="20" y="19" width="8" height="13" rx="1" fill="#fff" fill-opacity="0.92"/><path d="M20 19v-2.5a4 4 0 0 1 8 0V19" stroke="#fff" stroke-width="2" stroke-opacity="0.92" fill="none"/></svg>"##;

/// Minimal HTML escaping for text/attribute interpolation.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Human label for a status token.
pub fn status_label(status: &str) -> &'static str {
    match status {
        "down" => "Down",
        "degraded" => "Degraded",
        _ => "Operational",
    }
}

/// CSS modifier class for a status token's pill.
pub fn status_pill_class(status: &str) -> &'static str {
    match status {
        "down" => "pill-down",
        "degraded" => "pill-warn",
        _ => "pill-ok",
    }
}

/// A status pill (`<span class="pill ...">Label</span>`).
pub fn status_pill(status: &str) -> String {
    format!(
        r#"<span class="pill {cls}">{label}</span>"#,
        cls = status_pill_class(status),
        label = status_label(status),
    )
}

/// Render the overall banner block for the public page.
pub fn overall_banner(overall: &str) -> String {
    let (cls, headline, sub) = match overall {
        "down" => (
            "banner-down",
            "Service disruption",
            "One or more components are down. We are on it.",
        ),
        "degraded" => (
            "banner-warn",
            "Partial degradation",
            "Some components are degraded; service may be slower than usual.",
        ),
        _ => (
            "banner-ok",
            "All systems operational",
            "Every monitored component is up and healthy.",
        ),
    };
    format!(
        r#"<section class="banner {cls}"><span class="banner__dot" aria-hidden="true"></span><div><h2 class="banner__headline">{headline}</h2><p class="banner__sub">{sub}</p></div></section>"#
    )
}

/// Compact "N ago" relative time from `ts` to `now` (both epoch seconds). Avoids a date
/// dependency while staying readable for a status timeline.
pub fn rel_time(ts: i64, now: i64) -> String {
    let d = now.saturating_sub(ts);
    if d < 0 {
        return "just now".to_string();
    }
    if d < 5 {
        "just now".to_string()
    } else if d < 60 {
        format!("{d}s ago")
    } else if d < 3_600 {
        format!("{}m ago", d / 60)
    } else if d < 86_400 {
        format!("{}h ago", d / 3_600)
    } else {
        format!("{}d ago", d / 86_400)
    }
}

/// Format a latest-latency value (`Some(ms)` -> "42 ms"; `None` -> "—").
pub fn fmt_latency(latency_ms: Option<i64>) -> String {
    match latency_ms {
        Some(ms) => format!("{ms} ms"),
        None => "—".to_string(),
    }
}
