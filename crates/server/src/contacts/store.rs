//! Customers and suppliers. An invoice keeps a copy of its customer's data
//! as it was when issued, and a link to the contact.

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use vericount_core::invoice::{self, ValidationError};

use crate::{
    company::store::{optional_text, postal_code},
    error::StoreError,
};

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
