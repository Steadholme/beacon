//! PUBLIC status surface: the server-rendered status page and the machine-readable JSON.
//!
//! Both are unauthenticated by design (placed behind a Sluice `auth=public` route). The page
//! follows the Status v1 design (Figma "Status"): the overall state IS the page heading, an
//! estate-wide 30-day evidence strip sits directly under it, active incidents carry a stage
//! track instead of a status pill, maintenance windows show a countdown value, the public
//! catalog is a grid of component tiles (each opening a detail popover), host capacity renders
//! as gauges plus a 24-hour heat strip, and the 14-day history closes the live region.
//!
//! Vocabulary rule: every visible string is a name, a value, or an action — no counts, no
//! eyebrows, and no repeated state words on an all-operational page.

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;

use crate::handlers::{
    dynamic_js, esc, fmt_countdown, fmt_date, fmt_datetime, fmt_latency, hv, incident_status_pill,
    rel_time, render_theme_switch, severity_pill, APP_CSS_PATH, SHIELD_SVG,
};
use crate::i18n;
use crate::model::{
    affected_names, build_status, day_bucket, day_date, group_rollup, maintenance_ongoing,
    ComponentView, StatusView, DAY_SECS,
};
use crate::store::{Incident, IncidentUpdate};
use crate::{now_secs, vitals, AppState};

const STATUS_HTML: &str = include_str!("../../templates/status.html");
const STATUS_PAGE_JS: &str = include_str!("../../static/status-page.js");
const STATUS_LIVE_ID: &str = "status-live";
const STATUS_LIVE_SELECTOR: &str = "#status-live";
/// "Past incidents" horizon in days.
const HISTORY_DAYS: i64 = 14;

const ICON_BELL: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.94 1.94 0 0 0 3.4 0"/></svg>"#;
const ICON_RSS: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 11a9 9 0 0 1 9 9M4 4a16 16 0 0 1 16 16"/><circle cx="5" cy="19" r="1"/></svg>"#;
const ICON_BRACES: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M8 3H7a2 2 0 0 0-2 2v5a2 2 0 0 1-2 2 2 2 0 0 1 2 2v5c0 1.1.9 2 2 2h1M16 21h1a2 2 0 0 0 2-2v-5c0-1.1.9-2 2-2a2 2 0 0 1-2-2V5a2 2 0 0 0-2-2h-1"/></svg>"#;
const ICON_WEBHOOK: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M13 2 3 14h9l-1 8 10-12h-9l1-8z"/></svg>"#;
const ICON_HISTORY: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8M3 3v5h5M12 7v5l4 2"/></svg>"#;

/// `GET /status` — the public status page (no auth).
///
/// An Odyssey Wire request (`X-Wire: 1`) receives only the shared live region. A normal request,
/// including a no-JavaScript activation of the refresh link, always receives the complete SSR
/// document. The snapshot is explicitly non-cacheable: it varies by representation, locale, and
/// theme while also carrying live operational data. Locale comes from the estate-wide
/// `__Secure-lang` cookie / `Accept-Language` chain only — never from query parameters.
pub async fn status_page(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let now = now_secs();
    let loc = odyssey::resolve_locale(hv(&headers, "cookie"), hv(&headers, "accept-language"));
    let theme = odyssey::resolve_theme(hv(&headers, "cookie"));
    let mut view = state
        .public_status
        .get(
            std::sync::Arc::clone(&state.store),
            &state.config.public_catalog,
            now,
        )
        .await
        .as_ref()
        .clone();
    attach_infra(&mut view, &state, now);

    let body = if is_wire_request(&headers) {
        render_status_live(&view, now, loc)
    } else {
        render_status(&view, now, loc, theme, state.config.public_webhooks_enabled)
    };
    let mut response = Html(body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("X-Wire"));
    response
}

/// `GET /api/status` — the public machine-readable status snapshot (no auth).
pub async fn api_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let now = now_secs();
    let _loc = odyssey::resolve_locale(hv(&headers, "cookie"), hv(&headers, "accept-language"));
    let mut view = state
        .public_status
        .get(
            std::sync::Arc::clone(&state.store),
            &state.config.public_catalog,
            now,
        )
        .await
        .as_ref()
        .clone();
    attach_infra(&mut view, &state, now);
    let mut response = Json(view).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// `GET /api/status` on the dedicated internal listener — the complete raw check model used by
