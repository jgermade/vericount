//! Database access for invoices and billing records.
//!
//! Records are only ever inserted. Every insertion into an issuer's chain
//! happens in a transaction that first takes an advisory lock on that
//! chain, so invoice numbers and the hash chain never fork.

use chrono::{DateTime, NaiveDate};
use chrono_tz::Tz;
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};
use vericounts_core::{
    hash::{self, CancellationFields, RegistrationFields},
    invoice::{self, Line, TaxBreakdown, Totals, ValidationError},
};

use crate::config::Issuer;

/// Only standard invoices ("F1") are supported for now.
pub const INVOICE_TYPE: &str = "F1";

#[derive(Debug)]
pub enum StoreError {
    Validation(ValidationError),
    NotFound,
    AlreadyCancelled,
    Database(sqlx::Error),
}

impl From<sqlx::Error> for StoreError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<ValidationError> for StoreError {
    fn from(error: ValidationError) -> Self {
        Self::Validation(error)
    }
}

/// A validated invoice, ready to be stored.
#[derive(Debug, Clone)]
pub struct NewInvoice {
    pub series: String,
    /// Defaults to the current date in Spain.
    pub issue_date: Option<NaiveDate>,
    pub recipient_name: String,
    pub recipient_nif: String,
    pub description: String,
    pub totals: Totals,
}

#[derive(Debug, Serialize, FromRow)]
pub struct InvoiceSummary {
    pub id: i64,
    pub series_number: String,
    pub issue_date: NaiveDate,
    pub recipient_name: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_amount: Decimal,
    pub cancelled: bool,
}

#[derive(Debug, Serialize, FromRow)]
pub struct Invoice {
    pub id: i64,
    pub issuer_nif: String,
    pub issuer_name: String,
    pub series: String,
    pub number: i32,
    pub series_number: String,
    pub issue_date: NaiveDate,
    pub invoice_type: String,
    pub recipient_name: String,
    pub recipient_nif: String,
    pub description: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_base: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_tax: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub total_amount: Decimal,
}

#[derive(Debug, Serialize, FromRow)]
pub struct BillingRecord {
    pub id: i64,
    pub record_type: String,
    pub previous_hash: String,
    pub hash: String,
    pub generated_at: String,
}

#[derive(Debug, Serialize)]
pub struct InvoiceDetail {
    #[serde(flatten)]
    pub invoice: Invoice,
    pub cancelled: bool,
    pub lines: Vec<Line>,
    pub taxes: Vec<TaxBreakdown>,
    pub records: Vec<BillingRecord>,
}

/// Result of recomputing an issuer's chain of records.
#[derive(Debug, Serialize)]
pub struct ChainReport {
    pub valid: bool,
    pub records: usize,
    pub last_hash: Option<String>,
    pub error: Option<ChainError>,
}

#[derive(Debug, Serialize)]
pub struct ChainError {
    pub record_id: i64,
    pub reason: &'static str,
}

/// Serializes every writer of one issuer's chain until the transaction
/// ends.
async fn lock_chain(conn: &mut PgConnection, issuer_nif: &str) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("vericounts:chain:{issuer_nif}"))
        .execute(conn)
        .await?;
    Ok(())
}

