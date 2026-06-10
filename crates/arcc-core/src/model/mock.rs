//! Mock ModelProvider — echoes input with simulated streaming.
//! Used for development/testing when no real API key is available.

use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

use super::provider::{ModelError, ModelProvider};
use super::types::{ChatMessage, ChatRequest, ChatResponse, StreamChunk, Usage};

pub struct MockProvider {
    model: String,
    delay_ms: u64,
}

impl MockProvider {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            delay_ms: 50,
        }
    }

    /// Set per-chunk delay in milliseconds (default 50).
    pub fn with_delay(mut self, ms: u64) -> Self {
        self.delay_ms = ms;
        self
    }
}

#[async_trait]
impl ModelProvider for MockProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ModelError> {
        let user_input = req
            .messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();

        let response = format!(
            "[mock {}] You said: \"{user_input}\". This is a simulated response for testing.",
            self.model
        );

        let tokens = response.len() / 3;
        Ok(ChatResponse {
            message: ChatMessage {
                role: "assistant".into(),
                content: response,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            reasoning_content: None,
            usage: Usage {
                prompt_tokens: user_input.len() as u32 / 3,
                completion_tokens: tokens as u32,
            },
        })
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<
        Box<dyn Stream<Item = Result<StreamChunk, ModelError>> + Send + Unpin>,
        ModelError,
    > {
        let user_input = req
            .messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let delay = self.delay_ms;
        let model = self.model.clone();

        tokio::spawn(async move {
            let response = format!(
                "[mock {model}] You asked: \"{user_input}\"\n\nThis is a simulated streaming response. \
                 In production, you would see real AI-generated content here. \
                 To use the real model, set the DEEPSEEK_API_KEY environment variable.\n\n\
                 Prompt length: {} chars",
                user_input.len()
            );

            // Simulate streaming by sending chunks.
            let words: Vec<&str> = response.split_inclusive(' ').collect();
            for word in words {
                drop(tx.send(Ok(StreamChunk::Content(word.to_string()))));
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }

            drop(tx.send(Ok(StreamChunk::Finish(Usage {
                prompt_tokens: user_input.len() as u32 / 3,
                completion_tokens: response.len() as u32 / 3,
            }))));
        });

        Ok(Box::new(ReceiverStream::new(rx)))
    }

    fn count_tokens(&self, text: &str) -> usize {
        text.len() / 3
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

// ---------------------------------------------------------------------------
// Receiver-backed stream
// ---------------------------------------------------------------------------

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
