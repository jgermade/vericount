//! Outgoing email: sign-in codes and invitations.
//!
//! Mail goes through SMTP when `VERICOUNTS_SMTP_URL` is set. Without it,
//! outside production, messages are written to standard error so the
//! server can be tried locally. Tests keep them in memory.

mod es;

use std::sync::{Arc, Mutex};

use lettre::{
    message::{header::ContentType, Mailbox},
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use vericounts_core::qr::AeatEnvironment;

pub use es::{invitation, sign_in_code};

/// A message ready to send. Subject and body are plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub body: String,
}

// Boxing the SMTP variant would buy nothing: there is one mailer per process.
#[allow(clippy::large_enum_variant)]
pub enum Mailer {
    Smtp {
        transport: AsyncSmtpTransport<Tokio1Executor>,
        from: Mailbox,
    },
    /// Writes messages to standard error. Never used in production.
    Log,
    /// Keeps messages in memory, for tests.
    Memory(Arc<Mutex<Vec<Mail>>>),
}

impl Mailer {
    /// Reads `VERICOUNTS_SMTP_URL` and `VERICOUNTS_MAIL_FROM`.
    ///
    /// In production SMTP is required: sign-in codes must never end up in
    /// a log.
    pub fn from_env(environment: AeatEnvironment) -> Result<Self, String> {
        match crate::config::var("VERICOUNTS_SMTP_URL") {
            Ok(url) => {
                let from = crate::config::var("VERICOUNTS_MAIL_FROM")
                    .map_err(|_| "VERICOUNTS_MAIL_FROM is required with VERICOUNTS_SMTP_URL")?;
                Self::smtp(&url, &from)
            }
            Err(_) if environment == AeatEnvironment::Production => {
                Err("VERICOUNTS_SMTP_URL is required in production".to_owned())
            }
            Err(_) => Ok(Self::Log),
        }
    }

    /// An SMTP mailer. `url` is e.g. `smtps://user:password@smtp.example.com`,
    /// or `smtp://127.0.0.1:1025` for a local Mailpit; `from` is e.g.
    /// `vericounts <no-reply@example.com>`.
    pub fn smtp(url: &str, from: &str) -> Result<Self, String> {
        let from = from
            .parse()
            .map_err(|e| format!("VERICOUNTS_MAIL_FROM: {e}"))?;
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .map_err(|e| format!("VERICOUNTS_SMTP_URL: {e}"))?
            .build();
        Ok(Self::Smtp { transport, from })
    }

    /// A mailer for tests and the messages it has sent so far.
    pub fn memory() -> (Self, Arc<Mutex<Vec<Mail>>>) {
        let outbox = Arc::default();
        (Self::Memory(Arc::clone(&outbox)), outbox)
    }

    /// Sends a message without waiting for the result. Requests take the
    /// same time whether a message is sent or not, so their timing does
    /// not tell which addresses have an account. Failures are logged.
    pub fn dispatch(self: &Arc<Self>, mail: Mail) {
        match self.as_ref() {
            Self::Smtp { .. } => {
                let mailer = Arc::clone(self);
                tokio::spawn(async move {
                    if let Err(error) = mailer.send_smtp(&mail).await {
                        eprintln!("cannot send mail: {error}");
                    }
                });
            }
            Self::Log => eprintln!(
                "mail to {} (SMTP is not configured)\nSubject: {}\n\n{}\n",
                mail.to, mail.subject, mail.body
            ),
            Self::Memory(outbox) => outbox.lock().unwrap().push(mail),
        }
    }

    async fn send_smtp(&self, mail: &Mail) -> Result<(), String> {
        let Self::Smtp { transport, from } = self else {
            return Ok(());
        };
        let message = Message::builder()
            .from(from.clone())
            .to(mail.to.parse().map_err(|e| format!("{e}"))?)
            .subject(&mail.subject)
            .header(ContentType::TEXT_PLAIN)
            .body(mail.body.clone())
            .map_err(|e| e.to_string())?;
        transport
            .send(message)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}
