use async_trait::async_trait;
use futures::Stream;

use super::types::{ChatRequest, ChatResponse, StreamChunk};

/// Core model provider abstraction.
///
/// All LLM interactions must go through this trait — never call a
/// provider API directly.  Adding a new model means implementing
/// this trait once and registering it in the provider registry.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Non-streaming chat completion.
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ModelError>;

    /// Streaming chat completion.
    async fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Stream<Item = Result<StreamChunk, ModelError>> + Send + Unpin>, ModelError>;

    /// Exact token count for the given text, using this model's tokeniser.
    fn count_tokens(&self, text: &str) -> usize;

    /// Human-readable model identifier (e.g. "deepseek-v4-pro").
    fn model_name(&self) -> &str;
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error ({code}): {message}")]
    Api { code: u16, message: String },
    #[error("rate limited — retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("stream error: {0}")]
    Stream(String),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}
