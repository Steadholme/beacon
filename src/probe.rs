//! Active probes: HTTP (GET, expect 2xx) and TCP (connect succeeds).
//!
//! Deliberately dependency-light — a raw `tokio::net::TcpStream` for the connect, wrapped
//! in `tokio-rustls` (ring backend, Mozilla roots via `webpki-roots`) for `https://`
//! targets. This matches the keystone/keyward portability choice (rustls + ring, no
//! openssl) and gives full control over the timeout and the measured latency. Every probe
//! returns a [`ProbeOutcome`] (never errors out): a failure is just `ok = false` with the
//! elapsed time up to the failure.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// The result of a single probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// True when the check succeeded (TCP connected, or HTTP returned a 2xx status).
    pub ok: bool,
    /// Wall-clock latency in milliseconds, measured to success or to the failure/timeout.
    pub latency_ms: i64,
}

/// Run a probe by kind. `"tcp"` connects to `host:port`; anything else is treated as an
/// HTTP(S) GET against the URL `target` and succeeds on a 2xx status.
pub async fn probe(kind: &str, target: &str, timeout: Duration) -> ProbeOutcome {
    match kind {
        "tcp" => probe_tcp(target, timeout).await,
        _ => probe_http(target, timeout).await,
    }
}

/// TCP liveness: a successful connect within `timeout` is "up".
async fn probe_tcp(target: &str, timeout: Duration) -> ProbeOutcome {
    let start = Instant::now();
    let ok = matches!(
        tokio::time::timeout(timeout, TcpStream::connect(target)).await,
        Ok(Ok(_))
    );
    ProbeOutcome {
        ok,
        latency_ms: start.elapsed().as_millis() as i64,
    }
}

/// HTTP(S) liveness: GET the URL and succeed on a 2xx status, all within `timeout`.
async fn probe_http(target: &str, timeout: Duration) -> ProbeOutcome {
    let start = Instant::now();
    let result = tokio::time::timeout(timeout, http_status(target)).await;
    let latency_ms = start.elapsed().as_millis() as i64;
    let ok = matches!(result, Ok(Ok(code)) if (200..=299).contains(&code));
    if let Ok(Err(e)) = &result {
        tracing::debug!(target = target, error = %e, "http probe error");
    }
    ProbeOutcome { ok, latency_ms }
}

/// Connect, send a minimal `GET`, and return the response status code.
async fn http_status(target: &str) -> std::io::Result<u16> {
    let (tls, host, port, path) =
        parse_http_url(target).ok_or_else(|| io_err("invalid http target URL"))?;
    let tcp = TcpStream::connect((host.as_str(), port)).await?;
    if tls {
        let server_name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| io_err("invalid TLS server name"))?;
        let stream = tls_connector().connect(server_name, tcp).await?;
        send_recv_status(stream, &host, &path).await
    } else {
        send_recv_status(tcp, &host, &path).await
    }
}

/// Write a minimal HTTP/1.1 GET over `stream` and parse the status code from the first line.
async fn send_recv_status<S>(mut stream: S, host: &str, path: &str) -> std::io::Result<u16>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: beacon/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n"
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
    parse_status_line(&acc)
}

/// Extract the numeric status code from an HTTP status line (`HTTP/1.1 200 OK`).
fn parse_status_line(buf: &[u8]) -> std::io::Result<u16> {
    let text = String::from_utf8_lossy(buf);
    let line = text.lines().next().unwrap_or("");
    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| io_err("no HTTP status code in response"))
}

/// Parse `http(s)://host[:port]/path` into `(tls, host, port, path)`. Minimal by design —
/// the targets are operator-controlled service URLs, not arbitrary user input.
fn parse_http_url(url: &str) -> Option<(bool, String, u16, String)> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return None;
    };

    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return None;
    }

    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().ok()?),
        None => (authority.to_string(), if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    let path = if path.is_empty() { "/".to_string() } else { path.to_string() };
    Some((tls, host, port, path))
}

/// Process-wide rustls client connector (ring provider + Mozilla roots), built once.
fn tls_connector() -> TlsConnector {
    static CONNECTOR: OnceLock<TlsConnector> = OnceLock::new();
    CONNECTOR
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("ring provider supports the default protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            TlsConnector::from(Arc::new(config))
        })
        .clone()
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_url_variants() {
        assert_eq!(
            parse_http_url("https://sso.w33d.xyz/healthz"),
            Some((true, "sso.w33d.xyz".to_string(), 443, "/healthz".to_string()))
        );
        assert_eq!(
            parse_http_url("http://keyward:8200/healthz"),
            Some((false, "keyward".to_string(), 8200, "/healthz".to_string()))
        );
        assert_eq!(
            parse_http_url("http://host"),
            Some((false, "host".to_string(), 80, "/".to_string()))
        );
        assert_eq!(parse_http_url("ftp://nope"), None);
        assert_eq!(parse_http_url("https://:443/x"), None);
    }

    #[test]
    fn parse_status_line_reads_code() {
        assert_eq!(
            parse_status_line(b"HTTP/1.1 200 OK\r\n\r\n").unwrap(),
            200
        );
        assert_eq!(
            parse_status_line(b"HTTP/1.0 503 Service Unavailable\r\n").unwrap(),
            503
        );
        assert!(parse_status_line(b"garbage").is_err());
    }
}
