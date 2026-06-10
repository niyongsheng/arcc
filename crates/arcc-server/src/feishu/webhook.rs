use axum::{http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};

/// Feishu webhook event payload (simplified).
#[derive(Debug, Deserialize)]
pub struct FeishuEvent {
    pub challenge: Option<String>,
    pub token: Option<String>,
    #[serde(rename = "type")]
    pub event_type: Option<String>,
    pub event: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct ChallengeResponse {
    pub challenge: String,
}

/// POST /feishu/webhook — receives Feishu event callbacks.
///
/// - URL verification: echoes the `challenge` field.
/// - Message events: enqueued for processing by the ARCC agent.
pub async fn handler(Json(payload): Json<FeishuEvent>) -> (StatusCode, Json<serde_json::Value>) {
    // URL verification (one-time)
    if let Some(challenge) = payload.challenge {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"challenge": challenge})),
        );
    }

    // Handle incoming message events
    if let Some(event) = payload.event {
        tracing::info!(?event, "feishu event received");
        // TODO: dispatch to session manager
    }

    (StatusCode::OK, Json(serde_json::json!({"code": 0})))
}
