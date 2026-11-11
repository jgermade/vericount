//! Validates the generated request against the official AEAT schemas
//! with `xmllint`. The test is skipped when `xmllint` is not installed,
//! except in CI, where it is required.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Stdio},
};

use chrono::NaiveDate;
use vericount_core::{
    aeat::{self, Party, PreviousRejection, Record, Registration, SoftwareSystem},
    invoice::TaxBreakdown,
};

fn schemas() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../aeat/schemas")
}

fn xmllint_available() -> bool {
    let available = Command::new("xmllint")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    assert!(
        available || env::var_os("CI").is_none(),
        "xmllint is required in CI"
    );
    available
}

/// Returns the output of `xmllint` when the document is not valid.
fn validate(name: &str, xml: &str) -> Result<(), String> {
    let file = env::temp_dir().join(format!("vericount-{}-{name}.xml", std::process::id()));
    fs::write(&file, xml).unwrap();
    let output = Command::new("xmllint")
        .env("XML_CATALOG_FILES", schemas().join("catalog.xml"))
        .args(["--nonet", "--noout", "--schema"])
        .arg(schemas().join("SuministroLR.xsd"))
        .arg(&file)
        .output()
        .unwrap();
    fs::remove_file(&file).unwrap();
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

fn party(name: &str, nif: &str) -> Party {
    Party {
        name: name.to_owned(),
        nif: nif.to_owned(),
    }
}

fn registration() -> Registration {
    Registration {
        issuer: party("Example SL", "B12345674"),
        series_number: "A2026-0001".to_owned(),
        issue_date: NaiveDate::from_ymd_opt(2026, 11, 11).unwrap(),
        invoice_type: "F1".to_owned(),
        description: "Consulting & <services>".to_owned(),
        recipient: party("Customer SA", "A58818501"),
        correction: None,
        taxes: vec![TaxBreakdown {
            vat_rate: "21".parse().unwrap(),
            base_amount: "100".parse().unwrap(),
            tax_amount: "21".parse().unwrap(),
        }],
        total_tax: "21".parse().unwrap(),
        total_amount: "121".parse().unwrap(),
        previous: None,
        generated_at: "2026-11-11T12:00:00+01:00".to_owned(),
        hash: "A".repeat(64),
    }
}

fn system() -> SoftwareSystem {
    SoftwareSystem {
        producer: party("Example Software SL", "B12345674"),
        name: "vericount".to_owned(),
        id: "VC".to_owned(),
        version: "0.1.0".to_owned(),
        installation_number: "1".to_owned(),
    }
}

#[test]
fn request_is_valid_against_the_official_schema() {
    if !xmllint_available() {
        eprintln!("skipped: xmllint is not installed");
        return;
    }
    let first = registration();
    let cancellation = aeat::Cancellation {
        issuer_nif: first.issuer.nif.clone(),
        series_number: first.series_number.clone(),
        issue_date: first.issue_date,
        previous_rejection: true,
        previous: Some(aeat::PreviousRecord {
            issuer_nif: first.issuer.nif.clone(),
            series_number: first.series_number.clone(),
            issue_date: first.issue_date,
            hash: first.hash.clone(),
        }),
        generated_at: "2026-11-11T12:05:00+01:00".to_owned(),
        hash: "B".repeat(64),
    };
    let issuer = first.issuer.clone();
    let corrections = [
        PreviousRejection::No,
        PreviousRejection::Yes,
        PreviousRejection::NotRegistered,
    ]
    .map(|previous_rejection| {
        Record::Registration(Registration {
            correction: Some(previous_rejection),
            ..registration()
        })
    });
    let mut records = vec![
        Record::Registration(first),
        Record::Cancellation(cancellation),
    ];
    records.extend(corrections);
    let xml = aeat::submission_request(&issuer, None, &system(), &records);
    validate("valid", &xml).unwrap();
    assert!(
        xml.contains("<sf:Subsanacion>S</sf:Subsanacion><sf:RechazoPrevio>X</sf:RechazoPrevio>")
    );

    // Submitted by a representative, e.g. under a power of attorney.
    let representative = party("Jane Roe", "12345678Z");
    let xml = aeat::submission_request(&issuer, Some(&representative), &system(), &records[..1]);
    validate("representative", &xml).unwrap();
    assert!(xml.contains("<sf:Representante>"));

    // The check is real: a NIF that is not 9 characters long is refused.
    let mut invalid = registration();
    invalid.recipient.nif = "A1".to_owned();
    let xml = aeat::submission_request(&issuer, None, &system(), &[Record::Registration(invalid)]);
    let error = validate("invalid", &xml).unwrap_err();
    assert!(error.contains("NIF"), "{error}");
}
