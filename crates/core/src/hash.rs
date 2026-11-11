//! Veri*factu record hash ("huella").
//!
//! Implements the AEAT document "Detalle de las especificaciones técnicas
//! para generación de la huella o hash de los registros de facturación":
//! SHA-256 over a `name=value&name=value` string in UTF-8, written as
//! uppercase hexadecimal. Each record includes the hash of the previous
//! record of the same issuer, which chains them.

use chrono::{DateTime, NaiveDate, TimeZone};
use rust_decimal::{Decimal, RoundingStrategy};
use sha2::{Digest, Sha256};

/// Fields of a registration record ("RegistroAlta") that go into its hash.
#[derive(Debug, Clone, Copy)]
pub struct RegistrationFields<'a> {
    /// `IDEmisorFactura`: NIF of the issuer.
    pub issuer_nif: &'a str,
    /// `NumSerieFactura`: series and number of the invoice.
    pub series_number: &'a str,
    /// `FechaExpedicionFactura`.
    pub issue_date: NaiveDate,
    /// `TipoFactura`, e.g. `F1`.
    pub invoice_type: &'a str,
    /// `CuotaTotal`.
    pub total_tax: Decimal,
    /// `ImporteTotal`.
    pub total_amount: Decimal,
    /// `Huella` of the previous record; empty for the first record.
    pub previous_hash: &'a str,
    /// `FechaHoraHusoGenRegistro`, exactly as written in the record.
    pub generated_at: &'a str,
}

/// Fields of a cancellation record ("RegistroAnulacion") that go into its
/// hash.
#[derive(Debug, Clone, Copy)]
pub struct CancellationFields<'a> {
    /// `IDEmisorFacturaAnulada`.
    pub issuer_nif: &'a str,
    /// `NumSerieFacturaAnulada`.
    pub series_number: &'a str,
    /// `FechaExpedicionFacturaAnulada`.
    pub issue_date: NaiveDate,
    /// `Huella` of the previous record; empty for the first record.
    pub previous_hash: &'a str,
    /// `FechaHoraHusoGenRegistro`, exactly as written in the record.
    pub generated_at: &'a str,
}

/// Hash of a registration record.
pub fn registration_hash(fields: &RegistrationFields<'_>) -> String {
    sha256_hex(&format!(
        "IDEmisorFactura={}&NumSerieFactura={}&FechaExpedicionFactura={}&TipoFactura={}\
         &CuotaTotal={}&ImporteTotal={}&Huella={}&FechaHoraHusoGenRegistro={}",
        fields.issuer_nif.trim(),
        fields.series_number.trim(),
        format_date(fields.issue_date),
        fields.invoice_type.trim(),
        format_amount(fields.total_tax),
        format_amount(fields.total_amount),
        fields.previous_hash.trim(),
        fields.generated_at.trim(),
    ))
}

/// Hash of a cancellation record.
pub fn cancellation_hash(fields: &CancellationFields<'_>) -> String {
    sha256_hex(&format!(
        "IDEmisorFacturaAnulada={}&NumSerieFacturaAnulada={}&FechaExpedicionFacturaAnulada={}\
         &Huella={}&FechaHoraHusoGenRegistro={}",
        fields.issuer_nif.trim(),
        fields.series_number.trim(),
        format_date(fields.issue_date),
        fields.previous_hash.trim(),
        fields.generated_at.trim(),
    ))
}

/// Formats a date as `DD-MM-YYYY`, the format AEAT uses in records.
pub fn format_date(date: NaiveDate) -> String {
    date.format("%d-%m-%Y").to_string()
}

/// Formats an amount with exactly two decimals and a dot separator. The
/// same text must be used in the record XML and in the hash.
pub fn format_amount(amount: Decimal) -> String {
    let rounded = amount.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    format!("{rounded:.2}")
}

/// Formats a timestamp as `FechaHoraHusoGenRegistro`: ISO 8601 with
/// seconds and UTC offset, e.g. `2024-01-01T19:20:30+01:00`.
pub fn format_generated_at<Tz: TimeZone>(timestamp: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    timestamp.format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

fn sha256_hex(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()
    }

    // The three vectors below are the examples of the AEAT specification.
    const FIRST: &str = "3C464DAF61ACB827C65FDA19F352A4E3BDC2C640E9E9FC4CC058073F38F12F60";
    const SECOND: &str = "F7B94CFD8924EDFF273501B01EE5153E4CE8F259766F88CF6ACB8935802A2B97";
    const CANCELLATION: &str = "177547C0D57AC74748561D054A9CEC14B4C4EA23D1BEFD6F2E69E3A388F90C68";

    #[test]
    fn first_registration_matches_aeat_example() {
        let hash = registration_hash(&RegistrationFields {
            issuer_nif: "89890001K",
            series_number: "12345678/G33",
            issue_date: date(),
            invoice_type: "F1",
            total_tax: "12.35".parse().unwrap(),
            total_amount: "123.45".parse().unwrap(),
            previous_hash: "",
            generated_at: "2024-01-01T19:20:30+01:00",
        });
        assert_eq!(hash, FIRST);
    }

    #[test]
    fn chained_registration_matches_aeat_example() {
        let hash = registration_hash(&RegistrationFields {
            issuer_nif: "89890001K",
            series_number: "12345679/G34",
            issue_date: date(),
            invoice_type: "F1",
            total_tax: "12.35".parse().unwrap(),
            total_amount: "123.45".parse().unwrap(),
            previous_hash: FIRST,
            generated_at: "2024-01-01T19:20:35+01:00",
        });
        assert_eq!(hash, SECOND);
    }

    #[test]
    fn cancellation_matches_aeat_example() {
        let hash = cancellation_hash(&CancellationFields {
            issuer_nif: "89890001K",
            series_number: "12345679/G34",
            issue_date: date(),
            previous_hash: SECOND,
            generated_at: "2024-01-01T19:20:40+01:00",
        });
        assert_eq!(hash, CANCELLATION);
    }

    #[test]
    fn amounts_always_have_two_decimals() {
        for (input, expected) in [
            ("123.1", "123.10"),
            ("100", "100.00"),
            ("0.005", "0.01"),
            ("-0.005", "-0.01"),
            ("12.3449", "12.34"),
        ] {
            assert_eq!(format_amount(input.parse().unwrap()), expected);
        }
    }

    #[test]
    fn generated_at_has_seconds_and_offset() {
        let timestamp = FixedOffset::east_opt(3600)
            .unwrap()
            .with_ymd_and_hms(2024, 1, 1, 19, 20, 30)
            .unwrap();
        assert_eq!(format_generated_at(&timestamp), "2024-01-01T19:20:30+01:00");
    }
}
