//! The error every domain's data access returns. The API turns it into an
//! HTTP answer (`api::ApiError`).

use vericount_core::invoice::ValidationError;

#[derive(Debug)]
pub enum StoreError {
    Validation(ValidationError),
    NotFound,
    AlreadyCancelled,
    /// The company data has not been entered yet.
    CompanyMissing,
    /// The request conflicts with existing data: (code, message).
    Conflict(&'static str, &'static str),
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
