//! System prompt management.
//!
//! Provides compile-time embedded prompt templates and a `SystemPrompt` wrapper
//! for constructing system-level `ChatMessage` values.
//!
//! # Templates
//!
//! Prompt templates live as `.md` files alongside this module and are embedded
//! at compile time via `include_str!`. Variable interpolation uses `{KEY}`
//! placeholders replaced via `str::replace`.
//!
//! # Usage
//!
//! ```rust
//! use arcc_core::model::prompts::templates;
//!
//! let msg = templates::cli().to_chat_message();
//! // msg.content == "You are ARCC ..."
//! ```

use crate::model::types::ChatMessage;

/// A compiled system prompt ready for use in a chat request.
///
/// Wraps the prompt content and provides conversion methods to produce
/// a `ChatMessage` with `role = "system"`.
#[derive(Debug, Clone)]
pub struct SystemPrompt {
    content: String,
}

impl SystemPrompt {
    /// Create a new system prompt with the given content.
    pub fn new(content: String) -> Self {
        Self { content }
    }

    /// Convert into a `ChatMessage` with `role: "system"`.
    ///
    /// All optional fields are set to `None` — tool calls are never part
    /// of a system message.
    pub fn to_chat_message(&self) -> ChatMessage {
        ChatMessage {
            role: "system".into(),
            content: self.content.clone(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    /// Consume the wrapper and return the raw prompt text.
    pub fn into_string(self) -> String {
        self.content
    }

    /// Borrow the prompt text.
    pub fn as_str(&self) -> &str {
        &self.content
    }
}

/// Built-in prompt templates, embedded at compile time.
///
/// Each function returns a `SystemPrompt` with the content loaded from the
/// corresponding `.md` template file.
pub mod templates {
    use super::SystemPrompt;

    const CLI: &str = include_str!("cli.md");
    const TUI: &str = include_str!("tui.md");
    const PLAN: &str = include_str!("plan.md");
    const SERVER: &str = include_str!("server.md");
    const COMPRESS: &str = include_str!("compress.md");

    /// CLI mode system prompt — headless command execution.
    pub fn cli() -> SystemPrompt {
        SystemPrompt::new(CLI.to_owned())
    }

    /// TUI mode system prompt — interactive terminal with tool calling.
    pub fn tui() -> SystemPrompt {
        SystemPrompt::new(TUI.to_owned())
    }

    /// Plan mode system prompt — task breakdown with a specific goal.
    ///
    /// The `{TASK}` placeholder in the template is replaced with `task`.
    pub fn plan(task: &str) -> SystemPrompt {
        let content = PLAN.replace("{TASK}", task);
        SystemPrompt::new(content)
    }

    /// Server / webhook mode system prompt — backend API agent.
    pub fn server() -> SystemPrompt {
        SystemPrompt::new(SERVER.to_owned())
    }

    /// Context compression system prompt — summarises conversation history.
    pub fn compress() -> SystemPrompt {
        SystemPrompt::new(COMPRESS.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_prompt_is_not_empty() {
        let p = templates::cli();
        assert!(!p.as_str().is_empty());
        assert!(p.as_str().contains("ARCC"));
    }

    #[test]
    fn tui_prompt_is_not_empty() {
        let p = templates::tui();
        assert!(!p.as_str().is_empty());
        assert!(p.as_str().contains("ARCC"));
    }

    #[test]
    fn plan_prompt_interpolates_task() {
        let p = templates::plan("deploy the app");
        assert!(p.as_str().contains("deploy the app"));
        assert!(!p.as_str().contains("{TASK}"));
    }

    #[test]
    fn server_prompt_is_not_empty() {
        let p = templates::server();
        assert!(!p.as_str().is_empty());
        assert!(p.as_str().contains("ARCC"));
    }

    #[test]
    fn compress_prompt_is_not_empty() {
        let p = templates::compress();
        assert!(!p.as_str().is_empty());
        assert!(p.as_str().contains("summariser"));
    }

    #[test]
    fn to_chat_message_has_correct_role() {
        let msg = templates::cli().to_chat_message();
        assert_eq!(msg.role, "system");
        assert!(msg.content.contains("ARCC"));
        assert!(msg.tool_calls.is_none());
        assert!(msg.tool_call_id.is_none());
        assert!(msg.reasoning_content.is_none());
    }
}
