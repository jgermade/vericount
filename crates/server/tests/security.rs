//! Tests for the security headers and for CSP compliance of the web client.

mod common;

use std::{fs, path::PathBuf};

use axum::http::{HeaderMap, StatusCode};
use common::web_dir;
use sqlx::postgres::PgPoolOptions;
use vericounts_server::security::CONTENT_SECURITY_POLICY_VALUE;

/// These tests never reach the database, so the pool is never connected.
async fn get(path: &str) -> (StatusCode, HeaderMap) {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused@127.0.0.1:1/unused")
        .unwrap();
    let response = common::get(&common::app(pool), path).await;
    (response.status, response.headers)
}

fn directive<'a>(csp: &'a str, name: &str) -> Option<&'a str> {
    csp.split(';')
        .map(str::trim)
        .find_map(|d| d.strip_prefix(name).filter(|rest| rest.starts_with(' ')))
        .map(str::trim)
}

#[test]
fn csp_is_strict() {
    let csp = CONTENT_SECURITY_POLICY_VALUE;
    for forbidden in [
        "unsafe-inline",
        "unsafe-eval",
        "unsafe-hashes",
        "data:",
        "*",
    ] {
        assert!(!csp.contains(forbidden), "CSP must not contain {forbidden}");
    }
    assert_eq!(directive(csp, "default-src"), Some("'none'"));
    assert_eq!(directive(csp, "script-src"), Some("'self'"));
    assert_eq!(directive(csp, "style-src"), Some("'self'"));
    assert_eq!(directive(csp, "base-uri"), Some("'none'"));
    assert_eq!(directive(csp, "frame-ancestors"), Some("'none'"));
    assert_eq!(
        directive(csp, "require-trusted-types-for"),
        Some("'script'")
    );
    assert!(directive(csp, "trusted-types").is_some());
}

#[tokio::test]
async fn security_headers_are_set_on_every_response() {
    for (path, status) in [
        ("/", StatusCode::OK),
        ("/healthz", StatusCode::OK),
        ("/app.js", StatusCode::OK),
        ("/does-not-exist", StatusCode::NOT_FOUND),
        ("/api/does-not-exist", StatusCode::NOT_FOUND),
    ] {
        let (got, headers) = get(path).await;
        assert_eq!(got, status, "{path}");
        let header = |name: &str| {
            headers
                .get(name)
                .unwrap_or_else(|| panic!("{path}: missing {name}"))
                .to_str()
                .unwrap()
        };
        assert_eq!(
            header("content-security-policy"),
            CONTENT_SECURITY_POLICY_VALUE
        );
        assert_eq!(header("x-content-type-options"), "nosniff");
        assert_eq!(header("x-frame-options"), "DENY");
        assert_eq!(header("referrer-policy"), "no-referrer");
        assert_eq!(header("cross-origin-opener-policy"), "same-origin");
        assert_eq!(header("cross-origin-resource-policy"), "same-origin");
        assert!(header("strict-transport-security").starts_with("max-age="));
        assert!(header("permissions-policy").contains("camera=()"));
    }
}

fn html_files(dir: PathBuf, out: &mut Vec<PathBuf>) {
    if dir.is_file() {
        out.push(dir);
        return;
    }
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            html_files(path, out);
        } else if path.extension().is_some_and(|e| e == "html") {
            out.push(path);
        }
    }
}

/// Returns the CSP violations found in an HTML document: inline scripts,
/// inline styles and inline event handlers.
fn inline_violations(html: &str) -> Vec<String> {
    let html = html.to_ascii_lowercase();
    let mut violations = Vec::new();
    for tag in html.split('<').skip(1) {
        let Some(tag) = tag.split('>').next() else {
            continue;
        };
        let name = tag.split_whitespace().next().unwrap_or("");
        if name == "style" {
            violations.push("inline <style> element".to_owned());
        }
        if name == "script" && !tag.contains(" src=") {
            violations.push("inline <script> element".to_owned());
        }
        for attr in tag.split_whitespace().skip(1) {
            let attr_name = attr.split('=').next().unwrap_or("");
            if attr_name == "style" {
                violations.push(format!("style attribute in <{name}>"));
            }
            if attr_name.starts_with("on") && attr.contains('=') {
                violations.push(format!("{attr_name} handler in <{name}>"));
            }
            if attr.contains("javascript:") {
                violations.push(format!("javascript: URL in <{name}>"));
            }
        }
    }
    violations
}

#[test]
fn web_client_has_no_inline_code() {
    let mut files = Vec::new();
    html_files(web_dir(), &mut files);
    for path in common::web_client_dirs() {
        html_files(path, &mut files);
    }
    assert!(files.len() >= 2, "web client HTML not found: {files:?}");
    for file in files {
        let violations = inline_violations(&fs::read_to_string(&file).unwrap());
        assert!(violations.is_empty(), "{}: {violations:?}", file.display());
    }
}

#[test]
fn inline_code_is_detected() {
    assert!(inline_violations(r#"<script src="/app.js"></script><p class="a">x</p>"#).is_empty());
    for bad in [
        "<script>alert(1)</script>",
        "<style>p{}</style>",
        r#"<p style="color:red">x</p>"#,
        r#"<button onclick="x()">x</button>"#,
        r#"<a href="javascript:x()">x</a>"#,
    ] {
        assert!(!inline_violations(bad).is_empty(), "not detected: {bad}");
    }
}
