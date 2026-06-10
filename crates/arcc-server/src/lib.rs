pub mod feishu;
pub mod routes;

use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::info;

use arcc_core::context::SharedContext;

/// Start the ARCC server (axum HTTP + optional Feishu webhook).
pub async fn run(ctx: SharedContext, daemon: bool) -> anyhow::Result<()> {
    let addr: SocketAddr = format!(
        "{}:{}",
        ctx.storage.config.server.host,
        ctx.storage.config.server.port
    )
    .parse()
    .expect("invalid server bind address");

    let app = build_router(ctx);

    let listener = TcpListener::bind(addr).await?;
    info!(%addr, "arcc-server listening");

    if daemon {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
    } else {
        axum::serve(listener, app).await?;
    }

    Ok(())
}

fn build_router(ctx: SharedContext) -> axum::Router {
    use axum::routing::{get, post};

    axum::Router::new()
        .route("/health", get(routes::health::handler))
        .route("/chat", post(routes::chat::handler))
        .route("/feishu/webhook", post(feishu::webhook::handler))
        .with_state(ctx)
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
    info!("SIGINT received, starting graceful shutdown");
}
