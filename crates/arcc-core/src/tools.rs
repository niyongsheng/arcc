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

/// Returns the `reply_to_user` tool definition for LLM function calling.
///
/// This tool allows the AI to proactively send messages to the user,
/// useful for progress updates, confirmations, or notifying results of
/// long-running / scheduled tasks.
pub fn reply_to_user_definition() -> ToolDefinition {
    ToolDefinition {
        name: "reply_to_user".into(),
        description: "Send a text message to the user. Use this to proactively \
                      notify the user of progress, ask for confirmation, report \
                      results of long-running tasks, or send follow-up messages \
                      after a delay. The user will see this message immediately."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The message text to send to the user"
                }
            },
            "required": ["message"]
        }),
        strict: false,
    }
}

/// Returns the `schedule_task` tool definition for LLM function calling.
///
/// Allows the AI to schedule a task to run later or on a recurring schedule.
/// The scheduler runs in the background and re-uses the full feishu processing
/// flow (LLM + tool calls) when the task triggers.
pub fn schedule_task_definition() -> ToolDefinition {
    ToolDefinition {
        name: "schedule_task".into(),
        description: "Schedule a task to run later or on a recurring schedule. \
                      Use this when the user asks you to do something at a \
                      specific time or on a recurring basis (e.g. 'restart \
                      nginx at 1am every day'). For one-shot tasks (e.g. \
                      'remind me in 5 minutes'), omit the cron field and \
                      describe the timing in the task description instead. \
                      When the task triggers, the full LLM processing flow \
                      runs again — the AI will re-read the task description, \
                      plan the steps, and execute them. The result is sent \
                      back to the user automatically."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "cron": {
                    "type": "string",
                    "description": "Cron expression in 6-field format (seconds prefix). \
                                    Optional — omit for one-shot tasks.\n\
                                    Examples:\n\
                                    - '0 1 * * * *'     = daily at 1am\n\
                                    - '0 */5 * * * *'   = every 5 minutes\n\
                                    - '0 0 * * * 0'     = weekly on Sunday midnight\n\
                                    - '0 9-17 * * * 1-5' = every hour 9am-5pm weekdays\n\
                                    - '*/30 * * * * *'  = every 30 seconds\n\
                                    - '@daily'           = once daily at midnight\n\
                                    Fields (first is seconds!): \
                                    second minute hour day-of-month month day-of-week"
                },
                "task": {
                    "type": "string",
                    "description": "Natural language description of the task to \
                                    execute when the cron triggers. Same format \
                                    as if the user typed it directly."
                }
            },
            "required": ["task"]
        }),
        strict: false,
    }
}

/// Returns the `list_scheduled_tasks` tool definition.
pub fn list_scheduled_tasks_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_scheduled_tasks".into(),
        description: "List all active scheduled tasks for the current user. \
                      Use this when the user asks what tasks are scheduled or \
                      wants to manage their tasks."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
        }),
        strict: false,
    }
}

/// Returns the `use_pro_model` tool definition.
///
/// Allows the AI to switch from Flash (default) to Pro for tasks that
/// require deeper reasoning — analysis, debugging, design, etc.
pub fn use_pro_model_definition() -> ToolDefinition {
    ToolDefinition {
        name: "use_pro_model".into(),
        description: "Switch to the Pro model (DeepSeek-V4-Pro) for this turn. \
                      Use this when the user's request requires deep reasoning, \
                      complex analysis, debugging, design, or any task where \
                      you need more thinking capacity. The Pro model is more \
                      capable but slower and more expensive — only use when \
                      necessary. Call early in your response, before making \
                      tool calls."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Why you need the Pro model (e.g. 'need deep log analysis')"
                }
            },
            "required": ["reason"]
        }),
        strict: false,
    }
}

/// Returns the `cancel_scheduled_task` tool definition.
pub fn cancel_scheduled_task_definition() -> ToolDefinition {
    ToolDefinition {
        name: "cancel_scheduled_task".into(),
        description: "Pause or delete a scheduled task. Use this when the user \
                      asks to cancel, stop, pause, or remove a scheduled task. \
                      Pausing keeps the task in the database but prevents it \
                      from running. Deleting removes it permanently."
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the task to cancel"
                },
                "action": {
                    "type": "string",
                    "enum": ["pause", "delete"],
                    "description": "\"pause\" to temporarily stop the task, \
                                    \"delete\" to remove it permanently"
                }
            },
            "required": ["task_id", "action"]
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
