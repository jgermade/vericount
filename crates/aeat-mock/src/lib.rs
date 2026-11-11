//! Mock of the AEAT Veri*factu record submission service, for tests and
//! local development. It must never be used in production.
//!
//! What it checks, per request:
//!
//! - the message against the official schemas, when `xmllint` validation
//!   is enabled;
//! - the hash of every record, recomputed from its fields;
//! - the chain: every record must follow the last one accepted for its
//!   issuer;
//! - duplicated registrations and cancellations of unknown invoices.
//!
//! It keeps its state in memory and supports fault injection. Its error
//! codes and descriptions are illustrative: they have not been compared
//! with real responses of the AEAT pre-production service.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
};

use axum::{
    extract::State,
    http::{header::CONTENT_TYPE, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use chrono::NaiveDate;
use tokio::{io::AsyncWriteExt, process::Command};
use vericounts_core::{
    aeat::soap_envelope,
    hash::{self, CancellationFields, RegistrationFields},
    xml::{self, Element, Writer},
};

/// Path of the service, the same as in the real endpoints.
pub const SERVICE_PATH: &str = "/wlpl/TIKE-CONT/ws/SistemaFacturacion/VerifactuSOAP";

const RESPONSE_NAMESPACE: &str = "https://www2.agenciatributaria.gob.es/static_files/common/internet/dep/aplicaciones/es/aeat/tike/cont/ws/RespuestaSuministro.xsd";
const INFORMATION_NAMESPACE: &str = "https://www2.agenciatributaria.gob.es/static_files/common/internet/dep/aplicaciones/es/aeat/tike/cont/ws/SuministroInformacion.xsd";

/// Illustrative error codes; see the crate documentation.
pub mod codes {
    pub const SCHEMA: u32 = 4102;
    pub const DUPLICATED: u32 = 3000;
    pub const WRONG_HASH: u32 = 2000;
    pub const BROKEN_CHAIN: u32 = 2001;
    pub const UNKNOWN_INVOICE: u32 = 3002;
}

/// A failure to answer the next request with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Answer with this HTTP status and no SOAP body.
    HttpStatus(u16),
    /// Answer with a SOAP fault.
    SoapFault,
    /// Answer with a body that is not XML.
    Garbage,
}

#[derive(Debug, Default)]
struct Inner {
    /// Hash of the last accepted record, per issuer NIF.
    last_hash: HashMap<String, String>,
    /// Registered invoices: (issuer NIF, series and number).
    registered: HashSet<(String, String)>,
    cancelled: HashSet<(String, String)>,
    faults: VecDeque<Fault>,
    requests: Vec<String>,
}

/// State of the mock. Cloning shares the state.
#[derive(Debug, Clone, Default)]
pub struct Mock {
    inner: Arc<Mutex<Inner>>,
    schemas: Option<PathBuf>,
}

impl Mock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates every request with `xmllint` against the schemas in this
    /// directory (`aeat/schemas` in the repository).
    pub fn with_schema_validation(mut self, schemas: impl Into<PathBuf>) -> Self {
        self.schemas = Some(schemas.into());
        self
    }

    /// Makes the next request fail. Faults are consumed in order.
    pub fn inject(&self, fault: Fault) {
        self.inner.lock().unwrap().faults.push_back(fault);
    }

    /// Bodies of the requests received so far, including failed ones.
    pub fn requests(&self) -> Vec<String> {
        self.inner.lock().unwrap().requests.clone()
    }

    /// Number of records accepted for an issuer's registrations.
    pub fn registered_count(&self) -> usize {
        self.inner.lock().unwrap().registered.len()
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route(SERVICE_PATH, post(handle))
            .with_state(self.clone())
    }
}

fn xml_response(status: StatusCode, body: String) -> Response {
    (status, [(CONTENT_TYPE, "text/xml; charset=UTF-8")], body).into_response()
}

fn soap_fault(code: u32, message: &str) -> Response {
    let mut writer = Writer::new();
    writer
        .open("soapenv:Fault")
        .leaf("faultcode", "soapenv:Client")
        .leaf("faultstring", &format!("Codigo[{code}].{message}"))
        .close();
    // SOAP 1.1 faults travel with status 500.
    xml_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        soap_envelope(&writer.finish()),
    )
}

