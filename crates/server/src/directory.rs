//! Company data, numbering series and contacts: the editable data that
//! invoices are built from. Invoices copy what they need when issued, so
//! later edits never change an issued invoice.

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use vericounts_core::invoice::{self, ValidationError};

use crate::{config::Issuer, store::StoreError};

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(deny_unknown_fields)]
pub struct Company {
    pub nif: String,
    pub name: String,
    pub address: String,
    pub postal_code: String,
    pub city: String,
}

fn optional_text(field: &str, value: &str) -> Result<String, ValidationError> {
    if value.trim().is_empty() {
        Ok(String::new())
    } else {
        invoice::validate_text(field, value)
    }
}

fn postal_code(value: &str, required: bool) -> Result<String, ValidationError> {
    let value = value.trim();
    let valid = value.len() == 5 && value.bytes().all(|b| b.is_ascii_digit());
    if valid || (!required && value.is_empty()) {
        Ok(value.to_owned())
    } else {
        Err(ValidationError::new("postal_code", "must be 5 digits"))
    }
}

/// One-line address as printed on invoices.
pub fn full_address(address: &str, postal_code: &str, city: &str) -> String {
    [address, &format!("{postal_code} {city}")]
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

impl Company {
    fn validate(self) -> Result<Self, ValidationError> {
        Ok(Self {
            nif: invoice::validate_tax_id("nif", &self.nif)?,
            name: invoice::validate_text("name", &self.name)?,
            address: invoice::validate_text("address", &self.address)?,
            postal_code: postal_code(&self.postal_code, true)?,
            city: invoice::validate_text("city", &self.city)?,
        })
    }
}

pub async fn company(pool: &PgPool) -> Result<Option<Company>, StoreError> {
    Ok(
        sqlx::query_as("SELECT nif, name, address, postal_code, city FROM company")
            .fetch_optional(pool)
            .await?,
    )
}

/// The company as the issuer of invoices.
pub async fn issuer(pool: &PgPool) -> Result<Issuer, StoreError> {
    let company = company(pool).await?.ok_or(StoreError::CompanyMissing)?;
    Ok(Issuer {
        address: full_address(&company.address, &company.postal_code, &company.city),
        nif: company.nif,
        name: company.name,
    })
}

/// Creates or updates the company. The NIF identifies the chain of
/// records, so it cannot change once a record exists.
pub async fn save_company(pool: &PgPool, company: Company) -> Result<Company, StoreError> {
    let company = company.validate()?;
    let mut tx = pool.begin().await?;
    let current: Option<String> = sqlx::query_scalar("SELECT nif FROM company FOR UPDATE")
        .fetch_optional(&mut *tx)
        .await?;
    if current.as_ref().is_some_and(|nif| *nif != company.nif) {
        let has_records: bool = sqlx::query_scalar("SELECT EXISTS (SELECT FROM billing_records)")
            .fetch_one(&mut *tx)
            .await?;
        if has_records {
            return Err(StoreError::Conflict(
                "nif_locked",
                "the NIF cannot change once invoices exist",
            ));
        }
    }
    sqlx::query(
        "INSERT INTO company (nif, name, address, postal_code, city)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (singleton) DO UPDATE SET nif = EXCLUDED.nif, name = EXCLUDED.name,
             address = EXCLUDED.address, postal_code = EXCLUDED.postal_code,
             city = EXCLUDED.city, updated_at = now()",
    )
    .bind(&company.nif)
    .bind(&company.name)
    .bind(&company.address)
    .bind(&company.postal_code)
    .bind(&company.city)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(company)
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Series {
    pub code: String,
    pub description: String,
    pub active: bool,
    /// Number of invoices issued in the series.
    pub invoices: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewSeries {
    pub code: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeriesUpdate {
    pub description: String,
    pub active: bool,
}

const SERIES_COLUMNS: &str = "code, description, active,
    (SELECT count(*) FROM invoices i WHERE i.series = s.code) AS invoices";

pub async fn list_series(pool: &PgPool) -> Result<Vec<Series>, StoreError> {
    Ok(sqlx::query_as(&format!(
        "SELECT {SERIES_COLUMNS} FROM series s ORDER BY code"
    ))
    .fetch_all(pool)
    .await?)
}

pub async fn create_series(pool: &PgPool, new: NewSeries) -> Result<Series, StoreError> {
    let code = invoice::validate_series(&new.code)?;
    let description = optional_text("description", &new.description)?;
    let inserted = sqlx::query(
        "INSERT INTO series (code, description) VALUES ($1, $2) ON CONFLICT (code) DO NOTHING",
    )
    .bind(&code)
    .bind(&description)
    .execute(pool)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(StoreError::Conflict(
            "series_exists",
            "the series already exists",
        ));
    }
    Ok(Series {
        code,
        description,
        active: true,
        invoices: 0,
    })
}

/// Series are never deleted nor renamed; they can be described and
/// switched off.
pub async fn update_series(
    pool: &PgPool,
    code: &str,
    update: SeriesUpdate,
) -> Result<Series, StoreError> {
    let description = optional_text("description", &update.description)?;
    sqlx::query_as(&format!(
        "UPDATE series s SET description = $2, active = $3 WHERE code = $1
         RETURNING {SERIES_COLUMNS}"
    ))
    .bind(code)
    .bind(description)
    .bind(update.active)
    .fetch_optional(pool)
    .await?
    .ok_or(StoreError::NotFound)
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Contact {
    pub id: i64,
    pub name: String,
    pub nif: String,
    pub address: String,
    pub postal_code: String,
    pub city: String,
    pub email: String,
    pub is_customer: bool,
    pub is_supplier: bool,
    pub archived: bool,
    /// Number of invoices issued to the contact.
    pub invoices: i64,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContactInput {
    pub name: String,
    pub nif: String,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub postal_code: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub email: String,
    #[serde(default = "default_true")]
    pub is_customer: bool,
    #[serde(default)]
    pub is_supplier: bool,
    #[serde(default)]
    pub archived: bool,
}

impl ContactInput {
    fn validate(self) -> Result<Self, ValidationError> {
        let email = self.email.trim().to_owned();
        let email_ok = email.is_empty()
            || (email.len() <= 254
                && !email.chars().any(|c| c.is_whitespace() || c.is_control())
                && email
                    .split_once('@')
                    .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.')));
        if !email_ok {
            return Err(ValidationError::new("email", "is not an email address"));
        }
        if !self.is_customer && !self.is_supplier {
            return Err(ValidationError::new(
                "is_customer",
                "a contact is a customer, a supplier or both",
            ));
        }
        Ok(Self {
            name: invoice::validate_text("name", &self.name)?,
            nif: invoice::validate_tax_id("nif", &self.nif)?,
            address: optional_text("address", &self.address)?,
            postal_code: postal_code(&self.postal_code, false)?,
            city: optional_text("city", &self.city)?,
            email,
            ..self
        })
    }
}

const CONTACT_COLUMNS: &str = "id, name, nif, address, postal_code, city, email, is_customer,
    is_supplier, archived,
    (SELECT count(*) FROM invoices i WHERE i.contact_id = c.id) AS invoices";

pub async fn list_contacts(pool: &PgPool) -> Result<Vec<Contact>, StoreError> {
    Ok(sqlx::query_as(&format!(
        "SELECT {CONTACT_COLUMNS} FROM contacts c ORDER BY archived, lower(name), id"
    ))
    .fetch_all(pool)
    .await?)
}

pub async fn get_contact(pool: &PgPool, id: i64) -> Result<Contact, StoreError> {
    sqlx::query_as(&format!(
        "SELECT {CONTACT_COLUMNS} FROM contacts c WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(StoreError::NotFound)
}

fn nif_taken(error: sqlx::Error) -> StoreError {
    match error.as_database_error() {
        Some(db) if db.is_unique_violation() => {
            StoreError::Conflict("nif_exists", "another contact has this NIF")
        }
        _ => StoreError::Database(error),
    }
}

pub async fn create_contact(pool: &PgPool, input: ContactInput) -> Result<Contact, StoreError> {
    let input = input.validate()?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO contacts (name, nif, address, postal_code, city, email,
             is_customer, is_supplier, archived)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(&input.name)
    .bind(&input.nif)
    .bind(&input.address)
    .bind(&input.postal_code)
    .bind(&input.city)
    .bind(&input.email)
    .bind(input.is_customer)
    .bind(input.is_supplier)
    .bind(input.archived)
    .fetch_one(pool)
    .await
    .map_err(nif_taken)?;
    get_contact(pool, id).await
}

pub async fn update_contact(
    pool: &PgPool,
    id: i64,
    input: ContactInput,
) -> Result<Contact, StoreError> {
    let input = input.validate()?;
    let updated = sqlx::query(
        "UPDATE contacts SET name = $2, nif = $3, address = $4, postal_code = $5, city = $6,
             email = $7, is_customer = $8, is_supplier = $9, archived = $10, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&input.name)
    .bind(&input.nif)
    .bind(&input.address)
    .bind(&input.postal_code)
    .bind(&input.city)
    .bind(&input.email)
    .bind(input.is_customer)
    .bind(input.is_supplier)
    .bind(input.archived)
    .execute(pool)
    .await
    .map_err(nif_taken)?;
    if updated.rows_affected() == 0 {
        return Err(StoreError::NotFound);
    }
    get_contact(pool, id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_joined_without_empty_parts() {
        assert_eq!(
            full_address("Calle Real 5", "15001", "A Coruña"),
            "Calle Real 5, 15001 A Coruña"
        );
        assert_eq!(full_address("", "", ""), "");
        assert_eq!(full_address("Calle Real 5", "", ""), "Calle Real 5");
    }

    #[test]
    fn postal_codes() {
        assert!(postal_code("15001", true).is_ok());
        assert!(postal_code("1500", true).is_err());
        assert!(postal_code("", true).is_err());
        assert!(postal_code("", false).is_ok());
        assert!(postal_code("1500A", false).is_err());
    }
}