async fn last_hash(conn: &mut PgConnection, issuer_nif: &str) -> Result<String, sqlx::Error> {
    let hash: Option<String> = sqlx::query_scalar(
        "SELECT hash FROM billing_records WHERE issuer_nif = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(issuer_nif)
    .fetch_optional(conn)
    .await?;
    Ok(hash.unwrap_or_default())
}

/// Stores an invoice and its registration record. Returns the invoice id.
pub async fn create_invoice(
    pool: &PgPool,
    issuer: &Issuer,
    new: &NewInvoice,
    now: DateTime<Tz>,
) -> Result<i64, StoreError> {
    let today = now.date_naive();
    let issue_date = new.issue_date.unwrap_or(today);
    if issue_date > today {
        return Err(ValidationError::new("issue_date", "must not be in the future").into());
    }

    let mut tx = pool.begin().await?;
    lock_chain(&mut tx, &issuer.nif).await?;

    let (last_number, last_date): (Option<i32>, Option<NaiveDate>) = sqlx::query_as(
        "SELECT max(number), max(issue_date) FROM invoices WHERE issuer_nif = $1 AND series = $2",
    )
    .bind(&issuer.nif)
    .bind(&new.series)
    .fetch_one(&mut *tx)
    .await?;
    if last_date.is_some_and(|last| issue_date < last) {
        return Err(ValidationError::new(
            "issue_date",
            "must not be earlier than the last invoice of the series",
        )
        .into());
    }
    let number = last_number.unwrap_or(0) + 1;
    let series_number = invoice::series_number(&new.series, number);
    let totals = &new.totals;

    let invoice_id: i64 = sqlx::query_scalar(
        "INSERT INTO invoices (issuer_nif, issuer_name, series, number, series_number, issue_date,
             invoice_type, recipient_name, recipient_nif, description,
             total_base, total_tax, total_amount)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         RETURNING id",
    )
    .bind(&issuer.nif)
    .bind(&issuer.name)
    .bind(&new.series)
    .bind(number)
    .bind(&series_number)
    .bind(issue_date)
    .bind(INVOICE_TYPE)
    .bind(&new.recipient_name)
    .bind(&new.recipient_nif)
    .bind(&new.description)
    .bind(totals.total_base)
    .bind(totals.total_tax)
    .bind(totals.total_amount)
    .fetch_one(&mut *tx)
    .await?;

    for (index, line) in totals.lines.iter().enumerate() {
        sqlx::query(
            "INSERT INTO invoice_lines
                 (invoice_id, position, description, quantity, unit_price, vat_rate, base_amount)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(invoice_id)
        .bind(index as i32 + 1)
        .bind(&line.description)
        .bind(line.quantity)
        .bind(line.unit_price)
        .bind(line.vat_rate)
        .bind(line.base_amount)
        .execute(&mut *tx)
        .await?;
    }
    for tax in &totals.taxes {
        sqlx::query(
            "INSERT INTO invoice_taxes (invoice_id, vat_rate, base_amount, tax_amount)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(invoice_id)
        .bind(tax.vat_rate)
        .bind(tax.base_amount)
        .bind(tax.tax_amount)
        .execute(&mut *tx)
        .await?;
    }

    let previous_hash = last_hash(&mut tx, &issuer.nif).await?;
    let generated_at = hash::format_generated_at(&now);
    let record_hash = hash::registration_hash(&RegistrationFields {
        issuer_nif: &issuer.nif,
        series_number: &series_number,
        issue_date,
        invoice_type: INVOICE_TYPE,
        total_tax: totals.total_tax,
        total_amount: totals.total_amount,
        previous_hash: &previous_hash,
        generated_at: &generated_at,
    });
    sqlx::query(
        "INSERT INTO billing_records (invoice_id, record_type, issuer_nif, series_number,
             issue_date, invoice_type, total_tax, total_amount, previous_hash, hash, generated_at)
         VALUES ($1, 'registration', $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(invoice_id)
    .bind(&issuer.nif)
    .bind(&series_number)
    .bind(issue_date)
    .bind(INVOICE_TYPE)
    .bind(totals.total_tax)
    .bind(totals.total_amount)
    .bind(&previous_hash)
    .bind(&record_hash)
    .bind(&generated_at)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(invoice_id)
}

/// Adds a cancellation record for an invoice.
pub async fn cancel_invoice(
    pool: &PgPool,
    issuer: &Issuer,
    invoice_id: i64,
    now: DateTime<Tz>,
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    lock_chain(&mut tx, &issuer.nif).await?;

    let invoice: Option<(String, NaiveDate, bool)> = sqlx::query_as(
        "SELECT series_number, issue_date,
                EXISTS (SELECT FROM billing_records r
                        WHERE r.invoice_id = i.id AND r.record_type = 'cancellation')
         FROM invoices i WHERE id = $1 AND issuer_nif = $2",
    )
    .bind(invoice_id)
    .bind(&issuer.nif)
    .fetch_optional(&mut *tx)
    .await?;
    let (series_number, issue_date, cancelled) = invoice.ok_or(StoreError::NotFound)?;
    if cancelled {
        return Err(StoreError::AlreadyCancelled);
    }

    let previous_hash = last_hash(&mut tx, &issuer.nif).await?;
    let generated_at = hash::format_generated_at(&now);
    let record_hash = hash::cancellation_hash(&CancellationFields {
        issuer_nif: &issuer.nif,
        series_number: &series_number,
        issue_date,
        previous_hash: &previous_hash,
        generated_at: &generated_at,
    });
    sqlx::query(
        "INSERT INTO billing_records (invoice_id, record_type, issuer_nif, series_number,
             issue_date, previous_hash, hash, generated_at)
         VALUES ($1, 'cancellation', $2, $3, $4, $5, $6, $7)",
    )
    .bind(invoice_id)
    .bind(&issuer.nif)
    .bind(&series_number)
    .bind(issue_date)
    .bind(&previous_hash)
    .bind(&record_hash)
    .bind(&generated_at)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

const CANCELLED: &str = "EXISTS (SELECT FROM billing_records r
     WHERE r.invoice_id = i.id AND r.record_type = 'cancellation')";

pub async fn list_invoices(
    pool: &PgPool,
    issuer_nif: &str,
) -> Result<Vec<InvoiceSummary>, StoreError> {
    Ok(sqlx::query_as(&format!(
        "SELECT id, series_number, issue_date, recipient_name, total_amount,
                {CANCELLED} AS cancelled
         FROM invoices i WHERE issuer_nif = $1 ORDER BY id DESC"
    ))
    .bind(issuer_nif)
    .fetch_all(pool)
    .await?)
}

pub async fn get_invoice(
    pool: &PgPool,
    issuer_nif: &str,
    invoice_id: i64,
) -> Result<InvoiceDetail, StoreError> {
    let invoice: Invoice = sqlx::query_as(
        "SELECT id, issuer_nif, issuer_name, series, number, series_number, issue_date,
                invoice_type, recipient_name, recipient_nif, description,
                total_base, total_tax, total_amount
         FROM invoices WHERE id = $1 AND issuer_nif = $2",
    )
    .bind(invoice_id)
    .bind(issuer_nif)
    .fetch_optional(pool)
    .await?
    .ok_or(StoreError::NotFound)?;

    let lines = sqlx::query_as::<_, (String, Decimal, Decimal, Decimal, Decimal)>(
        "SELECT description, quantity, unit_price, vat_rate, base_amount
         FROM invoice_lines WHERE invoice_id = $1 ORDER BY position",
    )
    .bind(invoice_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(
        |(description, quantity, unit_price, vat_rate, base_amount)| Line {
            description,
            quantity: quantity.normalize(),
            unit_price: unit_price.normalize(),
            vat_rate: vat_rate.normalize(),
            base_amount,
        },
    )
    .collect();

    let taxes = sqlx::query_as::<_, (Decimal, Decimal, Decimal)>(
        "SELECT vat_rate, base_amount, tax_amount
         FROM invoice_taxes WHERE invoice_id = $1 ORDER BY vat_rate DESC",
    )
    .bind(invoice_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(vat_rate, base_amount, tax_amount)| TaxBreakdown {
        vat_rate: vat_rate.normalize(),
        base_amount,
        tax_amount,
    })
    .collect();

    let records: Vec<BillingRecord> = sqlx::query_as(
        "SELECT id, record_type, previous_hash, hash, generated_at
         FROM billing_records WHERE invoice_id = $1 ORDER BY id",
    )
    .bind(invoice_id)
    .fetch_all(pool)
    .await?;

    Ok(InvoiceDetail {
        cancelled: records.iter().any(|r| r.record_type == "cancellation"),
        invoice,
        lines,
        taxes,
        records,
    })
}

#[derive(FromRow)]
struct ChainRow {
    id: i64,
    record_type: String,
    series_number: String,
    issue_date: NaiveDate,
    invoice_type: Option<String>,
    total_tax: Option<Decimal>,
    total_amount: Option<Decimal>,
    previous_hash: String,
    hash: String,
    generated_at: String,
}

/// Recomputes every hash of an issuer's chain and checks the links.
pub async fn verify_chain(pool: &PgPool, issuer_nif: &str) -> Result<ChainReport, StoreError> {
    let rows: Vec<ChainRow> = sqlx::query_as(
        "SELECT id, record_type, series_number, issue_date, invoice_type, total_tax,
                total_amount, previous_hash, hash, generated_at
         FROM billing_records WHERE issuer_nif = $1 ORDER BY id",
    )
    .bind(issuer_nif)
    .fetch_all(pool)
    .await?;

    let mut previous_hash = String::new();
    for row in &rows {
        let failure = |reason| ChainReport {
            valid: false,
            records: rows.len(),
            last_hash: None,
            error: Some(ChainError {
                record_id: row.id,
                reason,
            }),
        };
        if row.previous_hash != previous_hash {
            return Ok(failure("previous hash does not match the previous record"));
        }
        let expected = match (
            row.record_type.as_str(),
            &row.invoice_type,
            row.total_tax,
            row.total_amount,
        ) {
            ("registration", Some(invoice_type), Some(total_tax), Some(total_amount)) => {
                hash::registration_hash(&RegistrationFields {
                    issuer_nif,
                    series_number: &row.series_number,
                    issue_date: row.issue_date,
                    invoice_type,
                    total_tax,
                    total_amount,
                    previous_hash: &row.previous_hash,
                    generated_at: &row.generated_at,
                })
            }
            ("cancellation", ..) => hash::cancellation_hash(&CancellationFields {
                issuer_nif,
                series_number: &row.series_number,
                issue_date: row.issue_date,
                previous_hash: &row.previous_hash,
                generated_at: &row.generated_at,
            }),
            _ => return Ok(failure("record has an unknown shape")),
        };
        if expected != row.hash {
            return Ok(failure("hash does not match the record contents"));
        }
        previous_hash.clone_from(&row.hash);
    }

    Ok(ChainReport {
        valid: true,
        records: rows.len(),
        last_hash: rows.last().map(|row| row.hash.clone()),
        error: None,
    })
}