async fn handle(State(mock): State<Mock>, body: String) -> Response {
    let fault = {
        let mut inner = mock.inner.lock().unwrap();
        inner.requests.push(body.clone());
        inner.faults.pop_front()
    };
    match fault {
        Some(Fault::HttpStatus(status)) => {
            return StatusCode::from_u16(status)
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
                .into_response()
        }
        Some(Fault::SoapFault) => return soap_fault(codes::SCHEMA, "Injected fault"),
        Some(Fault::Garbage) => return (StatusCode::OK, "<<not xml").into_response(),
        None => {}
    }

    let Some(request) = xml::parse(&body).ok().and_then(|envelope| {
        envelope
            .at(&["Body", "RegFactuSistemaFacturacion"])
            .cloned()
    }) else {
        return soap_fault(codes::SCHEMA, "The message is not a valid request");
    };
    if let Some(schemas) = &mock.schemas {
        if let Err(error) = validate(schemas, &body).await {
            return soap_fault(codes::SCHEMA, &error);
        }
    }
    match respond(&mock, &request) {
        Some(response) => xml_response(StatusCode::OK, soap_envelope(&response)),
        None => soap_fault(codes::SCHEMA, "The request lacks required elements"),
    }
}

/// Validates the payload of the SOAP body with `xmllint`.
async fn validate(schemas: &Path, soap: &str) -> Result<(), String> {
    // The payload is the content of the SOAP body, taken textually so that
    // the validator sees exactly what was sent.
    let start = soap.find("Body>").map(|i| i + "Body>".len());
    let end = soap.rfind("</").and_then(|i| soap[..i].rfind("</"));
    let (Some(start), Some(end)) = (start, end) else {
        return Err("no SOAP body".to_owned());
    };
    let payload = soap.get(start..end).ok_or("no SOAP body")?;

    let mut child = Command::new("xmllint")
        .env("XML_CATALOG_FILES", schemas.join("catalog.xml"))
        .args(["--nonet", "--noout", "--schema"])
        .arg(schemas.join("SuministroLR.xsd"))
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run xmllint: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    stdin
        .write_all(payload.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    drop(stdin);
    let output = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(stderr.lines().next().unwrap_or("schema error").to_owned())
    }
}

struct Line {
    issuer_nif: String,
    series_number: String,
    issue_date: String,
    cancellation: bool,
    error: Option<(u32, &'static str)>,
}

fn parse_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%d-%m-%Y").ok()
}

/// Checks one record against the state and, if it is accepted, updates
/// the state. Returns `None` when required elements are missing.
fn process(inner: &mut Inner, record: &Element) -> Option<Line> {
    let (body, cancellation) = match record.child("RegistroAlta") {
        Some(body) => (body, false),
        None => (record.child("RegistroAnulacion")?, true),
    };
    let id = body.child("IDFactura")?;
    let field = |plain: &str| {
        let name = if cancellation {
            format!("{plain}Anulada")
        } else {
            plain.to_owned()
        };
        id.text_at(&[&name]).map(str::to_owned)
    };
    let issuer_nif = field("IDEmisorFactura")?;
    let series_number = field("NumSerieFactura")?;
    let issue_date_text = field("FechaExpedicionFactura")?;
    let issue_date = parse_date(&issue_date_text)?;
    let generated_at = body.text_at(&["FechaHoraHusoGenRegistro"])?;
    let record_hash = body.text_at(&["Huella"])?;
    let chain = body.child("Encadenamiento")?;
    let previous_hash = match chain.child("RegistroAnterior") {
        Some(previous) => previous.text_at(&["Huella"])?,
        None => {
            chain.child("PrimerRegistro")?;
            ""
        }
    };

    let expected_hash = if cancellation {
        hash::cancellation_hash(&CancellationFields {
            issuer_nif: &issuer_nif,
            series_number: &series_number,
            issue_date,
            previous_hash,
            generated_at,
        })
    } else {
        hash::registration_hash(&RegistrationFields {
            issuer_nif: &issuer_nif,
            series_number: &series_number,
            issue_date,
            invoice_type: body.text_at(&["TipoFactura"])?,
            total_tax: body.text_at(&["CuotaTotal"])?.parse().ok()?,
            total_amount: body.text_at(&["ImporteTotal"])?.parse().ok()?,
            previous_hash,
            generated_at,
        })
    };

    let key = (issuer_nif.clone(), series_number.clone());
    let last_hash = inner.last_hash.get(&issuer_nif).map_or("", String::as_str);
    let error = if record_hash != expected_hash {
        Some((codes::WRONG_HASH, "The hash does not match the record"))
    } else if previous_hash != last_hash {
        Some((
            codes::BROKEN_CHAIN,
            "The record does not follow the last accepted record",
        ))
    } else if !cancellation && inner.registered.contains(&key) {
        Some((codes::DUPLICATED, "Duplicated record"))
    } else if cancellation && !inner.registered.contains(&key) {
        Some((codes::UNKNOWN_INVOICE, "The invoice is not registered"))
    } else if cancellation && inner.cancelled.contains(&key) {
        Some((codes::DUPLICATED, "The invoice is already cancelled"))
    } else {
        None
    };

    if error.is_none() {
        inner
            .last_hash
            .insert(issuer_nif.clone(), record_hash.to_owned());
        if cancellation {
            inner.cancelled.insert(key);
        } else {
            inner.registered.insert(key);
        }
    }
    Some(Line {
        issuer_nif,
        series_number,
        issue_date: issue_date_text,
        cancellation,
        error,
    })
}

