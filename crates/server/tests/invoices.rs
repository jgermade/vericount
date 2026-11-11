//! Tests for invoices and billing records against a real Postgres.
//!
//! `#[sqlx::test]` creates a fresh database per test from `DATABASE_URL`
//! and applies the migrations.

mod common;

use axum::http::StatusCode;
use chrono::{Days, Utc};
use common::{get, post, ISSUER_NIF};
use serde_json::{json, Value};
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions},
    PgPool,
};
use vericounts_core::hash::{self, CancellationFields, RegistrationFields};

fn invoice_body() -> Value {
    json!({
        "series": "A2026",
        "recipient": { "name": "Customer SA", "nif": "a58818501" },
        "description": "Consulting services",
        "lines": [
            { "description": "Consulting", "quantity": "10", "unit_price": "50.00", "vat_rate": "21" },
            { "description": "Books", "quantity": "2", "unit_price": "12.495", "vat_rate": "4" }
        ]
    })
}

fn today() -> chrono::NaiveDate {
    Utc::now()
        .with_timezone(&chrono_tz::Europe::Madrid)
        .date_naive()
}

#[sqlx::test]
async fn creates_invoices_with_chained_records(pool: PgPool) {
    let app = common::app(pool);

    let first = post(&app, "/api/invoices", invoice_body()).await;
    assert_eq!(first.status, StatusCode::CREATED, "{:?}", first.json());
    let first = first.json();
    assert_eq!(first["series_number"], "A2026-0001");
    assert_eq!(first["issuer_nif"], ISSUER_NIF);
    assert_eq!(first["recipient_nif"], "A58818501");
    assert_eq!(first["issue_date"], today().to_string());
    assert_eq!(first["total_base"], "524.99");
    assert_eq!(first["total_tax"], "106.00");
    assert_eq!(first["total_amount"], "630.99");
    assert_eq!(first["cancelled"], false);
    assert_eq!(first["lines"].as_array().unwrap().len(), 2);
    assert_eq!(first["lines"][1]["base_amount"], "24.99");
    assert_eq!(first["taxes"][0]["vat_rate"], "21");
    assert_eq!(first["taxes"][0]["tax_amount"], "105.00");
    assert_eq!(first["taxes"][1]["tax_amount"], "1.00");

    let record = &first["records"][0];
    assert_eq!(record["record_type"], "registration");
    assert_eq!(record["previous_hash"], "");
    let expected = hash::registration_hash(&RegistrationFields {
        issuer_nif: ISSUER_NIF,
        series_number: "A2026-0001",
        issue_date: today(),
        invoice_type: "F1",
        total_tax: "106.00".parse().unwrap(),
        total_amount: "630.99".parse().unwrap(),
        previous_hash: "",
        generated_at: record["generated_at"].as_str().unwrap(),
    });
    assert_eq!(record["hash"], expected);
    assert_eq!(
        first["qr_url"],
        format!(
            "https://prewww2.aeat.es/wlpl/TIKE-CONT/ValidarQR?nif={ISSUER_NIF}\
             &numserie=A2026-0001&fecha={}&importe=630.99",
            today().format("%d-%m-%Y")
        )
    );

    let second = post(&app, "/api/invoices", invoice_body()).await.json();
    assert_eq!(second["series_number"], "A2026-0002");
    assert_eq!(second["records"][0]["previous_hash"], record["hash"]);

    let mut other_series = invoice_body();
    other_series["series"] = json!("R2026");
    let third = post(&app, "/api/invoices", other_series).await.json();
    assert_eq!(third["series_number"], "R2026-0001");
    assert_eq!(
        third["records"][0]["previous_hash"],
        second["records"][0]["hash"]
    );

    let list = get(&app, "/api/invoices").await.json();
    let numbers: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|invoice| invoice["series_number"].as_str().unwrap())
        .collect();
    assert_eq!(numbers, ["R2026-0001", "A2026-0002", "A2026-0001"]);

    let fetched = get(&app, &format!("/api/invoices/{}", first["id"])).await;
    assert_eq!(fetched.status, StatusCode::OK);
    assert_eq!(fetched.json(), first);

    let report = get(&app, "/api/chain/verification").await.json();
    assert_eq!(report["valid"], true);
    assert_eq!(report["records"], 3);
    assert_eq!(report["last_hash"], third["records"][0]["hash"]);
}

