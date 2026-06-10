//! DeepSeek-V4 provider implementation.
//!
//! Supports dual-model dispatch: Pro (deep reasoning) vs Flash (fast chat).
//! API is OpenAI-compatible with `reasoning_content` extension.

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tracing::{debug, warn};

use super::provider::{ModelError, ModelProvider};
use super::types::{ChatMessage, ChatRequest, ChatResponse, StreamChunk, ToolCall, Usage};
use super::dsml;

// ---------------------------------------------------------------------------
// DeepSeek API types (OpenAI-compatible JSON)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct DeepSeekRequest<'a> {
    model: &'a str,
    messages: &'a [DeepSeekMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [DeepSeekTool]>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "tool_choice")]
    tool_choice: Option<&'a serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
    /// Thinking mode: `{"type": "disabled"}` or `{"type": "enabled"}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
    /// Reasoning effort: `"high"` or `"max"` (only when thinking is enabled).
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
}

/// Thinking mode control (V4 unified API).
///
/// DeepSeek-V4 expects `{"type": "enabled"}` / `{"type": "disabled"}`.
/// - `"enabled"` — emit `reasoning_content` (chain-of-thought).
/// - `"disabled"` — suppress reasoning, pure function calling.
#[derive(Serialize)]
struct ThinkingConfig {
    #[serde(rename = "type")]
    mode: String,
}

#[derive(Serialize, Clone)]
struct DeepSeekMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
}

#[derive(Serialize)]
struct DeepSeekTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: DeepSeekFunction,
}

#[derive(Serialize)]
struct DeepSeekFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    strict: bool,
}

// --- Non-streaming response ---

