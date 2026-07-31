//! Integration tests for the ACP prompt loop and dispatch layer.
//!
//! A scripted `ModelProvider` serves deterministic chunk sequences per
//! `chat_stream` call, so the tests exercise real execution paths:
//! streaming, tool calls, permission round-trips, cancellation and the
//! concurrent-prompt guard — without any network access.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use arcc_acp::handle_request;
use arcc_acp::permission::PermRegistry;
use arcc_acp::prompt;
use arcc_acp::protocol::RpcRequest;
use arcc_acp::session::AcpSessionRegistry;

use arcc_core::context::{AppContext, SharedContext};
use arcc_core::model::provider::{ModelError, ModelProvider};
use arcc_core::model::registry::ProviderRegistry;
use arcc_core::model::types::{
    ChatMessage, ChatRequest, ChatResponse, StreamChunk, ToolCall, Usage,
};

// ---------------------------------------------------------------------------
// Scripted provider
// ---------------------------------------------------------------------------

/// One scripted step of a `chat_stream` response.
#[derive(Clone)]
enum ChunkScript {
    /// Text content chunk.
    Content(&'static str),
    /// Reasoning chunk.
    Reasoning(&'static str),
    /// An `execute_command` tool call for the given command.
    ToolCall(&'static str),
    /// Usage report (prompt, completion tokens).
    Finish(u32, u32),
    /// Pause before the next step (ms).
    Sleep(u64),
}

/// Provider whose `chat_stream` calls pop the next scripted sequence.
/// A missing script falls back to a simple echo (never panics).
struct ScriptedProvider {
    model: String,
    scripts: Arc<Mutex<VecDeque<Vec<ChunkScript>>>>,
}

impl ScriptedProvider {
    fn new(model: &str, scripts: Vec<Vec<ChunkScript>>) -> Self {
        Self {
            model: model.to_owned(),
            scripts: Arc::new(Mutex::new(scripts.into_iter().collect())),
        }
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse, ModelError> {
        let text = req
            .messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();
        Ok(ChatResponse {
            message: ChatMessage::assistant(format!("[scripted] {text}")),
            reasoning_content: None,
            usage: Usage {
                prompt_tokens: 1,
                completion_tokens: 1,
            },
        })
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> Result<Box<dyn Stream<Item = Result<StreamChunk, ModelError>> + Send + Unpin>, ModelError> {
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![ChunkScript::Content("(fallback)"), ChunkScript::Finish(1, 1)]);

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<StreamChunk, ModelError>>(32);
        tokio::spawn(async move {
            for step in script {
                match step {
                    ChunkScript::Sleep(ms) => {
                        tokio::time::sleep(Duration::from_millis(ms)).await
                    }
                    ChunkScript::Content(text) => {
                        let _ = tx.send(Ok(StreamChunk::Content(text.to_string()))).await;
                    }
                    ChunkScript::Reasoning(text) => {
                        let _ = tx.send(Ok(StreamChunk::Reasoning(text.to_string()))).await;
                    }
                    ChunkScript::ToolCall(command) => {
                        let _ = tx
                            .send(Ok(StreamChunk::ToolCallStart(ToolCall {
                                id: "call-1".into(),
                                name: "execute_command".into(),
                                arguments: json!({ "command": command, "interactive": false }),
                            })))
                            .await;
                    }
                    ChunkScript::Finish(p, c) => {
                        let _ = tx
                            .send(Ok(StreamChunk::Finish(Usage {
                                prompt_tokens: p,
                                completion_tokens: c,
                            })))
                            .await;
                    }
                }
            }
            // Keep the channel open until the receiver goes away.
            let _ = req.model;
            drop(tx);
        });
        Ok(Box::new(ReceiverStream::new(rx)))
    }

    fn count_tokens(&self, text: &str) -> usize {
        text.len() / 3
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

struct ReceiverStream<T> {
    inner: tokio::sync::mpsc::Receiver<T>,
}

impl<T> ReceiverStream<T> {
    fn new(inner: tokio::sync::mpsc::Receiver<T>) -> Self {
        Self { inner }
    }
}

impl<T> Stream for ReceiverStream<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.poll_recv(cx)
    }
}

// ---------------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------------

struct TestEnv {
    _home: tempfile::TempDir,
    ctx: SharedContext,
    cwd: std::path::PathBuf,
}

/// Build a real `AppContext` over a temp ARCC home. Default config applies
/// (which already lists `rm` in `require_human_confirm`).
fn setup(flash_scripts: Vec<Vec<ChunkScript>>) -> TestEnv {
    let home = tempfile::tempdir().unwrap();
    let storage = arcc_storage::ArccStorage::init(home.path()).unwrap();
    let pro_name = storage.config.model.pro_model.clone();
    let flash_name = storage.config.model.flash_model.clone();

    let flash: Arc<dyn ModelProvider> =
        Arc::new(ScriptedProvider::new(&flash_name, flash_scripts));
    let pro: Arc<dyn ModelProvider> = Arc::new(ScriptedProvider::new(&pro_name, vec![]));
    let mut registry = ProviderRegistry::new(&pro_name, &flash_name);
    registry.register(&pro_name, pro);
    registry.register(&flash_name, flash);

    let ctx = Arc::new(AppContext::new(registry, storage, false));
    TestEnv {
        _home: home,
        ctx,
        cwd: std::env::temp_dir(),
    }
}

/// Drain the outbound channel into a vector of `session/update` messages.
fn drain_updates(rx: &mut mpsc::UnboundedReceiver<Value>) -> Vec<Value> {
    let mut msgs = Vec::new();
    while let Ok(m) = rx.try_recv() {
        assert_eq!(m["method"], "session/update");
        msgs.push(m);
    }
    msgs
}

fn update_type(m: &Value) -> &str {
    m["params"]["update"]["sessionUpdate"]["type"].as_str().unwrap_or("")
}

fn prompt_request(sid: &str, id: u64, text: &str) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(id)),
        method: "session/prompt".into(),
        params: Some(json!({
            "sessionId": sid,
            "prompt": [{ "type": "text", "text": text }],
        })),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Plain text turn: content chunks stream as `agent_message_chunk`, a
/// `usage_update` follows `Finish`, and the turn ends with `end_turn` +
/// usage. Messages are persisted to the core session.
#[tokio::test]
async fn text_turn_streams_chunks_and_reports_usage() {
    let env = setup(vec![vec![
        ChunkScript::Reasoning("thinking..."),
        ChunkScript::Content("Hello "),
        ChunkScript::Content("world"),
        ChunkScript::Finish(3, 2),
    ]]);
    let registry = Arc::new(AcpSessionRegistry::new());
    let handle = registry.create(&env.ctx, env.cwd.clone()).await;
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();

    let outcome = prompt::run_prompt(&env.ctx, handle.clone(), "hi".into(), out_tx, Arc::new(PermRegistry::new())).await;

    assert_eq!(outcome.stop_reason, "end_turn");
    let usage = outcome.usage.expect("usage reported");
    assert_eq!(usage["inputTokens"], 3);
    assert_eq!(usage["outputTokens"], 2);

    let msgs = drain_updates(&mut out_rx);
    let types: Vec<&str> = msgs.iter().map(update_type).collect();
    assert_eq!(
        types,
        ["agent_thought_chunk", "agent_message_chunk", "agent_message_chunk", "usage_update"]
    );
    let text: String = msgs
        .iter()
        .filter(|m| update_type(m) == "agent_message_chunk")
        .map(|m| m["params"]["update"]["sessionUpdate"]["content"]["text"].as_str().unwrap())
        .collect();
    assert_eq!(text, "Hello world");

    let usage_update = msgs
        .iter()
        .find(|m| update_type(m) == "usage_update")
        .expect("usage_update notification")
        .clone();
    assert!(usage_update["params"]["update"]["sessionUpdate"]["used"].as_u64().unwrap() > 0);
    assert!(usage_update["params"]["update"]["sessionUpdate"]["size"].as_u64().unwrap() > 0);

    // Session history: user message + assistant reply, persisted.
    let s = handle.session.read().await;
    assert_eq!(s.message_count(), 2);
    let roles: Vec<String> = s.context().iter().map(|m| m.role.clone()).collect();
    assert_eq!(roles, ["user", "assistant"]);
}

/// Tool call approved via `allow_once`: the command runs in the session
/// cwd and phase 2 produces the final answer.
#[tokio::test]
async fn tool_call_with_allow_once_permission_executes() {
    let env = setup(vec![
        vec![ChunkScript::ToolCall("rm --help"), ChunkScript::Finish(5, 1)],
        vec![ChunkScript::Content("done"), ChunkScript::Finish(1, 1)],
    ]);
    let registry = Arc::new(AcpSessionRegistry::new());
    let handle = registry.create(&env.ctx, env.cwd.clone()).await;
    let perms = Arc::new(PermRegistry::new());
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Value>();

    // Responder: records every outbound message and auto-approves any
    // permission request with `allow_once`.
    let (record_tx, mut record_rx) = mpsc::unbounded_channel::<Value>();
    let perms2 = Arc::clone(&perms);
    let responder = tokio::spawn(async move {
        let mut rx = out_rx;
        while let Some(msg) = rx.recv().await {
            let _ = record_tx.send(msg.clone());
            if msg["method"] == "session/request_permission" {
                let id = msg["id"].clone();
                perms2
                    .resolve(
                        &id,
                        Some(json!({ "outcome": { "outcome": "selected", "optionId": "allow_once" } })),
                    )
                    .await;
            }
        }
    });

    let outcome =
        prompt::run_prompt(&env.ctx, handle.clone(), "hi".into(), out_tx.clone(), perms).await;
    drop(out_tx); // close the channel so the responder exits
    responder.await.unwrap();

    assert_eq!(outcome.stop_reason, "end_turn");

    let mut msgs: Vec<Value> = Vec::new();
    while let Ok(m) = record_rx.try_recv() {
        msgs.push(m);
    }

    // Permission round-trip happened.
    let perm = msgs
        .iter()
        .find(|m| m["method"] == "session/request_permission")
        .expect("permission request sent");
    assert!(perm["id"].is_string());
    assert_eq!(perm["params"]["toolCall"]["kind"], "shell");
    let options: Vec<&str> = perm["params"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["optionId"].as_str().unwrap())
        .collect();
    assert!(options.contains(&"allow_once"));
    assert!(options.contains(&"reject_once"));

    // Tool lifecycle: pending → running → completed, with raw output.
    let updates: Vec<Value> = msgs
        .iter()
        .filter(|m| m["method"] == "session/update")
        .map(|m| m["params"]["update"]["sessionUpdate"].clone())
        .collect();
    let statuses: Vec<&str> = updates
        .iter()
        .filter(|u| u["type"] == "tool_call_update")
        .map(|u| u["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["running", "completed"]);

    let completed = updates
        .iter()
        .find(|u| u["type"] == "tool_call_update" && u["status"] == "completed")
        .unwrap();
    assert!(completed["rawOutput"]["exit_code"].is_number());

    // Phase 2 answer streamed.
    assert!(updates.iter().any(|u| u["content"]["text"] == "done"));
}

/// Tool call rejected via `reject_once`: the command never runs, the
/// client sees an error update, and the turn continues.
#[tokio::test]
async fn tool_call_with_reject_permission_is_skipped() {
    let env = setup(vec![
        vec![ChunkScript::ToolCall("rm --help"), ChunkScript::Finish(1, 1)],
        vec![ChunkScript::Content("skipped it"), ChunkScript::Finish(1, 1)],
    ]);
    let registry = Arc::new(AcpSessionRegistry::new());
    let handle = registry.create(&env.ctx, env.cwd.clone()).await;
    let perms = Arc::new(PermRegistry::new());
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Value>();

    let perms2 = Arc::clone(&perms);
    let responder = tokio::spawn(async move {
        let mut rx = out_rx;
        while let Some(msg) = rx.recv().await {
            if msg["method"] == "session/request_permission" {
                let id = msg["id"].clone();
                perms2
                    .resolve(
                        &id,
                        Some(json!({ "outcome": { "outcome": "selected", "optionId": "reject_once" } })),
                    )
                    .await;
            }
        }
    });

    let outcome =
        prompt::run_prompt(&env.ctx, handle.clone(), "hi".into(), out_tx.clone(), perms).await;
    drop(out_tx);
    responder.await.unwrap();

    assert_eq!(outcome.stop_reason, "end_turn");

    // Rejection surfaces as an error tool_call_update.
    let s = handle.session.read().await;
    let tool_msgs: Vec<ChatMessage> = s
        .context()
        .into_iter()
        .filter(|m| m.role == "tool")
        .collect();
    assert_eq!(tool_msgs.len(), 1);
    assert!(tool_msgs[0].content.contains("rejected by user"));
}

/// `session/cancel` mid-stream: the turn stops with `stopReason: cancelled`
/// and the response carries no usage.
#[tokio::test]
async fn cancel_mid_stream_stops_the_turn() {
    let env = setup(vec![vec![
        ChunkScript::Sleep(10),
        ChunkScript::Content("first bits"),
        ChunkScript::Sleep(60_000), // long stall — cancel lands here
        ChunkScript::Finish(1, 1),
    ]]);
    let registry = Arc::new(AcpSessionRegistry::new());
    let handle = registry.create(&env.ctx, env.cwd.clone()).await;
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();

    let ctx = env.ctx.clone();
    let handle2 = handle.clone();
    let task = tokio::spawn(async move {
        prompt::run_prompt(
            &ctx,
            handle2,
            "hi".into(),
            out_tx,
            Arc::new(PermRegistry::new()),
        )
        .await
    });

    // Wait for the first chunk to stream, then cancel.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(m) = out_rx.try_recv()
                && update_type(&m) == "agent_message_chunk"
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first chunk never arrived");

    handle.cancel().await;
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("prompt task hung")
        .unwrap();

    assert_eq!(outcome.stop_reason, "cancelled");
    assert!(outcome.usage.is_none());
}

/// `session/cancel` while waiting on a permission response: the turn
/// aborts with `cancelled` instead of hanging.
#[tokio::test]
async fn cancel_during_permission_request_aborts_turn() {
    let env = setup(vec![vec![
        ChunkScript::ToolCall("rm --help"),
        ChunkScript::Finish(1, 1),
    ]]);
    let registry = Arc::new(AcpSessionRegistry::new());
    let handle = registry.create(&env.ctx, env.cwd.clone()).await;
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();

    let ctx = env.ctx.clone();
    let handle2 = handle.clone();
    let task = tokio::spawn(async move {
        prompt::run_prompt(
            &ctx,
            handle2,
            "hi".into(),
            out_tx,
            Arc::new(PermRegistry::new()),
        )
        .await
    });

    // Wait until the permission request is on the wire, then cancel
    // without ever answering it.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(m) = out_rx.try_recv()
                && m["method"] == "session/request_permission"
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("permission request never sent");

    handle.cancel().await;
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("prompt task hung")
        .unwrap();
    assert_eq!(outcome.stop_reason, "cancelled");
}

/// Concurrent `session/prompt` calls on the same session: the second is
/// rejected with -32004 while the first runs, and accepted again after.
#[tokio::test]
async fn concurrent_prompt_rejected_with_busy_guard() {
    let env = setup(vec![
        vec![ChunkScript::Content("first"), ChunkScript::Finish(1, 1)],
        vec![ChunkScript::Content("(unused)"), ChunkScript::Sleep(60_000)],
    ]);
    let registry = Arc::new(AcpSessionRegistry::new());
    let handle = registry.create(&env.ctx, env.cwd.clone()).await;
    let sid = handle.id().await;
    let perms = Arc::new(PermRegistry::new());
    let (out_tx, _out_rx) = mpsc::unbounded_channel();

    let r1 = handle_request(&env.ctx, prompt_request(&sid, 1, "one"), &registry, &perms, &out_tx).await;
    assert!(r1.is_none(), "first prompt defers its response");

    let r2 = handle_request(&env.ctx, prompt_request(&sid, 2, "two"), &registry, &perms, &out_tx)
        .await
        .expect("second prompt responds immediately");
    assert_eq!(r2["error"]["code"], -32004);

    // Wait for the first turn to finish…
    tokio::time::timeout(Duration::from_secs(5), async {
        while handle.running.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first prompt never finished");

    // …and the session accepts prompts again.
    let r3 = handle_request(&env.ctx, prompt_request(&sid, 3, "three"), &registry, &perms, &out_tx).await;
    assert!(r3.is_none(), "session free again after the turn completed");
}

/// `session/close` cancels an in-flight prompt and removes the session —
/// subsequent prompts for that id are rejected with -32001.
#[tokio::test]
async fn close_cancels_and_removes_session() {
    let env = setup(vec![vec![ChunkScript::Sleep(60_000), ChunkScript::Finish(1, 1)]]);
    let registry = Arc::new(AcpSessionRegistry::new());
    let handle = registry.create(&env.ctx, env.cwd.clone()).await;
    let sid = handle.id().await;
    let perms = Arc::new(PermRegistry::new());
    let (out_tx, _out_rx) = mpsc::unbounded_channel();

    // Start a long prompt, then close the session underneath it.
    let _r1 = handle_request(&env.ctx, prompt_request(&sid, 1, "one"), &registry, &perms, &out_tx).await;
    let close = RpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "session/close".into(),
        params: Some(json!({ "sessionId": sid })),
    };
    let r2 = handle_request(&env.ctx, close, &registry, &perms, &out_tx)
        .await
        .expect("close responds");
    assert!(r2["result"].is_object());

    // The session is gone — prompts and re-close both fail with -32001.
    let r3 = handle_request(&env.ctx, prompt_request(&sid, 3, "three"), &registry, &perms, &out_tx).await;
    assert_eq!(r3.unwrap()["error"]["code"], -32001);
    assert!(registry.get(&sid).await.is_none());
}
