//! HTTP handlers + shared server-render helpers.
//!
//! `health` is the unauthenticated liveness probe; `status` carries the PUBLIC status page
//! and JSON API; `admin` carries the SSO-gated operator dashboard + incident posting.
//!
//! Odyssey canonical CSS plus Beacon service CSS are embedded and inlined into every page.

pub mod admin;
pub mod feed;
pub mod health;
pub mod status;
pub mod subscriptions;

use std::sync::OnceLock;

use axum::http::HeaderMap;

/// Beacon-only CSS layered after Odyssey's canonical font, tokens, and components.
pub const SERVICE_CSS: &str = include_str!("../../static/service.css");

static APP_CSS: OnceLock<String> = OnceLock::new();
static DYNAMIC_JS: OnceLock<String> = OnceLock::new();

/// Embedded design system, inlined into each rendered page's `<style>`.
pub fn app_css() -> &'static str {
    APP_CSS
        .get_or_init(|| {
            let mut css = String::with_capacity(odyssey::APP_CSS.len() + SERVICE_CSS.len());
            css.push_str(odyssey::APP_CSS);
            css.push_str(SERVICE_CSS);
            css
        })
        .as_str()
}

/// Embedded Odyssey dynamic layer for admin-only progressive enhancement.
pub fn dynamic_js() -> &'static str {
    DYNAMIC_JS
        .get_or_init(|| odyssey::dynamic_scripts().0)
        .as_str()
}

/// The HOLDFAST shield glyph (small, for the app-bar brand lockup).
pub const SHIELD_SVG: &str = r##"<svg viewBox="0 0 48 48" fill="none" xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="hf-shield-sm" x1="8" y1="4" x2="40" y2="44" gradientUnits="userSpaceOnUse"><stop stop-color="#818CF8"/><stop offset="1" stop-color="#4F46E5"/></linearGradient></defs><path d="M24 4 8 9.5V22c0 11 7 17.4 16 21.5C33 39.4 40 33 40 22V9.5L24 4Z" fill="url(#hf-shield-sm)"/><rect x="20" y="19" width="8" height="13" rx="1" fill="#fff" fill-opacity="0.92"/><path d="M20 19v-2.5a4 4 0 0 1 8 0V19" stroke="#fff" stroke-width="2" stroke-opacity="0.92" fill="none"/></svg>"##;

/// Cross-subdomain SSO logout (terminated at the gateway). The same path Beacon has always used.
pub const LOGOUT_URL: &str = "/_gw/auth/logout";

/// The right side of the app-bar: a page title, an "All apps" pill back to the apex portal, and —
/// when a gateway identity is known — a user chip (avatar initial + email) and the SSO logout.
/// Shared by every page so the chrome stays identical; public pages pass `None` for `email`
/// (no chip, no logout — just the title + "All apps" link).
pub fn userbox(title: &str, email: Option<&str>) -> String {
    let chip = match email {
        Some(e) if !e.is_empty() => {
            let initial = e
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "H".to_string());
            format!(
                concat!(
                    "<span class=\"userchip\"><span class=\"userchip__avatar\" aria-hidden=\"true\">{initial}</span>",
                    "<span class=\"user-email\">{email}</span></span>",
                    "<a class=\"btn btn-ghost btn-sm\" href=\"{logout}\">Log out</a>",
                ),
                initial = esc(&initial),
                email = esc(e),
                logout = LOGOUT_URL,
            )
        }
        _ => String::new(),
    };
    format!(
        concat!(
            "<span class=\"topbar__title\">{title}</span>",
            "<a class=\"allapps\" href=\"https://w33d.xyz\" title=\"All apps\">",
            "<svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\" aria-hidden=\"true\">",
            "<rect x=\"3\" y=\"3\" width=\"7\" height=\"7\" rx=\"1.5\"/><rect x=\"14\" y=\"3\" width=\"7\" height=\"7\" rx=\"1.5\"/>",
            "<rect x=\"3\" y=\"14\" width=\"7\" height=\"7\" rx=\"1.5\"/><rect x=\"14\" y=\"14\" width=\"7\" height=\"7\" rx=\"1.5\"/></svg>All apps</a>",
            "{chip}",
        ),
        title = esc(title),
        chip = chip,
    )
}

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
        "maintenance" => "Maintenance",
        _ => "Operational",
    }
}