#[allow(dead_code)] // fields read via serde
#[derive(Deserialize)]
struct DeepSeekResponse {
    choices: Vec<DeepSeekChoice>,
    #[serde(default)]
    usage: Option<DeepSeekUsage>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct DeepSeekChoice {
    message: DeepSeekRespMessage,
    finish_reason: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct DeepSeekRespMessage {
    role: String,
    content: String,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<DeepSeekToolCallResp>>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
struct DeepSeekToolCallResp {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: DeepSeekFunctionCallResp,
}

#[derive(Deserialize)]
struct DeepSeekFunctionCallResp {
    name: String,
    arguments: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct DeepSeekUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

// --- Streaming delta ---

#[derive(Deserialize, Debug)]
struct DeepSeekStreamChunk {
    choices: Vec<DeepSeekStreamChoice>,
    #[serde(default)]
    usage: Option<DeepSeekUsage>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct DeepSeekStreamChoice {
    delta: DeepSeekStreamDelta,
    #[serde(default)]
    index: u32,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Default)]
struct DeepSeekStreamDelta {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<DeepSeekStreamToolCall>>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
struct DeepSeekStreamToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type", default)]
    call_type: Option<String>,
    #[serde(default)]
    function: Option<DeepSeekStreamFunction>,
}

#[derive(Deserialize, Debug)]
struct DeepSeekStreamFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct DeepSeekProvider {
    client: reqwest::Client,
    api_base: String,
    api_key: String,
    model: String,
    /// When true, use the Beta endpoint and emit `strict: true` on every
    /// tool definition.  Requires the server-side strict-mode contract.
    strict_mode: bool,
}

impl DeepSeekProvider {
    pub fn new(api_base: &str, api_key: &str, model: &str) -> Self {
        Self::with_strict_mode(api_base, api_key, model, false)
    }

    pub fn with_strict_mode(api_base: &str, api_key: &str, model: &str, strict_mode: bool) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest::Client::build");
        Self {
            client,
            api_base: api_base.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            strict_mode,
        }
    }

    fn chat_url(&self) -> String {
        if self.strict_mode {
            format!("{}/beta/v1/chat/completions", self.api_base)
        } else {
            format!("{}/v1/chat/completions", self.api_base)
        }
    }

    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.api_key))
                .expect("invalid API key characters"),
        );
        headers
    }

    fn convert_messages(msgs: &[ChatMessage]) -> Vec<DeepSeekMessage> {
        msgs.iter()
            .map(|m| DeepSeekMessage {
                role: m.role.clone(),
                content: m.content.clone(),
                tool_calls: m.tool_calls.as_ref().map(|tc_list| {
                    serde_json::Value::Array(
                        tc_list.iter().map(|tc| {
                            serde_json::json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": tc.arguments.to_string(),
                                }
                            })
                        }).collect()
                    )
                }),
                tool_call_id: m.tool_call_id.clone(),
                // reasoning_content is always passed through — the API
                // ignores it when thinking is off.  When thinking is on,
                // it MUST be present for tool-call turns (otherwise 400).
                reasoning_content: m.reasoning_content.clone(),
            })
            .collect()
    }

    fn convert_tools(&self, tools: &[super::types::ToolDefinition]) -> Vec<DeepSeekTool> {
        tools
            .iter()
            .map(|t| DeepSeekTool {
                tool_type: "function".into(),
                function: DeepSeekFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                    strict: self.strict_mode || t.strict,
                },
            })
            .collect()
    }

    // --- exponential backoff helper ---
    async fn with_retry<F, Fut, T>(&self, mut f: F) -> Result<T, ModelError>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T, ModelError>>,
    {
        let mut attempt = 0u32;
        let max_retries = 3u32;
        let mut delay = Duration::from_secs(1);

        loop {
            match f().await {
                Ok(v) => return Ok(v),
                Err(ModelError::RateLimited { retry_after_secs }) => {
                    if attempt >= max_retries {
                        return Err(ModelError::RateLimited { retry_after_secs });
                    }
                    let wait = Duration::from_secs(retry_after_secs).max(delay);
                    warn!(attempt, wait_ms = wait.as_millis(), "rate limited, retrying");
                    tokio::time::sleep(wait).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ModelProvider impl
// ---------------------------------------------------------------------------

#[async_trait]
impl ModelProvider for DeepSeekProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ModelError> {
        let api_messages = Self::convert_messages(&req.messages);
        let api_tools: Option<Vec<DeepSeekTool>> =
            req.tools.as_ref().map(|t| self.convert_tools(t));
        let model = req.model.clone();
        let temperature = req.temperature;
        let max_tokens = req.max_tokens;

        self.with_retry(|| async {
            let thinking = req.thinking_mode.as_ref().map(|mode| ThinkingConfig {
                mode: mode.clone(),
            });
            let reasoning_effort = req.reasoning_effort.as_deref();

            let body = DeepSeekRequest {
                model: &model,
                messages: &api_messages,
                tools: api_tools.as_deref(),
                tool_choice: req.tool_choice.as_ref(),
                temperature,
                max_tokens,
                stream: false,
                thinking,
                reasoning_effort,
            };

            debug!(%model, msg_count = api_messages.len(), has_tools = api_tools.is_some(), "sending chat request");

            let resp = self
                .client
                .post(self.chat_url())
                .headers(self.auth_headers())
                .json(&body)
                .send()
                .await?;

            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(5);
                return Err(ModelError::RateLimited {
                    retry_after_secs: retry_after,
                });
            }

            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(ModelError::Api {
                    code: status.as_u16(),
                    message: text,
                });
            }

            let ds_resp: DeepSeekResponse = resp.json().await?;

            let choice = ds_resp
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| ModelError::Stream("no choices in response".into()))?;

            let usage = ds_resp.usage.map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
            }).unwrap_or_default();

            let mut tool_calls: Option<Vec<ToolCall>> = None;

            // 1. Native JSON tool_calls (OpenAI-compatible).
            if let Some(tc_list) = choice.message.tool_calls {
                let converted: Vec<ToolCall> = tc_list
                    .into_iter()
                    .map(|tc| ToolCall {
                        id: tc.id,
                        name: tc.function.name,
                        arguments: serde_json::from_str(&tc.function.arguments)
                            .unwrap_or(serde_json::Value::Null),
                    })
                    .collect();
                tool_calls = Some(converted);
            }

            // 2. DSML recovery: DeepSeek-V4 may emit tool calls as DSML
            //    markup embedded in `content` instead of native JSON.
            let (clean_content, dsml_calls) = dsml::extract_tool_calls(&choice.message.content);

            if !dsml_calls.is_empty() {
                tracing::debug!(
                    dsml_count = dsml_calls.len(),
                    "recovered tool calls from DSML in content"
                );
                match tool_calls.as_mut() {
                    Some(tc) => tc.extend(dsml_calls),
                    None => tool_calls = Some(dsml_calls),
                }
            }

            // Use cleaned content (DSML markup stripped).
            let display_content = if choice.message.content != clean_content {
                clean_content
            } else {
                choice.message.content
            };

            Ok(ChatResponse {
                message: ChatMessage {
                    role: choice.message.role,
                    content: display_content,
                    tool_calls,
                    tool_call_id: None,
                    // CRITICAL: reasoning_content must be included in the
                    // assistant message so it is passed back to the API in
                    // subsequent tool-call turns.  Omitting it causes a 400
                    // error from the DeepSeek API.
                    reasoning_content: choice.message.reasoning_content.clone(),
                },
                reasoning_content: choice.message.reasoning_content,
                usage,
            })
        })
        .await
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<
        Box<dyn Stream<Item = Result<StreamChunk, ModelError>> + Send + Unpin>,
        ModelError,
    > {
        let api_messages = Self::convert_messages(&req.messages);
        let api_tools: Option<Vec<DeepSeekTool>> =
            req.tools.as_ref().map(|t| self.convert_tools(t));

        let thinking = req.thinking_mode.as_ref().map(|mode| ThinkingConfig {
            mode: mode.clone(),
        });
        let reasoning_effort = req.reasoning_effort.as_deref();

        let body = DeepSeekRequest {
            model: &req.model,
            messages: &api_messages,
            tools: api_tools.as_deref(),
            tool_choice: req.tool_choice.as_ref(),
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            stream: true,
            thinking,
            reasoning_effort,
        };

        debug!(model = %req.model, msg_count = api_messages.len(), has_tools = api_tools.is_some(), "starting stream");

        let resp = self
            .client
            .post(self.chat_url())
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ModelError::Api {
                code: status.as_u16(),
                message: text,
            });
        }

        // Spawn a task that reads SSE bytes and sends parsed chunks.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<StreamChunk, ModelError>>(128);

        tokio::spawn(async move {
            let mut byte_stream = resp.bytes_stream();
            let mut buffer = Vec::new();
            let mut tool_builders: Vec<ToolCallAccumulator> = Vec::new();
            let mut dsml_acc = dsml::DsmlAccumulator::default();
            let mut reasoning_dsml_acc = dsml::DsmlAccumulator::default();
            let mut done = false;

            while !done {
                match byte_stream.next().await {
                    Some(Ok(bytes)) => {
                        buffer.extend_from_slice(&bytes);
                    }
                    Some(Err(e)) => {
                        let _ = tx.send(Err(ModelError::Http(e))).await;
                        return;
                    }
                    None => {
                        done = true;
                        // Flush DSML accumulators on stream end.
                        if let Some(remnant) = dsml_acc.flush() {
                            let _ = tx.send(Ok(StreamChunk::Content(remnant))).await;
                        }
                        if let Some(remnant) = reasoning_dsml_acc.flush() {
                            if !remnant.is_empty() {
                                let _ = tx.send(Ok(StreamChunk::Reasoning(remnant))).await;
                            }
                        }
                        continue;
                    }
                }

                // Drain complete SSE frames from the buffer.
                loop {
                    let pos = buffer.windows(2).position(|w| w == b"\n\n");
                    let Some(line_end) = pos else {
                        break;
                    };
                    let frame = buffer.drain(..line_end + 2).collect::<Vec<_>>();
                    let text = String::from_utf8_lossy(&frame).to_string();

                    for line in text.lines() {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        if line == "data: [DONE]" {
                            done = true;
                            // Flush any buffered DSML content before exiting.
                            if let Some(remnant) = dsml_acc.flush() {
                                let _ = tx.send(Ok(StreamChunk::Content(remnant))).await;
                            }
                            if let Some(remnant) = reasoning_dsml_acc.flush() {
                                if !remnant.is_empty() {
                                    let _ = tx.send(Ok(StreamChunk::Reasoning(remnant))).await;
                                }
                            }
                            break;
                        }
                        let Some(data) = line.strip_prefix("data: ") else {
                            continue;
                        };
                        let chunk: DeepSeekStreamChunk = match serde_json::from_str(data) {
                            Ok(c) => c,
                            Err(e) => {
                                warn!(%data, err = %e, "failed to parse SSE line");
                                continue;
                            }
                        };

                        for choice in chunk.choices {
                            let delta = choice.delta;
                            let is_tool_call_finish = choice.finish_reason
                                .as_deref() == Some("tool_calls");

                            // 1. Strip DSML from reasoning content.
                            if let Some(reasoning) = delta.reasoning_content
                                && !reasoning.is_empty() {
                                    let (clean_reasoning, _) = reasoning_dsml_acc.ingest(&reasoning);
                                    if let Some(clean) = clean_reasoning
                                        && !clean.is_empty() {
                                            let _ = tx
                                                .send(Ok(StreamChunk::Reasoning(clean)))
                                                .await;
                                        }
                                }

                            // 2. Feed content through DSML accumulator (always, even if this
                            //    chunk also carries a finish_reason).  This ensures a DSML
                            //    close tag that arrives in the same SSE frame as
                            //    finish_reason:"tool_calls" is ingested before flush.
                            if let Some(content) = delta.content
                                && !content.is_empty() {
                                    let (clean_content, dsml_tcs) = dsml_acc.ingest(&content);
                                    debug!(
                                        raw_len = content.len(),
                                        raw_first = %content.chars().take(8).collect::<String>(),
                                        clean = clean_content.as_deref().unwrap_or("(none)"),
                                        dsml_count = dsml_tcs.len(),
                                        "dsml accumulator ingested content"
                                    );

                                    if let Some(clean) = clean_content
                                        && !clean.is_empty() {
                                            let _ = tx
                                                .send(Ok(StreamChunk::Content(clean)))
                                                .await;
                                        }

                                    for tc in dsml_tcs {
                                        debug!(
                                            tool_name = %tc.name,
                                            "recovered tool call from DSML in stream"
                                        );
                                        let _ = tx
                                            .send(Ok(StreamChunk::ToolCallStart(tc)))
                                            .await;
                                    }
                                }

                            // 3. Accumulate native tool call deltas.
                            if let Some(tc_list) = delta.tool_calls {
                                for tc in tc_list {
                                    while tool_builders.len() <= tc.index {
                                        tool_builders.push(ToolCallAccumulator::default());
                                    }
                                    let tb = &mut tool_builders[tc.index];
                                    if let Some(id) = tc.id {
                                        tb.id = Some(id);
                                    }
                                    if let Some(func) = tc.function {
                                        if let Some(name) = func.name {
                                            tb.name = Some(name);
                                        }
                                        if let Some(args) = func.arguments {
                                            tb.arguments.push_str(&args);
                                        }
                                    }
                                }
                            }

                            // 4. Handle finish_reason (after content is already processed).
                            if is_tool_call_finish {
                                // Emit accumulated native tool calls.
                                for tb in &tool_builders {
                                    let tc = ToolCall {
                                        id: tb.id.clone().unwrap_or_default(),
                                        name: tb.name.clone().unwrap_or_default(),
                                        arguments: serde_json::from_str(&tb.arguments)
                                            .unwrap_or(serde_json::Value::String(
                                                tb.arguments.clone(),
                                            )),
                                    };
                                    let _ = tx
                                        .send(Ok(StreamChunk::ToolCallStart(tc)))
                                        .await;
                                }
                                tool_builders.clear();

                                // Flush DSML buffers (should be empty since content was
                                // ingested in step 2 above).
                                if let Some(remnant) = dsml_acc.flush() {
                                    if !remnant.is_empty() {
                                        debug!(remnant_len = remnant.len(), "dsml_acc flush had remnant — DSML close tag may have been split across chunks");
                                        let _ = tx
                                            .send(Ok(StreamChunk::Content(remnant)))
                                            .await;
                                    }
                                } else {
                                    debug!("dsml_acc flush clean (no remnant)");
                                }
                                if let Some(remnant) = reasoning_dsml_acc.flush() {
                                    if !remnant.is_empty() {
                                        debug!(remnant_len = remnant.len(), "reasoning_dsml_acc flush had remnant");
                                        let _ = tx
                                            .send(Ok(StreamChunk::Reasoning(remnant)))
                                            .await;
                                    }
                                }
                            }
                        }

                        // After choices, check usage → Finish
                        if let Some(usage) = chunk.usage {
                            let _ = tx
                                .send(Ok(StreamChunk::Finish(Usage {
                                    prompt_tokens: usage.prompt_tokens,
                                    completion_tokens: usage.completion_tokens,
                                })))
                                .await;
                        }
                    }
                }
            }
        });

        Ok(Box::new(ReceiverStream::new(rx)))
    }

    fn count_tokens(&self, text: &str) -> usize {
        match tiktoken_rs::o200k_base() {
            Ok(bpe) => bpe.encode_ordinary(text).len(),
            Err(_) => text.len() / 3,
        }
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

// ---------------------------------------------------------------------------
// MPSC-backed Stream wrapper
// ---------------------------------------------------------------------------

/// Wraps a `tokio::sync::mpsc::Receiver` as a `futures::Stream`.
struct ReceiverStream<T> {
    inner: tokio::sync::mpsc::Receiver<T>,
}

impl<T> ReceiverStream<T> {
    fn new(inner: tokio::sync::mpsc::Receiver<T>) -> Self {
        Self { inner }
    }
}

impl<T> Stream for ReceiverStream<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.poll_recv(cx)
    }
}

// ---------------------------------------------------------------------------
// SSE tool call accumulator
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::types::ChatMessage;
    use crate::tools;

    #[test]
    fn test_tool_request_serialization() {
        let req = ChatRequest {
            model: "deepseek-v4-pro".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "当前目录有什么文件".into(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            tools: Some(vec![tools::command_tool_definition()]),
            tool_choice: Some(serde_json::json!("auto")),
            temperature: Some(0.7),
            max_tokens: Some(4096),
            stream: true,
            thinking_mode: None,
            reasoning_effort: None,
        };

        let json = serde_json::to_string_pretty(&req).unwrap();
        println!("=== CHAT REQUEST BODY ===\n{json}\n==========================");

        // Verify tools field is present
        assert!(json.contains("\"tools\""), "JSON must contain tools field");
        assert!(json.contains("\"execute_command\""), "JSON must contain tool name");
        assert!(json.contains("\"tool_choice\""), "JSON must contain tool_choice");
    }
}
