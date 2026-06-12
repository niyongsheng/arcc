use axum::{
    extract::State,
    http::StatusCode,
    response::sse::{Event, Sse},
    Json,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use arcc_core::context::SharedContext;
use arcc_core::model::types::{ChatMessage, ChatRequest, StreamChunk};

#[derive(Debug, Deserialize)]
pub struct ChatInput {
    pub session_id: String,
    pub prompt: String,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Serialize)]
pub struct ChatError {
    pub error: String,
}

/// POST /chat — streaming chat completion via SSE.
pub async fn handler(
    State(ctx): State<SharedContext>,
    Json(input): Json<ChatInput>,
) -> Result<Sse<ChatStream>, (StatusCode, Json<ChatError>)> {
    if input.prompt.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ChatError {
                error: "prompt must not be empty".into(),
            }),
        ));
    }

    let provider = ctx
        .providers
        .pick(&input.prompt, false)
        .ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ChatError {
                    error: "no model provider available".into(),
                }),
            )
        })?
        .clone();

    let system_msg = arcc_core::model::prompts::templates::server().to_chat_message();

    // Inject known facts as a system message between system prompt and user message.
    let memory_context = ctx.memory.format_for_context(&input.session_id);
    let mut messages = Vec::new();
    messages.push(system_msg);
    if !memory_context.is_empty() {
        messages.push(ChatMessage {
            role: "system".into(),
            content: memory_context,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
    }

    // Load conversation history so the LLM has multi-turn context.
    let session = ctx.sessions.get_or_create(&input.session_id, "server").await;
    {
        let s = session.read().await;
        let history = s.context();
        for msg in history {
            if msg.role == "system" && !msg.content.starts_with("[conversation summary]") {
                continue;
            }
            messages.push(msg);
        }
    }

    messages.push(ChatMessage {
        role: "user".into(),
        content: input.prompt.clone(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    });

    let req = ChatRequest {
        model: provider.model_name().to_owned(),
        messages,
        tools: None,
        tool_choice: None,
        temperature: Some(ctx.storage.config.model.temperature),
        max_tokens: Some(ctx.storage.config.model.max_output_tokens),
        stream: true,
        thinking_mode: None,
        reasoning_effort: None,
    };

    info!(
        session = %input.session_id,
        prompt_len = input.prompt.len(),
        "chat request"
    );

    match provider.chat_stream(req).await {
        Ok(stream) => {
            let (tx, rx) = tokio::sync::mpsc::channel(128);

            let user_msg_tokens = provider.count_tokens(&input.prompt);

            // Save user message.
            {
                let mut s = session.write().await;
                s.push_message(
                    ChatMessage {
                        role: "user".into(),
                        content: input.prompt.clone(),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    },
                    user_msg_tokens,
                );
            }

            // Stream response to SSE.
            tokio::spawn(async move {
                let mut stream = Box::pin(stream);
                let mut full_response = String::new();
                let mut reasoning_buf = String::new();

                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(StreamChunk::Content(text)) => {
                            full_response.push_str(&text);
                            let _ = tx.send(Ok(Event::default().data(text))).await;
                        }
                        Ok(StreamChunk::Reasoning(text)) => {
                            reasoning_buf.push_str(&text);
                            let _ = tx
                                .send(Ok(Event::default().event("reasoning").data(text)))
                                .await;
                        }
                        Ok(StreamChunk::Finish(usage)) => {
                            // Persist assistant response.
                            let mut s = session.write().await;
                            let asst_tokens = provider.count_tokens(&full_response);
                            s.push_message(
                                ChatMessage {
                                    role: "assistant".into(),
                                    content: full_response.clone(),
                                    tool_calls: None,
                                    tool_call_id: None,
                                    reasoning_content: if reasoning_buf.is_empty() {
                                        None
                                    } else {
                                        Some(reasoning_buf.clone())
                                    },
                                },
                                asst_tokens,
                            );
                            // Record API-reported token usage
                            let model = provider.model_name().to_owned();
                            if let Err(e) = ctx.storage.record_token_usage(
                                &input.session_id,
                                &model,
                                usage.prompt_tokens as i64,
                                usage.completion_tokens as i64,
                            ) {
                                tracing::warn!(err = %e, "failed to record token usage");
                            }
                            info!(response_len = full_response.len(), "chat complete");

                            // Spawn background memory extraction.
                            let mem_mgr = ctx.memory.clone();
                            let uid = input.session_id.clone();
                            let umsg = input.prompt.clone();
                            let asst = full_response.clone();
                            tokio::spawn(async move {
                                if let Err(e) = mem_mgr.extract(&uid, &umsg, &asst).await {
                                    tracing::warn!(err = %e, "memory extraction failed");
                                }
                            });

                            let _ = tx
                                .send(Ok(Event::default().event("finish").data("[DONE]")))
                                .await;
                        }
                        Ok(other) => {
                            let _ = tx
                                .send(Ok(Event::default()
                                    .event("debug")
                                    .data(format!("{:?}", other))))
                                .await;
                        }
                        Err(e) => {
                            error!(err = %e, "stream error");
                            let _ = tx
                                .send(Ok(Event::default()
                                    .event("error")
                                    .data(e.to_string())))
                                .await;
                            break;
                        }
                    }
                }
            });

            Ok(Sse::new(ChatStream::new(rx)))
        }
        Err(e) => {
            error!(err = %e, "failed to start stream");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ChatError {
                    error: e.to_string(),
                }),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// SSE stream adapter
// ---------------------------------------------------------------------------

use std::pin::Pin;
use std::task::{Context, Poll};

pub struct ChatStream {
    rx: tokio::sync::mpsc::Receiver<Result<Event, std::convert::Infallible>>,
}

impl ChatStream {
    fn new(rx: tokio::sync::mpsc::Receiver<Result<Event, std::convert::Infallible>>) -> Self {
        Self { rx }
    }
}

impl tokio_stream::Stream for ChatStream {
    type Item = Result<Event, std::convert::Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}