/// CSS modifier class for a status token's pill.
pub fn status_pill_class(status: &str) -> &'static str {
    match status {
        "down" => "pill-down",
        "degraded" => "pill-warn",
        "maintenance" => "pill-info",
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

/// Human label for an incident lifecycle status. Unknown tokens pass through as-is (older
/// rows predate the allowlist), so the page never mislabels them.
pub fn incident_status_label(status: &str) -> String {
    match status {
        "investigating" => "Investigating".to_string(),
        "identified" => "Identified".to_string(),
        "monitoring" => "Monitoring".to_string(),
        "resolved" => "Resolved".to_string(),
        other => other.to_string(),
    }
}

/// A pill for an incident lifecycle status (investigating/identified draw attention,
/// monitoring is informational, resolved is green).
pub fn incident_status_pill(status: &str) -> String {
    let cls = match status {
        "investigating" | "identified" => "pill-warn",
        "monitoring" => "pill-info",
        "resolved" => "pill-ok",
        _ => "pill-state",
    };
    format!(
        r#"<span class="pill {cls}">{label}</span>"#,
        label = esc(&incident_status_label(status)),
    )
}

/// A pill for an incident severity (minor is neutral, major warns, critical is red).
pub fn severity_pill(severity: &str) -> String {
    let cls = match severity {
        "critical" => "pill-down",
        "major" => "pill-warn",
        _ => "pill-state",
    };
    format!(
        r#"<span class="pill {cls}">{label}</span>"#,
        label = esc(severity)
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

/// Compact human countdown for a future event `secs_until` seconds away: `"2d 3h"`,
/// `"3h 15m"`, `"15m"`, or `"under a minute"`. Non-positive inputs read as `"now"`.
pub fn fmt_countdown(secs_until: i64) -> String {
    if secs_until <= 0 {
        return "now".to_string();
    }
    let days = secs_until / 86_400;
    let hours = (secs_until % 86_400) / 3_600;
    let mins = (secs_until % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        "under a minute".to_string()
    }
}

/// Read a request header as UTF-8 text (`None` when absent or not valid text). Local helper
/// for deriving the Odyssey locale from the raw `Cookie` / `Accept-Language` header values.
pub fn hv<'a>(h: &'a HeaderMap, n: &str) -> Option<&'a str> {
    h.get(n).and_then(|v| v.to_str().ok())
}

/// Wrap `inner` HTML in the standard HOLDFAST chrome (app-bar + centered console + footer)
/// via the Odyssey shell layer, so `<html lang>` and the chrome strings follow the resolved
/// `locale` (i18n pilot). Used by the standalone public subscription notices; `title` is the
/// page `<title>` (already trusted/static text).
pub fn page_shell(locale: odyssey::Locale, title: &str, inner: &str) -> String {
    let full_title = format!("{title} · HOLDFAST");
    odyssey::page_shell(
        odyssey::PageChrome {
            title: &full_title,
            brand: odyssey::Brand {
                tile_svg: SHIELD_SVG,
                accent: "",
                name: "HOLDFAST",
                sub: "Status",
            },
            nav: &[],
            user: odyssey::UserBox {
                email: None,
                logout_url: LOGOUT_URL,
            },
            footer: odyssey::raw("<span>HOLDFAST · Sovereign infrastructure</span>"),
        },
        odyssey::raw(inner),
        odyssey::ShellOpts {
            extra_css: SERVICE_CSS,
            body_class: "page-console",
            locale,
            ..Default::default()
        },
    )
}

/// Human calendar date in UTC, e.g. `Jul 2, 2026`. Falls back to the raw integer on an
/// out-of-range timestamp.
pub fn fmt_date(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(dt) => format!("{} {}, {}", month_abbr(dt.month()), dt.day(), dt.year()),
        Err(_) => secs.to_string(),
    }
}

/// Human date + time in UTC, e.g. `Jul 2, 2026 14:30 UTC` (maintenance windows).
pub fn fmt_datetime(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(dt) => format!(
            "{} {}, {} {:02}:{:02} UTC",
            month_abbr(dt.month()),
            dt.day(),
            dt.year(),
            dt.hour(),
            dt.minute(),
        ),
        Err(_) => secs.to_string(),
    }
}

pub(crate) fn month_abbr(m: time::Month) -> &'static str {
    use time::Month::*;
    match m {
        January => "Jan",
        February => "Feb",
        March => "Mar",
        April => "Apr",
        May => "May",
        June => "Jun",
        July => "Jul",
        August => "Aug",
        September => "Sep",
        October => "Oct",
        November => "Nov",
        December => "Dec",
    }
}
