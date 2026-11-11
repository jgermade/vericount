//! Practice and live modes, and the certificate uploaded from the
//! settings.

mod common;

use std::sync::Arc;

use axum::{
    http::{Method, StatusCode},
    Router,
};
use base64::Engine;
use common::{get, post, request};
use p12_keystore::{
    Certificate, EncryptionAlgorithm, KeyStore, KeyStoreEntry, PrivateKey, PrivateKeyChain,
};
use rcgen::{CertificateParams, DnType, KeyPair};
use serde_json::{json, Value};
use sqlx::PgPool;
use vericount_aeat_mock::{Mock, SERVICE_PATH};
use vericount_core::aeat::Party;
use vericount_server::{
    aeat_service::{self, AeatService},
    config::{AeatSettings, SecretKey},
    directory, submission, AppState,
};

const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn invoice_body() -> Value {
    json!({
        "series": "A2026",
        "contact_id": common::CUSTOMER_ID,
        "description": "Consulting",
        "lines": [
            { "description": "Consulting", "quantity": "1", "unit_price": "100", "vat_rate": "21" }
        ]
    })
}

/// A company certificate and its key, as (PKCS#8 DER, certificate DER).
fn company_certificate(expired: bool) -> (Vec<u8>, Vec<u8>) {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, "EXAMPLE SL");
    params.distinguished_name.push(
        DnType::CustomDnType(vec![2, 5, 4, 97]),
        format!("VATES-{}", common::ISSUER_NIF),
    );
    if expired {
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2021, 1, 1);
    } else {
        params.not_after = rcgen::date_time_ymd(2099, 1, 1);
    }
    let key = KeyPair::generate().unwrap();
    let certificate = params.self_signed(&key).unwrap();
    (key.serialize_der(), certificate.der().to_vec())
}

/// The certificate in a PKCS#12 file, as certificates are exported, with
/// the legacy encryption many of them still use.
fn pkcs12(expired: bool, password: &str) -> Vec<u8> {
    let (key, certificate) = company_certificate(expired);
    let chain = PrivateKeyChain::new(
        b"1".to_vec(),
        PrivateKey::from_der(&key).unwrap(),
        [Certificate::from_der(&certificate).unwrap()],
    );
    let mut store = KeyStore::new();
    store.add_entry("certificate", KeyStoreEntry::PrivateKeyChain(chain));
    store
        .writer(password)
        .encryption_algorithm(EncryptionAlgorithm::PbeWithShaAnd3KeyTripleDesCbc)
        .write()
        .unwrap()
}

fn upload_body(file: &[u8], password: &str) -> Value {
    json!({
        "file": base64::engine::general_purpose::STANDARD.encode(file),
        "password": password,
    })
}

fn settings(endpoint_override: Option<String>) -> AeatSettings {
    AeatSettings {
        endpoint_override,
        producer: Some(Party {
            name: "Example Software SL".to_owned(),
            nif: "B12345674".to_owned(),
        }),
        secret_key: Some(SecretKey::parse(KEY).unwrap()),
        ..AeatSettings::test()
    }
}

/// Starts a mock on a free port and returns it with its URL.
async fn start_mock() -> (Mock, String) {
    let mock = Mock::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}{SERVICE_PATH}", listener.local_addr().unwrap());
    let router = mock.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (mock, endpoint)
}

/// The application with the practice mock running, as the server starts
/// it, and live records going to `live_mock`.
async fn app(pool: &PgPool, settings: AeatSettings) -> (AppState, Router) {
    common::sign_in(pool).await;
    common::seed(pool).await;
    let practice = aeat_service::start_practice_mock(pool).await.unwrap();
    let mut state = common::state(pool.clone());
    state.aeat = Arc::new(AeatService::new(settings, Some(practice)));
    state.aeat.reload(pool).await.unwrap();
    let app = vericount_server::app(state.clone(), common::web_dir());
    (state, app)
}

async fn send(state: &AppState) -> String {
    let issuer = directory::issuer(&state.pool).await.unwrap();
    let submitter = state.aeat.current().expect("records are being sent");
    match submission::run_once(&state.pool, &issuer, &submitter)
        .await
        .unwrap()
    {
        submission::RunOutcome::Sent { outcome, .. } => outcome,
        other => format!("{other:?}"),
    }
}

