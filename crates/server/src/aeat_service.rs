//! How this installation sends its records: the mode and the certificate.
//!
//! - In practice mode, the starting one, invoices are not real and their
//!   records go to a mock of the AEAT that runs inside the server. No
//!   certificate is needed.
//! - Live mode is turned on from the settings once a certificate has been
//!   uploaded. Records then go to the AEAT with that certificate. It
//!   cannot be turned off.
//!
//! The certificate and its private key are stored encrypted with
//! `VERICOUNT_SECRET_KEY`, so a copy of the database alone does not
//! reveal them.

use std::sync::{Arc, RwLock};

use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
    XChaCha20Poly1305, XNonce,
};
use chrono::{DateTime, Utc};
use p12_keystore::{KeyStore, Pkcs12ImportPolicy};
use sqlx::PgPool;
use vericount_aeat_mock::{Mock, SERVICE_PATH};
use vericount_core::aeat::{Party, SoftwareSystem};

use crate::{
    aeat_client::AeatClient,
    certificate::{self, CertificateInfo},
    config::{AeatSettings, SecretKey},
    store::{self, StoreError},
    submission::{self, RecordState, Submitter},
};

/// Additional data bound to the encrypted certificate.
const CERTIFICATE_AAD: &[u8] = b"vericount aeat certificate";

/// Declared as the producer of practice records when none is configured.
fn placeholder_producer() -> Party {
    Party {
        name: "vericount practice".to_owned(),
        nif: "B00000000".to_owned(),
    }
}

pub struct AeatService {
    pub settings: AeatSettings,
    /// Where the practice mock listens, when it runs.
    practice_endpoint: Option<String>,
    current: RwLock<Option<Arc<Submitter>>>,
}

impl AeatService {
    pub fn new(settings: AeatSettings, practice_endpoint: Option<String>) -> Self {
        Self {
            settings,
            practice_endpoint,
            current: RwLock::new(None),
        }
    }

    /// The submitter of the current mode, if records can be sent.
    pub fn current(&self) -> Option<Arc<Submitter>> {
        self.current.read().unwrap().clone()
    }

    pub fn set(&self, submitter: Option<Arc<Submitter>>) {
        *self.current.write().unwrap() = submitter;
    }

    fn system(&self, producer: Party) -> SoftwareSystem {
        SoftwareSystem {
            producer,
            name: "vericount".to_owned(),
            id: "VC".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            installation_number: self.settings.installation_number.clone(),
        }
    }

    /// Sets up the submitter for the mode and certificate in the database.
    /// Called on start and whenever either changes.
    pub async fn reload(&self, pool: &PgPool) -> Result<(), String> {
        let submitter = self.build(pool).await;
        let result = match &submitter {
            Ok(_) => Ok(()),
            Err(error) => Err(error.clone()),
        };
        self.set(submitter.ok().flatten().map(Arc::new));
        result
    }

    async fn build(&self, pool: &PgPool) -> Result<Option<Submitter>, String> {
        let live = store::is_live(pool).await.map_err(|e| e.to_string())?;
        if !live {
            let Some(endpoint) = &self.practice_endpoint else {
                return Ok(None);
            };
            let client = AeatClient::new(endpoint.clone(), None).map_err(|e| e.to_string())?;
            let producer = self
                .settings
                .producer
                .clone()
                .unwrap_or_else(placeholder_producer);
            let mut submitter = Submitter::new(client, self.system(producer));
            submitter.representative = self.settings.representative.clone();
            return Ok(Some(submitter));
        }
        let Some(producer) = self.settings.producer.clone() else {
            return Err("live mode needs VERICOUNT_SOFTWARE_PRODUCER_NAME and _NIF".to_owned());
        };
        let Some(key) = &self.settings.secret_key else {
            return Err("live mode needs VERICOUNT_SECRET_KEY to read the certificate".to_owned());
        };
        let Some((identity, info)) = load_identity(pool, key).await? else {
            // Sending waits until a certificate is uploaded.
            return Ok(None);
        };
        let client = AeatClient::new(self.settings.live_endpoint()?, Some(&identity))
            .map_err(|e| e.to_string())?;
        let mut submitter = Submitter::new(client, self.system(producer));
        submitter.live = true;
        submitter.representative = self.settings.representative.clone();
        submitter.certificate = Some(info);
        Ok(Some(submitter))
    }
}

/// Starts the mock that practice records are sent to, on a free port of
/// the loopback interface, and returns its URL. It continues from the
/// practice records sent before, since the mock forgets them on restart.
pub async fn start_practice_mock(pool: &PgPool) -> Result<String, String> {
    let mock = Mock::new();
    restore_practice_mock(pool, &mock)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let endpoint = format!(
        "http://{}{SERVICE_PATH}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let router = mock.router();
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router).await {
            eprintln!("practice mock stopped: {error}");
        }
    });
    Ok(endpoint)
}

