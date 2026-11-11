//! Company data and numbering series: the editable data that invoices are
//! built from. Invoices copy what they need when issued, so later edits
//! never change an issued invoice.

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use vericount_core::invoice::{self, ValidationError};

use crate::error::StoreError;

/// The company that issues the invoices, as stored in the `company` table.
#[derive(Debug, Clone)]
pub struct Issuer {
    pub nif: String,
    pub name: String,
    /// One-line address, printed on invoices.
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
#[serde(deny_unknown_fields)]
pub struct Company {
    pub nif: String,
    pub name: String,
    pub address: String,
    pub postal_code: String,
    pub city: String,
}

pub(crate) fn optional_text(field: &str, value: &str) -> Result<String, ValidationError> {
    if value.trim().is_empty() {
        Ok(String::new())
    } else {
        invoice::validate_text(field, value)
    }
}

pub(crate) fn postal_code(value: &str, required: bool) -> Result<String, ValidationError> {
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
/// records, so it cannot change once live mode is on.
pub async fn save_company(pool: &PgPool, company: Company) -> Result<Company, StoreError> {
    let company = company.validate()?;
    let mut tx = pool.begin().await?;
    let current: Option<String> = sqlx::query_scalar("SELECT nif FROM company FOR UPDATE")
        .fetch_optional(&mut *tx)
        .await?;
    if current.as_ref().is_some_and(|nif| *nif != company.nif) {
        // Practice invoices do not lock it: they are not real.
        let locked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM live_mode) OR EXISTS (SELECT FROM billing_records WHERE live)",
        )
        .fetch_one(&mut *tx)
        .await?;
        if locked {
            return Err(StoreError::Conflict(
                "nif_locked",
                "the NIF cannot change in live mode",
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
