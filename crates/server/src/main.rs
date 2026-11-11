use std::{env, sync::Arc, time::Duration};

use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use vericount_server::{
    access::users,
    aeat::{
        service::{self as aeat_service, AeatService},
        submission::{self, RunOutcome},
    },
    company,
    config::{self, AeatSettings, Config, Startup},
    error::StoreError,
    mail::Mailer,
    AppState, MIGRATOR,
};

const USAGE: &str = "usage: vericount-server [serve|migrate|invite <email>|healthcheck|version]

  serve    Run the HTTP server (default). DATABASE_URL should use a role
           that is a member of vericounts_app.
  migrate  Apply the database migrations and exit. DATABASE_URL must use
           the role that owns the schema; on the first run it also needs
           CREATEROLE. With VERICOUNT_APP_ROLE=<role>, that login role is
           then made a member of vericounts_app, for the server to use.
  invite   Let an email address sign up, e.g. the first user. They sign
           up in the web client with a code sent to that address.
  healthcheck  Exit with success if the server at VERICOUNT_ADDR answers.
               For container health checks, where there is no curl.
  version  Print the version.";

type AnyError = Box<dyn std::error::Error>;

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "serve".to_owned());
    match (command.as_str(), args.next()) {
        ("serve", None) => serve().await,
        ("migrate", None) => migrate().await,
        ("invite", Some(email)) => invite(&email).await,
        ("healthcheck", None) => healthcheck().await,
        ("version", None) => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
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
    if let Ok(role) = config::var("VERICOUNT_APP_ROLE") {
        if vericount_server::grant_app_role(&pool, &role).await? {
            println!("{role} is now a member of vericounts_app");
        }
    }
    Ok(())
}

async fn healthcheck() -> Result<(), AnyError> {
    let addr: std::net::SocketAddr = config::var("VERICOUNT_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()?;
    // A server listening on every interface answers on loopback.
    let host = if addr.ip().is_unspecified() {
        std::net::SocketAddr::from(([127, 0, 0, 1], addr.port()))
    } else {
        addr
    };
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?
        .get(format!("http://{host}/healthz"))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(format!("health check answered {}", response.status()).into());
    }
    Ok(())
}

async fn invite(email: &str) -> Result<(), AnyError> {
    let (_, pool) = connect(1).await?;
    let config = Config::from_env()?;
    let (_, email) = users::invite(&pool, email, None)
        .await
        .map_err(|error| match error {
            users::InviteError::InvalidEmail => format!("{email:?} is not an email address"),
            users::InviteError::AlreadyInvited => format!("{email} can already sign in"),
            users::InviteError::Database(error) => error.to_string(),
        })?;
    println!(
        "{email} can sign up at {}: a code will be sent to that address",
        config.public_url
    );
    Ok(())
}

/// Sends pending records forever, waiting between requests for as long
/// as the AEAT asks, and at least a minute.
async fn submission_loop(state: AppState) {
    const MIN_WAIT: u64 = 60;
    loop {
        // The submitter changes when live mode is turned on or the
        // certificate changes.
        let Some(submitter) = state.aeat.current() else {
            tokio::time::sleep(Duration::from_secs(MIN_WAIT)).await;
            continue;
        };
        let outcome = match company::store::issuer(&state.pool).await {
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
                let to = if submitter.live {
                    "the AEAT"
                } else {
                    "the practice mock"
                };
                println!("sent {records} records to {to}: {outcome}");
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
    for old in ["VERICOUNT_AEAT_SUBMIT", "VERICOUNT_AEAT_IDENTITY_PEM"] {
        if config::var(old).is_ok() {
            eprintln!(
                "{old} is no longer used: records are sent in practice mode from the start, \
                 and the certificate is uploaded in the settings"
            );
        }
    }
    let practice_endpoint = aeat_service::start_practice_mock(&pool).await?;
    let aeat = AeatService::new(
        AeatSettings::from_env(config.aeat_environment)?,
        Some(practice_endpoint),
    );
    if let Err(error) = aeat.reload(&pool).await {
        eprintln!("records are not being sent: {error}");
    }
    match aeat.current() {
        Some(submitter) if submitter.live => {
            println!("live mode: sending records to the AEAT");
            if let Some(info) = &submitter.certificate {
                println!(
                    "client certificate: {} (NIF {}), expires {}",
                    info.subject,
                    info.nif.as_deref().unwrap_or("unknown"),
                    info.expires_at
                );
            }
        }
        Some(_) => println!("practice mode: records go to the mock inside the server"),
        None => println!("live mode: no certificate, records wait until one is uploaded"),
    }
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
        aeat: Arc::new(aeat),
    };
    tokio::spawn(submission_loop(state.clone()));

    let listener = TcpListener::bind(startup.addr).await?;
    println!("listening on http://{}", listener.local_addr()?);

    axum::serve(listener, vericount_server::app(state, startup.web_dir))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