#[sqlx::test]
async fn preview_computes_totals_without_storing(pool: PgPool) {
    let app = common::app(pool.clone());
    let preview = post(&app, "/api/invoices/preview", invoice_body()).await;
    assert_eq!(preview.status, StatusCode::OK);
    let preview = preview.json();
    assert_eq!(preview["total_amount"], "630.99");
    assert_eq!(preview["taxes"][0]["base_amount"], "500.00");

    let mut invalid = invoice_body();
    invalid["recipient"]["nif"] = json!("");
    let response = post(&app, "/api/invoices/preview", invalid).await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.json()["error"]["field"], "recipient.nif");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn cancellation_adds_a_record(pool: PgPool) {
    let app = common::app(pool);
    let invoice = post(&app, "/api/invoices", invoice_body()).await.json();
    let path = format!("/api/invoices/{}/cancellation", invoice["id"]);

    let cancelled = post(&app, &path, json!({})).await;
    assert_eq!(cancelled.status, StatusCode::OK);
    let cancelled = cancelled.json();
    assert_eq!(cancelled["cancelled"], true);
    let record = &cancelled["records"][1];
    assert_eq!(record["record_type"], "cancellation");
    assert_eq!(record["previous_hash"], invoice["records"][0]["hash"]);
    let expected = hash::cancellation_hash(&CancellationFields {
        issuer_nif: ISSUER_NIF,
        series_number: "A2026-0001",
        issue_date: today(),
        previous_hash: invoice["records"][0]["hash"].as_str().unwrap(),
        generated_at: record["generated_at"].as_str().unwrap(),
    });
    assert_eq!(record["hash"], expected);

    let again = post(&app, &path, json!({})).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.json()["error"]["code"], "already_cancelled");

    let missing = post(&app, "/api/invoices/999/cancellation", json!({})).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    // The number of a cancelled invoice is not reused.
    let next = post(&app, "/api/invoices", invoice_body()).await.json();
    assert_eq!(next["series_number"], "A2026-0002");
    assert_eq!(next["records"][0]["previous_hash"], record["hash"]);

    let report = get(&app, "/api/chain/verification").await.json();
    assert_eq!(
        (&report["valid"], &report["records"]),
        (&json!(true), &json!(3))
    );
}

#[sqlx::test]
async fn invalid_invoices_are_rejected(pool: PgPool) {
    let app = common::app(pool.clone());
    let tomorrow = today().checked_add_days(Days::new(1)).unwrap().to_string();
    type Change = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, Change)> = vec![
        ("series", Box::new(|b| b["series"] = json!("a-1"))),
        (
            "recipient.name",
            Box::new(|b| b["recipient"]["name"] = json!(" ")),
        ),
        (
            "recipient.nif",
            Box::new(|b| b["recipient"]["nif"] = json!("A 1")),
        ),
        ("description", Box::new(|b| b["description"] = json!(""))),
        ("lines", Box::new(|b| b["lines"] = json!([]))),
        (
            "lines[0].vat_rate",
            Box::new(|b| b["lines"][0]["vat_rate"] = json!("7")),
        ),
        (
            "lines[1].quantity",
            Box::new(|b| b["lines"][1]["quantity"] = json!("0")),
        ),
        (
            "issue_date",
            Box::new(move |b| b["issue_date"] = json!(tomorrow)),
        ),
    ];
    for (field, change) in cases {
        let mut body = invoice_body();
        change(&mut body);
        let response = post(&app, "/api/invoices", body).await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        assert_eq!(response.json()["error"]["field"], field);
    }

    // Amounts must be strings, never JSON floats, and unknown fields fail.
    let mut float = invoice_body();
    float["lines"][0]["unit_price"] = json!(50.1);
    let mut unknown = invoice_body();
    unknown["total_amount"] = json!("1.00");
    for body in [float, unknown] {
        let response = post(&app, "/api/invoices", body).await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(response.json()["error"]["code"], "invalid_request");
    }

    // An invoice cannot be dated before the last one of its series.
    assert_eq!(
        post(&app, "/api/invoices", invoice_body()).await.status,
        StatusCode::CREATED
    );
    let mut earlier = invoice_body();
    earlier["issue_date"] = json!(today().checked_sub_days(Days::new(1)).unwrap().to_string());
    let response = post(&app, "/api/invoices", earlier).await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response.json()["error"]["field"], "issue_date");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM invoices")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "rejected invoices must leave nothing behind");
    assert_eq!(
        get(&app, "/api/invoices/abc").await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&app, "/api/invoices/42").await.status,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn concurrent_invoices_keep_one_chain(pool: PgPool) {
    let app = common::app(pool.clone());
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            post(&app, "/api/invoices", invoice_body()).await.status
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::CREATED);
    }

    let numbers: Vec<i32> = sqlx::query_scalar("SELECT number FROM invoices ORDER BY number")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(numbers, (1..=12).collect::<Vec<_>>());
    let report = get(&app, "/api/chain/verification").await.json();
    assert_eq!(
        (&report["valid"], &report["records"]),
        (&json!(true), &json!(12))
    );
}

