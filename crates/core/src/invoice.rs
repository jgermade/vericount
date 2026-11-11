//! Invoice validation and totals.

use std::collections::BTreeMap;
use std::fmt;

use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

/// VAT rates accepted for now: the general, reduced and super-reduced
/// rates. Exempt and non-subject operations need extra Veri*factu fields
/// and are not supported yet.
pub const ALLOWED_VAT_RATES: [u32; 3] = [21, 10, 4];

const MAX_LINES: usize = 200;
const MAX_TEXT: usize = 500;
/// Veri*factu amounts have at most 12 integer digits.
const MAX_AMOUNT: Decimal = Decimal::from_parts(276_447_231, 23_283, 0, false, 2);

/// A validation failure, with the name of the offending field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}

/// An invoice line as entered by the user.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineInput {
    pub description: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub unit_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub vat_rate: Decimal,
}

/// An invoice line with its computed base amount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    pub description: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub unit_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub vat_rate: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub base_amount: Decimal,
}

/// Base and tax for one VAT rate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaxBreakdown {
    #[serde(with = "rust_decimal::serde::str")]
    pub vat_rate: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub base_amount: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub tax_amount: Decimal,
}

/// Computed lines and totals of an invoice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Totals {
    pub lines: Vec<Line>,
    pub taxes: Vec<TaxBreakdown>,
    pub total_base: Decimal,
    pub total_tax: Decimal,
    pub total_amount: Decimal,
}

/// Rounds to cents, half away from zero.
pub fn round_cents(amount: Decimal) -> Decimal {
    amount.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

/// Validates the lines and computes the totals.
///
/// Each line base is rounded to cents. The tax is computed once per VAT
/// rate on the sum of the bases of that rate, then rounded to cents.
pub fn compute_totals(inputs: &[LineInput]) -> Result<Totals, ValidationError> {
    if inputs.is_empty() {
        return Err(ValidationError::new(
            "lines",
            "at least one line is required",
        ));
    }
    if inputs.len() > MAX_LINES {
        return Err(ValidationError::new("lines", "too many lines"));
    }

    let mut lines = Vec::with_capacity(inputs.len());
    let mut bases: BTreeMap<Decimal, Decimal> = BTreeMap::new();

    for (index, input) in inputs.iter().enumerate() {
        let field = |name: &str| format!("lines[{index}].{name}");
        let description = validate_text(&field("description"), &input.description)?;
        if input.quantity <= Decimal::ZERO {
            return Err(ValidationError::new(field("quantity"), "must be positive"));
        }
        if input.unit_price < Decimal::ZERO {
            return Err(ValidationError::new(
                field("unit_price"),
                "must not be negative",
            ));
        }
        if input.quantity.scale() > 4 || input.unit_price.scale() > 4 {
            return Err(ValidationError::new(
                field("quantity"),
                "quantity and unit price allow at most 4 decimals",
            ));
        }
        if input.quantity > MAX_AMOUNT || input.unit_price > MAX_AMOUNT {
            return Err(ValidationError::new(field("unit_price"), "too large"));
        }
        let vat_rate = input.vat_rate.normalize();
        if !ALLOWED_VAT_RATES
            .iter()
            .any(|rate| Decimal::from(*rate) == vat_rate)
        {
            return Err(ValidationError::new(
                field("vat_rate"),
                format!("must be one of {ALLOWED_VAT_RATES:?}"),
            ));
        }
        let base_amount = input
            .quantity
            .checked_mul(input.unit_price)
            .map(round_cents)
            .filter(|base| *base <= MAX_AMOUNT)
            .ok_or_else(|| ValidationError::new(field("unit_price"), "amount too large"))?;

        *bases.entry(vat_rate).or_default() += base_amount;
        lines.push(Line {
            description,
            quantity: input.quantity,
            unit_price: input.unit_price,
            vat_rate,
            base_amount,
        });
    }

    let taxes: Vec<TaxBreakdown> = bases
        .into_iter()
        .rev()
        .map(|(vat_rate, base_amount)| TaxBreakdown {
            vat_rate,
            base_amount,
            tax_amount: round_cents(base_amount * vat_rate / Decimal::ONE_HUNDRED),
        })
        .collect();
    let total_base: Decimal = taxes.iter().map(|tax| tax.base_amount).sum();
    let total_tax: Decimal = taxes.iter().map(|tax| tax.tax_amount).sum();
    let total_amount = total_base + total_tax;
    if total_amount > MAX_AMOUNT {
        return Err(ValidationError::new("lines", "total amount too large"));
    }

    Ok(Totals {
        lines,
        taxes,
        total_base,
        total_tax,
        total_amount,
    })
}

/// Validates a free-text field: trimmed, not empty, bounded and without
/// control characters. Returns the trimmed text.
pub fn validate_text(field: &str, value: &str) -> Result<String, ValidationError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ValidationError::new(field, "must not be empty"));
    }
    if value.chars().count() > MAX_TEXT {
        return Err(ValidationError::new(field, "too long"));
    }
    if value.chars().any(char::is_control) {
        return Err(ValidationError::new(
            field,
            "must not contain control characters",
        ));
    }
    Ok(value.to_owned())
}

