//! ACP (Agent Client Protocol v1) wire types and builders.
//!
//! Transport: JSON-RPC 2.0 over stdio, one JSON document per line
//! (`\n`-delimited, UTF-8). Only ACP messages go to stdout — every log
//! line goes to stderr. Batches (JSON arrays) are accepted on input.
//!
//! This module deliberately does NOT reuse `arcc_core::mcp::protocol`:
//! its `id: u64` and MCP-specific shapes don't fit ACP's
//! string/number/null ids, notifications and batches. All types here are
//! plain data + builders — no I/O.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// JSON-RPC error codes
// ---------------------------------------------------------------------------

/// Invalid JSON was received by the server.
pub const PARSE_ERROR: i32 = -32700;
/// The JSON sent is not a valid Request object.
pub const INVALID_REQUEST: i32 = -32600;
/// The method does not exist / is not implemented.
pub const METHOD_NOT_FOUND: i32 = -32601;
/// Invalid method parameter(s).
pub const INVALID_PARAMS: i32 = -32602;
/// Internal JSON-RPC error (reserved implementation-defined range start).
/// Not currently produced — kept as part of the wire error-code table.
#[allow(dead_code)]
pub const INTERNAL_ERROR: i32 = -32000;
/// Custom: the referenced session does not exist (or was closed).
pub const SESSION_NOT_FOUND: i32 = -32001;
/// Custom: the session already has a running prompt — concurrent
/// `session/prompt` calls are rejected.
pub const SESSION_BUSY: i32 = -32004;

// ---------------------------------------------------------------------------
// Envelope types
// ---------------------------------------------------------------------------

/// Incoming request or notification.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

impl RpcRequest {
    /// Notifications carry no `id` — the client does not expect a response.
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// Outgoing response to a client request.
#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// Successful response: `{jsonrpc: "2.0", id, result}`.
pub fn response_ok(id: Value, result: Value) -> Value {
    serde_json::to_value(RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    })
    .expect("rpc response is serializable")
}

/// Error response: `{jsonrpc: "2.0", id, error: {code, message}}`.
pub fn response_err(id: Value, code: i32, message: impl Into<String>) -> Value {
    serde_json::to_value(RpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(RpcError {
            code,
            message: message.into(),
        }),
    })
    .expect("rpc response is serializable")
}

/// Outbound notification: `{jsonrpc: "2.0", method, params}`.
pub fn notify(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

// ---------------------------------------------------------------------------
// session/update notifications
// ---------------------------------------------------------------------------

/// `session/update` notification wrapping a typed `sessionUpdate` union.
///
/// Wire format follows `agent_client_protocol_schema::v1` exactly: the
/// `SessionUpdate` enum is internally tagged on a **`sessionUpdate`** key
/// whose value is the snake_case variant identifier (a plain string) with
/// the variant's fields flattened into the same object — NOT a nested
/// `{"type": ...}` object. AionUI rejects the nested form with
/// "invalid type: map, expected variant identifier".
pub fn session_update(session_id: &str, session_update: Value) -> Value {
    notify(
        "session/update",
        json!({
            "sessionId": session_id,
            "update": session_update,
        }),
    )
}

/// Streamed assistant text — `agent_message_chunk`.
pub fn agent_message_chunk(message_id: Option<&str>, text: &str) -> Value {
    let mut update = json!({
        "sessionUpdate": "agent_message_chunk",
        "content": { "type": "text", "text": text },
    });
    if let Some(id) = message_id {
        update["messageId"] = json!(id);
    }
    update
}

/// Streamed model reasoning — `agent_thought_chunk`.
pub fn agent_thought_chunk(message_id: Option<&str>, text: &str) -> Value {
    let mut update = json!({
        "sessionUpdate": "agent_thought_chunk",
        "content": { "type": "text", "text": text },
    });
    if let Some(id) = message_id {
        update["messageId"] = json!(id);
    }
    update
}

/// Tool lifecycle start — `tool_call`, default `kind: "execute"` and
/// `status: "pending"`.
pub fn tool_call_status(
    tool_call_id: &str,
    title: &str,
    kind: &str,
    status: &str,
    raw_input: Option<&str>,
) -> Value {
    let mut update = json!({
        "sessionUpdate": "tool_call",
        "toolCallId": tool_call_id,
        "title": title,
    });
    if kind != "execute" {
        update["kind"] = json!(kind);
    }
    if status != "pending" {
        update["status"] = json!(status);
    }
    if let Some(raw) = raw_input {
        update["rawInput"] = json!(raw);
    }
    update
}

/// Tool lifecycle change — `tool_call_update`.
///
/// `status` is one of `pending` / `in_progress` / `completed` / `failed`
/// (the official `ToolCallStatus`). Arbitrary per-tool results go in
/// `rawOutput` — the official `content` field is a `ToolCallContent`
/// array with its own tagging rules, which we deliberately avoid.
pub fn tool_call_update(
    tool_call_id: &str,
    status: &str,
    raw_output: Option<Value>,
) -> Value {
    let mut update = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": tool_call_id,
        "status": status,
    });
    if let Some(r) = raw_output {
        update["rawOutput"] = r;
    }
    update
}

