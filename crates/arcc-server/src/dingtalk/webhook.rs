//! DingTalk (钉钉) HTTP callback handler.
//!
//! Two callback generations are supported:
//!
//! - **Modern robot callbacks** — plaintext JSON body signed via the
//!   `timestamp` and `sign` headers:
//!   `sign = Base64(HMAC-SHA256(appSecret, timestamp + "\n" + appSecret))`.
//! - **Legacy event callbacks** — AES-encrypted body (`{"encrypt": "..."}`)
//!   with `msg_signature = SHA1(sort(token, timeStamp, nonce, encrypt))`.
//!
//! The encrypted envelope is:
//!
//! ```text
//! random(16B) | msg_len(4B, big-endian) | msg | trailing id
//! ```
//!
//! Text messages are forwarded to the shared IM pipeline, which responds
//! asynchronously through the event's `sessionWebhook` (or the robot API for
//! scheduled tasks). The HTTP handler itself returns 200 immediately so
//! DingTalk's 5-second callback deadline is never exceeded.

use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use tracing::{info, warn};

use arcc_core::context::SharedContext;

use crate::im::{ChatEvent, chat_queue};

const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type HmacSha256 = Hmac<Sha256>;

/// Query parameters attached to every DingTalk callback.
#[derive(Debug, Default, Deserialize)]
pub struct CallbackQuery {
    #[serde(default, alias = "signature")]
    pub msg_signature: Option<String>,
    #[serde(default, alias = "timestamp", rename = "timeStamp")]
    pub time_stamp: Option<String>,
    #[serde(default)]
    pub nonce: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TextContent {
    #[serde(default)]
    content: Option<String>,
}

/// Decrypted DingTalk robot message payload.
#[derive(Debug, Default, Deserialize)]
struct DingtalkMessage {
    #[serde(default)]
    msgtype: Option<String>,
    #[serde(default)]
    text: Option<TextContent>,
    #[serde(default, rename = "senderStaffId")]
    sender_staff_id: Option<String>,
    #[serde(default, rename = "senderId")]
    sender_id: Option<String>,
    #[serde(default, rename = "conversationId")]
    conversation_id: Option<String>,
    #[serde(default, rename = "conversationType")]
    conversation_type: Option<String>,
    #[serde(default, rename = "sessionWebhook")]
    session_webhook: Option<String>,
    #[serde(default, rename = "isInAtList")]
    is_in_at_list: Option<bool>,
    #[serde(default, rename = "msgId")]
    msg_id: Option<String>,
    #[serde(default, rename = "eventType", alias = "EventType")]
    event_type: Option<String>,
}

/// POST /dingtalk/webhook — DingTalk enterprise internal robot callback.
pub async fn handler(
    State(ctx): State<SharedContext>,
    Query(query): Query<CallbackQuery>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let config = &ctx.storage.config.dingtalk;

    let value: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!(err = %e, "failed to parse dingtalk callback body");
            return (StatusCode::BAD_REQUEST, "invalid json").into_response();
        }
    };

    // --- Request authentication -------------------------------------------
    // Modern callbacks carry `timestamp` + `sign` headers; legacy encrypted
    // callbacks carry a `msg_signature` query parameter.
    let mut verified = false;

    if let (Some(timestamp), Some(sign_value)) = (
        headers.get("timestamp").and_then(|v| v.to_str().ok()),
        headers.get("sign").and_then(|v| v.to_str().ok()),
    ) {
        if config.client_secret.is_empty() {
            warn!("dingtalk callback is signed but [dingtalk].client_secret is empty");
            return (StatusCode::FORBIDDEN, "client_secret not configured").into_response();
        }
        if !verify_header_signature(&config.client_secret, timestamp, sign_value) {
            warn!("dingtalk callback header signature mismatch");
            return (StatusCode::FORBIDDEN, "invalid signature").into_response();
        }
        verified = true;
    }

    let encrypt = value.get("encrypt").and_then(|v| v.as_str());

    let (message, encrypted) = match encrypt {
        Some(encrypt) => {
            if config.aes_key.is_empty() {
                warn!("dingtalk callback is encrypted but [dingtalk].aes_key is empty");
                return (StatusCode::BAD_REQUEST, "aes_key not configured").into_response();
            }

            if !verified && !config.token.is_empty() {
                let signature = query.msg_signature.as_deref().unwrap_or("");
                let timestamp = query.time_stamp.as_deref().unwrap_or("");
                let nonce = query.nonce.as_deref().unwrap_or("");
                let expected = sign(&config.token, timestamp, nonce, encrypt);
                if !expected.eq_ignore_ascii_case(signature) {
                    warn!("dingtalk callback msg_signature mismatch");
                    return (StatusCode::FORBIDDEN, "invalid signature").into_response();
                }
                verified = true;
            }

            // Once credentials are configured, refuse unverified payloads.
            if !verified && (!config.token.is_empty() || !config.client_secret.is_empty()) {
                warn!("dingtalk callback could not be verified");
                return (StatusCode::FORBIDDEN, "unverified callback").into_response();
            }

            match decrypt_message(&config.aes_key, encrypt) {
                Ok(plain) => match serde_json::from_str::<DingtalkMessage>(&plain) {
                    Ok(msg) => (msg, true),
                    Err(e) => {
                        warn!(err = %e, plain = %plain, "failed to parse decrypted dingtalk payload");
                        return (StatusCode::BAD_REQUEST, "invalid payload").into_response();
                    }
                },
                Err(e) => {
                    warn!(err = %e, "failed to decrypt dingtalk callback");
                    return (StatusCode::BAD_REQUEST, "decrypt failed").into_response();
                }
            }
        }
        None => {
            // Plaintext callbacks must carry the header signature unless no
            // secret is configured (local testing).
            if !verified && !config.client_secret.is_empty() {
                warn!("dingtalk plaintext callback missing signature");
                return (StatusCode::FORBIDDEN, "missing signature").into_response();
            }
            match serde_json::from_value::<DingtalkMessage>(value) {
                Ok(msg) => (msg, false),
                Err(e) => {
                    warn!(err = %e, "failed to parse plaintext dingtalk payload");
                    return (StatusCode::BAD_REQUEST, "invalid payload").into_response();
                }
            }
        }
    };

    // URL verification handshake performed by the DingTalk console.
    if message.event_type.as_deref() == Some("check_url") {
        if !encrypted {
            return (StatusCode::OK, "success").into_response();
        }
        let owner_key = if config.corp_id.is_empty() {
            &config.client_id
        } else {
            &config.corp_id
        };
        return match encrypt_response("success", &config.aes_key, owner_key, &config.token) {
            Ok(reply) => (StatusCode::OK, Json(reply)).into_response(),
            Err(e) => {
                warn!(err = %e, "failed to encrypt dingtalk check_url response");
                (StatusCode::INTERNAL_SERVER_ERROR, "encrypt failed").into_response()
            }
        };
    }

    if message.msgtype.as_deref() != Some("text") {
        info!(msgtype = ?message.msgtype, "ignoring non-text dingtalk message");
        return StatusCode::OK.into_response();
    }

    let user_text = message
        .text
        .as_ref()
        .and_then(|t| t.content.as_deref())
        .unwrap_or("")
        .trim()
        .to_owned();
    if user_text.is_empty() {
        warn!("empty text content in dingtalk message");
        return StatusCode::OK.into_response();
    }

    let is_group = message.conversation_type.as_deref() == Some("2");
    // Some callback versions omit `isInAtList` while only delivering
    // @-mentioned group messages, so treat absent as "addressed to the bot".
    if is_group && message.is_in_at_list == Some(false) {
        info!("ignoring dingtalk group message without @mention");
        return StatusCode::OK.into_response();
    }

    let conversation_id = message.conversation_id.unwrap_or_default();
    let staff_id = match message
        .sender_staff_id
        .filter(|s| !s.is_empty())
        .or(message.sender_id)
    {
        Some(id) if !id.is_empty() => id,
        _ => {
            warn!("dingtalk callback missing sender id");
            return StatusCode::OK.into_response();
        }
    };
    if conversation_id.is_empty() {
        warn!("dingtalk callback missing conversationId");
        return StatusCode::OK.into_response();
    }

    let chat_type = if is_group { "group" } else { "p2p" };
    let (reply_id, reply_id_type) = if is_group {
        (conversation_id.clone(), "dingtalk_conversation_id")
    } else {
        (staff_id.clone(), "dingtalk_staff_id")
    };

    info!(
        conversation_id = %conversation_id,
        chat_type,
        len = user_text.len(),
        "dingtalk message received"
    );

    chat_queue()
        .enqueue(
            ctx.clone(),
            ChatEvent {
                platform: "dingtalk",
                // Namespace the session key so a DingTalk conversation id can
                // never collide with a Feishu chat id.
                chat_id: format!("dingtalk:{conversation_id}"),
                chat_type: chat_type.to_owned(),
                user_id: staff_id,
                message_id: message.msg_id.unwrap_or_default(),
                reply_id,
                reply_id_type: reply_id_type.to_owned(),
                session_webhook: message.session_webhook,
                user_text,
            },
        )
        .await;

    StatusCode::OK.into_response()
}

