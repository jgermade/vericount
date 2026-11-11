//! Runtime configuration, read from environment variables.

use std::{env, net::SocketAddr};

use vericount_core::{aeat::Party, invoice, qr::AeatEnvironment};
use webauthn_rs::prelude::{Url, Webauthn, WebauthnBuilder};

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
    /// Where users open the web client, e.g. `https://books.example.com`.
    /// Passkeys are bound to its host name, and invitations link to it.
    pub public_url: Url,
}

pub const DEFAULT_PUBLIC_URL: &str = "http://localhost:8080";

/// Settings that are only needed to start the server.
#[derive(Debug, Clone)]
pub struct Startup {
    pub addr: SocketAddr,
    pub web_dir: String,
    pub database_url: String,
}

/// Reads an environment variable. An empty value counts as unset, as
/// `.env` files and container settings often leave unused ones empty.
pub fn var(name: &str) -> Result<String, env::VarError> {
    match env::var(name) {
        Ok(value) if value.is_empty() => Err(env::VarError::NotPresent),
        other => other,
    }
}

fn required(name: &str) -> Result<String, String> {
    var(name).map_err(|_| format!("environment variable {name} is required"))
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let aeat_environment = match var("VERICOUNT_AEAT_ENV").as_deref() {
            Err(_) | Ok("test") => AeatEnvironment::Test,
            Ok("production") => AeatEnvironment::Production,
            Ok(other) => {
                return Err(format!(
                    "VERICOUNT_AEAT_ENV must be \"test\" or \"production\", got {other:?}"
                ))
            }
        };
        let public_url =
            var("VERICOUNT_PUBLIC_URL").unwrap_or_else(|_| DEFAULT_PUBLIC_URL.to_owned());
        let public_url =
            Url::parse(&public_url).map_err(|e| format!("VERICOUNT_PUBLIC_URL: {e}"))?;
        webauthn(&public_url)?;
        Ok(Self {
            aeat_environment,
            public_url,
        })
    }
}

/// Passkey settings for the site at `public_url`.
///
/// The relying party is the host name, which must be a domain (passkeys
/// do not work on an IP address such as 127.0.0.1). On `localhost` any
/// port is accepted, so the development server can be used too.
pub fn webauthn(public_url: &Url) -> Result<Webauthn, String> {
    let Some(host) = public_url.domain() else {
        return Err("VERICOUNT_PUBLIC_URL must have a domain name, not an IP address".into());
    };
    let localhost = host == "localhost";
    if public_url.scheme() != "https" && !localhost {
        return Err("VERICOUNT_PUBLIC_URL must use https outside localhost".into());
    }
    WebauthnBuilder::new(host, public_url)
        .and_then(|builder| {
            builder
                .rp_name("vericount")
                .allow_any_port(localhost)
                .build()
        })
        .map_err(|e| format!("VERICOUNT_PUBLIC_URL: {e}"))
}

impl Startup {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            addr: var("VERICOUNT_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
                .parse()
                .map_err(|e| format!("VERICOUNT_ADDR: {e}"))?,
            web_dir: var("VERICOUNT_WEB_DIR").unwrap_or_else(|_| "web/dist".to_owned()),
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

/// Key that encrypts the stored AEAT certificate. Not printable, so it
/// cannot end up in a log.
#[derive(Clone)]
pub struct SecretKey(pub [u8; 32]);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey(..)")
    }
}

impl SecretKey {
    /// 64 hexadecimal digits, e.g. from `openssl rand -hex 32`.
    pub fn parse(hex: &str) -> Result<Self, String> {
        let hex = hex.trim();
        let error = || "VERICOUNT_SECRET_KEY must be 64 hexadecimal digits".to_owned();
        if hex.len() != 64 || !hex.is_ascii() {
            return Err(error());
        }
        let mut key = [0; 32];
        for (index, byte) in key.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).map_err(|_| error())?;
        }
        Ok(Self(key))
    }
}

/// Settings for sending records to the AEAT. Which records are sent, and
/// with which certificate, depends on the mode, kept in the database.
#[derive(Debug, Clone)]
pub struct AeatSettings {
    /// Where live records go: the AEAT pre-production service or
    /// production.
    pub environment: AeatEnvironment,
    /// Service URL for live records, only in the test environment, e.g. a
    /// mock in development.
    pub endpoint_override: Option<String>,
    /// Producer of the software, declared in every record. Required for
    /// live mode.
    pub producer: Option<Party>,
    /// Who submits on behalf of the company, when declared.
    pub representative: Option<Party>,
    pub installation_number: String,
    /// Required to upload a certificate.
    pub secret_key: Option<SecretKey>,
}

impl AeatSettings {
    /// Nothing configured, in the test environment.
    pub fn test() -> Self {
        Self {
            environment: AeatEnvironment::Test,
            endpoint_override: None,
            producer: None,
            representative: None,
            installation_number: "1".to_owned(),
            secret_key: None,
        }
    }

    pub fn from_env(environment: AeatEnvironment) -> Result<Self, String> {
        let endpoint_override = var("VERICOUNT_AEAT_ENDPOINT").ok();
        // Checked now, so a production configuration with an override does
        // not start at all.
        aeat_endpoint(environment, endpoint_override.as_deref())?;
        Ok(Self {
            environment,
            endpoint_override,
            producer: party(
                "VERICOUNT_SOFTWARE_PRODUCER_NAME",
                "VERICOUNT_SOFTWARE_PRODUCER_NIF",
            )?,
            representative: party(
                "VERICOUNT_AEAT_REPRESENTATIVE_NAME",
                "VERICOUNT_AEAT_REPRESENTATIVE_NIF",
            )?,
            installation_number: var("VERICOUNT_INSTALLATION_NUMBER")
                .unwrap_or_else(|_| "1".to_owned()),
            secret_key: var("VERICOUNT_SECRET_KEY")
                .ok()
                .map(|key| SecretKey::parse(&key))
                .transpose()?,
        })
    }

    /// URL that live records are sent to.
    pub fn live_endpoint(&self) -> Result<String, String> {
        aeat_endpoint(self.environment, self.endpoint_override.as_deref())
    }
}

/// A name and NIF from two variables, set together or not at all.
fn party(name_var: &str, nif_var: &str) -> Result<Option<Party>, String> {
    match (var(name_var).ok(), var(nif_var).ok()) {
        (None, None) => Ok(None),
        (Some(name), Some(nif)) => Ok(Some(Party {
            name: invoice::validate_text(name_var, &name).map_err(|e| e.to_string())?,
            nif: invoice::validate_tax_id(nif_var, &nif).map_err(|e| e.to_string())?,
        })),
        _ => Err(format!("set both {name_var} and {nif_var}, or neither")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_variables_are_unset() {
        // Names no other test uses, since tests share the environment.
        env::set_var("VERICOUNT_TEST_EMPTY", "");
        env::set_var("VERICOUNT_TEST_FULL", "x");
        assert_eq!(var("VERICOUNT_TEST_EMPTY"), Err(env::VarError::NotPresent));
        assert_eq!(var("VERICOUNT_TEST_FULL").as_deref(), Ok("x"));
    }

    #[test]
    fn passkeys_need_a_domain_and_https() {
        let url = |s: &str| Url::parse(s).unwrap();
        assert!(webauthn(&url("http://localhost:8080")).is_ok());
        assert!(webauthn(&url("https://books.example.com")).is_ok());
        assert!(webauthn(&url("http://books.example.com")).is_err());
        assert!(webauthn(&url("http://127.0.0.1:8080")).is_err());
    }

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
