use std::{env, net::SocketAddr};

use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = env::var("VERICOUNTS_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_owned())
        .parse()?;
    let web_dir = env::var("VERICOUNTS_WEB_DIR").unwrap_or_else(|_| "web".to_owned());

    let listener = TcpListener::bind(addr).await?;
    println!("listening on http://{}", listener.local_addr()?);

    axum::serve(listener, vericounts_server::app(web_dir))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