/// Portal Estate's server-side joins. This handler is intentionally absent from the public
/// router and remains non-cacheable because it carries live operational data.
pub async fn api_internal_status(State(state): State<AppState>) -> Response {
    let now = now_secs();
    let mut view = build_status(state.store.as_ref(), now).await;
    attach_infra(&mut view, &state, now);
    let mut response = Json(view).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn attach_infra(view: &mut StatusView, state: &AppState, now: i64) {
    view.infra = state
        .vitals
        .as_ref()
        .and_then(|v| v.snapshot())
        .and_then(|snap| vitals::public_infra(&snap, now));
}

fn is_wire_request(headers: &HeaderMap) -> bool {
    hv(headers, "x-wire").is_some_and(|value| value.trim() == "1")
}

// ---------------------------------------------------------------------------------------------
// Locale-aware formatting helpers
// ---------------------------------------------------------------------------------------------

fn render_lang_switch(loc: odyssey::Locale) -> String {
    let mut out = format!(
        r#"<nav class="bc-lang" aria-label="{}">"#,
        esc(i18n::t(loc, "status.lang_label"))
    );
    for l in odyssey::Locale::all() {
        let (active, current) = if l == loc {
            (" is-active", r#" aria-current="true""#)
        } else {
            ("", "")
        };
        out.push_str(&format!(
            r#"<a class="langswitch__opt{active}" href="/_gw/lang?to={code}"{current}>{name}</a>"#,
            code = l.code(),
            name = esc(odyssey::t(
                l,
                match l {
                    odyssey::Locale::En => "lang.name.en",
                    odyssey::Locale::Zh => "lang.name.zh",
                    odyssey::Locale::Ja => "lang.name.ja",
                }
            )),
        ));
    }
    out.push_str("</nav>");
    out
}

fn rel_time_l(loc: odyssey::Locale, ts: i64, now: i64) -> String {
    if loc == odyssey::Locale::En {
        return rel_time(ts, now);
    }
    let d = now.saturating_sub(ts);
    if d < 5 {
        i18n::t(loc, "time.just_now").to_string()
    } else if d < 60 {
        i18n::tf(loc, "time.s_ago", &[("n", &d.to_string())])
    } else if d < 3_600 {
        i18n::tf(loc, "time.m_ago", &[("n", &(d / 60).to_string())])
    } else if d < 86_400 {
        i18n::tf(loc, "time.h_ago", &[("n", &(d / 3_600).to_string())])
    } else {
        i18n::tf(loc, "time.d_ago", &[("n", &(d / 86_400).to_string())])
    }
}

fn fmt_countdown_l(loc: odyssey::Locale, secs_until: i64) -> String {
    if loc == odyssey::Locale::En {
        return fmt_countdown(secs_until);
    }
    if secs_until <= 0 {
        return i18n::t(loc, "time.now").to_string();
    }
    let days = secs_until / 86_400;
    let hours = (secs_until % 86_400) / 3_600;
    let mins = (secs_until % 3_600) / 60;
    if days > 0 {
        i18n::tf(
            loc,
            "time.duration.dh",
            &[("d", &days.to_string()), ("h", &hours.to_string())],
        )
    } else if hours > 0 {
        i18n::tf(
            loc,
            "time.duration.hm",
            &[("h", &hours.to_string()), ("m", &mins.to_string())],
        )
    } else if mins > 0 {
        i18n::tf(loc, "time.duration.m", &[("m", &mins.to_string())])
    } else {
        i18n::t(loc, "time.under_minute").to_string()
    }
}

fn fmt_date_l(loc: odyssey::Locale, secs: i64) -> String {
    if loc == odyssey::Locale::En {
        return fmt_date(secs);
    }
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(dt) => odyssey::fmt_date(loc, dt.year(), u8::from(dt.month()), dt.day()),
        Err(_) => secs.to_string(),
    }
}

fn status_label_l(loc: odyssey::Locale, status: &str) -> String {
    let key = match status {
        "down" => "status.state.down",
        "degraded" => "status.state.degraded",
        "maintenance" => "status.state.maintenance",
        _ => "status.state.operational",
    };
    i18n::t(loc, key).to_string()
}

fn incident_status_label_l(loc: odyssey::Locale, status: &str) -> String {
    if loc == odyssey::Locale::En {
        return crate::handlers::incident_status_label(status);
    }
    match status {
        "investigating" => i18n::t(loc, "status.incident.investigating").to_string(),
        "identified" => i18n::t(loc, "status.incident.identified").to_string(),
        "monitoring" => i18n::t(loc, "status.incident.monitoring").to_string(),
        "resolved" => i18n::t(loc, "status.incident.resolved").to_string(),
        other => other.to_string(),
    }
}

fn incident_status_pill_l(loc: odyssey::Locale, status: &str) -> String {
    if loc == odyssey::Locale::En {
        return incident_status_pill(status);
    }
    let cls = match status {
        "investigating" | "identified" => "pill-warn",
        "monitoring" => "pill-info",
        "resolved" => "pill-ok",
        _ => "pill-state",
    };
    format!(
        r#"<span class="pill {cls}">{label}</span>"#,
        label = esc(&incident_status_label_l(loc, status)),
    )
}

fn severity_pill_l(loc: odyssey::Locale, severity: &str) -> String {
    if loc == odyssey::Locale::En {
        return severity_pill(severity);
    }
    let cls = match severity {
        "critical" => "pill-down",
        "major" => "pill-warn",
        _ => "pill-state",
    };
    let key = match severity {
        "critical" => "status.severity.critical",
        "major" => "status.severity.major",
        _ => "status.severity.minor",
    };
    format!(
        r#"<span class="pill {cls}">{label}</span>"#,
        label = esc(i18n::t(loc, key))
    )
}

/// The shape-coded state glyph. Circle = operational, diamond = degraded, square = down,
/// ring = maintenance, dash = pending (never probed). Colour alone never carries the state.
fn mark(state: &str, size: &str) -> String {
    let size_cls = if size.is_empty() {
        String::new()
    } else {
        format!(" mark--{size}")
    };
    format!(r#"<span class="mark mark--{state}{size_cls}" aria-hidden="true"></span>"#)
}

/// A component's visible state token: a never-probed component reads `pending` rather than
/// `operational`, so the page never claims evidence it does not have.
fn tile_state(c: &ComponentView) -> &'static str {
    if c.last_checked.is_none() && c.status == "operational" {
        "pending"
    } else {
        c.status
    }
}

fn render_refresh(loc: odyssey::Locale) -> String {
    odyssey::link_button_with_wire(
        "/status",
        i18n::t(loc, "status.refresh"),
        odyssey::Variant::Ghost,
        odyssey::BtnOpts {
            small: true,
            ..Default::default()
        },
        odyssey::WireOpts::new(STATUS_LIVE_SELECTOR)
            .select(STATUS_LIVE_SELECTOR)
            .swap(odyssey::WireSwap::Outer)
            .busy_label(i18n::t(loc, "status.refresh_busy"))
            .success_message(i18n::t(loc, "status.refresh_success"))
            .error_message(i18n::t(loc, "status.refresh_error")),
    )
    .0
}

/// Current UTC wall clock as a value in the app bar (`HH:MM UTC`); ticked by the page script.
fn render_clock(now: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(now) {
        Ok(dt) => format!(
            r#"<time class="clock" datetime="{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:00Z" data-clock>{h:02}:{mi:02} UTC</time>"#,
            y = dt.year(),
            mo = u8::from(dt.month()),
            d = dt.day(),
            h = dt.hour(),
            mi = dt.minute(),
        ),
        Err(_) => String::new(),
    }
}

/// The "Get updates" popover: RSS and JSON are always offered; the webhook entry appears only
/// when the operator has enabled anonymous webhook registration.
fn render_updates_menu(loc: odyssey::Locale, webhooks_enabled: bool) -> String {
    let webhook = if webhooks_enabled {
        format!(
            r##"<a class="updates-pop__item" href="#subscribe">{ICON_WEBHOOK}{}</a>"##,
            esc(i18n::t(loc, "status.webhook"))
        )
    } else {
        String::new()
    };
    format!(
        r#"<details class="updates-pop">
          <summary class="btn btn-secondary btn-sm updates-pop__btn" aria-label="{label}">{ICON_BELL}<span class="updates-pop__label">{label}</span></summary>
          <nav class="updates-pop__menu">
            <a class="updates-pop__item" href="/feed.xml">{ICON_RSS}{rss}</a>
            <a class="updates-pop__item" href="/api/status">{ICON_BRACES}{json}</a>
            {webhook}
          </nav>
        </details>"#,
        label = esc(i18n::t(loc, "status.get_updates")),
        rss = esc(i18n::t(loc, "status.rss_feed")),
        json = esc(i18n::t(loc, "status.json_api")),
    )
}

fn render_status(
    view: &StatusView,
    now: i64,
    loc: odyssey::Locale,
    theme: &str,
    webhooks_enabled: bool,
) -> String {
    // Static chrome first; content that may contain operator-authored text (incident titles and
    // bodies) is substituted LAST so a literal `{{...}}` inside it can never be re-expanded.
    STATUS_HTML
        .replace("{{LANG}}", loc.bcp47())
        .replace("{{THEME}}", odyssey::html_theme_attr(theme))
        .replace("{{COLOR_SCHEME}}", odyssey::color_scheme_meta(theme))
        .replace("{{CSS_PATH}}", APP_CSS_PATH)
        .replace("{{SHIELD}}", SHIELD_SVG)
        .replace("{{SKIP_TO_STATUS}}", i18n::t(loc, "status.skip_to_content"))
        .replace("{{CLOCK}}", &render_clock(now))
        .replace("{{LANGSWITCH}}", &render_lang_switch(loc))
        .replace("{{THEMESWITCH}}", &render_theme_switch(theme))
        .replace("{{UPDATES}}", &render_updates_menu(loc, webhooks_enabled))
        .replace("{{CHANNELS}}", &render_channels(loc, webhooks_enabled))
        .replace("{{FOOT}}", &render_footer(view, now, loc))
        .replace("{{SCRIPTS}}", dynamic_js())
        .replace("{{PAGE_JS}}", STATUS_PAGE_JS)
        .replace("{{STATUS_LIVE}}", &render_status_live(view, now, loc))
}

/// Render the one replaceable public status region. Full-page SSR and Wire fragments call this
/// exact function, so a dynamic refresh cannot drift from the no-JavaScript representation.
fn render_status_live(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    format!(
        r#"<div id="{id}" class="status-live" role="region" aria-label="{label}">
{masthead}
{estate}
{active}
{maintenance}
{catalog}
{infra}
{history}
</div>"#,
        id = STATUS_LIVE_ID,
        label = esc(i18n::t(loc, "status.live_region")),
        masthead = render_masthead(view, now, loc),
        estate = render_estate(view, now, loc),
        active = render_active_incidents(view, now, loc),
        maintenance = render_maintenances(view, now, loc),
        catalog = render_catalog(view, now, loc),
        infra = render_infra(loc, view.infra.as_ref(), now),
        history = render_history(view, now, loc),
    )
}

fn checked_uptime_avg<'a, I>(components: I) -> Option<f64>
where
    I: IntoIterator<Item = &'a ComponentView>,
{
    let (sum, count) = components
        .into_iter()
        .filter(|c| c.last_checked.is_some())
        .fold((0.0, 0usize), |(sum, count), c| {
            (sum + c.uptime_90d, count + 1)
        });
    (count > 0).then_some(sum / count as f64)
}

/// The masthead: the overall state is the page heading. Meta values follow — evidence-window
/// uptime, the affected count (only when something is not operational), the update stamp, and
/// the Refresh action.
fn render_masthead(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    let (state, headline_key) = match view.overall {
        "down" => ("down", "status.hero.down.title"),
        "degraded" => ("degraded", "status.hero.warn.title"),
        "maintenance" => ("maintenance", "status.hero.maint.title"),
        _ => ("operational", "status.hero.ok.title"),
    };
    // Affected is derived from the projected component states themselves — never from incident
    // affected-name lists, which may name internal checks or lag behind the probes. A
    // maintenance-masked component counts as affected: it is not operational right now.
    let affected_total = view.components.len();
    let affected_count = view
        .components
        .iter()
        .filter(|c| c.status != "operational")
        .count();
    let days = view.history_days.to_string();
    let uptime = checked_uptime_avg(view.components.iter())
        .map(|avg| {
            format!(
                r#"<span class="mast__uptime" title="{title}"><b>{avg:.2}%</b><small>{window}</small></span>"#,
                title = esc(&i18n::tf(loc, "status.uptime.average_all", &[("days", &days)])),
                window = esc(&i18n::tf(loc, "status.window.days", &[("days", &days)])),
            )
        })
        .unwrap_or_default();
    let affected = if affected_count > 0 {
        format!(
            r#"<span class="mast__affected">{}</span>"#,
            esc(&i18n::tf(
                loc,
                "status.hero.affected",
                &[
                    ("affected", &affected_count.to_string()),
                    ("total", &affected_total.to_string())
                ]
            ))
        )
    } else {
        String::new()
    };
    format!(
        r#"<section class="mast mast--{state}">
  {mark}
  <h1 id="status-title" class="mast__headline">{headline}</h1>
  <div class="mast__meta">{uptime}{affected}<span class="mast__updated">{updated}</span>{refresh}</div>
</section>"#,
        mark = mark(state, "xl"),
        headline = esc(i18n::t(loc, headline_key)),
        updated = esc(&i18n::tf(
            loc,
            "status.updated",
            &[("time", &rel_time_l(loc, view.updated_at, now))]
        )),
        refresh = render_refresh(loc),
    )
}

/// Estate timeline: one cell per UTC day of the evidence window, the WORST public component
/// state of that day (down > warn > ok; unknown only when no component has data).
fn render_estate(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    let Some(first) = view.components.first() else {
        return String::new();
    };
    let len = first.days.len();
    if len == 0 {
        return String::new();
    }
    let incident_by_day = incident_titles_by_day(view);
    let no_data = i18n::t(loc, "status.bar.no_data");
    let mut cells = String::new();
    for i in 0..len {
        let mut worst = "unknown";
        let (mut sum, mut n) = (0.0f64, 0usize);
        for c in &view.components {
            let Some(d) = c.days.get(i) else { continue };
            if let Some(pct) = d.uptime {
                sum += pct;
                n += 1;
            }
            worst = match (worst, d.status) {
                (_, "down") | ("down", _) => "down",
                (_, "warn") | ("warn", _) => "warn",
                (_, "ok") | ("ok", _) => "ok",
                (w, _) => w,
            };
        }
        let date = first.days[i].date.as_str();
        let uptime = if n > 0 {
            format!("{:.2}%", sum / n as f64)
        } else {
            no_data.to_string()
        };
        let inc_attr = if matches!(worst, "warn" | "down") {
            incident_by_day
                .get(date)
                .map(|s| format!(r#" data-inc="{}""#, esc(s)))
                .unwrap_or_default()
        } else {
            String::new()
        };
        cells.push_str(&format!(
            r#"<span class="cell cell--{worst}" data-date="{date}" data-uptime="{uptime}"{inc_attr}></span>"#,
            date = esc(date),
            uptime = esc(&uptime),
        ));
    }
    let days = len.to_string();
    format!(
        r#"<section class="estate">
  <div class="cells" role="img" aria-label="{label}">{cells}</div>
  {axis}
</section>"#,
        label = esc(&i18n::tf(loc, "status.estate.label", &[("days", &days)])),
        axis = render_axis(loc, now, len),
    )
}

/// Date axis under a day strip: first day · middle day · Today (all values).
fn render_axis(loc: odyssey::Locale, now: i64, len: usize) -> String {
    let today = day_bucket(now);
    let first = today - len.saturating_sub(1) as i64;
    let mid = today - (len / 2) as i64;
    format!(
        r#"<div class="axis"><span>{start}</span><span>{mid}</span><span>{today}</span></div>"#,
        start = esc(&fmt_date_l(loc, first * DAY_SECS)),
        mid = esc(&fmt_date_l(loc, mid * DAY_SECS)),
        today = esc(i18n::t(loc, "status.uptime.today")),
    )
}

/// The public catalog: one section per configured group (plus a trailing "Other" for
/// ungrouped components) holding a grid of component tiles. Without groups the tiles render as
/// one flat grid.
fn render_catalog(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    if view.components.is_empty() {
        return format!(
            r#"<section class="catalog" aria-label="{label}"><p class="catalog__empty">{empty}</p></section>"#,
            label = esc(i18n::t(loc, "status.components")),
            empty = esc(i18n::t(loc, "status.no_components")),
        );
    }
    let incident_by_day = incident_titles_by_day(view);
    let mut out = format!(
        r#"<section class="catalog" aria-label="{}">"#,
        esc(i18n::t(loc, "status.components"))
    );
    if view.groups.is_empty() {
        out.push_str(r#"<div class="tiles">"#);
        for c in &view.components {
            out.push_str(&render_tile(c, now, loc, &incident_by_day));
        }
        out.push_str("</div></section>");
        return out;
    }

    let mut sections: Vec<(&str, Vec<&ComponentView>)> = Vec::new();
    for g in &view.groups {
        let members: Vec<&ComponentView> = view
            .components
            .iter()
            .filter(|c| c.group_id.as_deref() == Some(g.id.as_str()))
            .collect();
        if !members.is_empty() {
            sections.push((g.name.as_str(), members));
        }
    }
    let known: std::collections::HashSet<&str> =
        view.groups.iter().map(|g| g.id.as_str()).collect();
    let ungrouped: Vec<&ComponentView> = view
        .components
        .iter()
        .filter(|c| c.group_id.as_deref().is_none_or(|id| !known.contains(id)))
        .collect();
    if !ungrouped.is_empty() {
        let other_label = if loc == odyssey::Locale::En {
            "Other"
        } else {
            i18n::t(loc, "status.group.other")
        };
        sections.push((other_label, ungrouped));
    }
    for (name, members) in sections {
        out.push_str(&render_group(name, &members, now, loc, &incident_by_day));
    }
    out.push_str("</section>");
    out
}

fn render_group(
    name: &str,
    members: &[&ComponentView],
    now: i64,
    loc: odyssey::Locale,
    incident_by_day: &std::collections::HashMap<String, String>,
) -> String {
    let statuses: Vec<&str> = members.iter().map(|c| c.status).collect();
    let rollup = group_rollup(&statuses);
    let days = members
        .first()
        .map_or(0, |component| component.days.len())
        .to_string();
    let uptime = checked_uptime_avg(members.iter().copied())
        .map(|avg| {
            format!(
                r#"<span class="group__uptime" title="{}">{avg:.2}%</span>"#,
                esc(&i18n::tf(
                    loc,
                    "status.uptime.average_group",
                    &[("days", &days)]
                )),
            )
        })
        .unwrap_or_default();
    let state_word = if rollup == "operational" {
        String::new()
    } else {
        format!(
            r#"<span class="group__state">{}</span>"#,
            esc(&status_label_l(loc, rollup))
        )
    };
    let mut out = format!(
        r#"<section class="group group--{rollup}">
  <header class="group__head">{mark}<h2 class="group__name">{name}</h2><span class="group__rule" aria-hidden="true"></span>{uptime}{state_word}</header>
  <div class="tiles">"#,
        mark = mark(rollup, "md"),
        name = esc(name),
    );
    for c in members {
        out.push_str(&render_tile(c, now, loc, incident_by_day));
    }
    out.push_str("</div></section>");
    out
}

/// Day cells for one component (mini in the tile face, full in the detail).
fn render_day_cells(
    c: &ComponentView,
    loc: odyssey::Locale,
    now: i64,
    incident_by_day: &std::collections::HashMap<String, String>,
    with_incidents: bool,
) -> String {
    let first_data_idx = c.days.iter().position(|d| d.uptime.is_some());
    let first_day = first_data_idx
        .map(|idx| day_bucket(now) - c.days.len().saturating_sub(1) as i64 + idx as i64);
    let no_data_text = match first_day.map(|day| fmt_date_l(loc, day * DAY_SECS)) {
        Some(date) if loc == odyssey::Locale::En => {
            format!("no data — monitoring began {date}")
        }
        Some(date) => i18n::tf(loc, "status.bar.no_data_since", &[("date", &date)]),
        None if loc == odyssey::Locale::En => "no data".to_string(),
        None => i18n::t(loc, "status.bar.no_data").to_string(),
    };
    let mut cells = String::new();
    for d in &c.days {
        let data_uptime = match d.uptime {
            Some(pct) => format!("{pct:.2}%"),
            None => no_data_text.clone(),
        };
        let inc_attr = if with_incidents && matches!(d.status, "warn" | "down") {
            incident_by_day
                .get(&d.date)
                .map(|s| format!(r#" data-inc="{}""#, esc(s)))
                .unwrap_or_default()
        } else {
            String::new()
        };
        cells.push_str(&format!(
            r#"<span class="cell cell--{cls}" data-date="{date}" data-uptime="{uptime}"{inc_attr}></span>"#,
            cls = d.status,
            date = esc(&d.date),
            uptime = esc(&data_uptime),
        ));
    }
    cells
}

/// One component tile: name · state mark · mini evidence strip · uptime · latency, plus the
/// state word only when the component is NOT operational. The tile is a native `<details>`
/// whose body is the detail popover (`name="tile"` keeps one open at a time).
fn render_tile(
    c: &ComponentView,
    now: i64,
    loc: odyssey::Locale,
    incident_by_day: &std::collections::HashMap<String, String>,
) -> String {
    let state = tile_state(c);
    let days = c.days.len().to_string();
    let checked = c.last_checked.is_some();
    let pct_title = if checked {
        i18n::tf(loc, "status.uptime.component_title", &[("days", &days)])
    } else if loc == odyssey::Locale::En {
        "awaiting first check".to_string()
    } else {
        i18n::t(loc, "status.row.awaiting_first_check").to_string()
    };
    let pct = if checked {
        format!("{:.2}%", c.uptime_90d)
    } else {
        "—".to_string()
    };
    let latest_latency = fmt_latency(c.latency_ms);
    let latency_title = if loc == odyssey::Locale::En {
        format!("24h average · latest {latest_latency}")
    } else {
        i18n::tf(
            loc,
            "status.row.latency_title",
            &[("latest", &latest_latency)],
        )
    };
    let latency = match c.latency_avg_ms {
        Some(avg) => format!("~{avg} ms"),
        None => String::new(),
    };
    let state_word = match state {
        "operational" => String::new(),
        "pending" => format!(
            r#"<span class="tile__state">{}</span>"#,
            esc(if loc == odyssey::Locale::En {
                "Awaiting first check"
            } else {
                i18n::t(loc, "status.row.awaiting_first_check")
            })
        ),
        other => format!(
            r#"<span class="tile__state">{}</span>"#,
            esc(&status_label_l(loc, other))
        ),
    };
    format!(
        r#"<details class="tile tile--{state}" name="tile">
  <summary class="tile__face" aria-label="{detail_label}">
    <span class="tile__head"><span class="tile__name">{name}</span>{mark}</span>
    <span class="cells cells--mini" aria-hidden="true">{mini}</span>
    <span class="tile__foot"><span class="tile__up" title="{pct_title}">{pct}</span><span class="tile__lat" title="{latency_title}">{latency}</span></span>
    {state_word}
  </summary>
  {detail}
</details>"#,
        detail_label = esc(&i18n::tf(loc, "status.tile.detail", &[("name", &c.name)])),
        name = esc(&c.name),
        mark = mark(state, "md"),
        mini = render_day_cells(c, loc, now, incident_by_day, false),
        pct_title = esc(&pct_title),
        pct = esc(&pct),
        latency_title = esc(&latency_title),
        latency = esc(&latency),
        detail = render_tile_detail(c, now, loc, incident_by_day),
    )
}

/// The tile detail: uptime over the evidence window / 24 hours / 7 days, the full dated
/// evidence strip, the 24-hour latency spark with average and latest values, and the
/// monitoring-since value.
fn render_tile_detail(
    c: &ComponentView,
    now: i64,
    loc: odyssey::Locale,
    incident_by_day: &std::collections::HashMap<String, String>,
) -> String {
    let state = tile_state(c);
    let checked = c.last_checked.is_some();
    let days = c.days.len().to_string();
    let value = |pct: f64| {
        if checked {
            format!("{pct:.2}%")
        } else {
            "—".to_string()
        }
    };
    let first_day = c
        .days
        .iter()
        .position(|d| d.uptime.is_some())
        .map(|idx| day_bucket(now) - c.days.len().saturating_sub(1) as i64 + idx as i64);
    let since = match first_day {
        Some(day) => {
            let date = fmt_date_l(loc, day * DAY_SECS);
            if loc == odyssey::Locale::En {
                format!("monitoring since {date}")
            } else {
                i18n::tf(loc, "status.row.monitoring_since", &[("date", &date)])
            }
        }
        None if loc == odyssey::Locale::En => "awaiting first check".to_string(),
        None => i18n::t(loc, "status.row.awaiting_first_check").to_string(),
    };
    let latest_latency = fmt_latency(c.latency_ms);
    let latency_title = if loc == odyssey::Locale::En {
        format!("24h average · latest {latest_latency}")
    } else {
        i18n::tf(
            loc,
            "status.row.latency_title",
            &[("latest", &latest_latency)],
        )
    };
    let latency = match c.latency_avg_ms {
        Some(avg) => format!("~{avg} ms"),
        None => "—".to_string(),
    };
    let evidence_label = format!(
        "{} · {} · {} · {}",
        c.name,
        status_label_l(loc, c.status),
        value(c.uptime_90d),
        i18n::tf(loc, "status.uptime.component_title", &[("days", &days)])
    );
    format!(
        r#"<div class="tile__detail">
    <div class="detail__head">{mark}<h3 class="detail__name">{name}</h3></div>
    <div class="detail__values"><span><b>{u30}</b><small>{w30}</small></span><span><b>{u24}</b><small>{w24}</small></span><span><b>{u7}</b><small>{w7}</small></span></div>
    <div class="cells" role="img" aria-label="{evidence_label}">{cells}</div>
    {axis}
    <div class="detail__latency">{spark}<span class="detail__ms"><b>{latency}</b><small>{latency_title}</small></span></div>
    <span class="detail__since">{since}</span>
  </div>"#,
        mark = mark(state, "lg"),
        name = esc(&c.name),
        u30 = esc(&value(c.uptime_90d)),
        w30 = esc(&i18n::tf(loc, "status.window.days", &[("days", &days)])),
        u24 = esc(&value(c.uptime_24h)),
        w24 = esc(&i18n::tf(loc, "status.window.hours", &[("hours", "24")])),
        u7 = esc(&value(c.uptime_7d)),
        w7 = esc(&i18n::tf(loc, "status.window.days", &[("days", "7")])),
        evidence_label = esc(&evidence_label),
        cells = render_day_cells(c, loc, now, incident_by_day, true),
        axis = render_axis(loc, now, c.days.len()),
        spark = render_latency_spark(&c.latency_points),
        latency = esc(&latency),
        latency_title = esc(&latency_title),
        since = esc(&since),
    )
}

fn render_latency_spark(points: &[Option<i64>]) -> String {
    let vals: Vec<(usize, i64)> = points
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.map(|v| (i, v.max(0))))
        .collect();
    if vals.len() < 2 {
        return String::new();
    }
    let max = vals.iter().map(|(_, v)| *v).max().unwrap_or(1).max(1) as f64;
    let denom = (points.len().saturating_sub(1)).max(1) as f64;
    let coords: Vec<String> = vals
        .iter()
        .map(|(i, v)| {
            let x = *i as f64 / denom * 200.0;
            let y = 38.0 - (*v as f64 / max * 36.0);
            format!("{x:.1},{y:.1}")
        })
        .collect();
    format!(
        r#"<svg class="spark" viewBox="0 0 200 40" preserveAspectRatio="none" aria-hidden="true"><polyline fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round" vector-effect="non-scaling-stroke" points="{}"/></svg>"#,
        coords.join(" ")
    )
}

fn incident_titles_by_day(view: &StatusView) -> std::collections::HashMap<String, String> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for inc in &view.incidents {
        out.entry(day_date(day_bucket(inc.created_at)))
            .or_default()
            .push(inc.title.clone());
    }
    out.into_iter()
        .map(|(day, titles)| (day, titles.join("; ")))
        .collect()
}

/// Public update channels: RSS feed and JSON API are always actions; the webhook form appears
/// only when the operator explicitly enabled the hardened egress path.
fn render_channels(loc: odyssey::Locale, webhooks_enabled: bool) -> String {
    let form = if webhooks_enabled {
        format!(
            r#"
  <form method="post" action="/subscriptions" class="webhook">
    <label class="sr-only" for="sub-target">{field}</label>
    <input type="url" id="sub-target" name="target" required placeholder="https://example.com/hooks/status">
    <button class="btn btn-primary" type="submit">{ICON_WEBHOOK}{button}</button>
  </form>"#,
            field = esc(i18n::t(loc, "status.subscribe.field")),
            button = esc(i18n::t(loc, "status.subscribe.button")),
        )
    } else {
        String::new()
    };
    format!(
        r#"<section class="channels" id="subscribe">
  <div class="channels__links"><a class="btn btn-secondary" href="/feed.xml">{ICON_RSS}{rss}</a><a class="btn btn-secondary" href="/api/status">{ICON_BRACES}{json}</a></div>{form}
</section>"#,
        rss = esc(i18n::t(loc, "status.rss_feed")),
        json = esc(i18n::t(loc, "status.json_api")),
    )
}

fn render_footer(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    format!(
        r##"<footer class="site-foot">
    <nav class="site-foot__links" aria-label="{past}"><a href="#past-incidents">{ICON_HISTORY}{past}</a><a href="/api/status">{ICON_BRACES}{json}</a><a href="/feed.xml">{ICON_RSS}{rss}</a></nav>
    <span class="site-foot__updated">{updated}</span>
  </footer>"##,
        past = esc(i18n::t(loc, "status.past")),
        json = esc(i18n::t(loc, "status.json_api")),
        rss = esc(i18n::t(loc, "status.rss")),
        updated = esc(&i18n::tf(
            loc,
            "status.updated",
            &[("time", &rel_time_l(loc, view.updated_at, now))]
        )),
    )
}

/// Host capacity: three gauges (worst host per metric) and the 24-hour heat strip.
fn render_infra(loc: odyssey::Locale, infra: Option<&vitals::InfraPublic>, now: i64) -> String {
    let Some(infra) = infra else {
        return String::new();
    };
    let pill_cls = match infra.overall {
        "high" => "pill-down",
        "elevated" => "pill-warn",
        "ok" => "pill-ok",
        _ => "pill-state",
    };
    let mut gauges = String::new();
    for band in &infra.bands {
        let pct_text = band
            .worst_pct
            .map(|pct| format!("{:.0}%", pct.round()))
            .unwrap_or_else(|| "—".to_string());
        let width = band
            .worst_pct
            .map(|pct| pct.clamp(0.0, 100.0))
            .unwrap_or(0.0);
        gauges.push_str(&format!(
            r#"<div class="gauge gauge--{band}"><span class="gauge__head"><span class="gauge__label">{label}</span><span class="gauge__value">{pct}</span></span><span class="gauge__track"><span class="gauge__fill" style="width:{width:.0}%"></span></span><span class="gauge__band">{band_label}</span></div>"#,
            label = esc(infra_metric_label(loc, band.metric)),
            band = esc(band.band),
            pct = esc(&pct_text),
            band_label = esc(infra_band_label(loc, band.band)),
        ));
    }

    let current = now / vitals::TREND_BUCKET_SECS;
    let mut cells = String::new();
    for (idx, band) in infra.trend.iter().enumerate() {
        let bucket = current - infra.trend.len() as i64 + 1 + idx as i64;
        cells.push_str(&format!(
            r#"<span class="heat__cell heat__cell--{band}" data-hour="{hour}" data-band="{band_label}"></span>"#,
            band = esc(band),
            hour = esc(&hour_label(bucket)),
            band_label = esc(infra_band_label(loc, band)),
        ));
    }

    format!(
        r#"<section class="infra">
  <div class="infra__head"><h2 class="infra__title">{title}</h2><span class="pill {pill_cls}">{state}</span></div>
  <div class="infra__body">
    <div class="gauges">{gauges}</div>
    <div class="heat"><div class="heat__cells" role="img" aria-label="{trend}">{cells}</div><div class="axis"><span>{h24}</span><span>{h12}</span><span>{now_label}</span></div></div>
  </div>
</section>"#,
        title = esc(i18n::t(loc, "infra.title")),
        state = esc(infra_state_label(loc, infra.overall)),
        trend = esc(i18n::t(loc, "infra.trend")),
        h24 = esc(i18n::t(loc, "status.heat.h24")),
        h12 = esc(i18n::t(loc, "status.heat.h12")),
        now_label = esc(i18n::t(loc, "status.heat.now")),
    )
}

fn infra_metric_label(loc: odyssey::Locale, metric: &str) -> &str {
    match metric {
        "cpu" => i18n::t(loc, "infra.metric.cpu"),
        "memory" => i18n::t(loc, "infra.metric.memory"),
        "disk" => i18n::t(loc, "infra.metric.disk"),
        _ => metric,
    }
}

fn infra_band_label(loc: odyssey::Locale, band: &str) -> &'static str {
    match band {
        "ok" => i18n::t(loc, "infra.band.ok"),
        "elevated" => i18n::t(loc, "infra.band.elevated"),
        "high" => i18n::t(loc, "infra.band.high"),
        _ => i18n::t(loc, "infra.band.unknown"),
    }
}

fn infra_state_label(loc: odyssey::Locale, band: &str) -> &'static str {
    match band {
        "ok" => i18n::t(loc, "infra.state.ok"),
        "elevated" => i18n::t(loc, "infra.state.elevated"),
        "high" => i18n::t(loc, "infra.state.high"),
        _ => i18n::t(loc, "infra.state.unknown"),
    }
}

fn hour_label(bucket: i64) -> String {
    let secs = bucket * vitals::TREND_BUCKET_SECS;
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(dt) => format!("{:02}:00 UTC", dt.hour()),
        Err(_) => "00:00 UTC".to_string(),
    }
}

/// Updates belonging to one incident, newest first (`view.updates` is already newest-first).
fn updates_for<'a>(view: &'a StatusView, incident_id: &str) -> Vec<&'a IncidentUpdate> {
    view.updates
        .iter()
        .filter(|u| u.incident_id == incident_id)
        .collect()
}

