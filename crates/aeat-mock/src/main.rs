use std::env;

use tokio::net::TcpListener;
use vericounts_aeat_mock::{Fault, Mock, SERVICE_PATH};

/// Runs the mock for local development.
///
/// `AEAT_MOCK_ADDR` sets the listen address (default `127.0.0.1:8090`) and
/// `AEAT_MOCK_SCHEMAS` enables schema validation with `xmllint`. With
/// `AEAT_MOCK_REJECT_FIRST` set, the records of the first request are
/// rejected, to try the correction flow.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = env::var("AEAT_MOCK_ADDR").unwrap_or_else(|_| "127.0.0.1:8090".to_owned());
    let mut mock = Mock::new();
    if let Ok(schemas) = env::var("AEAT_MOCK_SCHEMAS") {
        mock = mock.with_schema_validation(schemas);
    }
    if env::var("AEAT_MOCK_REJECT_FIRST").is_ok() {
        mock.inject(Fault::RejectRecords);
    }
    let listener = TcpListener::bind(&addr).await?;
    println!(
        "AEAT mock listening on http://{}{SERVICE_PATH}",
        listener.local_addr()?
    );
    axum::serve(listener, mock.router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
