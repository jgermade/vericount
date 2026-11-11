//! Runtime configuration, read from environment variables.

use std::{env, net::SocketAddr};

use vericounts_core::{invoice, qr::AeatEnvironment};

/// The company that issues the invoices.
#[derive(Debug, Clone)]
pub struct Issuer {
    pub nif: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub issuer: Issuer,
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
        let nif =
            invoice::validate_tax_id("VERICOUNTS_ISSUER_NIF", &required("VERICOUNTS_ISSUER_NIF")?)
                .map_err(|e| e.to_string())?;
        let name = invoice::validate_text(
            "VERICOUNTS_ISSUER_NAME",
            &required("VERICOUNTS_ISSUER_NAME")?,
        )
        .map_err(|e| e.to_string())?;
        let aeat_environment = match env::var("VERICOUNTS_AEAT_ENV").as_deref() {
            Err(_) | Ok("test") => AeatEnvironment::Test,
            Ok("production") => AeatEnvironment::Production,
            Ok(other) => {
                return Err(format!(
                    "VERICOUNTS_AEAT_ENV must be \"test\" or \"production\", got {other:?}"
                ))
            }
        };
        Ok(Self {
            issuer: Issuer { nif, name },
            aeat_environment,
        })
    }
}

impl Startup {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            addr: env::var("VERICOUNTS_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
                .parse()
                .map_err(|e| format!("VERICOUNTS_ADDR: {e}"))?,
            web_dir: env::var("VERICOUNTS_WEB_DIR").unwrap_or_else(|_| "web".to_owned()),
            database_url: required("DATABASE_URL")?,
        })
    }
}
