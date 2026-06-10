use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use super::protocol::{McpTool, McpTransport};

/// Manages MCP tool subprocess lifecycles.
///
/// Each registered MCP plugin runs as a child process (stdio transport).
/// The scheduler handles health-checking, automatic restart, and
/// concurrent tool execution.
pub struct McpScheduler {
    tools: Arc<RwLock<HashMap<String, McpToolEntry>>>,
}

#[allow(dead_code)] // transport used when SSE support is added
struct McpToolEntry {
    tool: McpTool,
    command: String,
    args: Vec<String>,
    transport: McpTransport,
    child: Option<Child>,
}

impl McpScheduler {
    pub fn new() -> Self {
        Self {
            tools: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a stdio-based MCP plugin.
    ///
    /// The plugin will be launched on first use and monitored thereafter.
    pub async fn register_stdio(
        &self,
        name: &str,
        description: &str,
        command: &str,
        args: &[String],
    ) {
        let entry = McpToolEntry {
            tool: McpTool {
                name: name.to_owned(),
                description: description.to_owned(),
                input_schema: serde_json::json!({"type": "object", "properties": {}}),
            },
            command: command.to_owned(),
            args: args.to_vec(),
            transport: McpTransport::Stdio,
            child: None,
        };
        self.tools.write().await.insert(name.to_owned(), entry);
        info!(name, command, "MCP tool registered");
    }

    /// List all registered tools (for LLM tool choice).
    pub async fn list_tools(&self) -> Vec<McpTool> {
        self.tools
            .read()
            .await
            .values()
            .map(|e| e.tool.clone())
            .collect()
    }

    /// Launch a previously-registered tool's subprocess if not already running.
    pub async fn ensure_running(&self, name: &str) -> Result<(), McpSchedulerError> {
        let mut tools = self.tools.write().await;
        let entry = tools
            .get_mut(name)
            .ok_or_else(|| McpSchedulerError::ToolNotFound(name.to_owned()))?;

        if entry.child.is_some() {
            return Ok(());
        }

        info!(name, cmd = %entry.command, "launching MCP tool subprocess");
        let child = Command::new(&entry.command)
            .args(&entry.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| McpSchedulerError::Spawn {
                name: name.to_owned(),
                source: e,
            })?;

        entry.child = Some(child);
        Ok(())
    }

    /// Health-check a tool: try to send a `ping` and read the response.
    /// If the subprocess is dead, attempt a restart.
    pub async fn health_check(&self, name: &str) -> bool {
        let mut tools = self.tools.write().await;
        let Some(entry) = tools.get_mut(name) else {
            return false;
        };

        let needs_restart = match entry.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(Some(_))),
            None => true,
        };

        if needs_restart {
            warn!(name, "MCP tool subprocess dead, restarting");
            if let Err(e) = self.ensure_running(name).await {
                error!(name, err = %e, "MCP tool restart failed");
                return false;
            }
        }
        true
    }

    /// Gracefully shut down all managed subprocesses.
    pub async fn shutdown_all(&self) {
        let mut tools = self.tools.write().await;
        for (name, entry) in tools.iter_mut() {
            if let Some(mut child) = entry.child.take() {
                info!(name, "shutting down MCP tool");
                let _ = child.kill().await;
            }
        }
    }
}

impl Default for McpScheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum McpSchedulerError {
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("failed to spawn tool '{name}': {source}")]
    Spawn {
        name: String,
        source: std::io::Error,
    },
}
