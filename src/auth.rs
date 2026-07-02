//! Gateway-injected identity + double-submit CSRF for the admin surface.
//!
//! Beacon's admin pages do NO login of their own. They sit behind a Sluice `auth=sso`
//! route, where the gateway runs the OIDC browser login against Keystone, STRIPS any
//! inbound `X-Auth-*`, and injects the verified `X-Auth-Subject` / `X-Auth-Email` /
//! `X-Auth-Scope`. Because Beacon is internal-only (never publicly reachable), it TRUSTS
//! those headers as the authenticated operator.
//!
//! State-changing POSTs (incident create/update/resolve, maintenance create) are
//! double-submit CSRF protected, mirroring lodestar: a random token lives in a JS-readable
//! `__Host-csrf` cookie AND in a hidden form field; the POST is accepted only when the two
//! match. The token is minted once and REUSED across page renders.

use axum::http::{header, HeaderMap};

use crate::error::AppError;

pub const HEADER_SUBJECT: &str = "x-auth-subject";
pub const HEADER_EMAIL: &str = "x-auth-email";

/// Double-submit CSRF cookie. `__Host-` prefix => Secure + Path=/ + no Domain, so the
/// browser only ever returns it over TLS to this exact host.
pub const CSRF_COOKIE: &str = "__Host-csrf";
/// CSRF cookie lifetime, seconds.
const CSRF_TTL: u64 = 3600;

/// The authenticated operator's email, if the gateway injected one.
pub fn admin_email(headers: &HeaderMap) -> Option<String> {
    header_value(headers, HEADER_EMAIL)
}

/// The authenticated operator's subject, if the gateway injected one.
pub fn admin_subject(headers: &HeaderMap) -> Option<String> {
    header_value(headers, HEADER_SUBJECT)
}

/// Require an authenticated operator (a gateway-injected subject). Returns the subject, or
/// `Unauthorized` when no SSO identity is present — defense in depth behind the gateway.
pub fn require_admin(headers: &HeaderMap) -> Result<String, AppError> {
    admin_subject(headers).ok_or_else(|| {
        AppError::Unauthorized("no gateway SSO identity (X-Auth-Subject missing)".to_string())
    })
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// CSRF (double-submit), mirrored from lodestar.
// ---------------------------------------------------------------------------

/// Read a single cookie value from the request's `Cookie` header(s).
pub fn get_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    for hv in headers.get_all(header::COOKIE).iter() {
        let Ok(raw) = hv.to_str() else { continue };
        for pair in raw.split(';') {
            let pair = pair.trim();
            if let Some((k, v)) = pair.split_once('=') {
                if k.trim() == name {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// `Set-Cookie` value for the (JS-readable) CSRF cookie.
pub fn csrf_cookie(value: &str) -> String {
    format!("{CSRF_COOKIE}={value}; Path=/; Secure; SameSite=Lax; Max-Age={CSRF_TTL}")
}

/// Mint a fresh CSRF token: 32 CSPRNG bytes, hex-encoded.
pub fn new_csrf_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("OS CSPRNG unavailable");
    hex::encode(bytes)
}

/// Resolve the CSRF token to embed in this render's forms. Reuses the existing cookie token
/// when present (so it is stable across pages/tabs); otherwise mints one and returns the
/// matching `Set-Cookie` to attach to the response.
pub fn ensure_csrf(headers: &HeaderMap) -> (String, Option<String>) {
    match get_cookie(headers, CSRF_COOKIE) {
        Some(c) if !c.is_empty() => (c, None),
        _ => {
            let token = new_csrf_token();
            let set = csrf_cookie(&token);
            (token, Some(set))
        }
    }
}

/// Double-submit check: the `submitted` form token must equal the `__Host-csrf` cookie.
pub fn verify_csrf(headers: &HeaderMap, submitted: &str) -> Result<(), AppError> {
    let ok = match get_cookie(headers, CSRF_COOKIE) {
        Some(cookie) if !cookie.is_empty() => ct_eq(cookie.as_bytes(), submitted.as_bytes()),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(AppError::Unauthorized("CSRF token mismatch".to_string()))
    }
}

/// Length-checked constant-time byte comparison.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csrf_token_is_random_and_hex() {
        let a = new_csrf_token();
        let b = new_csrf_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn csrf_double_submit_matches_and_rejects() {
        let token = new_csrf_token();
        let mut headers = HeaderMap::new();
        headers.append(
            header::COOKIE,
            format!("{CSRF_COOKIE}={token}").parse().unwrap(),
        );
        assert!(verify_csrf(&headers, &token).is_ok());
        assert!(verify_csrf(&headers, "not-the-token").is_err());
        assert!(verify_csrf(&HeaderMap::new(), &token).is_err());
    }

    #[test]
    fn require_admin_needs_subject() {
        assert!(require_admin(&HeaderMap::new()).is_err());
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_SUBJECT, "u_123".parse().unwrap());
        assert_eq!(require_admin(&headers).unwrap(), "u_123");
    }
}
