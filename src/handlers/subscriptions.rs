//! PUBLIC status-update subscriptions (webhook, double opt-in).
//!
//! Visitors subscribe a webhook URL from the public status page. Because the page is
//! unauthenticated there is no session to protect, so these routes carry NO CSRF (unlike the
//! admin surface) — abuse is instead bounded by DOUBLE OPT-IN: a new subscriber is created
//! UNCONFIRMED and receives no fan-out until it is confirmed via its capability-token link.
//! The subscriber's `id` is an unguessable random token that doubles as the confirm and
//! unsubscribe link, and a per-subscriber `secret` (shown once on subscribe) lets the
//! endpoint verify the HMAC-SHA256 signature on every delivered payload.
//!
//! Beacon has no outbound mail path, so subscriptions are WEBHOOK-ONLY (the form notes this).
//! Everything interpolated into the rendered notices is HTML-escaped.

use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::Form;
use serde::Deserialize;

use crate::auth::new_csrf_token;
use crate::handlers::{esc, page_shell};
use crate::probe::parse_http_url;
use crate::store::Subscriber;
use crate::{now_secs, AppState};

/// Reject absurdly long targets outright (defensive bound on stored/echoed text).
const MAX_TARGET_LEN: usize = 2000;

#[derive(Debug, Deserialize)]
pub struct SubscribeForm {
    #[serde(default)]
    pub target: String,
}

/// `POST /subscriptions` — create an UNCONFIRMED webhook subscriber and show its confirm +
/// unsubscribe links and signing secret. Public (no auth, no CSRF); double opt-in gates
/// fan-out. Rejects a non-http(s) or over-long target with a 400-style notice.
pub async fn subscribe(State(state): State<AppState>, Form(form): Form<SubscribeForm>) -> Response {
    let target = form.target.trim();
    // Only http(s) webhooks; validate with the same minimal parser the prober/deliverer use.
    if target.is_empty() || target.len() > MAX_TARGET_LEN || parse_http_url(target).is_none() {
        return Html(page_shell(
            "Subscribe",
            &notice(
                "Invalid webhook URL",
                "<p class=\"hint\">Enter a valid <code>http://</code> or <code>https://</code> \
                 webhook URL to receive status updates.</p>\
                 <p class=\"hint--muted\"><a href=\"/status\">Back to status</a></p>",
            ),
        ))
        .into_response();
    }

    let subscriber = Subscriber {
        // Random capability-token id (safe to place in confirm/unsubscribe links).
        id: format!("sub_{}", new_csrf_token()),
        kind: "webhook".to_string(),
        target: target.to_string(),
        secret: new_csrf_token(),
        confirmed: false,
        created_at: now_secs(),
    };
    state.store.insert_subscriber(&subscriber).await;
    tracing::info!(id = subscriber.id, "webhook subscription created (unconfirmed)");

    let confirm = format!("/subscriptions/confirm?token={}", esc(&subscriber.id));
    let unsub = format!("/subscriptions/unsubscribe?token={}", esc(&subscriber.id));
    let inner = notice(
        "Confirm your subscription",
        &format!(
            "<p class=\"hint\">We registered the webhook <code>{target}</code>. \
             It will receive status updates once confirmed.</p>\
             <p class=\"hint\">Deliveries are POSTed as JSON and signed with the header \
             <code>X-Beacon-Signature: sha256=&lt;hmac&gt;</code> using this secret \
             (store it to verify payloads):</p>\
             <pre class=\"code\">{secret}</pre>\
             <p style=\"margin-top:18px\"><a class=\"btn btn-primary\" href=\"{confirm}\">Confirm subscription</a> \
             <a class=\"btn btn-ghost btn-sm\" href=\"{unsub}\">Cancel</a></p>\
             <p class=\"hint--muted\">Webhook-only — Beacon has no outbound mail path.</p>",
            target = esc(target),
            secret = esc(&subscriber.secret),
            confirm = confirm,
            unsub = unsub,
        ),
    );
    Html(page_shell("Subscribe", &inner)).into_response()
}

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    #[serde(default)]
    pub token: String,
}

/// `GET /subscriptions/confirm?token=…` — confirm a subscription by its capability token.
/// Public; an unknown/removed token yields a neutral notice (no enumeration signal).
pub async fn confirm(State(state): State<AppState>, Query(q): Query<TokenQuery>) -> Response {
    let inner = match state.store.get_subscriber(&q.token).await {
        Some(sub) => {
            state.store.confirm_subscriber(&sub.id).await;
            tracing::info!(id = sub.id, "webhook subscription confirmed");
            notice(
                "Subscription confirmed",
                &format!(
                    "<p class=\"hint\">The webhook <code>{target}</code> is now subscribed to \
                     status updates.</p><p class=\"hint--muted\"><a href=\"/status\">Back to status</a></p>",
                    target = esc(&sub.target),
                ),
            )
        }
        None => not_found_notice(),
    };
    Html(page_shell("Confirm subscription", &inner)).into_response()
}

/// `GET /subscriptions/unsubscribe?token=…` — remove a subscription by its capability token.
/// Public + idempotent: an unknown/already-removed token yields the same neutral notice.
pub async fn unsubscribe(State(state): State<AppState>, Query(q): Query<TokenQuery>) -> Response {
    let existed = state.store.get_subscriber(&q.token).await.is_some();
    if existed {
        state.store.delete_subscriber(&q.token).await;
        tracing::info!(id = %q.token, "webhook subscription removed");
    }
    let inner = notice(
        "Unsubscribed",
        "<p class=\"hint\">You will no longer receive status updates at that endpoint.</p>\
         <p class=\"hint--muted\"><a href=\"/status\">Back to status</a></p>",
    );
    Html(page_shell("Unsubscribe", &inner)).into_response()
}

/// A single-card notice body for the standalone subscription pages.
fn notice(heading: &str, body_html: &str) -> String {
    format!(
        "<div class=\"console__head\"><h1>{heading}</h1></div>\
         <section class=\"card\"><div class=\"card__body\">{body}</div></section>",
        heading = esc(heading),
        body = body_html,
    )
}

/// Neutral "not found" notice reused by confirm/unsubscribe for unknown tokens.
fn not_found_notice() -> String {
    notice(
        "Nothing to do",
        "<p class=\"hint\">That subscription link is invalid or has already been used.</p>\
         <p class=\"hint--muted\"><a href=\"/status\">Back to status</a></p>",
    )
}