#[sqlx::test]
async fn an_installation_starts_in_practice_mode(pool: PgPool) {
    let (state, app) = app(&pool, AeatSettings::test()).await;
    let status = get(&app, "/api/aeat").await.json();
    assert_eq!(status["mode"], "practice");
    assert_eq!(status["enabled"], true);
    assert_eq!(status["certificate_upload"], false);
    assert_eq!(
        status["live_blockers"],
        json!(["certificate_missing", "producer_missing"])
    );

    // Practice records go to the mock inside the server.
    let invoice = post(&app, "/api/invoices", invoice_body()).await.json();
    assert_eq!(invoice["live"], false);
    assert!(invoice["qr_url"].as_str().unwrap().contains("prewww"));
    assert_eq!(send(&state).await, "accepted");

    // Without VERICOUNT_SECRET_KEY no certificate can be stored.
    let response = request(
        &app,
        Method::PUT,
        "/api/aeat/certificate",
        Some(upload_body(&pkcs12(false, "secret"), "secret")),
    )
    .await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.json()["error"]["code"], "secret_key_missing");

    let response = post(&app, "/api/aeat/live", json!({})).await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.json()["error"]["code"], "certificate_missing");
}

#[sqlx::test]
async fn the_practice_mock_continues_after_a_restart(pool: PgPool) {
    let (state, app) = app(&pool, AeatSettings::test()).await;
    post(&app, "/api/invoices", invoice_body()).await;
    let invoice = post(&app, "/api/invoices", invoice_body()).await.json();
    assert_eq!(send(&state).await, "accepted");

    // A new server: a new mock, which learns what the old one received.
    let practice = aeat_service::start_practice_mock(&pool).await.unwrap();
    let restarted = AeatService::new(AeatSettings::test(), Some(practice));
    restarted.reload(&pool).await.unwrap();
    let mut state = state.clone();
    state.aeat = Arc::new(restarted);
    let app = vericount_server::app(state.clone(), common::web_dir());

    post(&app, "/api/invoices", invoice_body()).await;
    let path = format!("/api/invoices/{}/cancellation", invoice["id"]);
    assert_eq!(post(&app, &path, json!({})).await.status, StatusCode::OK);
    assert_eq!(send(&state).await, "accepted");
    assert_eq!(get(&app, "/api/aeat").await.json()["pending"], 0);
}

#[sqlx::test]
async fn certificates_are_uploaded_checked_and_stored_encrypted(pool: PgPool) {
    let (_, app) = app(&pool, settings(None)).await;
    let upload = |body| request(&app, Method::PUT, "/api/aeat/certificate", Some(body));

    let wrong = upload(upload_body(&pkcs12(false, "secret"), "wrong")).await;
    assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(wrong.json()["error"]["code"], "certificate_unreadable");
    let garbage = upload(upload_body(b"not a certificate", "")).await;
    assert_eq!(garbage.json()["error"]["code"], "certificate_unreadable");
    let expired = upload(upload_body(&pkcs12(true, "secret"), "secret")).await;
    assert_eq!(expired.json()["error"]["code"], "certificate_expired");

    let uploaded = upload(upload_body(&pkcs12(false, "secret"), "secret")).await;
    assert_eq!(uploaded.status, StatusCode::OK);
    assert_eq!(uploaded.json()["nif"], common::ISSUER_NIF);
    assert_eq!(uploaded.json()["of_legal_person"], true);

    let status = get(&app, "/api/aeat").await.json();
    assert_eq!(status["certificate_upload"], true);
    assert_eq!(status["certificate"]["subject"], "EXAMPLE SL");
    assert_eq!(status["certificate_matches_company"], true);
    assert_eq!(status["live_blockers"], json!([]));

    // Neither the key nor the certificate can be read from the database.
    let stored: Vec<u8> = sqlx::query_scalar("SELECT identity FROM aeat_certificate")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains("-----BEGIN"));

    // A PEM file works too, and replaces the certificate.
    let (key, certificate) = company_certificate(false);
    let pem = format!(
        "{}{}",
        pem_block("PRIVATE KEY", &key),
        pem_block("CERTIFICATE", &certificate)
    );
    assert_eq!(
        upload(upload_body(pem.as_bytes(), "")).await.status,
        StatusCode::OK
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM aeat_certificate")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    let removed = request(&app, Method::DELETE, "/api/aeat/certificate", None).await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        get(&app, "/api/aeat").await.json()["live_blockers"],
        json!(["certificate_missing"])
    );
}

