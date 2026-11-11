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
use vericount_aeat_mock::{codes, Fault, Mock, SERVICE_PATH};
use vericount_core::aeat::{
    self, Operation, Party, Record, RecordStatus, ResponseError, SoftwareSystem, SubmissionStatus,
};
use vericount_server::{
    aeat_client::{AeatClient, SubmitError},
    store,
};

fn invoice_body() -> Value {
    json!({
        "series": "A2026",
        "contact_id": common::CUSTOMER_ID,
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
        name: "vericount".to_owned(),
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
    let app = common::app(pool.clone()).await;
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
    let request = aeat::submission_request(&issuer(), None, &system(), &records[..2]);
    let submission = client.submit(&request).await.unwrap();
    assert_eq!(submission.response.status, SubmissionStatus::Accepted);
    assert_eq!(submission.response.wait_seconds, 60);
    assert!(submission.response.csv.is_some());
    assert_eq!(submission.response.records.len(), 2);
    assert_eq!(submission.response.records[1].series_number, "A2026-0002");
    assert!(submission.request_xml.contains("Customer &amp; Sons SA"));

    let request = aeat::submission_request(&issuer(), None, &system(), &records[2..]);
    let submission = client.submit(&request).await.unwrap();
    assert_eq!(submission.response.status, SubmissionStatus::Accepted);
    assert_eq!(
        submission.response.records[0].operation,
        Operation::Cancellation
    );
    assert_eq!(mock.registered_count(), 2);
    assert_eq!(mock.requests().len(), 2);

    // Sending a record again is refused, and so is the whole request.
    let request = aeat::submission_request(&issuer(), None, &system(), &records[2..]);
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
    let app = common::app(pool.clone()).await;
    for _ in 0..3 {
        post(&app, "/api/invoices", invoice_body()).await;
    }
    let (_mock, client) = start_mock().await;
    let records = records(&pool).await;
    let code = |code: u32| Some(code.to_string());

    // Skipping the first record breaks the chain.
    let request = aeat::submission_request(&issuer(), None, &system(), &records[1..2]);
    let response = client.submit(&request).await.unwrap().response;
    assert_eq!(response.status, SubmissionStatus::Rejected);
    assert_eq!(response.records[0].error_code, code(codes::BROKEN_CHAIN));

    // A record whose content does not match its hash is refused, and the
    // records after it no longer follow an accepted record.
    let (_fresh_mock, client) = start_mock().await;
    let mut tampered = records.clone();
    if let Record::Registration(record) = &mut tampered[1] {
        record.total_amount += rust_decimal::Decimal::ONE;
    }
    let request = aeat::submission_request(&issuer(), None, &system(), &tampered);
    let response = client.submit(&request).await.unwrap().response;
    assert_eq!(response.status, SubmissionStatus::PartiallyAccepted);
    assert_eq!(response.records[0].status, RecordStatus::Accepted);
    assert_eq!(response.records[1].error_code, code(codes::WRONG_HASH));
    assert_eq!(response.records[2].error_code, code(codes::BROKEN_CHAIN));
}

#[sqlx::test]
async fn failures_are_reported_by_kind(pool: PgPool) {
    let app = common::app(pool.clone()).await;
    post(&app, "/api/invoices", invoice_body()).await;
    let (mock, client) = start_mock().await;
    let request = aeat::submission_request(&issuer(), None, &system(), &records(&pool).await);

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

mod worker {
    use std::sync::Arc;

    use super::*;
    use vericount_server::submission::{self, RunOutcome, Submitter};

    async fn submitter() -> (Mock, Submitter) {
        let (mock, client) = start_mock().await;
        (mock, Submitter::new(client, system()))
    }

    async fn run(pool: &PgPool, submitter: &Submitter) -> RunOutcome {
        submission::run_once(pool, &common::issuer(), submitter)
            .await
            .unwrap()
    }

    fn sent(outcome: &RunOutcome) -> (usize, &str) {
        match outcome {
            RunOutcome::Sent {
                records, outcome, ..
            } => (*records, outcome),
            other => panic!("expected a submission, got {other:?}"),
        }
    }

    #[sqlx::test]
    async fn pending_records_are_sent_and_their_state_is_shown(pool: PgPool) {
        let app = common::app(pool.clone()).await;
        let (mock, submitter) = submitter().await;
        assert_eq!(run(&pool, &submitter).await, RunOutcome::NothingPending);

        let first = post(&app, "/api/invoices", invoice_body()).await.json();
        post(&app, "/api/invoices", invoice_body()).await;
        assert_eq!(first["records"][0]["aeat"]["state"], "pending");
        let status = get(&app, "/api/aeat").await.json();
        assert_eq!(
            (&status["enabled"], &status["pending"]),
            (&json!(false), &json!(2))
        );

        assert_eq!(sent(&run(&pool, &submitter).await), (2, "accepted"));
        assert_eq!(run(&pool, &submitter).await, RunOutcome::NothingPending);
        assert_eq!(mock.requests().len(), 1);

        let list = get(&app, "/api/invoices").await.json();
        assert_eq!(list[0]["aeat_state"], "accepted");
        let detail = get(&app, &format!("/api/invoices/{}", first["id"]))
            .await
            .json();
        assert_eq!(detail["records"][0]["aeat"]["state"], "accepted");

        // A later cancellation is a new pending record.
        let path = format!("/api/invoices/{}/cancellation", first["id"]);
        let cancelled = post(&app, &path, json!({})).await.json();
        assert_eq!(cancelled["records"][0]["aeat"]["state"], "accepted");
        assert_eq!(cancelled["records"][1]["aeat"]["state"], "pending");
        let list = get(&app, "/api/invoices").await.json();
        assert_eq!(list[1]["aeat_state"], "pending");
        assert_eq!(sent(&run(&pool, &submitter).await), (1, "accepted"));
        let status = get(&app, "/api/aeat").await.json();
        assert_eq!(status["pending"], 0);
        assert_eq!(status["last_outcome"], "accepted");

        // Request and response are stored as exchanged.
        let (request, response): (String, String) = sqlx::query_as(
            "SELECT s.request_xml, r.response_xml FROM aeat_submissions s
             JOIN aeat_responses r ON r.submission_id = s.id ORDER BY s.id LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(request, mock.requests()[0]);
        assert!(response.contains("RespuestaRegFactuSistemaFacturacion"));
    }

    #[sqlx::test]
    async fn failed_attempts_are_recorded_and_retried(pool: PgPool) {
        let app = common::app(pool.clone()).await;
        let (mock, submitter) = submitter().await;
        post(&app, "/api/invoices", invoice_body()).await;

        mock.inject(Fault::HttpStatus(503));
        mock.inject(Fault::SoapFault);
        assert_eq!(sent(&run(&pool, &submitter).await), (1, "http_error"));
        let status = get(&app, "/api/aeat").await.json();
        assert_eq!(status["pending"], 1);
        assert_eq!(status["last_error"], "HTTP 503");
        assert_eq!(sent(&run(&pool, &submitter).await), (1, "fault"));
        assert_eq!(sent(&run(&pool, &submitter).await), (1, "accepted"));

        let outcomes: Vec<String> =
            sqlx::query_scalar("SELECT outcome FROM aeat_responses ORDER BY submission_id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(outcomes, ["http_error", "fault", "accepted"]);
    }

    #[sqlx::test]
    async fn a_rejected_record_blocks_the_queue_until_it_is_corrected(pool: PgPool) {
        let app = common::app(pool.clone()).await;
        let (mock, submitter) = submitter().await;
        let first = post(&app, "/api/invoices", invoice_body()).await.json();
        let second = post(&app, "/api/invoices", invoice_body()).await.json();
        let correction = |invoice: &Value| format!("/api/invoices/{}/correction", invoice["id"]);

        // Nothing to correct while nothing is rejected.
        let response = post(&app, &correction(&first), json!({})).await;
        assert_eq!(response.status, StatusCode::CONFLICT);
        assert_eq!(response.json()["error"]["code"], "nothing_to_correct");

        mock.inject(Fault::RejectRecords);
        assert_eq!(sent(&run(&pool, &submitter).await), (2, "rejected"));
        let record_id = first["records"][0]["id"].as_i64().unwrap();
        assert_eq!(
            run(&pool, &submitter).await,
            RunOutcome::Blocked { record_id }
        );
        assert_eq!(mock.requests().len(), 1, "nothing is sent while blocked");
        let status = get(&app, "/api/aeat").await.json();
        assert_eq!(
            (&status["rejected"], &status["pending"]),
            (&json!(2), &json!(0))
        );
        let list = get(&app, "/api/invoices").await.json();
        assert_eq!(list[0]["aeat_state"], "rejected");

        // Correcting the first invoice adds a record at the end of the
        // chain; the second invoice still blocks what comes after it.
        let corrected = post(&app, &correction(&first), json!({})).await;
        assert_eq!(corrected.status, StatusCode::OK, "{:?}", corrected.json());
        let corrected = corrected.json();
        let records = corrected["records"].as_array().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["aeat"]["state"], "superseded");
        assert_eq!(
            records[0]["aeat"]["error_code"],
            codes::INJECTED.to_string()
        );
        assert_eq!(records[1]["aeat"]["state"], "pending");
        assert_eq!(records[1]["correction"], true);
        assert_eq!(records[1]["previous_hash"], second["records"][0]["hash"]);
        let second_record = second["records"][0]["id"].as_i64().unwrap();
        assert_eq!(
            run(&pool, &submitter).await,
            RunOutcome::Blocked {
                record_id: second_record
            }
        );

        post(&app, &correction(&second), json!({})).await;
        assert_eq!(sent(&run(&pool, &submitter).await), (2, "accepted"));
        assert_eq!(run(&pool, &submitter).await, RunOutcome::NothingPending);
        assert_eq!(mock.registered_count(), 2);

        // The corrections say that the AEAT did not hold the records.
        let request = mock.requests().pop().unwrap();
        assert_eq!(
            request
                .matches("<sf:Subsanacion>S</sf:Subsanacion><sf:RechazoPrevio>X</sf:RechazoPrevio>")
                .count(),
            2
        );
        let list = get(&app, "/api/invoices").await.json();
        assert_eq!(list[0]["aeat_state"], "accepted");
        assert_eq!(list[1]["aeat_state"], "accepted");
        let status = get(&app, "/api/aeat").await.json();
        assert_eq!(
            (&status["rejected"], &status["pending"]),
            (&json!(0), &json!(0))
        );
        assert_eq!(
            get(&app, "/api/chain/verification").await.json()["records"],
            4
        );
        assert_eq!(
            get(&app, "/api/chain/verification").await.json()["valid"],
            true
        );
        let response = post(&app, &correction(&first), json!({})).await;
        assert_eq!(response.status, StatusCode::CONFLICT);
    }

    #[sqlx::test]
    async fn rejected_cancellations_and_corrections_are_corrected_too(pool: PgPool) {
        let app = common::app(pool.clone()).await;
        let (mock, submitter) = submitter().await;
        let invoice = post(&app, "/api/invoices", invoice_body()).await.json();
        let id = &invoice["id"];
        assert_eq!(sent(&run(&pool, &submitter).await), (1, "accepted"));

        post(&app, &format!("/api/invoices/{id}/cancellation"), json!({})).await;
        mock.inject(Fault::RejectRecords);
        assert_eq!(sent(&run(&pool, &submitter).await), (1, "rejected"));
        let corrected = post(&app, &format!("/api/invoices/{id}/correction"), json!({}))
            .await
            .json();
        assert_eq!(corrected["cancelled"], true);
        let records = corrected["records"].as_array().unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[2]["record_type"], "cancellation");
        assert_eq!(sent(&run(&pool, &submitter).await), (1, "accepted"));
        let request = mock.requests().pop().unwrap();
        assert!(request.contains("</sf:IDFactura><sf:RechazoPrevio>S</sf:RechazoPrevio>"));
        let list = get(&app, "/api/invoices").await.json();
        assert_eq!(list[0]["aeat_state"], "accepted");
    }

    #[sqlx::test]
    async fn submissions_can_be_requested_from_the_api_when_enabled(pool: PgPool) {
        common::sign_in(&pool).await;
        common::seed(&pool).await;
        let disabled = common::app_signed_out(pool.clone());
        let response = post(&disabled, "/api/aeat/submissions", json!({})).await;
        assert_eq!(response.status, StatusCode::CONFLICT);

        let (_mock, submitter) = submitter().await;
        let mut state = common::state(pool.clone());
        state.submitter = Some(Arc::new(submitter));
        let app = vericount_server::app(state, common::web_dir());
        post(&app, "/api/invoices", invoice_body()).await;
        let response = post(&app, "/api/aeat/submissions", json!({})).await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.json()["result"], "sent");
        assert_eq!(response.json()["outcome"], "accepted");
        assert_eq!(get(&app, "/api/aeat").await.json()["enabled"], true);
    }
}

#[sqlx::test]
async fn a_representative_and_the_certificate_are_declared(pool: PgPool) {
    use std::sync::Arc;
    use vericount_server::{certificate::CertificateInfo, submission::Submitter};

    common::sign_in(&pool).await;
    common::seed(&pool).await;
    let (mock, client) = start_mock().await;
    let mut submitter = Submitter::new(client, system());
    submitter.representative = Some(Party {
        name: "Jane Roe".to_owned(),
        nif: "12345678Z".to_owned(),
    });
    submitter.certificate = Some(CertificateInfo {
        subject: "ROE JANE - 12345678Z".to_owned(),
        nif: Some("12345678Z".to_owned()),
        of_legal_person: false,
        expires_at: chrono::Utc::now(),
    });
    let mut state = common::state(pool.clone());
    state.submitter = Some(Arc::new(submitter));
    let app = vericount_server::app(state, common::web_dir());

    post(&app, "/api/invoices", invoice_body()).await;
    let response = post(&app, "/api/aeat/submissions", json!({})).await.json();
    assert_eq!(response["outcome"], "accepted");
    assert!(mock.requests()[0].contains(
        "<sf:Representante><sf:NombreRazon>Jane Roe</sf:NombreRazon><sf:NIF>12345678Z</sf:NIF>"
    ));

    let status = get(&app, "/api/aeat").await.json();
    assert_eq!(status["certificate"]["nif"], "12345678Z");
    assert_eq!(status["certificate_matches_company"], false);
    assert_eq!(status["representative_nif"], "12345678Z");
}
