use std::net::SocketAddr;

use comp2resp::{app, config, error, observability, state};
use tokio::net::TcpListener;
use tracing::info;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("fatal error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), error::ProxyError> {
    let config = config::Config::from_env()?;
    observability::init(&config);

    let app_state = state::AppState::from_config(config.clone())?;
    let app = app::build_router(app_state);

    let listener = TcpListener::bind(config.listen_addr)
        .await
        .map_err(|source| {
            error::ProxyError::internal_with_source("failed to bind listen socket", source)
        })?;

    let local_addr: SocketAddr = listener.local_addr().map_err(|source| {
        error::ProxyError::internal_with_source("failed to get local listen address", source)
    })?;

    info!(address = %local_addr, "comp2resp listening");

    axum::serve(listener, app)
        .await
        .map_err(|source| error::ProxyError::internal_with_source("server error", source))
}
