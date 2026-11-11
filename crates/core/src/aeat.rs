//! Veri*factu web service messages: the record submission request
//! (`RegFactuSistemaFacturacion`) and its response.
//!
//! Element names are the ones defined by the AEAT schemas in
//! `aeat/schemas/`.

use std::fmt;

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::{
    hash::{format_amount, format_date},
    invoice::TaxBreakdown,
    xml::{self, Element, Writer},
};

const SCHEMA_BASE: &str =
    "https://www2.agenciatributaria.gob.es/static_files/common/internet/dep/aplicaciones/es/aeat/tike/cont/ws";

/// Maximum number of records in one request.
pub const MAX_RECORDS_PER_REQUEST: usize = 1000;

/// A party identified by a Spanish NIF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Party {
    pub name: String,
    pub nif: String,
}

/// The invoicing software that generates the records
/// (`SistemaInformatico`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoftwareSystem {
    /// Producer of the software.
    pub producer: Party,
    /// `NombreSistemaInformatico`, at most 30 characters.
    pub name: String,
    /// `IdSistemaInformatico`, at most 2 characters.
    pub id: String,
    pub version: String,
    /// `NumeroInstalacion`: identifies this installation.
    pub installation_number: String,
}

/// Identification of the previous record of the chain
/// (`RegistroAnterior`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviousRecord {
    pub issuer_nif: String,
    pub series_number: String,
    pub issue_date: NaiveDate,
    pub hash: String,
}

/// A registration record (`RegistroAlta`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub issuer: Party,
    pub series_number: String,
    pub issue_date: NaiveDate,
    pub invoice_type: String,
    pub description: String,
    pub recipient: Party,
    pub taxes: Vec<TaxBreakdown>,
    pub total_tax: Decimal,
    pub total_amount: Decimal,
    pub previous: Option<PreviousRecord>,
    pub generated_at: String,
    pub hash: String,
}

/// A cancellation record (`RegistroAnulacion`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cancellation {
    pub issuer_nif: String,
    pub series_number: String,
    pub issue_date: NaiveDate,
    pub previous: Option<PreviousRecord>,
    pub generated_at: String,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Registration(Registration),
    Cancellation(Cancellation),
}

fn write_chain_and_system(
    writer: &mut Writer,
    previous: &Option<PreviousRecord>,
    system: &SoftwareSystem,
    generated_at: &str,
    hash: &str,
) {
    writer.open("sf:Encadenamiento");
    match previous {
        None => {
            writer.leaf("sf:PrimerRegistro", "S");
        }
        Some(previous) => {
            writer
                .open("sf:RegistroAnterior")
                .leaf("sf:IDEmisorFactura", &previous.issuer_nif)
                .leaf("sf:NumSerieFactura", &previous.series_number)
                .leaf(
                    "sf:FechaExpedicionFactura",
                    &format_date(previous.issue_date),
                )
                .leaf("sf:Huella", &previous.hash)
                .close();
        }
    }
    writer
        .close()
        .open("sf:SistemaInformatico")
        .leaf("sf:NombreRazon", &system.producer.name)
        .leaf("sf:NIF", &system.producer.nif)
        .leaf("sf:NombreSistemaInformatico", &system.name)
        .leaf("sf:IdSistemaInformatico", &system.id)
        .leaf("sf:Version", &system.version)
        .leaf("sf:NumeroInstalacion", &system.installation_number)
        // The system only works in Veri*factu mode, for a single taxpayer.
        .leaf("sf:TipoUsoPosibleSoloVerifactu", "S")
        .leaf("sf:TipoUsoPosibleMultiOT", "N")
        .leaf("sf:IndicadorMultiplesOT", "N")
        .close()
        .leaf("sf:FechaHoraHusoGenRegistro", generated_at)
        // 01: SHA-256.
        .leaf("sf:TipoHuella", "01")
        .leaf("sf:Huella", hash);
}

