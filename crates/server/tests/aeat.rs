//! End-to-end tests of the AEAT integration: records stored through the
//! API are sent with the SOAP client to the mock, which validates them
//! against the official schemas (when `xmllint` is installed) and
//! recomputes hashes and chain.

mod common;

use std::{env, path::PathBuf, process::Command};

use axum::http::StatusCode;
use common::{get, post};
use serde_json::{json, Value};
use sqlx::PgPool;
use vericounts_aeat_mock::{codes, Fault, Mock, SERVICE_PATH};
use vericounts_core::aeat::{
    self, Operation, Party, Record, RecordStatus, ResponseError, SoftwareSystem, SubmissionStatus,
};
use vericounts_server::{
    aeat_client::{AeatClient, SubmitError},
    store,
};

fn invoice_body() -> Value {
    json!({
        "series": "A2026",
        "recipient": { "name": "Customer & Sons SA", "nif": "A58818501" },
        "description": "Consulting <services>",
        "lines": [
            { "description": "Consulting", "quantity": "10", "unit_price": "50.00", "vat_rate": "21" },
            { "description": "Books", "quantity": "2", "unit_price": "12.495", "vat_rate": "4" }
        ]
    })
}

fn system() -> SoftwareSystem {
    SoftwareSystem {
        producer: Party {
            name: "Example Software SL".to_owned(),
            nif: "B12345674".to_owned(),
        },
        name: "vericounts".to_owned(),
        id: "VC".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        installation_number: "1".to_owned(),
    }
}

fn issuer() -> Party {
    let issuer = common::issuer();
    Party {
        name: issuer.name,
        nif: issuer.nif,
    }
}

