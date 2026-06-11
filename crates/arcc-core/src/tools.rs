//! Built-in tools for ARCC — command execution, file operations, etc.
//!
//! Each tool is defined as a `ToolDefinition` for LLM function calling,
//! paired with a Rust implementation that validates and executes it.

use std::time::Duration;
use tokio::time::timeout;

use crate::model::types::ToolDefinition;
use crate::safety::allowlist::Allowlist;

/// Maximum output bytes captured from any single command execution.
const MAX_OUTPUT_BYTES: usize = 4096;

/// Maximum wall-clock time for a command before it is killed.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// Returns the `execute_command` tool definition for LLM function calling.
pub fn command_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "execute_command".into(),
        description: "Execute a shell command on the user's local system and return its stdout/stderr output. \
                      Use this when the user asks you to run a command, check system status, debug problems, \
                      or interact with files. The command runs through a safety allowlist."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute (e.g. \"ls -la /tmp\")"
                },
                "interactive": {
                    "type": "boolean",
                    "description": "CRITICAL: Set to true for ANY command that may prompt for \
                                    user input, require elevated privileges, or run an \
                                    interactive TUI. Examples: sudo, ssh, vim, nano, htop, \
                                    top, less, more, passwd, telnet, package managers, \
                                    editors, password prompts. The TUI will temporarily exit \
                                    alternate screen and let the command access the real \
                                    terminal. Set to false only for batch commands that run \
                                    to completion without any prompts. When in doubt, prefer true."
                }
            },
            "required": ["command", "interactive"]
        }),
        strict: false,
    }
}

// ---------------------------------------------------------------------------
// Command execution
// ---------------------------------------------------------------------------

/// Result of a command execution.
#[derive(Debug)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub truncated: bool,
}

/// Execute a command via piped stdio (default, no TTY).
///
/// 1. Check for dangerous commands (`require_human_confirm` list).
/// 2. Run with a 30-second timeout via `tokio::process::Command`.
/// 3. Truncate output at `MAX_OUTPUT_BYTES`.
///
/// All commands are allowed unless they match a dangerous pattern and the
/// caller has not opted out of permission checks.  The interactive TUI
/// prompt happens *before* calling this function — once it reaches here
/// with `skip_permissions = true` the command is cleared to run.
pub async fn execute_command(
    cmd: &str,
    allowlist: &Allowlist,
    skip_permissions: bool,
) -> Result<CommandOutput, ToolError> {
    // --- safety check ---
    if !skip_permissions && allowlist.check(cmd).unwrap_or(false) {
        return Err(ToolError::RequiresConfirmation(
            "command requires human confirmation".into(),
        ));
    }

    // --- execute ---
    let (shell, arg) = crate::system::shell_and_arg();

    let child = tokio::process::Command::new(shell)
        .arg(arg)
        .arg(cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ToolError::Spawn(e.to_string()))?;

    // --- wait with timeout ---
    let output = timeout(COMMAND_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| ToolError::Timeout(COMMAND_TIMEOUT.as_secs()))?
        .map_err(|e| ToolError::Spawn(e.to_string()))?;

    // --- capture and truncate ---
    let mut stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
    let mut stderr_str = String::from_utf8_lossy(&output.stderr).to_string();
    let mut truncated = false;

    if stdout_str.len() > MAX_OUTPUT_BYTES {
        let boundary = stdout_str.floor_char_boundary(MAX_OUTPUT_BYTES);
        stdout_str.truncate(boundary);
        stdout_str.push_str("\n... (truncated)");
        truncated = true;
    }
    if stderr_str.len() > MAX_OUTPUT_BYTES {
        let boundary = stderr_str.floor_char_boundary(MAX_OUTPUT_BYTES);
        stderr_str.truncate(boundary);
        stderr_str.push_str("\n... (truncated)");
        truncated = true;
    }

    Ok(CommandOutput {
        stdout: stdout_str,
        stderr: stderr_str,
        exit_code: output.status.code(),
        truncated,
    })
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("command requires human confirmation: {0}")]
    RequiresConfirmation(String),
    #[error("failed to spawn command: {0}")]
    Spawn(String),
    #[error("command timed out after {0}s")]
    Timeout(u64),
}
