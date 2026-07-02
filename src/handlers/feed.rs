//! Public incident feed: RSS 2.0 (`GET /feed.xml`).
//!
//! A DERIVED, read-only view over the incident history, mirroring inkwell's feed: incident
//! OPENS (the incident row is its own opening event) and every timeline UPDATE — including
//! resolves, which are just updates whose status is `resolved` — become items, newest
//! first. Being a public read it carries no identity, no CSRF, and no mutation, so there is
//! nothing to audit. All interpolated strings go through the same `esc` the HTML uses
//! (`&<>"'` — a strict superset of XML's requirements).

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};

use crate::handlers::{esc, incident_status_label, month_abbr};
use crate::AppState;

/// Canonical public origin of the status page (Beacon serves `status.w33d.xyz` behind the
/// gateway). Absolute links in the feed resolve against this, matching the estate's other
/// hardcoded origins (the app-bar portal + gateway logout links).
const SITE_BASE_URL: &str = "https://status.w33d.xyz";

/// Feed size cap: plenty of history for a reader without unbounded growth.
const MAX_ITEMS: usize = 100;

/// One flattened feed entry (an incident open or a timeline update), pre-sorting.
struct Entry {
    guid: String,
    title: String,
    body: String,
    ts: i64,
}

/// `GET /feed.xml` — RSS 2.0 of incident opens/updates/resolves, newest first (no auth).
pub async fn feed_xml(State(state): State<AppState>) -> Response {
    let incidents = state.store.list_incidents().await;
    let updates = state.store.list_incident_updates().await;

    let mut entries = Vec::with_capacity(incidents.len() + updates.len());
    for inc in &incidents {
        entries.push(Entry {
            guid: inc.id.clone(),
            title: format!("{} — new {} incident", inc.title, inc.severity),
            body: inc.body.clone(),
            ts: inc.created_at,
        });
    }
    for u in &updates {
        // Title each update after its incident; orphaned updates (incident deleted
        // out-of-band) fall back to the raw id rather than vanishing.
        let title = incidents
            .iter()
            .find(|i| i.id == u.incident_id)
            .map(|i| i.title.as_str())
            .unwrap_or(u.incident_id.as_str());
        entries.push(Entry {
            guid: u.id.clone(),
            title: format!("{} — {}", title, incident_status_label(&u.status)),
            body: u.body.clone(),
            ts: u.created_at,
        });
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.ts));
    entries.truncate(MAX_ITEMS);

    let last_build = entries.first().map(|e| e.ts).unwrap_or_else(crate::now_secs);
    let mut items = String::new();
    for e in &entries {
        items.push_str(&format!(
            "  <item>\n\
             \x20   <title>{title}</title>\n\
             \x20   <link>{base}/status</link>\n\
             \x20   <guid isPermaLink=\"false\">{guid}</guid>\n\
             \x20   <pubDate>{date}</pubDate>\n\
             \x20   <description>{desc}</description>\n\
             \x20 </item>\n",
            title = esc(&e.title),
            base = SITE_BASE_URL,
            guid = esc(&e.guid),
            date = fmt_rfc822(e.ts),
            desc = esc(&e.body),
        ));
    }

    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\">\n\
         <channel>\n\
         \x20 <title>HOLDFAST Status</title>\n\
         \x20 <link>{base}/status</link>\n\
         \x20 <atom:link href=\"{base}/feed.xml\" rel=\"self\" type=\"application/rss+xml\"/>\n\
         \x20 <description>Incident history for the HOLDFAST sovereign infrastructure.</description>\n\
         \x20 <lastBuildDate>{last_build}</lastBuildDate>\n\
         {items}</channel>\n\
         </rss>\n",
        base = SITE_BASE_URL,
        last_build = fmt_rfc822(last_build),
        items = items,
    );
    (
        [(header::CONTENT_TYPE, "application/rss+xml; charset=utf-8")],
        body,
    )
        .into_response()
}

/// Format epoch seconds as an RFC 822 date in GMT (RSS `pubDate` / `lastBuildDate`), e.g.
/// `Mon, 29 Jun 2026 12:00:00 GMT`. Falls back to the raw integer on an out-of-range
/// timestamp.
fn fmt_rfc822(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(dt) => format!(
            "{wd}, {day:02} {mon} {year} {h:02}:{m:02}:{s:02} GMT",
            wd = weekday_abbr(dt.weekday()),
            day = dt.day(),
            mon = month_abbr(dt.month()),
            year = dt.year(),
            h = dt.hour(),
            m = dt.minute(),
            s = dt.second(),
        ),
        Err(_) => secs.to_string(),
    }
}

fn weekday_abbr(wd: time::Weekday) -> &'static str {
    use time::Weekday::*;
    match wd {
        Monday => "Mon",
        Tuesday => "Tue",
        Wednesday => "Wed",
        Thursday => "Thu",
        Friday => "Fri",
        Saturday => "Sat",
        Sunday => "Sun",
    }
}