fn write_registration(writer: &mut Writer, record: &Registration, system: &SoftwareSystem) {
    writer
        .open("sf:RegistroAlta")
        .leaf("sf:IDVersion", "1.0")
        .open("sf:IDFactura")
        .leaf("sf:IDEmisorFactura", &record.issuer.nif)
        .leaf("sf:NumSerieFactura", &record.series_number)
        .leaf("sf:FechaExpedicionFactura", &format_date(record.issue_date))
        .close()
        .leaf("sf:NombreRazonEmisor", &record.issuer.name)
        .leaf("sf:TipoFactura", &record.invoice_type)
        .leaf("sf:DescripcionOperacion", &record.description)
        .open("sf:Destinatarios")
        .open("sf:IDDestinatario")
        .leaf("sf:NombreRazon", &record.recipient.name)
        .leaf("sf:NIF", &record.recipient.nif)
        .close()
        .close()
        .open("sf:Desglose");
    for tax in &record.taxes {
        writer
            .open("sf:DetalleDesglose")
            // 01: VAT. 01: general regime. S1: subject, not exempt.
            .leaf("sf:Impuesto", "01")
            .leaf("sf:ClaveRegimen", "01")
            .leaf("sf:CalificacionOperacion", "S1")
            .leaf("sf:TipoImpositivo", &format_amount(tax.vat_rate))
            .leaf(
                "sf:BaseImponibleOimporteNoSujeto",
                &format_amount(tax.base_amount),
            )
            .leaf("sf:CuotaRepercutida", &format_amount(tax.tax_amount))
            .close();
    }
    writer
        .close()
        .leaf("sf:CuotaTotal", &format_amount(record.total_tax))
        .leaf("sf:ImporteTotal", &format_amount(record.total_amount));
    write_chain_and_system(
        writer,
        &record.previous,
        system,
        &record.generated_at,
        &record.hash,
    );
    writer.close();
}

fn write_cancellation(writer: &mut Writer, record: &Cancellation, system: &SoftwareSystem) {
    writer
        .open("sf:RegistroAnulacion")
        .leaf("sf:IDVersion", "1.0")
        .open("sf:IDFactura")
        .leaf("sf:IDEmisorFacturaAnulada", &record.issuer_nif)
        .leaf("sf:NumSerieFacturaAnulada", &record.series_number)
        .leaf(
            "sf:FechaExpedicionFacturaAnulada",
            &format_date(record.issue_date),
        )
        .close();
    write_chain_and_system(
        writer,
        &record.previous,
        system,
        &record.generated_at,
        &record.hash,
    );
    writer.close();
}

/// Builds the `RegFactuSistemaFacturacion` element for a batch of records
/// of one issuer, in chain order.
///
/// # Panics
///
/// If `records` is empty or has more than [`MAX_RECORDS_PER_REQUEST`]
/// records.
pub fn submission_request(issuer: &Party, system: &SoftwareSystem, records: &[Record]) -> String {
    assert!(
        (1..=MAX_RECORDS_PER_REQUEST).contains(&records.len()),
        "a request carries 1 to {MAX_RECORDS_PER_REQUEST} records"
    );
    let mut writer = Writer::new();
    writer
        .open_with(
            "sfLR:RegFactuSistemaFacturacion",
            &format!(
                " xmlns:sfLR=\"{SCHEMA_BASE}/SuministroLR.xsd\" \
                 xmlns:sf=\"{SCHEMA_BASE}/SuministroInformacion.xsd\""
            ),
        )
        .open("sfLR:Cabecera")
        .open("sf:ObligadoEmision")
        .leaf("sf:NombreRazon", &issuer.name)
        .leaf("sf:NIF", &issuer.nif)
        .close()
        .close();
    for record in records {
        writer.open("sfLR:RegistroFactura");
        match record {
            Record::Registration(record) => write_registration(&mut writer, record, system),
            Record::Cancellation(record) => write_cancellation(&mut writer, record, system),
        }
        writer.close();
    }
    writer.close();
    writer.finish()
}

/// Wraps a message in a SOAP 1.1 envelope.
pub fn soap_envelope(body: &str) -> String {
    let mut writer = Writer::new();
    writer
        .raw("<?xml version=\"1.0\" encoding=\"UTF-8\"?>")
        .open_with(
            "soapenv:Envelope",
            " xmlns:soapenv=\"http://schemas.xmlsoap.org/soap/envelope/\"",
        )
        .raw("<soapenv:Header/>")
        .open("soapenv:Body")
        .raw(body)
        .close()
        .close();
    writer.finish()
}

