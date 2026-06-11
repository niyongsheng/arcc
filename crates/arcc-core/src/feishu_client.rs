//! Feishu (Lark) HTTP API client.
//!
//! Manages tenant access tokens and provides methods to send messages
//! and update interactive cards via the Feishu Open API.

use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use tokio::sync::RwLock;
use tracing::warn;

/// A thin HTTP client for the Feishu Open API.
///
/// Automatically obtains and caches tenant access tokens via
/// `app_id` + `app_secret`. All public methods are `Send + Sync`.
#[derive(Clone)]
pub struct FeishuClient {
    client: reqwest::Client,
    app_id: String,
    app_secret: String,
    token_cache: Arc<RwLock<Option<(String, Instant)>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum FeishuError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error: code={code} msg={msg}")]
    Api { code: i64, msg: String },
    #[error("rate-limited, retry after {retry_after}s")]
    RateLimited { retry_after: u64 },
}

const FEISHU_OPEN_API: &str = "https://open.feishu.cn/open-apis";
const TOKEN_REFRESH_MARGIN_SECS: u64 = 300; // refresh 5 min before expiry

impl FeishuClient {
    /// Create a new FeishuClient.
    ///
    /// The client will fetch a `tenant_access_token` on the first API call
    /// and cache it until 5 minutes before expiry.
    pub fn new(app_id: String, app_secret: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            app_id,
            app_secret,
            token_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Obtain a cached or freshly-fetched `tenant_access_token`.
    pub async fn get_token(&self) -> Result<String, FeishuError> {
        // Check cache first.
        {
            let cache = self.token_cache.read().await;
            if let Some((token, expires_at)) = cache.as_ref() {
                if Instant::now() < *expires_at {
                    return Ok(token.clone());
                }
            }
        }

        // Fetch a new token.
        let resp = self
            .client
            .post(format!("{FEISHU_OPEN_API}/auth/v3/tenant_access_token/internal"))
            .json(&json!({
                "app_id": self.app_id,
                "app_secret": self.app_secret,
            }))
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;
        let code = body["code"].as_i64().unwrap_or(-1);
        if code != 0 {
            let msg = body["msg"].as_str().unwrap_or("unknown").to_owned();
            warn!(code, msg, "failed to get feishu tenant access token");
            return Err(FeishuError::Api { code, msg });
        }

        let token = body["tenant_access_token"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        let expire_in = body["expire"].as_u64().unwrap_or(7200);

        let expires_at = Instant::now()
            + std::time::Duration::from_secs(expire_in.saturating_sub(TOKEN_REFRESH_MARGIN_SECS));

        let mut cache = self.token_cache.write().await;
        *cache = Some((token.clone(), expires_at));
        Ok(token)
    }

    /// Send a message (text or interactive card) to a user via `open_id`.
    pub async fn send_message(
        &self,
        open_id: &str,
        content: serde_json::Value,
        msg_type: &str,
    ) -> Result<(), FeishuError> {
        let token = self.get_token().await?;
        let content_str = serde_json::to_string(&content).unwrap_or_default();

        let resp = self
            .client
            .post(format!(
                "{FEISHU_OPEN_API}/im/v1/messages?receive_id_type=open_id"
            ))
            .header("Authorization", format!("Bearer {token}"))
            .json(&json!({
                "receive_id": open_id,
                "msg_type": msg_type,
                "content": content_str,
            }))
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;
        let code = body["code"].as_i64().unwrap_or(-1);
        if code == 99991663 {
            // Rate limited
            let retry_after = body["data"]["retry_after"].as_u64().unwrap_or(5);
            return Err(FeishuError::RateLimited { retry_after });
        }
        if code != 0 {
            let msg = body["msg"].as_str().unwrap_or("unknown").to_owned();
            warn!(code, msg, "feishu send_message failed");
            return Err(FeishuError::Api { code, msg });
        }

        Ok(())
    }

    /// Update an existing message (used to replace interactive cards after
    /// a button action). `message_id` is the ID returned by `send_message`.
    pub async fn update_message(
        &self,
        message_id: &str,
        content: serde_json::Value,
    ) -> Result<(), FeishuError> {
        let token = self.get_token().await?;

        let body: serde_json::Value = self
            .client
            .patch(format!(
                "{FEISHU_OPEN_API}/im/v1/messages/{message_id}"
            ))
            .header("Authorization", format!("Bearer {token}"))
            .json(&json!({
                "content": serde_json::to_string(&content).unwrap_or_default(),
            }))
            .send()
            .await?
            .json()
            .await?;

        let code = body["code"].as_i64().unwrap_or(-1);
        if code != 0 {
            let msg = body["msg"].as_str().unwrap_or("unknown").to_owned();
            warn!(code, msg, "feishu update_message failed");
            return Err(FeishuError::Api { code, msg });
        }

        Ok(())
    }
}
