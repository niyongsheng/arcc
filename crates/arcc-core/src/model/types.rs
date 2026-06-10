use serde::{Deserialize, Serialize};

/// Unified chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

/// Tool call request (compatible with MCP).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Unified chat request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    pub stream: bool,
    /// Thinking mode: `"off"` (disable, no reasoning_content) or
    /// `"enabled"` (enable chain-of-thought).  None = model default.
    /// Set to `"off"` for pure function-calling workloads — this
    /// eliminates DSML leakage at the source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_mode: Option<String>,
    /// Reasoning effort: `"high"` (default) or `"max"`.
    /// Only meaningful when thinking is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// Unified chat response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub message: ChatMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    pub usage: Usage,
}

/// Token usage statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

/// Streaming response chunk.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    Content(String),
    Reasoning(String),
    ToolCallStart(ToolCall),
    ToolCallEnd { id: String, output: String },
    Finish(Usage),
}

/// Tool definition for function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    /// When `true`, the model will strictly follow the JSON Schema.
    /// Requires the Beta endpoint (`https://api.deepseek.com/beta`).
    /// See: https://api-docs.deepseek.com/guides/tool_calls#strict-mode
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
}
