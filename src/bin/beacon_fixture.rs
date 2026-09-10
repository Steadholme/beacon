//! Synthetic Beacon for local UI review of the public status page.
//!
//! Boots the real router against the in-memory dev store (seeded checks, no
//! database, no egress), so the public page can be rendered and screenshotted
//! without touching the estate. `main.rs` remains the production entry point.

use std::net::SocketAddr;

const LISTEN_ADDR: &str = "127.0.0.1:9171";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let state = beacon::build_dev_state().await;
    let addr: SocketAddr = LISTEN_ADDR.parse().expect("fixed fixture address is valid");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|error| panic!("failed to bind fixture server at {addr}: {error}"));
    tracing::info!(%addr, "Beacon synthetic fixture listening");
    axum::serve(listener, beacon::app(state))
        .await
        .expect("fixture server");
}
