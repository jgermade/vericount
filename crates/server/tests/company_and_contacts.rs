//! Tests for company data, series and contacts.

mod common;

use axum::http::{Method, StatusCode};
use common::{get, post, request};
use serde_json::{json, Value};
use sqlx::PgPool;
use vericount_server::invoices::store;

async fn put(app: &axum::Router, path: &str, body: Value) -> common::TestResponse {
    request(app, Method::PUT, path, Some(body)).await
}

fn company() -> Value {
    json!({
        "nif": "b12345674", "name": "Example SL", "address": "Calle Real 5",
        "postal_code": "15001", "city": "A Coruña"
    })
}

fn invoice_body() -> Value {
    json!({
        "series": "A2026", "contact_id": common::CUSTOMER_ID, "description": "Consulting",
        "lines": [{ "description": "Hours", "quantity": "1", "unit_price": "100", "vat_rate": "21" }]
    })
}

#[sqlx::test]
async fn nothing_can_be_invoiced_before_the_company_exists(pool: PgPool) {
    common::sign_in(&pool).await;
    let app = common::app_signed_out(pool);

    let missing = get(&app, "/api/company").await;
    assert_eq!(missing.status, StatusCode::CONFLICT);
    assert_eq!(missing.json()["error"]["code"], "company_missing");
    for path in ["/api/invoices", "/api/chain/verification", "/api/aeat"] {
        let response = get(&app, path).await;
        assert_eq!(
            response.json()["error"]["code"],
            "company_missing",
            "{path}"
        );
    }

    for (field, value) in [
        ("nif", "B1"),
        ("name", " "),
        ("postal_code", "1500"),
        ("city", ""),
    ] {
        let mut body = company();
        body[field] = json!(value);
        let response = put(&app, "/api/company", body).await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        assert_eq!(response.json()["error"]["field"], field);
    }

    let saved = put(&app, "/api/company", company()).await;
    assert_eq!(saved.status, StatusCode::OK);
    assert_eq!(saved.json()["nif"], "B12345674");
    assert_eq!(get(&app, "/api/company").await.json(), saved.json());
    assert_eq!(get(&app, "/api/invoices").await.json(), json!([]));
}

#[sqlx::test]
async fn company_data_changes_but_issued_invoices_keep_theirs(pool: PgPool) {
    let app = common::app(pool.clone()).await;
    let invoice = post(&app, "/api/invoices", invoice_body()).await.json();

    let mut moved = company();
    moved["name"] = json!("Example Renamed SL");
    moved["address"] = json!("Avenida Nueva 9");
    assert_eq!(
        put(&app, "/api/company", moved).await.status,
        StatusCode::OK
    );
    let later = post(&app, "/api/invoices", invoice_body()).await.json();
    assert_eq!(later["issuer_name"], "Example Renamed SL");
    assert_eq!(later["issuer_address"], "Avenida Nueva 9, 15001 A Coruña");

    let earlier = get(&app, &format!("/api/invoices/{}", invoice["id"]))
        .await
        .json();
    assert_eq!(earlier["issuer_name"], "Example SL");
    assert_eq!(earlier["issuer_address"], "Calle Real 5, 15001 A Coruña");

    // The NIF identifies the chain. Practice invoices do not lock it, so
    // a mistake made while practising can be fixed.
    let mut other = company();
    other["nif"] = json!("A58818501");
    let response = put(&app, "/api/company", other.clone()).await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(
        put(&app, "/api/company", company()).await.status,
        StatusCode::OK
    );

    // In live mode it is locked.
    let user_id: i64 = sqlx::query_scalar("SELECT id FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    store::activate_live_mode(&pool, common::ISSUER_NIF, user_id)
        .await
        .unwrap();
    let response = put(&app, "/api/company", other).await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.json()["error"]["code"], "nif_locked");
    assert_eq!(
        get(&app, "/api/chain/verification").await.json()["valid"],
        true
    );
}

