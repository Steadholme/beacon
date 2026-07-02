//! Outbound webhook notifications for public subscribers.
//!
//! When an incident is opened or updated, Beacon fans a signed JSON payload out to every
//! CONFIRMED webhook subscriber. Delivery is BEST-EFFORT and BOUNDED: each POST runs under a
//! short timeout on its own task, failures are logged (never fatal — an unreachable
//! subscriber must never break the operator's admin action or the public page), and the
//! fan-out is capped at [`MAX_FANOUT`] recipients.
//!
//! Each payload is signed with the subscriber's own `secret` using HMAC-SHA256 (pure Rust —
//! `hmac` + `sha2`, the same primitive eddy/relay use), carried in the `X-Beacon-Signature:
//! sha256=<hex>` header so the endpoint can verify authenticity. The HTTP client reuses the
//! dependency-light raw-TCP + `tokio-rustls` transport from [`crate::probe`] (no reqwest).

use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::probe::{parse_http_url, tls_connector};
use crate::store::{Incident, IncidentUpdate, Store};

type HmacSha256 = Hmac<Sha256>;

/// Public origin the payload links back to (matches the RSS feed's `SITE_BASE_URL`).
const PAGE_URL: &str = "https://status.w33d.xyz/status";

/// Fan-out recipient cap: plenty for a status page while bounding a single event's work.
pub const MAX_FANOUT: usize = 200;

/// Webhook event token for a newly opened incident.
pub const EVENT_OPENED: &str = "incident.opened";
/// Webhook event token for a timeline update / status move / resolve.
pub const EVENT_UPDATED: &str = "incident.updated";