// ---------------------------------------------------------------------------
// Crypto
// ---------------------------------------------------------------------------

/// Decode the 43-character DingTalk AES key into 32 key bytes.
fn decode_aes_key(aes_key: &str) -> Result<Vec<u8>, String> {
    let key = BASE64
        .decode(format!("{aes_key}="))
        .map_err(|e| format!("invalid aes_key: {e}"))?;
    if key.len() != 32 {
        return Err(format!(
            "aes_key must decode to 32 bytes, got {}",
            key.len()
        ));
    }
    Ok(key)
}

/// Verify the legacy callback signature. `msg_signature` is the hex SHA1 of
/// the token, timestamp, nonce, and ciphertext sorted lexicographically.
fn sign(token: &str, timestamp: &str, nonce: &str, encrypt: &str) -> String {
    let mut parts = [token, timestamp, nonce, encrypt];
    parts.sort_unstable();
    let mut hasher = Sha1::new();
    for part in parts {
        hasher.update(part.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Verify a modern robot callback's `timestamp` / `sign` headers.
///
/// `sign = Base64(HMAC-SHA256(appSecret, timestamp + "\n" + appSecret))`,
/// with a one-hour freshness window.
fn verify_header_signature(app_secret: &str, timestamp: &str, sign_value: &str) -> bool {
    let Ok(timestamp_ms) = timestamp.parse::<i64>() else {
        return false;
    };
    let now = chrono::Utc::now().timestamp_millis();
    if (now - timestamp_ms).abs() > 3_600_000 {
        warn!(timestamp, now, "dingtalk callback outside the 1h window");
        return false;
    }

    let Ok(sign_bytes) = BASE64.decode(sign_value) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(app_secret.as_bytes()) else {
        return false;
    };
    mac.update(format!("{timestamp}\n{app_secret}").as_bytes());
    mac.verify_slice(&sign_bytes).is_ok()
}

/// Decrypt a callback payload and extract the JSON message body.
fn decrypt_message(aes_key: &str, encrypt: &str) -> Result<String, String> {
    let key = decode_aes_key(aes_key)?;
    let iv = &key[..16];

    let mut buffer = BASE64
        .decode(encrypt)
        .map_err(|e| format!("invalid ciphertext encoding: {e}"))?;
    let plain = Aes256CbcDec::new_from_slices(&key, iv)
        .map_err(|e| format!("invalid key/iv: {e}"))?
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|e| format!("decryption failed: {e}"))?;

    // Envelope: random(16) | msg_len(4, BE) | msg | trailing id.
    if plain.len() >= 20 {
        let len = u32::from_be_bytes([plain[16], plain[17], plain[18], plain[19]]) as usize;
        if len <= plain.len() - 20 {
            return String::from_utf8(plain[20..20 + len].to_vec())
                .map_err(|e| format!("payload is not utf-8: {e}"));
        }
    }

    // Tolerate non-enveloped payloads.
    String::from_utf8(plain.to_vec()).map_err(|e| format!("payload is not utf-8: {e}"))
}

/// Encrypt a plaintext reply for the `check_url` handshake.
fn encrypt_response(
    message: &str,
    aes_key: &str,
    client_id: &str,
    token: &str,
) -> Result<serde_json::Value, String> {
    let key = decode_aes_key(aes_key)?;
    let iv = &key[..16];

    let uuid = uuid::Uuid::new_v4().simple().to_string();
    let random = &uuid[..16];
    let nonce = &uuid[16..24];
    let timestamp = chrono::Utc::now().timestamp_millis().to_string();

    let mut plain = Vec::with_capacity(20 + message.len() + client_id.len());
    plain.extend_from_slice(random.as_bytes());
    plain.extend_from_slice(&(message.len() as u32).to_be_bytes());
    plain.extend_from_slice(message.as_bytes());
    plain.extend_from_slice(client_id.as_bytes());

    let cipher = Aes256CbcEnc::new_from_slices(&key, iv)
        .map_err(|e| format!("invalid key/iv: {e}"))?
        .encrypt_padded_vec_mut::<Pkcs7>(&plain);
    let encrypt = BASE64.encode(cipher);
    let signature = sign(token, &timestamp, nonce, &encrypt);

    Ok(json!({
        "msg_signature": signature,
        "timeStamp": timestamp,
        "nonce": nonce,
        "encrypt": encrypt,
    }))
}

// ---------------------------------------------------------------------------
// Proactive send
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SendRequest {
    pub user_id: String,
    pub text: String,
}

#[derive(Debug, serde::Serialize)]
pub struct SendResponse {
    pub success: bool,
    pub error: Option<String>,
}

/// POST /dingtalk/send — proactively send a markdown message to a DingTalk user.
pub async fn send_handler(
    State(ctx): State<SharedContext>,
    Json(body): Json<SendRequest>,
) -> Json<SendResponse> {
    let Some(client) = ctx.dingtalk_client.as_ref() else {
        return Json(SendResponse {
            success: false,
            error: Some("DingTalk client not configured".into()),
        });
    };

    match client
        .send_markdown_to_user(&body.user_id, "ARCC", &body.text)
        .await
    {
        Ok(()) => Json(SendResponse {
            success: true,
            error: None,
        }),
        Err(e) => Json(SendResponse {
            success: false,
            error: Some(e.to_string()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_aes_key() -> String {
        BASE64.encode([7u8; 32]).trim_end_matches('=').to_owned()
    }

    #[test]
    fn parses_robot_message_payload() {
        let payload = r#"{
            "msgtype": "text",
            "text": {"content": " /status "},
            "senderStaffId": "staff-1",
            "conversationId": "cid-1",
            "conversationType": "2",
            "isInAtList": true,
            "sessionWebhook": "https://oapi.dingtalk.com/robot/sendBySession?session=x",
            "msgId": "msg-1"
        }"#;

        let message: DingtalkMessage = serde_json::from_str(payload).expect("parse payload");
        assert_eq!(message.msgtype.as_deref(), Some("text"));
        assert_eq!(
            message.text.as_ref().and_then(|t| t.content.as_deref()),
            Some(" /status ")
        );
        assert_eq!(message.sender_staff_id.as_deref(), Some("staff-1"));
        assert_eq!(message.conversation_id.as_deref(), Some("cid-1"));
        assert_eq!(message.conversation_type.as_deref(), Some("2"));
        assert_eq!(message.is_in_at_list, Some(true));
        assert!(message.session_webhook.is_some());
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = test_aes_key();
        assert_eq!(key.len(), 43);

        let reply =
            encrypt_response("success", &key, "client-id", "token").expect("encrypt response");
        let encrypt = reply["encrypt"].as_str().expect("encrypt field");

        assert_eq!(decrypt_message(&key, encrypt).expect("decrypt"), "success");

        let signature = reply["msg_signature"].as_str().expect("signature");
        let timestamp = reply["timeStamp"].as_str().expect("timestamp");
        let nonce = reply["nonce"].as_str().expect("nonce");
        assert!(signature.eq_ignore_ascii_case(&sign("token", timestamp, nonce, encrypt)));
    }

    #[test]
    fn signature_rejects_tampering() {
        let sig = sign("token", "123", "nonce", "ciphertext");
        assert_eq!(sig, sign("token", "123", "nonce", "ciphertext"));
        assert_ne!(sig, sign("token", "124", "nonce", "ciphertext"));
    }

    #[test]
    fn header_signature_roundtrip() {
        let secret = "test-secret";
        let timestamp = chrono::Utc::now().timestamp_millis().to_string();

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key");
        mac.update(format!("{timestamp}\n{secret}").as_bytes());
        let signature = BASE64.encode(mac.finalize().into_bytes());

        assert!(verify_header_signature(secret, &timestamp, &signature));
        assert!(!verify_header_signature(secret, &timestamp, "invalid"));
        assert!(!verify_header_signature(
            "other-secret",
            &timestamp,
            &signature
        ));

        // Stale timestamps are rejected.
        let stale = (chrono::Utc::now().timestamp_millis() - 2 * 3_600_000).to_string();
        assert!(!verify_header_signature(secret, &stale, &signature));
    }
}
