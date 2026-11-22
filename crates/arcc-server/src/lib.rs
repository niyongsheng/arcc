pub mod dingtalk;
pub mod feishu;
pub mod im;
pub mod middleware;
pub mod routes;
pub mod scheduler;

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

use arcc_core::context::SharedContext;

use crate::dingtalk::executor::DingtalkTaskExecutor;
use crate::feishu::executor::FeishuTaskExecutor;
use crate::scheduler::RoutingExecutor;

/// Start the ARCC server (axum HTTP + optional Feishu / DingTalk webhooks).
pub async fn run(ctx: SharedContext, daemon: bool) -> anyhow::Result<()> {
    let addr: SocketAddr = format!(
        "{}:{}",
        ctx.storage.config.server.host, ctx.storage.config.server.port
    )
    .parse()
    .map_err(|e| {
        anyhow::anyhow!(
            "invalid server bind address '{}:{}': {e}",
            ctx.storage.config.server.host,
            ctx.storage.config.server.port
        )
    })?;

    // Spawn the background scheduler with an executor per configured platform.
    let has_platform = ctx.feishu_client.is_some() || ctx.dingtalk_client.is_some();
    let mut routing = RoutingExecutor::new();
    if ctx.feishu_client.is_some() {
        routing = routing.register("feishu", Arc::new(FeishuTaskExecutor::new(ctx.clone())));
    }
    if ctx.dingtalk_client.is_some() {
        routing = routing.register("dingtalk", Arc::new(DingtalkTaskExecutor::new(ctx.clone())));
    }
    if has_platform {
        tokio::spawn(scheduler::scheduler_loop(ctx.clone(), Arc::new(routing)));
    }

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
    use axum::middleware;
    use axum::routing::{get, post, put};

    // Public routes (no auth required).
    let mut router = axum::Router::new().route("/health", get(routes::health::handler));

    // Protected routes — require API key when configured.
    let mut protected = axum::Router::new()
        .route("/chat", post(routes::chat::handler))
        .route(
            "/memory/{user_id}",
            get(routes::memory::list_memories).post(routes::memory::create_memory),
        )
        .route(
            "/memory/{user_id}/{key}",
            put(routes::memory::update_memory).delete(routes::memory::delete_memory),
        );
    // Only add platform send endpoints when the backend is configured.
    if ctx.feishu_client.is_some() {
        protected = protected.route("/feishu/send", post(feishu::webhook::send_handler));
    }
    if ctx.dingtalk_client.is_some() {
        protected = protected.route("/dingtalk/send", post(dingtalk::webhook::send_handler));
    }
    protected = protected.route_layer(middleware::from_fn_with_state(
        ctx.clone(),
        crate::middleware::require_api_key,
    ));

    router = router.merge(protected);

    // Webhooks use their own platform-specific signature/token verification,
    // so they are mounted outside the API-key-protected group.
    if ctx.feishu_client.is_some() {
        router = router.route("/feishu/webhook", post(feishu::webhook::handler));
    }
    if ctx.dingtalk_client.is_some() {
        router = router.route("/dingtalk/webhook", post(dingtalk::webhook::handler));
    }

    router.with_state(ctx)
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
    info!("SIGINT received, starting graceful shutdown");
}
