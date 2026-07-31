//! arcc-acp: ACP (Agent Client Protocol v1) stdio server.
//!
//! Speaks JSON-RPC 2.0 over stdin/stdout (one JSON document per line).
//! AionUI and other ACP clients can register `arcc --acp` as an agent
//! and get: chat + streaming output, tool execution with per-command
//! permission prompts, turn cancellation, and model switching.
//!
//! Concurrency model (why the read loop never blocks):
//! - one stdin read loop — the only `await` on stdin, so `session/cancel`
//!   notifications and permission responses are always reachable;
//! - every request handled in its own `tokio::spawn`ed task;
//! - one writer task drains a FIFO outbound channel, so notifications
//!   emitted before a response always land on stdout before it.

// Modules are `pub` so integration tests (tests/) can drive the prompt
// loop and the dispatch layer directly, without a subprocess.
pub mod permission;
pub mod prompt;
pub mod protocol;
pub mod session;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tracing::{info, warn};

use arcc_core::context::SharedContext;

use crate::permission::PermRegistry;
use crate::session::AcpSessionRegistry;

/// Run the ACP server until stdin closes (client exit), then return.
pub async fn run(ctx: SharedContext) -> anyhow::Result<()> {
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Value>();
    // Blocking stdout writes never run on the runtime — spawn_blocking
    // satisfies the I/O isolation rule and avoids a non-Send future.
    let writer = tokio::task::spawn_blocking(move || writer_task(out_rx));
    let registry = Arc::new(AcpSessionRegistry::new());
    let perms = Arc::new(PermRegistry::new());

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value = match serde_json::from_str::<Value>(line) {
            Ok(v) => v,
            Err(e) => {
                warn!(err = %e, "invalid JSON on stdin");
                let _ = out_tx.send(protocol::response_err(
                    Value::Null,
                    protocol::PARSE_ERROR,
                    "parse error",
                ));
                continue;
            }
        };
        handle_value(&ctx, value, &registry, &perms, &out_tx).await;
    }

    info!("stdin closed — shutting down ACP server");
    perms.cancel_all().await;
    writer.abort();
    Ok(())
}

/// Single writer task: serialises all outbound messages in FIFO order
/// (line-delimited) and flushes after every line. Runs inside
/// `spawn_blocking` — the blocking `StdoutLock` must never cross an await.
fn writer_task(mut rx: mpsc::UnboundedReceiver<Value>) {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    while let Some(msg) = rx.blocking_recv() {
        let line = match serde_json::to_string(&msg) {
            Ok(l) => l,
            Err(e) => {
                warn!(err = %e, "failed to serialize outbound message");
                continue;
            }
        };
        if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
            break; // client closed stdout — nothing more we can do
        }
    }
}

/// Dispatch one JSON-RPC message or a batch of them.
///
/// Batches are handled iteratively (not recursively — a recursive async
/// fn would need boxing) and responses are sent one by one. AionUI does
/// not send batches, but tolerating them keeps the parser honest.
async fn handle_value(
    ctx: &SharedContext,
    value: Value,
    registry: &Arc<AcpSessionRegistry>,
    perms: &Arc<PermRegistry>,
    out_tx: &mpsc::UnboundedSender<Value>,
) {
    if let Value::Array(batch) = value {
        for item in batch {
            handle_single(ctx, item, registry, perms, out_tx).await;
        }
        return;
    }
    handle_single(ctx, value, registry, perms, out_tx).await;
}

/// Dispatch a single (non-batch) JSON-RPC message: request, notification,
/// or client response (permission outcome).
async fn handle_single(
    ctx: &SharedContext,
    value: Value,
    registry: &Arc<AcpSessionRegistry>,
    perms: &Arc<PermRegistry>,
    out_tx: &mpsc::UnboundedSender<Value>,
) {
    match value {
        Value::Object(_) => {
            // Peek the envelope without holding a borrow across the move.
            let (is_request, id, result) = {
                let map = value.as_object().expect("just matched Object");
                (
                    map.contains_key("method"),
                    map.get("id").cloned().unwrap_or(Value::Null),
                    map.get("result").cloned(),
                )
            };
            if is_request {
                // Request or notification.
                let req: protocol::RpcRequest = match serde_json::from_value(value) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(err = %e, "malformed request");
                        let _ = out_tx.send(protocol::response_err(
                            id,
                            protocol::INVALID_REQUEST,
                            "invalid request",
                        ));
                        return;
                    }
                };
                if let Some(resp) = handle_request(ctx, req, registry, perms, out_tx).await {
                    let _ = out_tx.send(resp);
                }
            } else {
                // Client response — currently only `session/request_permission`
                // outcomes (our requests carry uuid ids registered in `perms`).
                if perms.resolve(&id, result).await {
                    return;
                }
                warn!(id = %id, "unexpected response from client");
            }
        }
        _ => {
            let _ = out_tx.send(protocol::response_err(
                Value::Null,
                protocol::INVALID_REQUEST,
                "invalid request",
            ));
        }
    }
}