/// Starts the mock on a free port. Schema validation is on when `xmllint`
/// is installed, which is required in CI.
async fn start_mock() -> (Mock, AeatClient) {
    let xmllint = Command::new("xmllint").arg("--version").output().is_ok();
    assert!(
        xmllint || env::var_os("CI").is_none(),
        "xmllint is required in CI"
    );
    let mut mock = Mock::new();
    if xmllint {
        mock = mock.with_schema_validation(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../aeat/schemas"),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}{SERVICE_PATH}", listener.local_addr().unwrap());
    let router = mock.router();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (mock, AeatClient::new(endpoint, None).unwrap())
}

async fn records(pool: &PgPool) -> Vec<Record> {
    store::chain_records(pool, common::ISSUER_NIF)
        .await
        .unwrap()
        .into_iter()
        .map(|(_, record)| record)
        .collect()
}

#[sqlx::test]
async fn stored_records_are_accepted_by_the_mock(pool: PgPool) {
    let app = common::app(pool.clone());
    let first = post(&app, "/api/invoices", invoice_body()).await;
    assert_eq!(first.status, StatusCode::CREATED);
    post(&app, "/api/invoices", invoice_body()).await;
    let path = format!("/api/invoices/{}/cancellation", first.json()["id"]);
    assert_eq!(post(&app, &path, json!({})).await.status, StatusCode::OK);
    assert_eq!(
        get(&app, "/api/chain/verification").await.json()["records"],
        3
    );

    let (mock, client) = start_mock().await;
    let records = records(&pool).await;
    assert_eq!(records.len(), 3);

    // Two requests, to check that the chain continues across requests.
    let request = aeat::submission_request(&issuer(), &system(), &records[..2]);
    let submission = client.submit(&request).await.unwrap();
    assert_eq!(submission.response.status, SubmissionStatus::Accepted);
    assert_eq!(submission.response.wait_seconds, 60);
    assert!(submission.response.csv.is_some());
    assert_eq!(submission.response.records.len(), 2);
    assert_eq!(submission.response.records[1].series_number, "A2026-0002");
    assert!(submission.request_xml.contains("Customer &amp; Sons SA"));

    let request = aeat::submission_request(&issuer(), &system(), &records[2..]);
    let submission = client.submit(&request).await.unwrap();
    assert_eq!(submission.response.status, SubmissionStatus::Accepted);
    assert_eq!(
        submission.response.records[0].operation,
        Operation::Cancellation
    );
    assert_eq!(mock.registered_count(), 2);
    assert_eq!(mock.requests().len(), 2);

    // Sending a record again is refused, and so is the whole request.
    let request = aeat::submission_request(&issuer(), &system(), &records[2..]);
    let submission = client.submit(&request).await.unwrap();
    assert_eq!(submission.response.status, SubmissionStatus::Rejected);
    assert_eq!(submission.response.csv, None);
    assert_eq!(
        submission.response.records[0].status,
        RecordStatus::Rejected
    );
    assert!(submission.response.records[0].error_code.is_some());
}

#[sqlx::test]
async fn the_mock_rejects_wrong_hashes_and_broken_chains(pool: PgPool) {
    let app = common::app(pool.clone());
    for _ in 0..3 {
        post(&app, "/api/invoices", invoice_body()).await;
    }
    let (_mock, client) = start_mock().await;
    let records = records(&pool).await;
    let code = |code: u32| Some(code.to_string());

    // Skipping the first record breaks the chain.
    let request = aeat::submission_request(&issuer(), &system(), &records[1..2]);
    let response = client.submit(&request).await.unwrap().response;
    assert_eq!(response.status, SubmissionStatus::Rejected);
    assert_eq!(response.records[0].error_code, code(codes::BROKEN_CHAIN));

    // A record whose content does not match its hash is refused, and the
    // records after it no longer follow an accepted record.
    let mut tampered = records.clone();
    if let Record::Registration(record) = &mut tampered[1] {
        record.total_amount += rust_decimal::Decimal::ONE;
    }
    let request = aeat::submission_request(&issuer(), &system(), &tampered);
    let response = client.submit(&request).await.unwrap().response;
    assert_eq!(response.status, SubmissionStatus::PartiallyAccepted);
    assert_eq!(response.records[0].status, RecordStatus::Accepted);
    assert_eq!(response.records[1].error_code, code(codes::WRONG_HASH));
    assert_eq!(response.records[2].error_code, code(codes::BROKEN_CHAIN));

    // The untouched records are then accepted.
    let request = aeat::submission_request(&issuer(), &system(), &records[1..]);
    let response = client.submit(&request).await.unwrap().response;
    assert_eq!(response.status, SubmissionStatus::Accepted);
}

#[sqlx::test]
async fn failures_are_reported_by_kind(pool: PgPool) {
    let app = common::app(pool.clone());
    post(&app, "/api/invoices", invoice_body()).await;
    let (mock, client) = start_mock().await;
    let request = aeat::submission_request(&issuer(), &system(), &records(&pool).await);

    mock.inject(Fault::HttpStatus(503));
    mock.inject(Fault::SoapFault);
    mock.inject(Fault::Garbage);
    assert!(matches!(
        client.submit(&request).await,
        Err(SubmitError::Http { status: 503, .. })
    ));
    assert!(matches!(
        client.submit(&request).await,
        Err(SubmitError::Response {
            error: ResponseError::Fault { .. },
            ..
        })
    ));
    assert!(matches!(
        client.submit(&request).await,
        Err(SubmitError::Response {
            error: ResponseError::Malformed(_),
            ..
        })
    ));
    // Nothing was registered by the failed attempts, so a retry works.
    assert_eq!(mock.registered_count(), 0);
    let response = client.submit(&request).await.unwrap().response;
    assert_eq!(response.status, SubmissionStatus::Accepted);

    // A request that breaks the schema gets a SOAP fault.
    if mock.requests().len() == 4 && Command::new("xmllint").arg("--version").output().is_ok() {
        let invalid = request.replace("<sf:TipoFactura>F1<", "<sf:TipoFactura>ZZ<");
        assert!(matches!(
            client.submit(&invalid).await,
            Err(SubmitError::Response {
                error: ResponseError::Fault { .. },
                ..
            })
        ));
    }

    // An unreachable service is a transport error.
    let unreachable = AeatClient::new("http://127.0.0.1:1/".to_owned(), None).unwrap();
    assert!(matches!(
        unreachable.submit(&request).await,
        Err(SubmitError::Transport(_))
    ));
}