#[sqlx::test]
async fn series_are_created_described_and_switched_off(pool: PgPool) {
    let app = common::app(pool).await;

    let created = post(
        &app,
        "/api/series",
        json!({ "code": "B2026", "description": "Shop" }),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED);
    let duplicate = post(&app, "/api/series", json!({ "code": "B2026" })).await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);
    assert_eq!(duplicate.json()["error"]["code"], "series_exists");
    let invalid = post(&app, "/api/series", json!({ "code": "b-1" })).await;
    assert_eq!(invalid.json()["error"]["field"], "series");

    post(&app, "/api/invoices", invoice_body()).await;
    let list = get(&app, "/api/series").await.json();
    let summary: Vec<(&str, i64, bool)> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["code"].as_str().unwrap(),
                s["invoices"].as_i64().unwrap(),
                s["active"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [("A2026", 1, true), ("B2026", 0, true), ("R2026", 0, true)]
    );

    let off = put(
        &app,
        "/api/series/A2026",
        json!({ "description": "Old", "active": false }),
    )
    .await;
    assert_eq!(off.status, StatusCode::OK);
    assert_eq!(off.json()["invoices"], 1);
    let refused = post(&app, "/api/invoices", invoice_body()).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.json()["error"]["field"], "series");

    let missing = put(
        &app,
        "/api/series/NOPE",
        json!({ "description": "", "active": true }),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn contacts_are_created_edited_and_archived(pool: PgPool) {
    let app = common::app(pool).await;

    let created = post(
        &app,
        "/api/contacts",
        json!({ "name": " Taller Ruiz SL ", "nif": "b87654321", "email": "info@ruiz.example" }),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.json());
    let contact = created.json();
    assert_eq!(contact["name"], "Taller Ruiz SL");
    assert_eq!(contact["nif"], "B87654321");
    assert_eq!(
        (&contact["is_customer"], &contact["is_supplier"]),
        (&json!(true), &json!(false))
    );
    let path = format!("/api/contacts/{}", contact["id"]);

    let duplicate = post(
        &app,
        "/api/contacts",
        json!({ "name": "Other", "nif": "B87654321" }),
    )
    .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);
    assert_eq!(duplicate.json()["error"]["code"], "nif_exists");

    for (field, value) in [
        ("nif", json!("123")),
        ("name", json!("")),
        ("email", json!("not an email")),
        ("postal_code", json!("ABCDE")),
        ("is_customer", json!(false)),
    ] {
        let mut body = json!({ "name": "X SL", "nif": "B11111111" });
        body[field] = value;
        let response = post(&app, "/api/contacts", body).await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        assert_eq!(response.json()["error"]["field"], field);
    }

    // Invoices copy the customer's data when issued.
    let mut body = invoice_body();
    body["contact_id"] = contact["id"].clone();
    let invoice = post(&app, "/api/invoices", body.clone()).await.json();
    assert_eq!(invoice["recipient_name"], "Taller Ruiz SL");
    assert_eq!(invoice["recipient_address"], "");

    let edited = put(
        &app,
        &path,
        json!({ "name": "Taller Ruiz e Hijos SL", "nif": "B87654321", "address": "Rúa Nova 3",
                "postal_code": "15003", "city": "A Coruña", "is_supplier": true }),
    )
    .await;
    assert_eq!(edited.status, StatusCode::OK);
    assert_eq!(edited.json()["invoices"], 1);
    assert_eq!(edited.json()["is_supplier"], true);
    let unchanged = get(&app, &format!("/api/invoices/{}", invoice["id"]))
        .await
        .json();
    assert_eq!(unchanged["recipient_name"], "Taller Ruiz SL");
    let next = post(&app, "/api/invoices", body.clone()).await.json();
    assert_eq!(next["recipient_address"], "Rúa Nova 3, 15003 A Coruña");

    // Archived contacts and pure suppliers cannot be invoiced.
    let archived = put(
        &app,
        &path,
        json!({ "name": "Taller Ruiz", "nif": "B87654321", "archived": true }),
    )
    .await;
    assert_eq!(archived.json()["archived"], true);
    let refused = post(&app, "/api/invoices", body.clone()).await;
    assert_eq!(refused.json()["error"]["field"], "contact_id");
    put(&app, &path, json!({ "name": "Taller Ruiz", "nif": "B87654321", "is_customer": false, "is_supplier": true })).await;
    let refused = post(&app, "/api/invoices", body).await;
    assert_eq!(refused.json()["error"]["field"], "contact_id");

    let list = get(&app, "/api/contacts").await.json();
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(
        get(&app, "/api/contacts/999").await.status,
        StatusCode::NOT_FOUND
    );
}
