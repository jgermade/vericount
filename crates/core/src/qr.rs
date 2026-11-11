//! URL encoded in the QR code of an invoice, which lets the recipient
//! check the invoice at the AEAT.

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::hash::{format_amount, format_date};

/// AEAT environment a record is sent to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AeatEnvironment {
    /// AEAT pre-production (testing) environment.
    Test,
    Production,
}

impl AeatEnvironment {
    fn qr_base_url(self) -> &'static str {
        match self {
            Self::Test => "https://prewww2.aeat.es/wlpl/TIKE-CONT/ValidarQR",
            Self::Production => "https://www2.agenciatributaria.gob.es/wlpl/TIKE-CONT/ValidarQR",
        }
    }
}

/// Builds the QR URL of a Veri*factu invoice.
pub fn qr_url(
    environment: AeatEnvironment,
    issuer_nif: &str,
    series_number: &str,
    issue_date: NaiveDate,
    total_amount: Decimal,
) -> String {
    format!(
        "{}?nif={}&numserie={}&fecha={}&importe={}",
        environment.qr_base_url(),
        percent_encode(issuer_nif),
        percent_encode(series_number),
        format_date(issue_date),
        format_amount(total_amount),
    )
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_validation_url() {
        let url = qr_url(
            AeatEnvironment::Test,
            "89890001K",
            "12345678&G33",
            NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            "241.4".parse().unwrap(),
        );
        assert_eq!(
            url,
            "https://prewww2.aeat.es/wlpl/TIKE-CONT/ValidarQR\
             ?nif=89890001K&numserie=12345678%26G33&fecha=01-01-2024&importe=241.40"
        );
    }
}
