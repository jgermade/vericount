//! Security headers applied to every response.

use axum::{
    extract::Request,
    http::{
        header::{
            HeaderName, CONTENT_SECURITY_POLICY, REFERRER_POLICY, STRICT_TRANSPORT_SECURITY,
            X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
        },
        HeaderValue,
    },
    middleware::Next,
    response::Response,
};

/// Strict Content Security Policy.
///
/// - No `unsafe-inline` and no `unsafe-eval`: scripts and styles are only
///   loaded from same-origin files.
/// - Trusted Types are enforced and no policy may be created yet. When the
///   UI needs to insert HTML, allow a single named policy here and keep the
///   sanitization in that one place.
pub const CONTENT_SECURITY_POLICY_VALUE: &str = "default-src 'none'; \
    script-src 'self'; \
    style-src 'self'; \
    img-src 'self'; \
    font-src 'self'; \
    connect-src 'self'; \
    manifest-src 'self'; \
    base-uri 'none'; \
    form-action 'self'; \
    frame-ancestors 'none'; \
    require-trusted-types-for 'script'; \
    trusted-types 'none'";

const STATIC_HEADERS: [(HeaderName, &str); 8] = [
    (CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY_VALUE),
    (X_CONTENT_TYPE_OPTIONS, "nosniff"),
    (X_FRAME_OPTIONS, "DENY"),
    (REFERRER_POLICY, "no-referrer"),
    (
        STRICT_TRANSPORT_SECURITY,
        "max-age=63072000; includeSubDomains",
    ),
    (
        HeaderName::from_static("cross-origin-opener-policy"),
        "same-origin",
    ),
    (
        HeaderName::from_static("cross-origin-resource-policy"),
        "same-origin",
    ),
    (
        HeaderName::from_static("permissions-policy"),
        "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
    ),
];

/// Middleware that sets the security headers, overriding any value set by
/// a handler.
pub async fn headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in STATIC_HEADERS {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}
