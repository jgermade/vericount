//! Runtime configuration, read from environment variables.

use std::{env, net::SocketAddr};

use vericounts_core::{invoice, qr::AeatEnvironment};

/// The company that issues the invoices, as stored in the `company` table.
#[derive(Debug, Clone)]
pub struct Issuer {
    pub nif: String,
    pub name: String,
    /// One-line address, printed on invoices.
    pub address: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub aeat_environment: AeatEnvironment,
}

/// Settings that are only needed to start the server.
#[derive(Debug, Clone)]
pub struct Startup {
    pub addr: SocketAddr,
    pub web_dir: String,
    pub database_url: String,
}

fn required(name: &str) -> Result<String, String> {
    env::var(name).map_err(|_| format!("environment variable {name} is required"))
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let aeat_environment = match env::var("VERICOUNTS_AEAT_ENV").as_deref() {
            Err(_) | Ok("test") => AeatEnvironment::Test,
            Ok("production") => AeatEnvironment::Production,
            Ok(other) => {
                return Err(format!(
                    "VERICOUNTS_AEAT_ENV must be \"test\" or \"production\", got {other:?}"
                ))
            }
        };
        Ok(Self { aeat_environment })
    }
}

impl Startup {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            addr: env::var("VERICOUNTS_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
                .parse()
                .map_err(|e| format!("VERICOUNTS_ADDR: {e}"))?,
            web_dir: env::var("VERICOUNTS_WEB_DIR").unwrap_or_else(|_| "web/dist".to_owned()),
            database_url: required("DATABASE_URL")?,
        })
    }
}

const AEAT_PRODUCTION_ENDPOINT: &str =
    "https://www1.agenciatributaria.gob.es/wlpl/TIKE-CONT/ws/SistemaFacturacion/VerifactuSOAP";
const AEAT_TEST_ENDPOINT: &str =
    "https://prewww1.aeat.es/wlpl/TIKE-CONT/ws/SistemaFacturacion/VerifactuSOAP";

/// URL of the AEAT record submission service.
///
/// In production the official endpoint is always used: an override is an
/// error, so a mock can never be reached from a production configuration.
/// In the test environment the override points to a mock; without it, the
/// AEAT pre-production service is used.
pub fn aeat_endpoint(
    environment: AeatEnvironment,
    endpoint_override: Option<&str>,
) -> Result<String, String> {
    match (environment, endpoint_override) {
        (AeatEnvironment::Production, None) => Ok(AEAT_PRODUCTION_ENDPOINT.to_owned()),
        (AeatEnvironment::Production, Some(_)) => {
            Err("the AEAT endpoint cannot be overridden in production".to_owned())
        }
        (AeatEnvironment::Test, None) => Ok(AEAT_TEST_ENDPOINT.to_owned()),
        (AeatEnvironment::Test, Some(endpoint)) => Ok(endpoint.to_owned()),
    }
}

/// Settings for sending records to the AEAT. Sending is off unless
/// `VERICOUNTS_AEAT_SUBMIT=1`.
#[derive(Debug, Clone)]
pub struct Submission {
    pub endpoint: String,
    /// Path of a PEM file with the client certificate and its key.
    pub identity_pem_path: Option<String>,
    pub producer_name: String,
    pub producer_nif: String,
    pub installation_number: String,
    /// Name and NIF of whoever submits on behalf of the company, e.g.
    /// under a power of attorney registered at the AEAT.
    pub representative: Option<(String, String)>,
}

impl Submission {
    pub fn from_env(environment: AeatEnvironment) -> Result<Option<Self>, String> {
        if env::var("VERICOUNTS_AEAT_SUBMIT").as_deref() != Ok("1") {
            return Ok(None);
        }
        Ok(Some(Self {
            endpoint: aeat_endpoint(
                environment,
                env::var("VERICOUNTS_AEAT_ENDPOINT").ok().as_deref(),
            )?,
            identity_pem_path: env::var("VERICOUNTS_AEAT_IDENTITY_PEM").ok(),
            producer_name: invoice::validate_text(
                "VERICOUNTS_SOFTWARE_PRODUCER_NAME",
                &required("VERICOUNTS_SOFTWARE_PRODUCER_NAME")?,
            )
            .map_err(|e| e.to_string())?,
            producer_nif: invoice::validate_tax_id(
                "VERICOUNTS_SOFTWARE_PRODUCER_NIF",
                &required("VERICOUNTS_SOFTWARE_PRODUCER_NIF")?,
            )
            .map_err(|e| e.to_string())?,
            installation_number: env::var("VERICOUNTS_INSTALLATION_NUMBER")
                .unwrap_or_else(|_| "1".to_owned()),
            representative: match (
                env::var("VERICOUNTS_AEAT_REPRESENTATIVE_NAME").ok(),
                env::var("VERICOUNTS_AEAT_REPRESENTATIVE_NIF").ok(),
            ) {
                (None, None) => None,
                (Some(name), Some(nif)) => Some((
                    invoice::validate_text("VERICOUNTS_AEAT_REPRESENTATIVE_NAME", &name)
                        .map_err(|e| e.to_string())?,
                    invoice::validate_tax_id("VERICOUNTS_AEAT_REPRESENTATIVE_NIF", &nif)
                        .map_err(|e| e.to_string())?,
                )),
                _ => {
                    return Err("set both VERICOUNTS_AEAT_REPRESENTATIVE_NAME and \
                         VERICOUNTS_AEAT_REPRESENTATIVE_NIF, or neither"
                        .to_owned())
                }
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_never_uses_an_overridden_endpoint() {
        assert_eq!(
            aeat_endpoint(AeatEnvironment::Production, None).unwrap(),
            AEAT_PRODUCTION_ENDPOINT
        );
        assert!(aeat_endpoint(AeatEnvironment::Production, Some("http://127.0.0.1:8090")).is_err());
        assert_eq!(
            aeat_endpoint(AeatEnvironment::Test, None).unwrap(),
            AEAT_TEST_ENDPOINT
        );
        assert_eq!(
            aeat_endpoint(AeatEnvironment::Test, Some("http://127.0.0.1:8090/x")).unwrap(),
            "http://127.0.0.1:8090/x"
        );
    }
}