/// `EstadoEnvio`: result of a whole request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionStatus {
    /// `Correcto`: every record was accepted.
    Accepted,
    /// `ParcialmenteCorrecto`: some records were rejected or accepted
    /// with errors.
    PartiallyAccepted,
    /// `Incorrecto`: every record was rejected.
    Rejected,
}

/// `EstadoRegistro`: result of one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordStatus {
    /// `Correcto`.
    Accepted,
    /// `AceptadoConErrores`: registered, but it has to be corrected.
    AcceptedWithErrors,
    /// `Incorrecto`: not registered.
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// `Alta`.
    Registration,
    /// `Anulacion`.
    Cancellation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordResult {
    pub series_number: String,
    pub operation: Operation,
    pub status: RecordStatus,
    pub error_code: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionResponse {
    /// Secure verification code of the submission, when any record was
    /// registered.
    pub csv: Option<String>,
    pub status: SubmissionStatus,
    /// `TiempoEsperaEnvio`: seconds to wait before the next request.
    pub wait_seconds: u32,
    pub records: Vec<RecordResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseError {
    /// The service answered with a SOAP fault: the whole request was
    /// refused, for instance because it does not follow the schema.
    Fault { code: String, message: String },
    /// The response could not be understood.
    Malformed(String),
}

impl fmt::Display for ResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fault { code, message } => write!(f, "SOAP fault {code}: {message}"),
            Self::Malformed(reason) => write!(f, "malformed response: {reason}"),
        }
    }
}

impl std::error::Error for ResponseError {}

fn required<'a>(element: &'a Element, path: &[&str]) -> Result<&'a str, ResponseError> {
    element
        .text_at(path)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| ResponseError::Malformed(format!("missing {}", path.join("/"))))
}

fn unknown(what: &str, value: &str) -> ResponseError {
    ResponseError::Malformed(format!("unknown {what} {value:?}"))
}

