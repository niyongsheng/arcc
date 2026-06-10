use serde::{Deserialize, Serialize};
use chrono::Utc;

/// Audit event — recorded for every command execution, MCP call,
/// and human-in-the-loop decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum AuditEvent {
    #[serde(rename = "cmd_exec")]
    CommandExec {
        ts: String,
        session: String,
        cmd: String,
        risk: RiskLevel,
        approved_by: Approval,
        result: ExecResult,
        elapsed_ms: u64,
    },
    #[serde(rename = "cmd_blocked")]
    CommandBlocked {
        ts: String,
        session: String,
        cmd: String,
        risk: RiskLevel,
        reason: String,
    },
    #[serde(rename = "mcp_tool")]
    McpToolCall {
        ts: String,
        session: String,
        tool: String,
        arguments: String,
        result: ExecResult,
        elapsed_ms: u64,
    },
    #[serde(rename = "human_confirm")]
    HumanConfirm {
        ts: String,
        session: String,
        action: String,
        decision: ConfirmDecision,
        user: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Approval {
    Auto,
    Human,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecResult {
    Ok,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfirmDecision {
    Approved,
    Denied,
}

impl AuditEvent {
    /// Serialise to a single JSON line (no trailing newline inside the JSON).
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            format!(r#"{{"event":"serialize_error","error":"{}","ts":"{}"}}"#, e, Utc::now())
        })
    }
}