/// Validates an invoice series: 1 to 20 characters, `A-Z` and `0-9`.
pub fn validate_series(series: &str) -> Result<String, ValidationError> {
    let valid = (1..=20).contains(&series.len())
        && series
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    if valid {
        Ok(series.to_owned())
    } else {
        Err(ValidationError::new(
            "series",
            "must be 1 to 20 characters, A-Z and 0-9 only",
        ))
    }
}

/// Validates the shape of a tax identifier: 1 to 20 characters, `A-Z` and
/// `0-9`. It does not verify check digits nor the AEAT census.
pub fn validate_tax_id(field: &str, value: &str) -> Result<String, ValidationError> {
    let value = value.trim().to_ascii_uppercase();
    let valid = (1..=20).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    if valid {
        Ok(value)
    } else {
        Err(ValidationError::new(
            field,
            "must be 1 to 20 characters, letters and digits only",
        ))
    }
}

/// Builds `NumSerieFactura` from a series and a number, e.g. `A2026-0001`.
pub fn series_number(series: &str, number: i32) -> String {
    format!("{series}-{number:04}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(quantity: &str, unit_price: &str, vat_rate: &str) -> LineInput {
        LineInput {
            description: "Service".to_owned(),
            quantity: quantity.parse().unwrap(),
            unit_price: unit_price.parse().unwrap(),
            vat_rate: vat_rate.parse().unwrap(),
        }
    }

    fn dec(value: &str) -> Decimal {
        value.parse().unwrap()
    }

    #[test]
    fn totals_group_tax_by_rate() {
        let totals = compute_totals(&[
            line("1", "100", "21"),
            line("2", "10.555", "21.00"),
            line("3", "1.10", "10"),
        ])
        .unwrap();
        assert_eq!(totals.lines[1].base_amount, dec("21.11"));
        assert_eq!(totals.taxes.len(), 2);
        assert_eq!(totals.taxes[0].vat_rate, dec("21"));
        assert_eq!(totals.taxes[0].base_amount, dec("121.11"));
        assert_eq!(totals.taxes[0].tax_amount, dec("25.43"));
        assert_eq!(totals.taxes[1].tax_amount, dec("0.33"));
        assert_eq!(totals.total_base, dec("124.41"));
        assert_eq!(totals.total_tax, dec("25.76"));
        assert_eq!(totals.total_amount, dec("150.17"));
    }

    #[test]
    fn invalid_lines_are_rejected() {
        assert_eq!(compute_totals(&[]).unwrap_err().field, "lines");
        for (input, field) in [
            (line("0", "1", "21"), "lines[0].quantity"),
            (line("-1", "1", "21"), "lines[0].quantity"),
            (line("1", "-1", "21"), "lines[0].unit_price"),
            (line("1", "1", "20"), "lines[0].vat_rate"),
            (line("1", "1", "0"), "lines[0].vat_rate"),
            (line("1.00001", "1", "21"), "lines[0].quantity"),
            (
                line("999999999999", "999999999999", "21"),
                "lines[0].unit_price",
            ),
        ] {
            assert_eq!(compute_totals(&[input]).unwrap_err().field, field);
        }
        let mut blank = line("1", "1", "21");
        blank.description = "  ".to_owned();
        assert_eq!(
            compute_totals(&[blank]).unwrap_err().field,
            "lines[0].description"
        );
    }

    #[test]
    fn max_amount_is_twelve_integer_digits() {
        assert_eq!(MAX_AMOUNT, dec("999999999999.99"));
    }

    #[test]
    fn series_and_tax_ids() {
        assert!(validate_series("A2026").is_ok());
        for bad in ["", "a", "A-1", "A 1", "ABCDEFGHIJKLMNOPQRSTU"] {
            assert!(validate_series(bad).is_err(), "{bad}");
        }
        assert_eq!(validate_tax_id("nif", " b12345678 ").unwrap(), "B12345678");
        assert!(validate_tax_id("nif", "B-123").is_err());
        assert_eq!(series_number("A2026", 7), "A2026-0007");
        assert_eq!(series_number("A", 12345), "A-12345");
    }
}