/// Lowercase-hex HMAC-SHA256 of `body` under `secret` — the value carried (as `sha256=<hex>`)
/// in the `X-Beacon-Signature` header so a subscriber can verify the payload's authenticity.
pub fn sign(secret: &str, body: &str) -> String {
    // HMAC accepts any key length, so `new_from_slice` never errors here.
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Build the canonical JSON payload for an incident event. `serde_json` guarantees every
/// interpolated string is escaped, and signing/verification both run over these exact bytes.
pub fn incident_body(
    event: &str,
    incident: &Incident,
    update: Option<&IncidentUpdate>,
    sent_at: i64,
) -> String {
    let payload = serde_json::json!({
        "event": event,
        "sent_at": sent_at,
        "page_url": PAGE_URL,
        "incident": incident,
        "update": update,
    });
    payload.to_string()
}

/// Fan a signed `body` out to every CONFIRMED webhook subscriber (best-effort, bounded).
/// Each recipient gets its own signature (its own `secret`); a failed delivery is logged and
/// skipped, never propagated. Intended to be `tokio::spawn`ed from a write handler so the
/// operator's response is never blocked on subscriber reachability.
pub async fn fan_out(store: Arc<dyn Store>, event: &'static str, body: String, timeout: Duration) {
    let recipients: Vec<_> = store
        .list_subscribers()
        .await
        .into_iter()
        .filter(|s| s.confirmed && s.kind == "webhook")
        .take(MAX_FANOUT)
        .collect();
    if recipients.is_empty() {
        return;
    }

    let mut handles = Vec::with_capacity(recipients.len());
    for sub in recipients {
        let body = body.clone();
        handles.push(tokio::spawn(async move {
            let signature = sign(&sub.secret, &body);
            match deliver(&sub.target, event, &signature, &body, timeout).await {
                Ok(code) if (200..=299).contains(&code) => {
                    tracing::debug!(subscriber = sub.id, code, "webhook delivered");
                }
                Ok(code) => {
                    tracing::warn!(subscriber = sub.id, code, "webhook non-2xx (ignored)");
                }
                Err(e) => {
                    tracing::warn!(subscriber = sub.id, error = %e, "webhook delivery failed (ignored)");
                }
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

/// POST `body` as `application/json` to `url` (http or https), carrying the event + signature
/// headers, and return the HTTP status code. Bounded by `timeout` end-to-end.
pub async fn deliver(
    url: &str,
    event: &str,
    signature: &str,
    body: &str,
    timeout: Duration,
) -> std::io::Result<u16> {
    match tokio::time::timeout(timeout, post_json(url, event, signature, body)).await {
        Ok(res) => res,
        Err(_) => Err(io_err("webhook delivery timed out")),
    }
}

async fn post_json(url: &str, event: &str, signature: &str, body: &str) -> std::io::Result<u16> {
    let (tls, host, port, path) =
        parse_http_url(url).ok_or_else(|| io_err("invalid webhook target URL"))?;
    let tcp = TcpStream::connect((host.as_str(), port)).await?;
    if tls {
        let server_name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| io_err("invalid TLS server name"))?;
        let stream = tls_connector().connect(server_name, tcp).await?;
        send_recv(stream, &host, &path, event, signature, body).await
    } else {
        send_recv(tcp, &host, &path, event, signature, body).await
    }
}

/// Write a minimal HTTP/1.1 POST over `stream` and parse the status code from the first line.
async fn send_recv<S>(
    mut stream: S,
    host: &str,
    path: &str,
    event: &str,
    signature: &str,
    body: &str,
) -> std::io::Result<u16>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: beacon/0.1\r\n\
         Content-Type: application/json\r\nX-Beacon-Event: {event}\r\n\
         X-Beacon-Signature: sha256={signature}\r\nContent-Length: {len}\r\n\
         Connection: close\r\n\r\n{body}",
        len = body.len(),
    );
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    // We only need the status line. Read until the first CRLF (or a small cap / EOF).
    let mut acc: Vec<u8> = Vec::with_capacity(256);
    let mut buf = [0u8; 256];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        acc.extend_from_slice(&buf[..n]);
        if acc.windows(2).any(|w| w == b"\r\n") || acc.len() > 8192 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&acc);
    let line = text.lines().next().unwrap_or("");
    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| io_err("no HTTP status code in webhook response"))
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_stable_hex_and_key_sensitive() {
        let a = sign("secret-key", "payload");
        // Deterministic + hex + 32-byte (64 hex char) SHA-256 tag.
        assert_eq!(a, sign("secret-key", "payload"));
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Different key or body -> different signature.
        assert_ne!(a, sign("other-key", "payload"));
        assert_ne!(a, sign("secret-key", "payload2"));
    }

    #[test]
    fn signature_matches_known_vector() {
        // RFC 4231-style check against an independently computed HMAC-SHA256.
        // hex(HMAC_SHA256("key", "The quick brown fox jumps over the lazy dog"))
        assert_eq!(
            sign("key", "The quick brown fox jumps over the lazy dog"),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn incident_body_is_valid_json_with_fields() {
        let inc = Incident {
            id: "inc_1".to_string(),
            // A double-quote must be JSON-escaped in the wire bytes (webhook consumers parse
            // JSON, not HTML — angle brackets stay literal, which is correct for a payload).
            title: "Cache \"CDN\" degraded".to_string(),
            status: "investigating".to_string(),
            severity: "major".to_string(),
            affected: "Gateway".to_string(),
            body: "5% errors".to_string(),
            created_at: 100,
            updated_at: 100,
            resolved_at: 0,
        };
        let body = incident_body(EVENT_OPENED, &inc, None, 123);
        // Structural JSON escaping: the embedded quote is backslash-escaped in the raw bytes.
        assert!(body.contains(r#"Cache \"CDN\" degraded"#), "quotes are JSON-escaped");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["event"], "incident.opened");
        assert_eq!(v["sent_at"], 123);
        assert_eq!(v["incident"]["title"], "Cache \"CDN\" degraded", "round-trips exactly");
        assert_eq!(v["incident"]["severity"], "major");
        assert_eq!(v["page_url"], "https://status.w33d.xyz/status");
        assert!(v["update"].is_null(), "no update on an open event");
        // The signature is computed over these exact bytes and is deterministic.
        let sig = sign("s3cr3t", &body);
        assert_eq!(sig, sign("s3cr3t", &body));
    }
}