/// Parses the SOAP response of a record submission.
pub fn parse_submission_response(soap: &str) -> Result<SubmissionResponse, ResponseError> {
    let envelope = xml::parse(soap).map_err(|e| ResponseError::Malformed(e.to_string()))?;
    let body = envelope
        .child("Body")
        .ok_or_else(|| ResponseError::Malformed("missing SOAP body".to_owned()))?;
    if let Some(fault) = body.child("Fault") {
        return Err(ResponseError::Fault {
            code: fault.text_at(&["faultcode"]).unwrap_or_default().to_owned(),
            message: fault
                .text_at(&["faultstring"])
                .unwrap_or_default()
                .to_owned(),
        });
    }
    let response = body
        .child("RespuestaRegFactuSistemaFacturacion")
        .ok_or_else(|| ResponseError::Malformed("missing response element".to_owned()))?;

    let status = match required(response, &["EstadoEnvio"])? {
        "Correcto" => SubmissionStatus::Accepted,
        "ParcialmenteCorrecto" => SubmissionStatus::PartiallyAccepted,
        "Incorrecto" => SubmissionStatus::Rejected,
        other => return Err(unknown("EstadoEnvio", other)),
    };
    let wait_seconds = required(response, &["TiempoEsperaEnvio"])?
        .parse()
        .map_err(|_| ResponseError::Malformed("invalid TiempoEsperaEnvio".to_owned()))?;

    let mut records = Vec::new();
    for line in response.children_named("RespuestaLinea") {
        records.push(RecordResult {
            series_number: required(line, &["IDFactura", "NumSerieFactura"])?.to_owned(),
            operation: match required(line, &["Operacion", "TipoOperacion"])? {
                "Alta" => Operation::Registration,
                "Anulacion" => Operation::Cancellation,
                other => return Err(unknown("TipoOperacion", other)),
            },
            status: match required(line, &["EstadoRegistro"])? {
                "Correcto" => RecordStatus::Accepted,
                "AceptadoConErrores" => RecordStatus::AcceptedWithErrors,
                "Incorrecto" => RecordStatus::Rejected,
                other => return Err(unknown("EstadoRegistro", other)),
            },
            error_code: line.text_at(&["CodigoErrorRegistro"]).map(str::to_owned),
            error_description: line
                .text_at(&["DescripcionErrorRegistro"])
                .map(str::to_owned),
        });
    }

    Ok(SubmissionResponse {
        csv: response
            .text_at(&["CSV"])
            .filter(|csv| !csv.is_empty())
            .map(str::to_owned),
        status,
        wait_seconds,
        records,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::hash::{self, CancellationFields, RegistrationFields};

    pub fn system() -> SoftwareSystem {
        SoftwareSystem {
            producer: Party {
                name: "Example Software SL".to_owned(),
                nif: "B12345674".to_owned(),
            },
            name: "vericounts".to_owned(),
            id: "VC".to_owned(),
            version: "0.1.0".to_owned(),
            installation_number: "1".to_owned(),
        }
    }

    pub fn issuer() -> Party {
        Party {
            name: "Tom & Jerry <SL>".to_owned(),
            nif: "B12345674".to_owned(),
        }
    }

    pub fn records() -> Vec<Record> {
        let issue_date = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let taxes = vec![
            TaxBreakdown {
                vat_rate: "21".parse().unwrap(),
                base_amount: "500.00".parse().unwrap(),
                tax_amount: "105.00".parse().unwrap(),
            },
            TaxBreakdown {
                vat_rate: "4".parse().unwrap(),
                base_amount: "24.99".parse().unwrap(),
                tax_amount: "1.00".parse().unwrap(),
            },
        ];
        let first_generated_at = "2026-11-11T12:00:00+01:00";
        let first_hash = hash::registration_hash(&RegistrationFields {
            issuer_nif: "B12345674",
            series_number: "A2026-0001",
            issue_date,
            invoice_type: "F1",
            total_tax: "106.00".parse().unwrap(),
            total_amount: "630.99".parse().unwrap(),
            previous_hash: "",
            generated_at: first_generated_at,
        });
        let second_generated_at = "2026-11-11T12:05:00+01:00";
        let second_hash = hash::cancellation_hash(&CancellationFields {
            issuer_nif: "B12345674",
            series_number: "A2026-0001",
            issue_date,
            previous_hash: &first_hash,
            generated_at: second_generated_at,
        });
        vec![
            Record::Registration(Registration {
                issuer: issuer(),
                series_number: "A2026-0001".to_owned(),
                issue_date,
                invoice_type: "F1".to_owned(),
                description: "Consulting \"services\"".to_owned(),
                recipient: Party {
                    name: "Customer SA".to_owned(),
                    nif: "A58818501".to_owned(),
                },
                taxes,
                total_tax: "106.00".parse().unwrap(),
                total_amount: "630.99".parse().unwrap(),
                previous: None,
                generated_at: first_generated_at.to_owned(),
                hash: first_hash.clone(),
            }),
            Record::Cancellation(Cancellation {
                issuer_nif: "B12345674".to_owned(),
                series_number: "A2026-0001".to_owned(),
                issue_date,
                previous: Some(PreviousRecord {
                    issuer_nif: "B12345674".to_owned(),
                    series_number: "A2026-0001".to_owned(),
                    issue_date,
                    hash: first_hash,
                }),
                generated_at: second_generated_at.to_owned(),
                hash: second_hash,
            }),
        ]
    }

    #[test]
    fn request_has_the_expected_content() {
        let xml = submission_request(&issuer(), &system(), &records());
        let root = xml::parse(&xml).unwrap();
        assert_eq!(root.name, "RegFactuSistemaFacturacion");
        assert_eq!(
            root.text_at(&["Cabecera", "ObligadoEmision", "NombreRazon"]),
            Some("Tom & Jerry <SL>")
        );
        let registros: Vec<_> = root.children_named("RegistroFactura").collect();
        assert_eq!(registros.len(), 2);

        let alta = registros[0].child("RegistroAlta").unwrap();
        assert_eq!(
            alta.text_at(&["IDFactura", "FechaExpedicionFactura"]),
            Some("11-11-2026")
        );
        assert_eq!(alta.text_at(&["CuotaTotal"]), Some("106.00"));
        assert_eq!(
            alta.text_at(&["Encadenamiento", "PrimerRegistro"]),
            Some("S")
        );
        let detalles: Vec<_> = alta
            .child("Desglose")
            .unwrap()
            .children_named("DetalleDesglose")
            .collect();
        assert_eq!(detalles[1].text_at(&["TipoImpositivo"]), Some("4.00"));
        assert_eq!(detalles[1].text_at(&["CuotaRepercutida"]), Some("1.00"));

        let anulacion = registros[1].child("RegistroAnulacion").unwrap();
        assert_eq!(
            anulacion.text_at(&["Encadenamiento", "RegistroAnterior", "Huella"]),
            alta.text_at(&["Huella"])
        );
    }

    fn response(body: &str) -> String {
        soap_envelope(&format!(
            "<r:RespuestaRegFactuSistemaFacturacion xmlns:r=\"urn:r\" xmlns:sf=\"urn:sf\">{body}\
             </r:RespuestaRegFactuSistemaFacturacion>"
        ))
    }

    #[test]
    fn parses_a_partially_accepted_response() {
        let parsed = parse_submission_response(&response(
            "<r:CSV>A-ABCDEFGH12345678</r:CSV>\
             <r:TiempoEsperaEnvio>60</r:TiempoEsperaEnvio>\
             <r:EstadoEnvio>ParcialmenteCorrecto</r:EstadoEnvio>\
             <r:RespuestaLinea><r:IDFactura><sf:NumSerieFactura>A2026-0001</sf:NumSerieFactura>\
             </r:IDFactura><r:Operacion><sf:TipoOperacion>Alta</sf:TipoOperacion></r:Operacion>\
             <r:EstadoRegistro>Correcto</r:EstadoRegistro></r:RespuestaLinea>\
             <r:RespuestaLinea><r:IDFactura><sf:NumSerieFactura>A2026-0001</sf:NumSerieFactura>\
             </r:IDFactura><r:Operacion><sf:TipoOperacion>Anulacion</sf:TipoOperacion></r:Operacion>\
             <r:EstadoRegistro>Incorrecto</r:EstadoRegistro>\
             <r:CodigoErrorRegistro>3000</r:CodigoErrorRegistro>\
             <r:DescripcionErrorRegistro>Duplicated &amp; refused</r:DescripcionErrorRegistro>\
             </r:RespuestaLinea>",
        ))
        .unwrap();
        assert_eq!(parsed.csv.as_deref(), Some("A-ABCDEFGH12345678"));
        assert_eq!(parsed.status, SubmissionStatus::PartiallyAccepted);
        assert_eq!(parsed.wait_seconds, 60);
        assert_eq!(parsed.records[0].status, RecordStatus::Accepted);
        assert_eq!(parsed.records[0].operation, Operation::Registration);
        assert_eq!(parsed.records[1].status, RecordStatus::Rejected);
        assert_eq!(parsed.records[1].error_code.as_deref(), Some("3000"));
        assert_eq!(
            parsed.records[1].error_description.as_deref(),
            Some("Duplicated & refused")
        );
    }

    #[test]
    fn faults_and_malformed_responses_are_errors() {
        let fault = soap_envelope(
            "<soapenv:Fault><faultcode>soapenv:Client</faultcode>\
             <faultstring>Codigo[4102].Schema error</faultstring></soapenv:Fault>",
        );
        assert_eq!(
            parse_submission_response(&fault),
            Err(ResponseError::Fault {
                code: "soapenv:Client".to_owned(),
                message: "Codigo[4102].Schema error".to_owned(),
            })
        );
        for bad in [
            "not xml".to_owned(),
            soap_envelope("<other/>"),
            response("<r:EstadoEnvio>Correcto</r:EstadoEnvio>"),
            response(
                "<r:TiempoEsperaEnvio>60</r:TiempoEsperaEnvio><r:EstadoEnvio>Maybe</r:EstadoEnvio>",
            ),
        ] {
            assert!(
                matches!(
                    parse_submission_response(&bad),
                    Err(ResponseError::Malformed(_))
                ),
                "{bad}"
            );
        }
    }
}