async fn restore_practice_mock(pool: &PgPool, mock: &Mock) -> Result<(), StoreError> {
    let issuers: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT issuer_nif FROM billing_records WHERE NOT live")
            .fetch_all(pool)
            .await?;
    for issuer_nif in issuers {
        let states = submission::record_states(pool, &issuer_nif, false).await?;
        let records: Vec<(i64, String, String, String)> = sqlx::query_as(
            "SELECT id, record_type, series_number, hash FROM billing_records
             WHERE issuer_nif = $1 AND NOT live ORDER BY id",
        )
        .bind(&issuer_nif)
        .fetch_all(pool)
        .await?;
        let answered: Vec<i64> = sqlx::query_scalar(
            "SELECT DISTINCT r.record_id FROM aeat_record_results r
             JOIN billing_records b ON b.id = r.record_id
             WHERE b.issuer_nif = $1 AND NOT b.live",
        )
        .bind(&issuer_nif)
        .fetch_all(pool)
        .await?;
        let registered_state = |id: &i64| {
            states.get(id).is_some_and(|result| {
                matches!(
                    result.state,
                    RecordState::Accepted | RecordState::AcceptedWithErrors
                )
            })
        };
        let of_type = |wanted: &str| {
            records
                .iter()
                .filter(|(id, record_type, ..)| record_type == wanted && registered_state(id))
                .map(|(_, _, series_number, _)| series_number.clone())
                .collect::<Vec<_>>()
        };
        // The mock chains on the last record it answered for, accepted or not.
        let last_hash = records
            .iter()
            .rev()
            .find(|(id, ..)| answered.contains(id))
            .map_or("", |(.., hash)| hash.as_str());
        mock.restore(
            &issuer_nif,
            last_hash,
            of_type("registration"),
            of_type("cancellation"),
        );
    }
    Ok(())
}

#[derive(Debug)]
pub enum CertificateError {
    /// VERICOUNT_SECRET_KEY is not set.
    SecretKeyMissing,
    /// Not a certificate with its key, or the password is wrong.
    Unreadable(String),
    Expired,
    Database(sqlx::Error),
}

impl From<sqlx::Error> for CertificateError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

fn pem_block(label: &str, der: &[u8]) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// Reads a certificate with its private key: a PKCS#12 file (.p12, .pfx)
/// with its password, or a PEM file. Returns it as PEM, the private key
/// first and then the certificates, leaf first.
pub fn read_identity(file: &[u8], password: &str) -> Result<Vec<u8>, String> {
    let pem = if file.trim_ascii_start().starts_with(b"-----BEGIN") {
        file.to_vec()
    } else {
        let store = KeyStore::from_pkcs12(file, password, Pkcs12ImportPolicy::Strict)
            .map_err(|e| format!("cannot open the PKCS#12 file: {e}"))?;
        let (_, chain) = store
            .private_key_chain()
            .ok_or("the file has no private key with its certificate")?;
        let mut pem = pem_block("PRIVATE KEY", chain.key().as_der());
        for certificate in chain.certs() {
            pem.push_str(&pem_block("CERTIFICATE", certificate.as_der()));
        }
        pem.into_bytes()
    };
    let text = String::from_utf8_lossy(&pem);
    if !text.contains("PRIVATE KEY-----") {
        return Err("the file has no private key".to_owned());
    }
    // The same check the client does when it is built.
    reqwest::Identity::from_pem(&pem).map_err(|e| format!("unusable certificate: {e}"))?;
    Ok(pem)
}

fn cipher(key: &SecretKey) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new((&key.0).into())
}

/// Stores a certificate, replacing the one there was.
pub async fn save_certificate(
    pool: &PgPool,
    key: Option<&SecretKey>,
    user_id: i64,
    file: &[u8],
    password: &str,
    now: DateTime<Utc>,
) -> Result<CertificateInfo, CertificateError> {
    let key = key.ok_or(CertificateError::SecretKeyMissing)?;
    let identity = read_identity(file, password).map_err(CertificateError::Unreadable)?;
    let info = certificate::inspect(&identity).map_err(CertificateError::Unreadable)?;
    if info.expires_at <= now {
        return Err(CertificateError::Expired);
    }
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let encrypted = cipher(key)
        .encrypt(
            &nonce,
            Payload {
                msg: &identity,
                aad: CERTIFICATE_AAD,
            },
        )
        .map_err(|_| CertificateError::Unreadable("cannot encrypt".to_owned()))?;

    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM aeat_certificate")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO aeat_certificate
             (uploaded_by, subject, nif, of_legal_person, expires_at, nonce, identity)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(user_id)
    .bind(&info.subject)
    .bind(&info.nif)
    .bind(info.of_legal_person)
    .bind(info.expires_at)
    .bind(nonce.as_slice())
    .bind(encrypted)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(info)
}

/// Deletes the certificate. Returns false if there was none.
pub async fn remove_certificate(pool: &PgPool) -> Result<bool, StoreError> {
    let deleted = sqlx::query("DELETE FROM aeat_certificate")
        .execute(pool)
        .await?
        .rows_affected();
    Ok(deleted > 0)
}

/// The stored certificate, without its key.
pub async fn certificate_info(pool: &PgPool) -> Result<Option<CertificateInfo>, StoreError> {
    let row: Option<(String, Option<String>, bool, DateTime<Utc>)> =
        sqlx::query_as("SELECT subject, nif, of_legal_person, expires_at FROM aeat_certificate")
            .fetch_optional(pool)
            .await?;
    Ok(row.map(
        |(subject, nif, of_legal_person, expires_at)| CertificateInfo {
            subject,
            nif,
            of_legal_person,
            expires_at,
        },
    ))
}

/// The stored certificate and key, decrypted, in PEM format.
async fn load_identity(
    pool: &PgPool,
    key: &SecretKey,
) -> Result<Option<(Vec<u8>, CertificateInfo)>, String> {
    let row: Option<(Vec<u8>, Vec<u8>)> =
        sqlx::query_as("SELECT nonce, identity FROM aeat_certificate")
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;
    let Some((nonce, encrypted)) = row else {
        return Ok(None);
    };
    let identity = cipher(key)
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &encrypted,
                aad: CERTIFICATE_AAD,
            },
        )
        .map_err(|_| {
            "cannot decrypt the stored certificate: VERICOUNT_SECRET_KEY is not the key it \
             was uploaded with; upload it again"
                .to_owned()
        })?;
    let info = certificate::inspect(&identity)?;
    Ok(Some((identity, info)))
}