fn pem_block(label: &str, der: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    format!("-----BEGIN {label}-----\n{encoded}\n-----END {label}-----\n")
}

#[sqlx::test]
async fn live_mode_starts_a_new_chain_and_hides_practice_data(pool: PgPool) {
    let (live_mock, live_endpoint) = start_mock().await;
    let (state, app) = app(&pool, settings(Some(live_endpoint))).await;

    let practice = post(&app, "/api/invoices", invoice_body()).await.json();
    assert_eq!(practice["series_number"], "A2026-0001");
    post(&app, "/api/invoices", invoice_body()).await;
    assert_eq!(send(&state).await, "accepted");

    let uploaded = request(
        &app,
        Method::PUT,
        "/api/aeat/certificate",
        Some(upload_body(&pkcs12(false, "secret"), "secret")),
    )
    .await;
    assert_eq!(uploaded.status, StatusCode::OK);
    // Still practice: the certificate alone changes nothing.
    assert!(!state.aeat.current().unwrap().live);

    let status = post(&app, "/api/aeat/live", json!({})).await;
    assert_eq!(status.status, StatusCode::OK);
    assert_eq!(status.json()["mode"], "live");
    assert_eq!(status.json()["pending"], 0);
    assert!(state.aeat.current().unwrap().live);
    let again = post(&app, "/api/aeat/live", json!({})).await;
    assert_eq!(again.json()["error"]["code"], "already_live");

    // Practice invoices are gone from view.
    assert_eq!(get(&app, "/api/invoices").await.json(), json!([]));
    let path = format!("/api/invoices/{}", practice["id"]);
    assert_eq!(get(&app, &path).await.status, StatusCode::NOT_FOUND);
    let cancel = post(&app, &format!("{path}/cancellation"), json!({})).await;
    assert_eq!(cancel.status, StatusCode::NOT_FOUND);

    // Numbering and chain start again.
    let live = post(&app, "/api/invoices", invoice_body()).await.json();
    assert_eq!(live["live"], true);
    assert_eq!(live["series_number"], "A2026-0001");
    assert_eq!(live["records"][0]["previous_hash"], "");
    let chain = get(&app, "/api/chain/verification").await.json();
    assert_eq!(
        (chain["valid"].clone(), chain["records"].clone()),
        (json!(true), json!(1))
    );

    // Only the live record goes to the live service.
    assert_eq!(send(&state).await, "accepted");
    assert_eq!(live_mock.registered_count(), 1);
    assert_eq!(get(&app, "/api/aeat").await.json()["pending"], 0);

    // Without a certificate, live records wait.
    request(&app, Method::DELETE, "/api/aeat/certificate", None).await;
    assert!(state.aeat.current().is_none());
    post(&app, "/api/invoices", invoice_body()).await;
    let status = get(&app, "/api/aeat").await.json();
    assert_eq!(
        (status["enabled"].clone(), status["pending"].clone()),
        (json!(false), json!(1))
    );
}

#[sqlx::test]
async fn live_mode_needs_the_software_producer(pool: PgPool) {
    let settings = AeatSettings {
        producer: None,
        ..settings(None)
    };
    let (_, app) = app(&pool, settings).await;
    request(
        &app,
        Method::PUT,
        "/api/aeat/certificate",
        Some(upload_body(&pkcs12(false, "secret"), "secret")),
    )
    .await;
    let response = post(&app, "/api/aeat/live", json!({})).await;
    assert_eq!(response.json()["error"]["code"], "producer_missing");
    assert_eq!(get(&app, "/api/aeat").await.json()["mode"], "practice");
}

#[test]
fn the_secret_key_is_64_hexadecimal_digits() {
    assert!(SecretKey::parse(KEY).is_ok());
    assert!(SecretKey::parse(&KEY[1..]).is_err());
    assert!(SecretKey::parse(&KEY.replace('0', "g")).is_err());
    assert_eq!(
        format!("{:?}", SecretKey::parse(KEY).unwrap()),
        "SecretKey(..)"
    );
}