/// Incident progress: Investigating → Identified → Monitoring → Resolved. The current stage is
/// marked `aria-current="step"`; a resolved incident turns the whole track green.
fn render_stage_track(loc: odyssey::Locale, status: &str) -> String {
    const STAGES: [&str; 4] = ["investigating", "identified", "monitoring", "resolved"];
    let current = STAGES.iter().position(|s| *s == status).unwrap_or(0);
    let resolved = status == "resolved";
    let mut out = format!(
        r#"<div class="stages{}" aria-label="{}">"#,
        if resolved { " stages--resolved" } else { "" },
        esc(i18n::t(loc, "status.incident.progress"))
    );
    for (i, stage) in STAGES.iter().enumerate() {
        let (cls, current_attr) = if i < current {
            (" stage--done", "")
        } else if i == current {
            (" stage--now", r#" aria-current="step""#)
        } else {
            (" stage--todo", "")
        };
        if i > 0 {
            out.push_str(&format!(
                r#"<span class="stage__link{}" aria-hidden="true"></span>"#,
                if i <= current {
                    " stage__link--done"
                } else {
                    ""
                }
            ));
        }
        out.push_str(&format!(
            r#"<span class="stage{cls}"{current_attr}><span class="stage__dot" aria-hidden="true"></span><span class="stage__label">{label}</span></span>"#,
            label = esc(&incident_status_label_l(loc, stage)),
        ));
    }
    out.push_str("</div>");
    out
}

/// The expandable update timeline for one incident: its updates newest first, closing with
/// the opening report (the incident row itself).
fn render_timeline(
    inc: &Incident,
    updates: &[&IncidentUpdate],
    now: i64,
    loc: odyssey::Locale,
) -> String {
    let mut items = String::new();
    for u in updates {
        items.push_str(&format!(
            r#"<li class="timeline__item" data-stage="{stage}">{pill} <span class="timeline__time">{ago}</span><p class="timeline__body">{body}</p></li>"#,
            pill = incident_status_pill_l(loc, &u.status),
            stage = esc(&u.status),
            ago = esc(&rel_time_l(loc, u.created_at, now)),
            body = esc(&u.body),
        ));
    }
    let reported_label = if loc == odyssey::Locale::En {
        "reported"
    } else {
        i18n::t(loc, "status.incident.reported")
    };
    items.push_str(&format!(
        r#"<li class="timeline__item" data-stage="reported"><span class="pill pill-state">{reported}</span> <span class="timeline__time">{ago}</span><p class="timeline__body">{body}</p></li>"#,
        reported = esc(reported_label),
        ago = esc(&rel_time_l(loc, inc.created_at, now)),
        body = esc(&inc.body),
    ));
    let summary = if loc == odyssey::Locale::En {
        format!("Timeline ({})", updates.len() + 1)
    } else {
        i18n::tf(
            loc,
            "status.timeline",
            &[("n", &(updates.len() + 1).to_string())],
        )
    };
    format!(
        r#"<details class="timeline"><summary>{summary}</summary><ol>{items}</ol></details>"#,
        summary = esc(&summary),
    )
}

/// Affected components as chips carrying each component's CURRENT state mark (looked up from the
/// projected catalog, so an incident cannot claim a state the probes do not show).
fn render_chips(view: &StatusView, affected: &str) -> String {
    let names = affected_names(affected);
    if names.is_empty() {
        return String::new();
    }
    let chips: String = names
        .iter()
        .map(|n| {
            let state = view
                .components
                .iter()
                .find(|c| c.name == *n)
                .map(tile_state)
                .unwrap_or("operational");
            format!(
                r#"<span class="chip chip--{state}">{mark}{name}</span>"#,
                mark = mark(state, "sm"),
                name = esc(n)
            )
        })
        .collect();
    format!(r#"<div class="chips">{chips}</div>"#)
}

/// The "Active incidents" ledger — one entry per non-resolved incident. Empty string (section
/// omitted) when everything is resolved.
fn render_active_incidents(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    let active: Vec<&Incident> = view
        .incidents
        .iter()
        .filter(|i| i.status != "resolved")
        .collect();
    if active.is_empty() {
        return String::new();
    }
    let mut out = format!(
        r#"<section class="ledger" id="active-incidents"><h2 class="ledger__title">{}</h2>"#,
        esc(i18n::t(loc, "status.active"))
    );
    for inc in active {
        let updates = updates_for(view, &inc.id);
        // The card leads with the LATEST update (or the opening report when none yet).
        let latest = updates
            .first()
            .map(|u| u.body.as_str())
            .unwrap_or(inc.body.as_str());
        let opened_ago = rel_time_l(loc, inc.created_at, now);
        let updated_ago = rel_time_l(loc, inc.updated_at, now);
        let time_line = if loc == odyssey::Locale::En {
            format!("Opened {opened_ago} · Last update {updated_ago}")
        } else {
            i18n::tf(
                loc,
                "status.incident.opened_updated",
                &[("opened", &opened_ago), ("updated", &updated_ago)],
            )
        };
        out.push_str(&format!(
            r#"<article class="incident incident--{sev}">
  <div class="incident__head"><h3 class="incident__title">{title}</h3>{sev_pill}</div>
  {stages}
  {chips}
  <p class="incident__body">{latest}</p>
  <p class="incident__time">{time_line}</p>
  {timeline}
</article>"#,
            sev = esc(&inc.severity),
            title = esc(&inc.title),
            sev_pill = severity_pill_l(loc, &inc.severity),
            stages = render_stage_track(loc, &inc.status),
            chips = render_chips(view, &inc.affected),
            latest = esc(latest),
            time_line = esc(&time_line),
            timeline = render_timeline(inc, &updates, now, loc),
        ));
    }
    out.push_str("</section>");
    out
}

/// Upcoming/ongoing maintenance windows. Empty string when none.
fn render_maintenances(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    if view.maintenances.is_empty() {
        return String::new();
    }
    let mut out = format!(
        r#"<section class="ledger" id="maintenance"><h2 class="ledger__title">{}</h2>"#,
        esc(i18n::t(loc, "status.maintenance"))
    );
    for m in &view.maintenances {
        // A prominent countdown value: ongoing windows show time-to-end, upcoming ones
        // time-to-start (upcoming/ongoing windows are the only ones on the public surface).
        let ongoing = maintenance_ongoing(m, now);
        let (state_pill, countdown) = if ongoing {
            let label = if loc == odyssey::Locale::En {
                "in progress"
            } else {
                i18n::t(loc, "status.maint.in_progress")
            };
            let ends = fmt_countdown_l(loc, m.ends_at - now);
            let cd = if loc == odyssey::Locale::En {
                format!("ends in {ends}")
            } else {
                i18n::tf(loc, "status.maint.ends_in", &[("t", &ends)])
            };
            (
                format!(r#"<span class="pill pill-info">{}</span>"#, esc(label)),
                cd,
            )
        } else {
            let label = if loc == odyssey::Locale::En {
                "scheduled"
            } else {
                i18n::t(loc, "status.maint.scheduled")
            };
            let starts = fmt_countdown_l(loc, m.starts_at - now);
            let cd = if loc == odyssey::Locale::En {
                format!("starts in {starts}")
            } else {
                i18n::tf(loc, "status.maint.starts_in", &[("t", &starts)])
            };
            (
                format!(r#"<span class="pill pill-state">{}</span>"#, esc(label)),
                cd,
            )
        };
        out.push_str(&format!(
            r#"<article class="incident incident--maintenance{live}">
  <div class="incident__head">{mark}<h3 class="incident__title">{title}</h3>{state_pill}</div>
  <div class="incident__timing"><span class="countdown">{countdown}</span><span class="window">{starts} → {ends}</span></div>
  {chips}
  <p class="incident__body">{body}</p>
</article>"#,
            live = if ongoing { " is-live" } else { "" },
            mark = mark("maintenance", "lg"),
            title = esc(&m.title),
            countdown = esc(&countdown),
            starts = esc(&fmt_datetime(m.starts_at)),
            ends = esc(&fmt_datetime(m.ends_at)),
            chips = render_chips(view, &m.affected),
            body = esc(&m.body),
        ));
    }
    out.push_str("</section>");
    out
}

/// "Past incidents": every RESOLVED public incident opened in the last 14 days, newest day
/// first (active ones live in the ledger above). One all-clear row replaces fourteen empty
/// buckets.
fn render_history(view: &StatusView, now: i64, loc: odyssey::Locale) -> String {
    let mut out = format!(
        r##"<section class="history" id="past-incidents"><h2 class="history__title">{}</h2>"##,
        esc(i18n::t(loc, "status.past"))
    );
    let today = day_bucket(now);
    let mut rendered = 0usize;
    for offset in 0..HISTORY_DAYS {
        let day = today - offset;
        for inc in view
            .incidents
            .iter()
            .filter(|inc| inc.status == "resolved" && day_bucket(inc.created_at) == day)
        {
            rendered += 1;
            let resolved = inc.status == "resolved";
            let opened_ago = rel_time_l(loc, inc.created_at, now);
            let mut when = if resolved && inc.resolved_at > 0 {
                let resolved_ago = rel_time_l(loc, inc.resolved_at, now);
                if loc == odyssey::Locale::En {
                    format!("Opened {opened_ago} · Resolved {resolved_ago}")
                } else {
                    i18n::tf(
                        loc,
                        "status.incident.opened_resolved",
                        &[("opened", &opened_ago), ("resolved", &resolved_ago)],
                    )
                }
            } else if loc == odyssey::Locale::En {
                format!("Opened {opened_ago}")
            } else {
                i18n::tf(loc, "status.incident.opened_at", &[("ago", &opened_ago)])
            };
            if resolved && inc.resolved_at > 0 {
                let lasted = fmt_countdown_l(loc, inc.resolved_at - inc.created_at);
                let lasted_text = if loc == odyssey::Locale::En {
                    format!("· lasted {lasted}")
                } else {
                    i18n::tf(loc, "status.incident.lasted", &[("duration", &lasted)])
                };
                when.push(' ');
                when.push_str(&lasted_text);
            }
            out.push_str(&format!(
                r#"<article class="hrow{muted}">
  <time class="hrow__date" datetime="{iso}">{date}</time>
  <div class="hrow__main">
    <div class="hrow__head"><h3 class="hrow__title">{title}</h3>{sev_pill}{status_pill}</div>
    <p class="hrow__body">{body}</p>
    <span class="hrow__times">{when}</span>
  </div>
</article>"#,
                muted = if resolved { " hrow--resolved" } else { "" },
                iso = esc(&day_date(day)),
                date = esc(&fmt_date_l(loc, day * DAY_SECS)),
                title = esc(&inc.title),
                sev_pill = severity_pill_l(loc, &inc.severity),
                status_pill = incident_status_pill_l(loc, &inc.status),
                body = esc(&inc.body),
                when = esc(&when),
            ));
        }
    }
    if rendered == 0 {
        out.push_str(&format!(
            r#"<div class="hclear">{mark}<span>{text}</span></div>"#,
            mark = mark("operational", "md"),
            text = esc(i18n::t(loc, "status.none_history"))
        ));
    }
    out.push_str("</section>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ComponentView, DayStat, GroupView, StatusView};
    use crate::vitals::InfraPublic;
    use odyssey::Locale;

    fn component(name: &str, group_id: Option<&str>) -> ComponentView {
        ComponentView {
            name: name.to_string(),
            kind: "http".to_string(),
            status: "operational",
            group_id: group_id.map(str::to_string),
            uptime_24h: 100.0,
            uptime_7d: 99.98,
            uptime_90d: 99.95,
            latency_ms: Some(42),
            latency_avg_ms: Some(38),
            latency_points: vec![],
            last_checked: Some(1700000000),
            days: vec![DayStat {
                date: "2023-11-14".to_string(),
                status: "ok",
                uptime: Some(100.0),
            }],
        }
    }

    fn view(components: Vec<ComponentView>, groups: Vec<GroupView>) -> StatusView {
        StatusView {
            overall: "operational",
            updated_at: 1700000000,
            history_days: 30,
            components,
            groups,
            incidents: vec![],
            updates: vec![],
            maintenances: vec![],
            infra: None,
        }
    }

    /// Synthetic trailing "Other" section: ungrouped components when groups exist.
    #[test]
    fn render_status_live_with_trailing_other_section() {
        let view = view(
            vec![
                component("Gateway", Some("g_core")),
                component("Orphan", None),
            ],
            vec![GroupView {
                id: "g_core".to_string(),
                name: "Core".to_string(),
                position: 0,
                status: "operational",
            }],
        );

        let html = render_status_live(&view, 1700000000, Locale::En);

        assert_eq!(
            html.matches(r#"<section class="group group-"#).count(),
            2,
            "one named group plus one trailing Other section"
        );
        assert!(html.contains(r#"<h2 class="group__name">Core</h2>"#));
        assert!(html.contains(r#"<h2 class="group__name">Other</h2>"#));
        assert!(html.contains(r#"<span class="tile__name">Gateway</span>"#));
        assert!(html.contains(r#"<span class="tile__name">Orphan</span>"#));
        // An all-operational catalog never repeats the state word on tiles or groups.
        assert!(!html.contains(r#"class="tile__state""#));
        assert!(!html.contains(r#"class="group__state""#));
    }

    /// The trailing "Other" section label localizes with the page.
    #[test]
    fn render_status_live_localizes_the_trailing_other_section() {
        let view = view(
            vec![
                component("Gateway", Some("g_core")),
                component("Orphan", None),
            ],
            vec![GroupView {
                id: "g_core".to_string(),
                name: "Core".to_string(),
                position: 0,
                status: "operational",
            }],
        );

        let zh = render_status_live(&view, 1700000000, Locale::Zh);
        assert!(
            zh.contains(r#"<h2 class="group__name">其他</h2>"#),
            "zh Other label"
        );
        assert!(!zh.contains(r#"<h2 class="group__name">Other</h2>"#));
        assert!(zh.contains("30 天"), "zh evidence window value");

        let ja = render_status_live(&view, 1700000000, Locale::Ja);
        assert!(
            ja.contains(r#"<h2 class="group__name">その他</h2>"#),
            "ja Other label"
        );
    }

    /// A never-probed component reads pending: dash mark, no uptime value, explicit state word.
    #[test]
    fn pending_component_never_claims_operational_evidence() {
        let mut pending = component("Wiki", None);
        pending.last_checked = None;
        pending.days = vec![DayStat {
            date: "2023-11-14".to_string(),
            status: "unknown",
            uptime: None,
        }];
        let html = render_status_live(&view(vec![pending], vec![]), 1700000000, Locale::En);
        assert!(html.contains(r#"<details class="tile tile--pending" name="tile">"#));
        assert!(html.contains(r#"<span class="tile__state">Awaiting first check</span>"#));
        assert!(html.contains(r#"class="cell cell--unknown""#));
        assert!(
            !html.contains(r#"class="mast__uptime""#),
            "no evidence-window average before the first check"
        );
    }

    /// The estate strip takes the worst state per day across components.
    #[test]
    fn estate_strip_takes_the_worst_state_per_day() {
        let mut a = component("A", None);
        let mut b = component("B", None);
        a.days = vec![
            DayStat {
                date: "2023-11-13".to_string(),
                status: "ok",
                uptime: Some(100.0),
            },
            DayStat {
                date: "2023-11-14".to_string(),
                status: "warn",
                uptime: Some(98.0),
            },
        ];
        b.days = vec![
            DayStat {
                date: "2023-11-13".to_string(),
                status: "down",
                uptime: Some(40.0),
            },
            DayStat {
                date: "2023-11-14".to_string(),
                status: "ok",
                uptime: Some(100.0),
            },
        ];
        let html = render_status_live(&view(vec![a, b], vec![]), 1700000000, Locale::En);
        let estate_start = html.find(r#"<section class="estate">"#).unwrap();
        let estate =
            &html[estate_start..html[estate_start..].find("</section>").unwrap() + estate_start];
        assert!(estate.contains(
            r#"<span class="cell cell--down" data-date="2023-11-13" data-uptime="70.00%""#
        ));
        assert!(estate.contains(
            r#"<span class="cell cell--warn" data-date="2023-11-14" data-uptime="99.00%""#
        ));
    }

    /// Synthetic infra block without a live vitals poller.
    #[test]
    fn render_status_live_with_synthetic_infra() {
        let mut v = view(vec![], vec![]);
        v.infra = Some(InfraPublic {
            overall: "ok",
            bands: vec![],
            trend: vec!["ok"; 24],
        });

        let html = render_status_live(&v, 1700000000, Locale::En);

        assert!(
            html.contains(r#"<section class="infra">"#),
            "infra section renders when present"
        );
        assert_eq!(
            html.matches(r#"class="heat__cell heat__cell--ok""#).count(),
            24
        );
        assert!(html.contains("<span>24h ago</span><span>12h ago</span><span>Now</span>"));
    }

    /// The stage track marks exactly one current stage and turns green once resolved.
    #[test]
    fn stage_track_marks_the_current_stage() {
        let html = render_stage_track(Locale::En, "identified");
        assert_eq!(html.matches(r#"aria-current="step""#).count(), 1);
        assert!(html.contains(r#"<span class="stage stage--done"><span class="stage__dot" aria-hidden="true"></span><span class="stage__label">Investigating</span></span>"#));
        assert!(html.contains(r#"<span class="stage stage--now" aria-current="step"><span class="stage__dot" aria-hidden="true"></span><span class="stage__label">Identified</span></span>"#));
        assert!(!html.contains("stages--resolved"));
        let resolved = render_stage_track(Locale::Ja, "resolved");
        assert!(resolved.starts_with(r#"<div class="stages stages--resolved""#));
        assert!(resolved.contains("解決済み"));
    }
}
