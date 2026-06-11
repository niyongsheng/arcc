pub mod context;
pub mod feishu_client;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod safety;
pub mod session;
pub mod system;
pub mod tools;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("model error: {0}")]
    Model(#[from] model::provider::ModelError),
    #[error("MCP scheduler error: {0}")]
    McpScheduler(#[from] mcp::scheduler::McpSchedulerError),
    #[error("safety validation error: {0}")]
    Safety(#[from] safety::validator::ValidationError),
}
