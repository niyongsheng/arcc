//! The `session/prompt` turn loop — streams model output as ACP
//! `session/update` notifications, executes tools through the permission
//! flow, and returns a stop reason for the `session/prompt` response.
//!
//! Modeled on the TUI's `run_tool_calling_loop` (arcc-tui app.rs):
//! phase 1 carries the `execute_command` tool with `tool_choice: auto`,
//! phase 2 is a plain-text continuation after tool results. Messages are
//! persisted to the core `Session` (SQLite) exactly like the TUI path.

use std::sync::Arc;

use futures::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, info};

use arcc_core::context::SharedContext;
use arcc_core::model::types::{ChatMessage, ChatRequest, StreamChunk, ToolCall};
use arcc_core::tools;

use crate::permission::PermRegistry;
use crate::protocol;
use crate::session::AcpSessionHandle;

/// Outcome of a prompt turn, reported in the `session/prompt` response.
#[derive(Debug, Default)]
pub struct PromptOutcome {
    pub stop_reason: String,
    /// `{inputTokens, outputTokens}` — present when the model reported usage.
    pub usage: Option<Value>,
}

/// Run one prompt turn to completion (or cancellation).
///
/// The turn owns `handle` (an `Arc`) for its whole lifetime, so the
/// session stays alive even if the client closes it mid-run — the
/// spawned task in lib.rs drops it when done.
pub async fn run_prompt(
    ctx: &SharedContext,
    handle: Arc<AcpSessionHandle>,
    user_text: String,
    outbound: UnboundedSender<Value>,
    perms: Arc<PermRegistry>,
) -> PromptOutcome {
    // Fresh cancellation token for this run — `session/cancel` (or
    // `session/close`) cancels it; every await point below selects on it.
    let cancel = handle.fresh_cancel_token().await;
    let session_id = handle.id().await;
    let cwd = handle.cwd.clone();

    // Model: explicit preference ("pro"/"flash") from set_config_option,
    // else flash (fast chat).
    let provider = match handle.model_pref.read().await.as_deref() {
        Some("pro") => ctx.providers.pro(),
        _ => ctx.providers.flash(),
    }
    .cloned()
    .unwrap_or_else(|| panic!("no model provider registered"));

    // Persist the user message before anything else — the model must see
    // it and it must survive even if the turn is cancelled mid-flight.
    {
        let mut s = handle.session.write().await;
        s.push_message(
            ChatMessage::user(user_text.clone()),
            provider.count_tokens(&user_text),
        );
    }

    let system_msg = arcc_core::model::prompts::templates::tui().to_chat_message();
    let temperature = ctx.storage.config.model.temperature;
    let max_tokens = ctx.storage.config.model.max_output_tokens;
    let timeout_secs = ctx.storage.config.execution.command_timeout_seconds;
    let max_bytes = ctx.storage.config.execution.max_output_bytes;
    let skip_permissions = ctx.dangerously_skip_permissions;

    let mut phase = 1;
    let mut usage: Option<Value> = None;

    loop {
        let has_tools = phase == 1;
        let messages = build_messages(&handle, &system_msg).await;
        let req = ChatRequest {
            model: provider.model_name().to_owned(),
            messages,
            tools: if has_tools {
                Some(vec![tools::command_tool_definition()])
            } else {
                None
            },
            tool_choice: if has_tools { Some(json!("auto")) } else { None },
            temperature: Some(temperature),
            max_tokens: Some(max_tokens),
            stream: true,
            thinking_mode: None,
            reasoning_effort: None,
        };

        let stream = match tokio::select! {
            r = provider.chat_stream(req) => r,
            _ = cancel.cancelled() => {
                info!(%session_id, "prompt cancelled before stream start");
                return PromptOutcome { stop_reason: "cancelled".into(), usage };
            }
        } {
            Ok(s) => s,
            Err(e) => {
                error!(err = %e, "chat_stream failed");
                let _ = outbound.send(protocol::session_update(
                    &session_id,
                    protocol::agent_message_chunk(None, &format!("❌ {e}")),
                ));
                return PromptOutcome { stop_reason: "end_turn".into(), usage };
            }
        };

        let mut stream = Box::pin(stream);
        let mut content_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut last_usage = None;

        loop {
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = cancel.cancelled() => {
                    info!(%session_id, "prompt cancelled mid-stream");
                    // Dropping the stream here tears down DeepSeek's SSE
                    // reading task (its channel send fails once the receiver
                    // is gone). Dropping an in-flight tool future similarly
                    // kills the child process via `kill_on_drop`.
                    return PromptOutcome { stop_reason: "cancelled".into(), usage };
                }
            };
            let Some(chunk) = chunk else { break };
            match chunk {
                Ok(StreamChunk::Content(text)) => {
                    content_buf.push_str(&text);
                    let _ = outbound.send(protocol::session_update(
                        &session_id,
                        protocol::agent_message_chunk(None, &text),
                    ));
                }
                Ok(StreamChunk::Reasoning(text)) => {
                    reasoning_buf.push_str(&text);
                    let _ = outbound.send(protocol::session_update(
                        &session_id,
                        protocol::agent_thought_chunk(None, &text),
                    ));
                }
                Ok(StreamChunk::ToolCallStart(tc)) => tool_calls.push(tc),
                Ok(StreamChunk::ToolCallEnd { .. }) => {}
                Ok(StreamChunk::Finish(u)) => {
                    last_usage = Some(u);
                    let (used, size) = {
                        let s = handle.session.read().await;
                        (s.token_count(), s.context_max())
                    };
                    // The assistant message isn't persisted until after the
                    // stream ends — include this turn's in-flight tokens so
                    // the client sees an accurate context usage.
                    let used = used
                        + provider.count_tokens(&content_buf)
                        + provider.count_tokens(&reasoning_buf);
                    let _ = outbound.send(protocol::session_update(
                        &session_id,
                        protocol::usage_update(used, size),
                    ));
                }
                Err(e) => {
                    error!(err = %e, "stream error");
                    let _ = outbound.send(protocol::session_update(
                        &session_id,
                        protocol::agent_message_chunk(None, &format!("❌ stream error: {e}")),
                    ));
                    return PromptOutcome { stop_reason: "end_turn".into(), usage };
                }
            }
        }

        if let Some(u) = &last_usage {
            usage = Some(json!({
                "inputTokens": u.prompt_tokens,
                "outputTokens": u.completion_tokens,
            }));
        }

        // Persist the assistant response (content + reasoning + tool calls).
        {
            let mut s = handle.session.write().await;
            let assistant_msg = ChatMessage {
                role: "assistant".into(),
                content: content_buf.clone(),
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls.clone())
                },
                tool_call_id: None,
                reasoning_content: if reasoning_buf.is_empty() {
                    None
                } else {
                    Some(reasoning_buf.clone())
                },
            };
            s.push_message(
                assistant_msg,
                provider.count_tokens(&content_buf) + provider.count_tokens(&reasoning_buf),
            );
        }

        if tool_calls.is_empty() {
            return PromptOutcome { stop_reason: "end_turn".into(), usage };
        }

        // Execute tool calls, one by one.
        for tc in &tool_calls {
            info!(tool = %tc.name, id = %tc.id, "executing tool call");
            let command = tc.arguments["command"].as_str().unwrap_or("").to_owned();

            let _ = outbound.send(protocol::session_update(
                &session_id,
                protocol::tool_call_status(&tc.id, &command, "shell", "pending", Some(&command)),
            ));

            // --- permission policy ---
            // --unsafe → allow all; allowlist-safe → allow; matching a
            // `require_human_confirm` pattern → session/request_permission.
            let mut rejected = false;
            if !skip_permissions {
                let needs_confirm = ctx.allowlist.read().await.check(&command).unwrap_or(false);
                if needs_confirm {
                    match request_permission(&session_id, tc, &command, &outbound, &perms, &cancel).await {
                        PermissionOutcome::Allowed => {}
                        PermissionOutcome::Rejected => rejected = true,
                        PermissionOutcome::Cancelled => {
                            info!(%session_id, "permission request cancelled — aborting turn");
                            return PromptOutcome { stop_reason: "cancelled".into(), usage };
                        }
                    }
                }
            }

            if rejected {
                let mut s = handle.session.write().await;
                s.push_message(
                    ChatMessage::tool_result(tc.id.clone(), "execution rejected by user".into()),
                    0,
                );
                let _ = outbound.send(protocol::session_update(
                    &session_id,
                    protocol::tool_call_update(
                        &tc.id,
                        "error",
                        Some(json!({ "type": "text", "text": "rejected by user" })),
                        None,
                    ),
                ));
                continue;
            }

            // --- execute (skip_permissions=true — already checked above) ---
            // ACP has no TTY, so the model's `interactive` flag is ignored:
            // everything runs piped. Cancelling the turn mid-run drops the
            // future and `kill_on_drop` reaps the child process.
            let _ = outbound.send(protocol::session_update(
                &session_id,
                protocol::tool_call_update(&tc.id, "running", None, None),
            ));
            let al = ctx.allowlist.read().await;
            let executed = tokio::select! {
                r = tools::execute_command_acp(&command, &cwd, &al, true, timeout_secs, max_bytes) => r,
                _ = cancel.cancelled() => {
                    info!(%session_id, tool = %tc.id, "prompt cancelled during tool execution");
                    return PromptOutcome { stop_reason: "cancelled".into(), usage };
                }
            };
            drop(al);

            match executed {
                Ok(output) => {
                    let content = output.to_content();
                    let tokens = provider.count_tokens(&content);
                    let mut s = handle.session.write().await;
                    s.push_message(ChatMessage::tool_result(tc.id.clone(), content), tokens);
                    let _ = outbound.send(protocol::session_update(
                        &session_id,
                        protocol::tool_call_update(
                            &tc.id,
                            "completed",
                            Some(json!({ "type": "text", "text": output.stdout })),
                            Some(json!({
                                "stdout": output.stdout,
                                "stderr": output.stderr,
                                "exit_code": output.exit_code,
                                "truncated": output.truncated,
                            })),
                        ),
                    ));
                }
                Err(e) => {
                    let mut s = handle.session.write().await;
                    s.push_message(
                        ChatMessage::tool_result(tc.id.clone(), format!("error: {e}")),
                        0,
                    );
                    let _ = outbound.send(protocol::session_update(
                        &session_id,
                        protocol::tool_call_update(
                            &tc.id,
                            "error",
                            Some(json!({ "type": "text", "text": format!("{e}") })),
                            None,
                        ),
                    ));
                }
            }
        }

        // Rebuild messages for phase 2 from session (same as TUI).
        phase = 2;
    }
}