/// Insert-only tables, each with an `UPDATE` that would otherwise succeed.
const TABLES: [(&str, &str); 4] = [
    ("invoices", "UPDATE invoices SET description = 'x'"),
    (
        "invoice_lines",
        "UPDATE invoice_lines SET description = 'x'",
    ),
    ("invoice_taxes", "UPDATE invoice_taxes SET tax_amount = 0"),
    (
        "billing_records",
        "UPDATE billing_records SET generated_at = 'x'",
    ),
];

#[sqlx::test]
async fn tables_are_insert_only_even_for_the_owner(pool: PgPool) {
    let app = common::app(pool.clone());
    post(&app, "/api/invoices", invoice_body()).await;

    for (table, update) in TABLES {
        for statement in [
            update.to_owned(),
            format!("DELETE FROM {table}"),
            format!("TRUNCATE {table} CASCADE"),
        ] {
            let error = sqlx::query(&statement)
                .execute(&pool)
                .await
                .expect_err(&statement)
                .to_string();
            assert!(error.contains("insert-only"), "{statement}: {error}");
        }
    }
    let report = get(&app, "/api/chain/verification").await.json();
    assert_eq!(
        (&report["valid"], &report["records"]),
        (&json!(true), &json!(1))
    );
}

/// The application role can do everything the API needs, and nothing else.
#[sqlx::test]
async fn application_role_can_only_read_and_insert(
    pool_options: PgPoolOptions,
    connect_options: PgConnectOptions,
) {
    let pool = pool_options
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE vericounts_app").execute(conn).await?;
                Ok(())
            })
        })
        .connect_with(connect_options)
        .await
        .unwrap();
    let app = common::app(pool.clone());

    let invoice = post(&app, "/api/invoices", invoice_body()).await;
    assert_eq!(invoice.status, StatusCode::CREATED, "{:?}", invoice.json());
    let path = format!("/api/invoices/{}/cancellation", invoice.json()["id"]);
    assert_eq!(post(&app, &path, json!({})).await.status, StatusCode::OK);
    assert_eq!(get(&app, "/api/invoices").await.status, StatusCode::OK);
    assert_eq!(
        get(&app, "/api/chain/verification").await.json()["valid"],
        true
    );

    for (table, update) in TABLES {
        for statement in [
            update.to_owned(),
            format!("DELETE FROM {table}"),
            format!("TRUNCATE {table} CASCADE"),
            format!("ALTER TABLE {table} DISABLE TRIGGER ALL"),
            format!("DROP TABLE {table} CASCADE"),
        ] {
            let error = sqlx::query(&statement)
                .execute(&pool)
                .await
                .expect_err(&statement)
                .to_string();
            assert!(
                error.contains("permission denied") || error.contains("must be owner"),
                "{statement}: {error}"
            );
        }
    }
}

/// If someone with owner rights bypasses the triggers, verification
/// reports the first broken record.
#[sqlx::test]
async fn verification_detects_tampering(pool: PgPool) {
    let app = common::app(pool.clone());
    for _ in 0..3 {
        post(&app, "/api/invoices", invoice_body()).await;
    }
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM billing_records ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();

    sqlx::query("ALTER TABLE billing_records DISABLE TRIGGER USER")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE billing_records SET total_amount = total_amount + 1 WHERE id = $1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();

    let report = get(&app, "/api/chain/verification").await.json();
    assert_eq!(report["valid"], false);
    assert_eq!(report["error"]["record_id"], ids[1]);
    assert_eq!(
        report["error"]["reason"],
        "hash does not match the record contents"
    );

    // Removing a record breaks the link of the next one.
    sqlx::query("UPDATE billing_records SET total_amount = total_amount - 1 WHERE id = $1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&app, "/api/chain/verification").await.json()["valid"],
        true
    );
    sqlx::query("DELETE FROM billing_records WHERE id = $1")
        .bind(ids[1])
        .execute(&pool)
        .await
        .unwrap();
    let report = get(&app, "/api/chain/verification").await.json();
    assert_eq!(report["valid"], false);
    assert_eq!(report["error"]["record_id"], ids[2]);
}
