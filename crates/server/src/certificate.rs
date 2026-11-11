//! Reads the client certificate used with the AEAT, to tell whose it is
//! and when it expires.
//!
//! Two kinds of certificate can send the records of a company:
//!
//! - a certificate of the company itself (a "representative of a legal
//!   person" certificate), whose subject carries the NIF of the company in
//!   `organizationIdentifier`, as `VATES-<NIF>`;
//! - a personal certificate of someone the company has empowered at the
//!   AEAT ("apoderamiento"), whose subject carries that person's NIF in
//!   `serialNumber`, as `IDCES-<NIF>`.

use chrono::{DateTime, Utc};
use serde::Serialize;
use x509_parser::{der_parser::oid, oid_registry::OID_X509_SERIALNUMBER, pem::Pem};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CertificateInfo {
    /// Common name of the holder.
    pub subject: String,
    /// NIF the certificate acts for: the company's for a company
    /// certificate, the person's for a personal one.
    pub nif: Option<String>,
    /// Whether it is a certificate of a legal person.
    pub of_legal_person: bool,
    pub expires_at: DateTime<Utc>,
}

fn strip_prefix(value: &str) -> String {
    // "IDCES-12345678Z" and "VATES-B12345674" carry the NIF after the dash.
    value
        .rsplit('-')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_uppercase()
}

/// Reads the first certificate of a PEM file.
pub fn inspect(pem: &[u8]) -> Result<CertificateInfo, String> {
    let pem = Pem::iter_from_buffer(pem)
        .filter_map(Result::ok)
        .find(|block| block.label == "CERTIFICATE")
        .ok_or("the file has no certificate")?;
    let certificate = pem.parse_x509().map_err(|e| e.to_string())?;
    let subject = certificate.subject();
    let organization_identifier_oid = oid!(2.5.4 .97);
    let organization_identifier = subject
        .iter_by_oid(&organization_identifier_oid)
        .next()
        .and_then(|value| value.as_str().ok())
        .map(strip_prefix);
    let serial_number = subject
        .iter_by_oid(&OID_X509_SERIALNUMBER)
        .next()
        .and_then(|value| value.as_str().ok())
        .map(strip_prefix);
    let common_name = subject
        .iter_common_name()
        .next()
        .and_then(|value| value.as_str().ok())
        .unwrap_or_default()
        .to_owned();
    let expires_at = DateTime::from_timestamp(certificate.validity().not_after.timestamp(), 0)
        .ok_or("the certificate has an invalid expiry date")?;
    Ok(CertificateInfo {
        subject: common_name,
        of_legal_person: organization_identifier.is_some(),
        nif: organization_identifier.or(serial_number),
        expires_at,
    })
}
