//! API Key authentication middleware for selected HTTP endpoints.
//!
//! Protects sensitive routes (`/chat`, `/memory/*`) from unauthorized access.
//! The API key is configured in `config.toml` under `[server] api_key`.
//! An empty / unset key disables authentication (backwards compatible).

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
    Json,
};
use serde_json::json;
use tracing::warn;

use arcc_core::context::SharedContext;

/// Middleware that validates `Authorization: Bearer <key>` against the
/// configured server API key.
///
/// - If no API key is configured → passes through (backwards compatible).
/// - If the header matches → passes through.
/// - Otherwise → `401 Unauthorized`.
pub async fn require_api_key(
    State(ctx): State<SharedContext>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let expected = &ctx.storage.config.server.api_key;

    // No key configured → skip auth (backwards compatible).
    let Some(expected) = expected.as_ref().filter(|k| !k.is_empty()) else {
        return Ok(next.run(request).await);
    };

    let auth = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let actual = auth.strip_prefix("Bearer ").unwrap_or(auth);

    if actual == expected {
        return Ok(next.run(request).await);
    }

    warn!("rejected request with invalid API key");
    Err((
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "invalid or missing API key"})),
    ))
}
