//! DingTalk (钉钉) HTTP API client.
//!
//! Handles two outbound channels:
//! - **Session webhook** — the per-message URL included in an inbound callback.
//!   Works without app credentials but expires shortly after the message.
//! - **Robot send APIs** — `oToMessages/batchSend` / `groupMessages/send`,
//!   authenticated with an app access token. Required for scheduled tasks
//!   and other proactive messages sent long after the original callback.

use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use tokio::sync::RwLock;
use tracing::warn;

const DINGTALK_API: &str = "https://api.dingtalk.com";
const TOKEN_REFRESH_MARGIN_SECS: u64 = 300; // refresh 5 min before expiry

/// Maximum markdown body length accepted by DingTalk robots.
const MAX_MARKDOWN_CHARS: usize = 18_000;

/// A thin HTTP client for the DingTalk Open API.
#[derive(Clone)]
pub struct DingtalkClient {
    client: reqwest::Client,
    client_id: String,
    client_secret: String,
    robot_code: String,
    token_cache: Arc<RwLock<Option<(String, Instant)>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum DingtalkError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error: code={code} msg={msg}")]
    Api { code: String, msg: String },
    #[error("client_id/client_secret not configured")]
    NotConfigured,
}

impl DingtalkClient {
    /// Create a new client. `robot_code` falls back to `client_id` when empty,
    /// which is the common case for enterprise internal robots.
    pub fn new(client_id: String, client_secret: String, robot_code: String) -> Self {
        let robot_code = if robot_code.is_empty() {
            client_id.clone()
        } else {
            robot_code
        };
        Self {
            client: reqwest::Client::new(),
            client_id,
            client_secret,
            robot_code,
            token_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Whether app credentials are available for proactive sending.
    pub fn is_configured(&self) -> bool {
        !self.client_id.is_empty() && !self.client_secret.is_empty()
    }

    /// Obtain a cached or freshly-fetched app access token.
    pub async fn get_access_token(&self) -> Result<String, DingtalkError> {
        if !self.is_configured() {
            return Err(DingtalkError::NotConfigured);
        }

        {
            let cache = self.token_cache.read().await;
            if let Some((token, expires_at)) = cache.as_ref()
                && Instant::now() < *expires_at
            {
                return Ok(token.clone());
            }
        }

        let resp = self
            .client
            .post(format!("{DINGTALK_API}/v1.0/oauth2/accessToken"))
            .json(&json!({
                "appKey": self.client_id,
                "appSecret": self.client_secret,
            }))
            .send()
            .await?;

        let body: serde_json::Value = resp.json().await?;
        let Some(token) = body["accessToken"].as_str() else {
            let code = body["code"].as_str().unwrap_or("unknown").to_owned();
            let msg = body["message"].as_str().unwrap_or("unknown").to_owned();
            warn!(%code, %msg, "failed to get dingtalk access token");
            return Err(DingtalkError::Api { code, msg });
        };
        let expire_in = body["expireIn"].as_u64().unwrap_or(7200);

        let expires_at = Instant::now()
            + std::time::Duration::from_secs(expire_in.saturating_sub(TOKEN_REFRESH_MARGIN_SECS));

        let mut cache = self.token_cache.write().await;
        *cache = Some((token.to_owned(), expires_at));
        Ok(token.to_owned())
    }

    /// Send a markdown message to a single user via the robot API.
    pub async fn send_markdown_to_user(
        &self,
        user_id: &str,
        title: &str,
        text: &str,
    ) -> Result<(), DingtalkError> {
        let token = self.get_access_token().await?;
        let msg_param = markdown_param(title, text);

        let resp = self
            .client
            .post(format!("{DINGTALK_API}/v1.0/robot/oToMessages/batchSend"))
            .header("x-acs-dingtalk-access-token", token)
            .json(&json!({
                "robotCode": self.robot_code,
                "userIds": [user_id],
                "msgKey": "sampleMarkdown",
                "msgParam": msg_param,
            }))
            .send()
            .await?;

        check_api_response(resp, "send_markdown_to_user").await
    }

    /// Send a markdown message to a group conversation via the robot API.
    pub async fn send_markdown_to_group(
        &self,
        open_conversation_id: &str,
        title: &str,
        text: &str,
    ) -> Result<(), DingtalkError> {
        let token = self.get_access_token().await?;
        let msg_param = markdown_param(title, text);

        let resp = self
            .client
            .post(format!("{DINGTALK_API}/v1.0/robot/groupMessages/send"))
            .header("x-acs-dingtalk-access-token", token)
            .json(&json!({
                "robotCode": self.robot_code,
                "openConversationId": open_conversation_id,
                "msgKey": "sampleMarkdown",
                "msgParam": msg_param,
            }))
            .send()
            .await?;

        check_api_response(resp, "send_markdown_to_group").await
    }

    /// Reply through the per-message `sessionWebhook` URL.
    ///
    /// No credentials are required, but the URL expires (~1.5h), so this is
    /// only suitable for replying to the message that carried it.
    pub async fn send_via_session_webhook(
        &self,
        webhook_url: &str,
        title: &str,
        text: &str,
    ) -> Result<(), DingtalkError> {
        let safe_text = truncate_chars(text, MAX_MARKDOWN_CHARS);
        let body: serde_json::Value = self
            .client
            .post(webhook_url)
            .json(&json!({
                "msgtype": "markdown",
                "markdown": {
                    "title": title,
                    "text": safe_text,
                },
            }))
            .send()
            .await?
            .json()
            .await?;

        let errcode = body["errcode"].as_i64().unwrap_or(0);
        if errcode != 0 {
            let msg = body["errmsg"].as_str().unwrap_or("unknown").to_owned();
            warn!(errcode, %msg, "dingtalk session webhook send failed");
            return Err(DingtalkError::Api {
                code: errcode.to_string(),
                msg,
            });
        }
        Ok(())
    }
}

/// Serialize the `msgParam` payload for `sampleMarkdown`.
fn markdown_param(title: &str, text: &str) -> String {
    let safe_text = truncate_chars(text, MAX_MARKDOWN_CHARS);
    serde_json::json!({
        "title": title,
        "text": safe_text,
    })
    .to_string()
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut truncated: String = text.chars().take(max).collect();
        truncated.push_str("\n\n…(truncated)");
        truncated
    }
}

/// Accept both new-style `{code,message}` errors and success payloads.
async fn check_api_response(resp: reqwest::Response, op: &str) -> Result<(), DingtalkError> {
    let status = resp.status();
    let body: serde_json::Value = resp.json().await?;

    if status.is_success() && body.get("code").is_none() {
        return Ok(());
    }

    let code = body["code"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| body["code"].as_i64().map(|c| c.to_string()))
        .unwrap_or_else(|| status.as_u16().to_string());
    let msg = body["message"]
        .as_str()
        .or_else(|| body["msg"].as_str())
        .unwrap_or("unknown")
        .to_owned();
    warn!(op, %code, %msg, "dingtalk API call failed");
    Err(DingtalkError::Api { code, msg })
}