/// Builds the `RespuestaRegFactuSistemaFacturacion` element.
fn respond(mock: &Mock, request: &Element) -> Option<String> {
    let issuer = request.at(&["Cabecera", "ObligadoEmision"])?;
    let issuer_name = issuer.text_at(&["NombreRazon"])?;
    let issuer_nif = issuer.text_at(&["NIF"])?;

    let mut inner = mock.inner.lock().unwrap();
    let mut lines = Vec::new();
    for record in request.children_named("RegistroFactura") {
        lines.push(process(&mut inner, record)?);
    }
    if lines.is_empty() {
        return None;
    }
    let accepted = lines.iter().filter(|line| line.error.is_none()).count();
    let status = match accepted {
        0 => "Incorrecto",
        n if n == lines.len() => "Correcto",
        _ => "ParcialmenteCorrecto",
    };

    let mut writer = Writer::new();
    writer.open_with(
        "sfR:RespuestaRegFactuSistemaFacturacion",
        &format!(" xmlns:sfR=\"{RESPONSE_NAMESPACE}\" xmlns:sf=\"{INFORMATION_NAMESPACE}\""),
    );
    if accepted > 0 {
        writer.leaf("sfR:CSV", &format!("A-MOCK{:012}", inner.requests.len()));
    }
    writer
        .open("sfR:Cabecera")
        .open("sf:ObligadoEmision")
        .leaf("sf:NombreRazon", issuer_name)
        .leaf("sf:NIF", issuer_nif)
        .close()
        .close()
        .leaf("sfR:TiempoEsperaEnvio", "60")
        .leaf("sfR:EstadoEnvio", status);
    for line in &lines {
        writer
            .open("sfR:RespuestaLinea")
            .open("sfR:IDFactura")
            .leaf("sf:IDEmisorFactura", &line.issuer_nif)
            .leaf("sf:NumSerieFactura", &line.series_number)
            .leaf("sf:FechaExpedicionFactura", &line.issue_date)
            .close()
            .open("sfR:Operacion")
            .leaf(
                "sf:TipoOperacion",
                if line.cancellation {
                    "Anulacion"
                } else {
                    "Alta"
                },
            )
            .close();
        match line.error {
            None => {
                writer.leaf("sfR:EstadoRegistro", "Correcto");
            }
            Some((code, description)) => {
                writer
                    .leaf("sfR:EstadoRegistro", "Incorrecto")
                    .leaf("sfR:CodigoErrorRegistro", &code.to_string())
                    .leaf("sfR:DescripcionErrorRegistro", description);
            }
        }
        writer.close();
    }
    writer.close();
    Some(writer.finish())
}
