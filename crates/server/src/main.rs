use std::{env, sync::Arc};

use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use vericounts_server::{
    config::{Config, Startup},
    AppState, MIGRATOR,
};

const USAGE: &str = "usage: vericounts-server [serve|migrate]

  serve    Run the HTTP server (default). DATABASE_URL should use a role
           that is a member of vericounts_app.
  migrate  Apply the database migrations and exit. DATABASE_URL must use
           the role that owns the schema.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let command = env::args().nth(1).unwrap_or_else(|| "serve".to_owned());
    match command.as_str() {
        "serve" => serve().await,
        "migrate" => migrate().await,
        _ => Err(USAGE.into()),
    }
}

async fn migrate() -> Result<(), Box<dyn std::error::Error>> {
    let startup = Startup::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&startup.database_url)
        .await?;
    MIGRATOR.run(&pool).await?;
    println!("migrations applied");
    Ok(())
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let startup = Startup::from_env()?;
    let config = Config::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&startup.database_url)
        .await?;
    let state = AppState {
        pool,
        config: Arc::new(config),
    };

    let listener = TcpListener::bind(startup.addr).await?;
    println!("listening on http://{}", listener.local_addr()?);

    axum::serve(listener, vericounts_server::app(state, startup.web_dir))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
