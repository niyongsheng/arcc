//! DingTalk implementation of the shared `ChatBackend` transport trait.

use async_trait::async_trait;
use tracing::warn;

use arcc_core::dingtalk_client::DingtalkClient;

use crate::im::{ChatBackend, ChatEvent};

pub struct DingtalkBackend<'a> {
    pub client: &'a DingtalkClient,
}

#[async_trait]
impl ChatBackend for DingtalkBackend<'_> {
    fn platform(&self) -> &'static str {
        "dingtalk"
    }

    async fn send(&self, event: &ChatEvent, text: &str) -> Result<(), String> {
        // Group messages get an @mention so the sender is notified.
        let text = if event.chat_type == "group" {
            format!("@{} {text}", event.user_id)
        } else {
            text.to_owned()
        };

        // Prefer the per-message sessionWebhook: it needs no app permissions
        // and always targets the originating conversation. Fall back to the
        // robot API for expired webhooks and scheduler-triggered messages.
        if let Some(url) = event.session_webhook.as_deref() {
            match self
                .client
                .send_via_session_webhook(url, "ARCC", &text)
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!(err = %e, "dingtalk session webhook failed, falling back to robot API");
                }
            }
        }

        if event.reply_id_type == "dingtalk_conversation_id" {
            self.client
                .send_markdown_to_group(&event.reply_id, "ARCC", &text)
                .await
                .map_err(|e| e.to_string())
        } else {
            self.client
                .send_markdown_to_user(&event.reply_id, "ARCC", &text)
                .await
                .map_err(|e| e.to_string())
        }
    }
}