/// Handle one request or notification. Returns the response for the
/// outbound channel, or `None` for notifications and for `session/prompt`
/// (which responds asynchronously when the turn completes).
///
/// `pub` so integration tests can exercise the dispatch layer directly.
pub async fn handle_request(
    ctx: &SharedContext,
    req: protocol::RpcRequest,
    registry: &Arc<AcpSessionRegistry>,
    perms: &Arc<PermRegistry>,
    out_tx: &mpsc::UnboundedSender<Value>,
) -> Option<Value> {
    let id = req.id.clone().unwrap_or(Value::Null);
    let is_notification = req.is_notification();
    // Protocol hygiene: reject anything that is not JSON-RPC 2.0.
    if !is_notification && req.jsonrpc != "2.0" {
        return Some(protocol::response_err(
            id,
            protocol::INVALID_REQUEST,
            "jsonrpc must be \"2.0\"",
        ));
    }
    let sid = || -> String {
        req.params
            .as_ref()
            .and_then(|p| p.get("sessionId"))
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string()
    };

    match req.method.as_str() {
        "initialize" => Some(protocol::response_ok(id, initialize_response())),

        "session/new" => {
            // `cwd` defaults to the server's own working directory.
            let cwd: PathBuf = req
                .params
                .as_ref()
                .and_then(|p| p.get("cwd"))
                .and_then(|c| c.as_str())
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let handle = registry.create(ctx, cwd).await;
            let sid = handle.id().await;
            Some(protocol::response_ok(id, session_new_response(&sid)))
        }

        "session/prompt" => {
            if is_notification {
                return None;
            }
            let session_id = sid();
            let Some(handle) = registry.get(&session_id).await else {
                return Some(protocol::response_err(
                    id,
                    protocol::SESSION_NOT_FOUND,
                    format!("session {session_id} not found"),
                ));
            };
            // Concurrent-prompt guard: only one running prompt per session.
            if handle.running.swap(true, Ordering::SeqCst) {
                return Some(protocol::response_err(
                    id,
                    protocol::SESSION_BUSY,
                    format!("session {session_id} already has a running prompt"),
                ));
            }

            // Extract text blocks from the prompt array.
            let mut text = String::new();
            if let Some(blocks) = req
                .params
                .as_ref()
                .and_then(|p| p.get("prompt"))
                .and_then(|p| p.as_array())
            {
                for block in blocks {
                    if let Ok(protocol::ContentBlock::Text { text: t }) =
                        serde_json::from_value::<protocol::ContentBlock>(block.clone())
                    {
                        text.push_str(&t);
                    }
                }
            }
            let text = text.trim().to_string();
            if text.is_empty() {
                handle.running.store(false, Ordering::SeqCst);
                return Some(protocol::response_err(
                    id,
                    protocol::INVALID_PARAMS,
                    "prompt must contain at least one non-empty text block",
                ));
            }

            // Run the turn in its own task. It answers through the outbound
            // channel when done; holding an `Arc` of the handle keeps the
            // session alive even if the client closes it mid-run.
            let ctx2 = Arc::clone(ctx);
            let handle2 = handle.clone();
            let perms2 = Arc::clone(perms);
            let out_tx2 = out_tx.clone();
            tokio::spawn(async move {
                let outcome = prompt::run_prompt(&ctx2, handle2.clone(), text, out_tx2.clone(), perms2).await;
                // Release the guard BEFORE the response lands, so a queued
                // follow-up prompt is not spuriously rejected.
                handle2.running.store(false, Ordering::SeqCst);
                let mut result = json!({ "stopReason": outcome.stop_reason });
                if let Some(u) = outcome.usage {
                    result["usage"] = u;
                }
                let _ = out_tx2.send(protocol::response_ok(id, result));
            });
            None
        }

        "session/close" => {
            let session_id = sid();
            match registry.remove(ctx, &session_id).await {
                Some(_) => Some(protocol::response_ok(id, json!({}))),
                None => Some(protocol::response_err(
                    id,
                    protocol::SESSION_NOT_FOUND,
                    format!("session {session_id} not found"),
                )),
            }
        }

        "session/set_mode" => {
            let session_id = sid();
            if registry.get(&session_id).await.is_none() {
                return Some(protocol::response_err(
                    id,
                    protocol::SESSION_NOT_FOUND,
                    format!("session {session_id} not found"),
                ));
            }
            let mode = req
                .params
                .as_ref()
                .and_then(|p| p.get("mode"))
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if mode != "default" {
                return Some(protocol::response_err(
                    id,
                    protocol::INVALID_PARAMS,
                    "only mode \"default\" is supported",
                ));
            }
            Some(protocol::response_ok(id, json!({})))
        }

        "session/set_config_option" => {
            let session_id = sid();
            let Some(handle) = registry.get(&session_id).await else {
                return Some(protocol::response_err(
                    id,
                    protocol::SESSION_NOT_FOUND,
                    format!("session {session_id} not found"),
                ));
            };
            let key = req
                .params
                .as_ref()
                .and_then(|p| p.get("key"))
                .and_then(|k| k.as_str())
                .unwrap_or("");
            let value = req
                .params
                .as_ref()
                .and_then(|p| p.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if key == "model" && (value == "pro" || value == "flash") {
                *handle.model_pref.write().await = Some(value.to_string());
                return Some(protocol::response_ok(
                    id,
                    json!({ "configOptions": [model_config_option(value)] }),
                ));
            }
            Some(protocol::response_err(
                id,
                protocol::INVALID_PARAMS,
                format!("unsupported config option {key}={value}"),
            ))
        }

        "session/cancel" => {
            let session_id = sid();
            if let Some(handle) = registry.get(&session_id).await {
                handle.cancel().await;
            }
            if is_notification {
                None
            } else {
                Some(protocol::response_ok(id, json!({})))
            }
        }

        // `session/load` / `session/resume` are intentionally unsupported —
        // SQLite stores role/content only, so tool calls cannot be replayed
        // losslessly — and anything unknown falls through here too.
        _ => Some(protocol::response_err(
            id,
            protocol::METHOD_NOT_FOUND,
            format!("method not found: {}", req.method),
        )),
    }
}

// ---------------------------------------------------------------------------
// Response payloads
// ---------------------------------------------------------------------------

/// `initialize` result — the `protocolVersion`, capabilities and agent
/// info required by the official ACP v1 spec.
fn initialize_response() -> Value {
    let caps = json!({
        "loadSession": false,
        "promptCapabilities": {},
        "sessionCapabilities": { "close": {} },
    });
    let info = json!({
        "name": "arcc",
        "title": "ARCC",
        "version": env!("CARGO_PKG_VERSION"),
    });
    json!({
        "protocolVersion": 1,
        "agentCapabilities": caps,
        "agentInfo": info,
        "authMethods": [],
        // Redundant copies of the official fields — emitted by the
        // reference fake-acp CLI that AionUI's fixtures were built from.
        // Unknown keys are ignored by strict clients, so keeping them
        // costs nothing while maximising compatibility. Remove once
        // AionUI converges on the official names.
        "serverCapabilities": caps.clone(),
        "serverInfo": info.clone(),
    })
}

/// `session/new` result.
fn session_new_response(sid: &str) -> Value {
    json!({
        "sessionId": sid,
        "modes": ["default"],
        "configOptions": [model_config_option("flash")],
        "models": {
            "currentModelId": "flash",
            "availableModels": [
                { "modelId": "flash", "name": "DeepSeek-V4-Flash" },
                { "modelId": "pro", "name": "DeepSeek-V4-Pro" },
            ],
        },
    })
}

/// The model selector config option exposed to the client.
fn model_config_option(current: &str) -> Value {
    json!({
        "key": "model",
        "name": "Model",
        "type": "select",
        "options": [
            { "label": "DeepSeek-V4-Flash", "value": "flash" },
            { "label": "DeepSeek-V4-Pro", "value": "pro" },
        ],
        "defaultValue": current,
    })
}