/// Context-window usage — `usage_update`: `used` = tokens consumed so far,
/// `size` = the session's context budget (compression threshold).
pub fn usage_update(used: usize, size: usize) -> Value {
    json!({ "sessionUpdate": "usage_update", "used": used, "size": size })
}

// ---------------------------------------------------------------------------
// Prompt input
// ---------------------------------------------------------------------------

/// One entry of `session/prompt`'s `prompt` array, tagged on `type`.
///
/// Unknown block types (image, file, …) are accepted and ignored so a
/// client can never crash the parser by sending a newer block shape.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_vs_notification() {
        let req: RpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).unwrap();
        assert!(!req.is_notification());
        let note: RpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"session/cancel"}"#).unwrap();
        assert!(note.is_notification());
    }

    #[test]
    fn string_id_supported() {
        let req: RpcRequest = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":"abc","method":"session/new","params":{"cwd":"/tmp"}}"#,
        )
        .unwrap();
        assert_eq!(req.id, Some(Value::String("abc".into())));
        assert_eq!(req.params.unwrap()["cwd"], "/tmp");
    }

    #[test]
    fn content_block_tagged() {
        let b: ContentBlock = serde_json::from_str(r#"{"type":"text","text":"hi"}"#).unwrap();
        match b {
            ContentBlock::Text { text } => assert_eq!(text, "hi"),
            ContentBlock::Other => panic!("wrong variant"),
        }
        let img: ContentBlock = serde_json::from_str(r#"{"type":"image","source":{}}"#).unwrap();
        assert!(matches!(img, ContentBlock::Other));
    }

    #[test]
    fn session_update_wraps_union() {
        let v = session_update("s1", agent_message_chunk(None, "hello"));
        assert_eq!(v["method"], "session/update");
        assert_eq!(v["params"]["sessionId"], "s1");
        // The variant identifier is the value of `sessionUpdate` (a string),
        // with variant fields flattened alongside it — the official schema.
        assert_eq!(v["params"]["update"]["sessionUpdate"], "agent_message_chunk");
        assert_eq!(
            v["params"]["update"]["content"]["text"],
            "hello"
        );
    }

    #[test]
    fn session_update_variants_match_official_schema() {
        // Spot-check every builder against the tagged-union shape.
        let v = session_update("s1", agent_thought_chunk(Some("m1"), "hmm"));
        assert_eq!(v["params"]["update"]["sessionUpdate"], "agent_thought_chunk");
        assert_eq!(v["params"]["update"]["messageId"], "m1");

        let v = session_update("s1", tool_call_status("t1", "ls", "execute", "pending", Some("ls")));
        assert_eq!(v["params"]["update"]["sessionUpdate"], "tool_call");
        assert_eq!(v["params"]["update"]["toolCallId"], "t1");
        assert_eq!(v["params"]["update"]["title"], "ls");
        assert_eq!(v["params"]["update"]["rawInput"], "ls");
        // Defaults are omitted.
        assert!(v["params"]["update"].get("kind").is_none());
        assert!(v["params"]["update"].get("status").is_none());

        let v = session_update("s1", tool_call_update("t1", "in_progress", None));
        assert_eq!(v["params"]["update"]["sessionUpdate"], "tool_call_update");
        assert_eq!(v["params"]["update"]["status"], "in_progress");

        let v = session_update("s1", tool_call_update("t1", "completed", Some(json!({"exit": 0}))));
        assert_eq!(v["params"]["update"]["rawOutput"]["exit"], 0);

        let v = session_update("s1", usage_update(10, 100));
        assert_eq!(v["params"]["update"]["sessionUpdate"], "usage_update");
        assert_eq!(v["params"]["update"]["used"], 10);
        assert_eq!(v["params"]["update"]["size"], 100);
    }
}
