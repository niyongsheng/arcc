//! Feishu implementation of the shared `ChatBackend` transport trait.

use async_trait::async_trait;
use serde_json::json;

use arcc_core::feishu_client::FeishuClient;

use crate::im::{ChatBackend, ChatEvent};

pub struct FeishuBackend<'a> {
    pub client: &'a FeishuClient,
}

#[async_trait]
impl ChatBackend for FeishuBackend<'_> {
    fn platform(&self) -> &'static str {
        "feishu"
    }

    async fn send(&self, event: &ChatEvent, text: &str) -> Result<(), String> {
        // In group chats, prepend an @mention so the sender gets notified.
        let text = if event.chat_type == "group" {
            format!("<at id=\"{}\"></at> {text}", event.user_id)
        } else {
            text.to_owned()
        };

        self.client
            .send_message_to(
                &event.reply_id,
                &event.reply_id_type,
                post_md(&text),
                "post",
            )
            .await
            .map_err(|e| e.to_string())
    }
}

/// Build the content body for a Feishu `msg_type: "post"` message.
///
/// The `msg_type: "post"` already indicates this is a rich-text post, so the
/// content JSON string has NO outer `"post"` wrapper — it starts with the
/// language key (`zh_cn`) directly. Uses `tag: md` for CommonMark/GFM.
///
/// Note: Feishu's `md` tag uses `text` (not `content`) as the field name
/// for the markdown body. Using `content` causes `text field can't be nil`.
fn post_md(md_text: &str) -> serde_json::Value {
    let safe_text = if md_text.trim().is_empty() {
        "(no content)"
    } else {
        md_text
    };
    json!({
        "zh_cn": {
            "title": "ARCC",
            "content": [[
                { "tag": "md", "text": safe_text }
            ]]
        }
    })
}
