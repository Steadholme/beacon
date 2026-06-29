//! Gateway-injected identity for the admin surface.
//!
//! Beacon's admin pages do NO login of their own. They sit behind a Sluice `auth=sso`
//! route, where the gateway runs the OIDC browser login against Keystone, STRIPS any
//! inbound `X-Auth-*`, and injects the verified `X-Auth-Subject` / `X-Auth-Email` /
//! `X-Auth-Scope`. Because Beacon is internal-only (never publicly reachable), it TRUSTS
//! those headers as the authenticated operator.

use axum::http::HeaderMap;

use crate::error::AppError;

pub const HEADER_SUBJECT: &str = "x-auth-subject";
pub const HEADER_EMAIL: &str = "x-auth-email";

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