/// Result of a `session/request_permission` round-trip.
enum PermissionOutcome {
    Allowed,
    Rejected,
    Cancelled,
}

/// Ask the client for permission and await its response.
///
/// The request is an agent→client method call carrying a generated id;
/// the stdin read loop resolves it back through `perms`, so this future
/// completes when (and only when) the client answers.
async fn request_permission(
    session_id: &str,
    tc: &ToolCall,
    command: &str,
    outbound: &UnboundedSender<Value>,
    perms: &PermRegistry,
    cancel: &tokio_util::sync::CancellationToken,
) -> PermissionOutcome {
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    perms.register(request_id.clone(), tx).await;

    let _ = outbound.send(json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "session/request_permission",
        "params": {
            "sessionId": session_id,
            "toolCall": {
                "toolCallId": tc.id,
                "title": command,
                "kind": "shell",
                "status": "pending",
                "rawInput": command,
            },
            "options": [
                { "optionId": "allow_once", "name": "Allow once", "kind": "select" },
                { "optionId": "reject_once", "name": "Reject", "kind": "select" },
            ],
        },
    }));

    let response = tokio::select! {
        r = rx => r.unwrap_or(Value::Null),
        _ = cancel.cancelled() => return PermissionOutcome::Cancelled,
    };

    // Expected: {outcome: {outcome: "selected", optionId}} or
    //           {outcome: {outcome: "cancelled"}}
    let outcome = response.get("outcome").and_then(|o| o.get("outcome"));
    match outcome.and_then(|o| o.as_str()) {
        Some("selected") => {
            let option = response
                .get("outcome")
                .and_then(|o| o.get("optionId"))
                .and_then(|o| o.as_str());
            if option == Some("allow_once") {
                PermissionOutcome::Allowed
            } else {
                PermissionOutcome::Rejected
            }
        }
        _ => PermissionOutcome::Rejected,
    }
}

/// Rebuild the wire messages for the next phase from the session history:
/// fresh system prompt + everything the session has accumulated (summary
/// and prior turns included, minus old system copies).
async fn build_messages(handle: &AcpSessionHandle, system_msg: &ChatMessage) -> Vec<ChatMessage> {
    let s = handle.session.read().await;
    let mut base = s.prepare_for_request(false);
    base.retain(|m| m.role != "system");
    std::iter::once(system_msg.clone()).chain(base).collect()
}
