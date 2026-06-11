use serde::Serialize;
use serde_json::json;

/// Feishu interactive message card — used for human-in-the-loop confirmations.
#[derive(Debug, Clone, Serialize)]
pub struct ConfirmCard {
    pub msg_type: String,
    pub content: ConfirmCardContent,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfirmCardContent {
    pub title: String,
    pub text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<CardElement>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "tag")]
pub enum CardElement {
    #[serde(rename = "button")]
    Button {
        text: ButtonText,
        #[serde(rename = "type")]
        button_type: String,
        value: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ButtonText {
    pub tag: String,
    pub text: String,
}

/// Build a card showing that an operation was approved (replaces buttons with
/// a green status message). Returns a `serde_json::Value` suitable for
/// `update_message`.
pub fn build_approved_card(action: &str, details: &str) -> serde_json::Value {
    json!({
        "config": { "wide_screen_mode": true },
        "header": {
            "title": { "tag": "plain_text", "content": "✅ 操作已批准" },
            "template": "green"
        },
        "elements": [
            { "tag": "div", "text": { "tag": "lark_md", "content": format!("**操作**: {action}\n\n**状态**: ✅ 已批准\n\n{details}") } }
        ]
    })
}

/// Build a card showing that an operation was denied (replaces buttons with
/// a red status message). Returns a `serde_json::Value` suitable for
/// `update_message`.
pub fn build_denied_card(action: &str, details: &str) -> serde_json::Value {
    json!({
        "config": { "wide_screen_mode": true },
        "header": {
            "title": { "tag": "plain_text", "content": "❌ 操作已拒绝" },
            "template": "red"
        },
        "elements": [
            { "tag": "div", "text": { "tag": "lark_md", "content": format!("**操作**: {action}\n\n**状态**: ❌ 已拒绝\n\n{details}") } }
        ]
    })
}

/// Build a "allow / deny" confirmation card for high-risk operations.
pub fn build_confirm_card(
    session_id: &str,
    action: &str,
    details: &str,
) -> ConfirmCard {
    ConfirmCard {
        msg_type: "interactive".into(),
        content: ConfirmCardContent {
            title: "⚠️ 高危操作确认".into(),
            text: format!(
                "**操作**: {action}\n\n**详情**: {details}\n\n请在下方确认是否允许执行此操作。"
            ),
            elements: vec![
                CardElement::Button {
                    text: ButtonText {
                        tag: "plain_text".into(),
                        text: "✅ 允许执行".into(),
                    },
                    button_type: "primary".into(),
                    value: serde_json::json!({
                        "action": "approve",
                        "session_id": session_id,
                        "operation": action,
                    }),
                },
                CardElement::Button {
                    text: ButtonText {
                        tag: "plain_text".into(),
                        text: "❌ 拒绝".into(),
                    },
                    button_type: "danger".into(),
                    value: serde_json::json!({
                        "action": "deny",
                        "session_id": session_id,
                        "operation": action,
                    }),
                },
            ],
        },
    }
}
