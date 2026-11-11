use std::{env, fs, sync::Arc, time::Duration};

use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use vericounts_core::aeat::{Party, SoftwareSystem};
use vericounts_server::{
    aeat_client::AeatClient,
    auth, certificate,
    config::{self, Config, Startup},
    directory,
    mail::Mailer,
    store::StoreError,
    submission::{self, RunOutcome, Submitter},
    AppState, MIGRATOR,
};

const USAGE: &str = "usage: vericounts-server [serve|migrate|invite <email>]

  serve    Run the HTTP server (default). DATABASE_URL should use a role
           that is a member of vericounts_app.
  migrate  Apply the database migrations and exit. DATABASE_URL must use
           the role that owns the schema.
  invite   Let an email address sign up, e.g. the first user. They sign
           up in the web client with a code sent to that address.";

type AnyError = Box<dyn std::error::Error>;

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "serve".to_owned());
    match (command.as_str(), args.next()) {
        ("serve", None) => serve().await,
        ("migrate", None) => migrate().await,
        ("invite", Some(email)) => invite(&email).await,
        _ => Err(USAGE.into()),
    }
}

async fn connect(max_connections: u32) -> Result<(Startup, PgPool), AnyError> {
    let startup = Startup::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(&startup.database_url)
        .await?;
    Ok((startup, pool))
}

async fn migrate() -> Result<(), AnyError> {
    let (_, pool) = connect(1).await?;
    MIGRATOR.run(&pool).await?;
    println!("migrations applied");
    Ok(())
}

async fn invite(email: &str) -> Result<(), AnyError> {
    let (_, pool) = connect(1).await?;
    let config = Config::from_env()?;
    let (_, email) = auth::invite(&pool, email, None)
        .await
        .map_err(|error| match error {
            auth::InviteError::InvalidEmail => format!("{email:?} is not an email address"),
            auth::InviteError::AlreadyInvited => format!("{email} can already sign in"),
            auth::InviteError::Database(error) => error.to_string(),
        })?;
    println!(
        "{email} can sign up at {}: a code will be sent to that address",
        config.public_url
    );
    Ok(())
}

fn submitter(config: &Config) -> Result<Option<Arc<Submitter>>, AnyError> {
    let Some(settings) = config::Submission::from_env(config.aeat_environment)? else {
        return Ok(None);
    };
    let identity = settings
        .identity_pem_path
        .as_deref()
        .map(fs::read)
        .transpose()?;
    let client = AeatClient::new(settings.endpoint.clone(), identity.as_deref())?;
    println!("sending records to {}", settings.endpoint);
    let mut submitter = Submitter::new(
        client,
        SoftwareSystem {
            producer: Party {
                name: settings.producer_name,
                nif: settings.producer_nif,
            },
            name: "vericounts".to_owned(),
            id: "VC".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            installation_number: settings.installation_number,
        },
    );
    submitter.representative = settings
        .representative
        .map(|(name, nif)| Party { name, nif });
    if let Some(identity) = &identity {
        let info = certificate::inspect(identity)?;
        println!(
            "client certificate: {} (NIF {}), expires {}",
            info.subject,
            info.nif.as_deref().unwrap_or("unknown"),
            info.expires_at
        );
        submitter.certificate = Some(info);
    }
    Ok(Some(Arc::new(submitter)))
}

/// Sends pending records forever, waiting between requests for as long
/// as the AEAT asks, and at least a minute.
async fn submission_loop(state: AppState, submitter: Arc<Submitter>) {
    const MIN_WAIT: u64 = 60;
    loop {
        let outcome = match directory::issuer(&state.pool).await {
            Ok(issuer) => submission::run_once(&state.pool, &issuer, &submitter).await,
            Err(error) => Err(error),
        };
        let wait = match outcome {
            Ok(RunOutcome::Sent {
                outcome,
                records,
                wait_seconds,
                ..
            }) => {
                println!("sent {records} records to the AEAT: {outcome}");
                u64::from(wait_seconds.unwrap_or(0)).max(MIN_WAIT)
            }
            Ok(_) => MIN_WAIT,
            // Nothing to send until the company data is entered.
            Err(StoreError::CompanyMissing) => MIN_WAIT,
            Err(error) => {
                eprintln!("submission failed: {error:?}");
                MIN_WAIT
            }
        };
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}

async fn serve() -> Result<(), AnyError> {
    let (startup, pool) = connect(10).await?;
    let config = Config::from_env()?;
    let submitter = submitter(&config)?;
    let mailer = Mailer::from_env(config.aeat_environment)?;
    if matches!(mailer, Mailer::Log) {
        eprintln!("SMTP is not configured: emails, sign-in codes included, go to standard error");
    }
    let webauthn = config::webauthn(&config.public_url)?;
    println!("passkeys for {}", config.public_url);
    let state = AppState {
        pool,
        config: Arc::new(config),
        mailer: Arc::new(mailer),
        webauthn: Arc::new(webauthn),
        submitter: submitter.clone(),
    };
    if let Some(submitter) = submitter {
        tokio::spawn(submission_loop(state.clone(), submitter));
    }

    let listener = TcpListener::bind(startup.addr).await?;
    println!("listening on http://{}", listener.local_addr()?);

    axum::serve(listener, vericounts_server::app(state, startup.web_dir))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
