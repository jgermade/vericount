//! SOAP client for the AEAT record submission service.

use std::{fmt, time::Duration};

use reqwest::{header::CONTENT_TYPE, Identity};
use vericounts_core::aeat::{self, ResponseError, SubmissionResponse};

#[derive(Debug)]
pub enum SubmitError {
    /// The client could not be built, e.g. the certificate is not valid.
    Setup(String),
    /// No response arrived: connection, TLS or timeout error. The request
    /// may or may not have been processed.
    Transport(String),
    /// The service answered with an HTTP error and no usable SOAP body.
    Http { status: u16, body: String },
    /// The service answered with a SOAP fault or an unreadable response.
    Response { error: ResponseError, body: String },
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(reason) => write!(f, "AEAT client setup failed: {reason}"),
            Self::Transport(reason) => write!(f, "AEAT request failed: {reason}"),
            Self::Http { status, .. } => write!(f, "AEAT answered with HTTP {status}"),
            Self::Response { error, .. } => write!(f, "AEAT response: {error}"),
        }
    }
}

impl std::error::Error for SubmitError {}

/// A parsed response together with the exact XML that was exchanged, so
/// both can be stored.
#[derive(Debug)]
pub struct Submission {
    pub request_xml: String,
    pub response_xml: String,
    pub response: SubmissionResponse,
}

#[derive(Debug, Clone)]
pub struct AeatClient {
    http: reqwest::Client,
    endpoint: String,
}

impl AeatClient {
    /// `identity_pem` is the client certificate and its private key in PEM
    /// format, used for mutual TLS. The real service requires it.
    pub fn new(endpoint: String, identity_pem: Option<&[u8]>) -> Result<Self, SubmitError> {
        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10));
        if let Some(pem) = identity_pem {
            let identity =
                Identity::from_pem(pem).map_err(|e| SubmitError::Setup(e.to_string()))?;
            builder = builder.identity(identity);
        }
        Ok(Self {
            http: builder
                .build()
                .map_err(|e| SubmitError::Setup(e.to_string()))?,
            endpoint,
        })
    }

    /// Sends a `RegFactuSistemaFacturacion` message, as built by
    /// [`aeat::submission_request`].
    pub async fn submit(&self, request: &str) -> Result<Submission, SubmitError> {
        let request_xml = aeat::soap_envelope(request);
        let response = self
            .http
            .post(&self.endpoint)
            .header(CONTENT_TYPE, "text/xml; charset=UTF-8")
            .header("SOAPAction", "\"\"")
            .body(request_xml.clone())
            .send()
            .await
            .map_err(|e| SubmitError::Transport(e.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| SubmitError::Transport(e.to_string()))?;

        match aeat::parse_submission_response(&body) {
            Ok(parsed) if status.is_success() => Ok(Submission {
                request_xml,
                response_xml: body,
                response: parsed,
            }),
            // SOAP faults arrive with status 500 and a readable body.
            Err(error @ ResponseError::Fault { .. }) => Err(SubmitError::Response { error, body }),
            _ if !status.is_success() => Err(SubmitError::Http {
                status: status.as_u16(),
                body,
            }),
            Ok(_) => unreachable!("successful responses are handled above"),
            Err(error) => Err(SubmitError::Response { error, body }),
        }
    }
}
