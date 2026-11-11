use std::{env, fs, io, sync::Arc, time::Duration};

use sqlx::{postgres::PgPoolOptions, PgPool};
use tokio::net::TcpListener;
use vericounts_core::aeat::{Party, SoftwareSystem};
use vericounts_server::{
    aeat_client::AeatClient,
    auth,
    config::{self, Config, Startup},
    directory,
    store::StoreError,
    submission::{self, RunOutcome, Submitter},
    AppState, MIGRATOR,
};

const USAGE: &str = "usage: vericounts-server [serve|migrate|create-user <username>]

  serve        Run the HTTP server (default). DATABASE_URL should use a
               role that is a member of vericounts_app.
  migrate      Apply the database migrations and exit. DATABASE_URL must
               use the role that owns the schema.
  create-user  Create a user. The password is read from standard input.";

type AnyError = Box<dyn std::error::Error>;

#[tokio::main]
async fn main() -> Result<(), AnyError> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "serve".to_owned());
    match (command.as_str(), args.next()) {
        ("serve", None) => serve().await,
        ("migrate", None) => migrate().await,
        ("create-user", Some(username)) => create_user(&username).await,
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

async fn create_user(username: &str) -> Result<(), AnyError> {
    let (_, pool) = connect(1).await?;
    eprintln!("password for {username}:");
    let mut password = String::new();
    io::stdin().read_line(&mut password)?;
    let password = password.trim_end_matches(['\r', '\n']);
    auth::create_user(&pool, username, password).await?;
    println!("user {username} created");
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
    Ok(Some(Arc::new(Submitter::new(
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
    ))))
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
    let state = AppState {
        pool,
        config: Arc::new(config),
        throttle: Arc::default(),
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
