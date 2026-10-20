use chrono::Utc;
use serde::{Deserialize, Serialize};

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
    #[serde(rename = "computer_action")]
    ComputerAction {
        ts: String,
        session: String,
        action: String,
        /// JSON-encoded tool arguments (never contains screenshot data).
        args: String,
        risk: RiskLevel,
        approved_by: Approval,
        result: ExecResult,
        elapsed_ms: u64,
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
            format!(
                r#"{{"event":"serialize_error","error":"{}","ts":"{}"}}"#,
                e,
                Utc::now()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computer_action_roundtrips_through_jsonl() {
        let event = AuditEvent::ComputerAction {
            ts: "2026-10-20T00:00:00Z".into(),
            session: "s1".into(),
            action: "click".into(),
            args: r#"{"action":"click","x":1,"y":2}"#.into(),
            risk: RiskLevel::High,
            approved_by: Approval::Human,
            result: ExecResult::Ok,
            elapsed_ms: 12,
        };

        let line = event.to_line();
        assert!(line.contains(r#""event":"computer_action""#));

        let parsed: AuditEvent = serde_json::from_str(&line).expect("audit line must parse");
        assert!(matches!(parsed, AuditEvent::ComputerAction { .. }));
    }
}
