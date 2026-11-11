//! Tests for reading client certificates. The certificates are generated
//! here with the subject attributes Spanish certificates carry.

use rcgen::{CertificateParams, DnType, KeyPair};
use vericounts_server::certificate::inspect;

/// Self-signed certificate and key in one PEM file, as the server expects.
fn identity(attributes: &[(DnType, &str)]) -> String {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name = rcgen::DistinguishedName::new();
    for (kind, value) in attributes {
        params.distinguished_name.push(kind.clone(), *value);
    }
    let key = KeyPair::generate().unwrap();
    let certificate = params.self_signed(&key).unwrap();
    format!("{}{}", key.serialize_pem(), certificate.pem())
}

fn serial_number() -> DnType {
    DnType::CustomDnType(vec![2, 5, 4, 5])
}

fn organization_identifier() -> DnType {
    DnType::CustomDnType(vec![2, 5, 4, 97])
}

#[test]
fn company_certificate_carries_the_nif_of_the_company() {
    let pem = identity(&[
        (DnType::CommonName, "12345678Z JANE ROE (R: B12345674)"),
        (serial_number(), "IDCES-12345678Z"),
        (organization_identifier(), "VATES-B12345674"),
        (DnType::OrganizationName, "EXAMPLE SL"),
    ]);
    let info = inspect(pem.as_bytes()).unwrap();
    assert_eq!(info.subject, "12345678Z JANE ROE (R: B12345674)");
    assert!(info.of_legal_person);
    assert_eq!(info.nif.as_deref(), Some("B12345674"));
    assert!(info.expires_at > chrono::Utc::now());
}

#[test]
fn personal_certificate_carries_the_nif_of_its_holder() {
    let pem = identity(&[
        (DnType::CommonName, "ROE JANE - 12345678Z"),
        (serial_number(), "IDCES-12345678Z"),
    ]);
    let info = inspect(pem.as_bytes()).unwrap();
    assert!(!info.of_legal_person);
    assert_eq!(info.nif.as_deref(), Some("12345678Z"));
}

#[test]
fn files_without_a_certificate_are_refused() {
    assert!(inspect(b"").is_err());
    assert!(inspect(b"not a pem file").is_err());
    let key_only = KeyPair::generate().unwrap().serialize_pem();
    assert!(inspect(key_only.as_bytes()).is_err());
    let pem = identity(&[(DnType::CommonName, "No NIF")]);
    assert_eq!(inspect(pem.as_bytes()).unwrap().nif, None);
}

/// The same file works as the TLS client identity.
#[test]
fn the_identity_file_is_accepted_by_the_client() {
    let pem = identity(&[(DnType::CommonName, "ROE JANE - 12345678Z")]);
    vericounts_server::aeat_client::AeatClient::new(
        "https://127.0.0.1:1/".to_owned(),
        Some(pem.as_bytes()),
    )
    .unwrap();
}
