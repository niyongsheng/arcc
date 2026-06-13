//! Per-chat-id message queue — serializes feishu message processing.
//!
//! Each active chat has a dedicated mpsc channel + consumer task.
//! Messages are processed one at a time in FIFO order, so the session
//! VecDeque is never corrupted by concurrent writes.
//! The consumer exits after `IDLE_TIMEOUT` of inactivity.
//!
//! Both webhook user messages and scheduler task triggers go through
//! this queue, so they never race on the same session.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{info, warn};

use arcc_core::context::SharedContext;

/// Consumer task exits after this long without any new messages.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Parameters needed to call `process_feishu_chat`.
#[derive(Debug, Clone)]
pub struct ChatEvent {
    pub chat_id: String,
    pub chat_type: String,
    pub open_id: String,
    pub message_id: String,
    pub user_text: String,
}

/// Internal message — either a normal ChatEvent or a scheduler request
/// that carries a oneshot sender for the result.
enum QueueMsg {
    Event(ChatEvent),
    Scheduler(ChatEvent, oneshot::Sender<bool>),
}

/// A per-chat-id, single-consumer queue backed by a tokio mpsc channel.
///
/// The first message for a chat spawns a consumer task.  Subsequent
/// messages are sent to the channel and processed in order.  The
/// consumer exits after `IDLE_TIMEOUT` of idleness and cleans up.
///
/// Both webhook user messages and scheduler task triggers go through
/// this queue, so they never race on the same session.
#[derive(Clone)]
pub struct ChatQueue {
    consumers: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<QueueMsg>>>>,
}

impl Default for ChatQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatQueue {
    pub fn new() -> Self {
        Self {
            consumers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Enqueue a user chat event for processing.
    ///
    /// If a consumer is already running for this `chat_id`, the event
    /// is queued.  Otherwise a new consumer is spawned.
    pub async fn enqueue(&self, ctx: SharedContext, event: ChatEvent) {
        self.enqueue_inner(ctx, QueueMsg::Event(event)).await;
    }

    /// Enqueue a scheduler task-trigger event and wait for the result.
    ///
    /// Returns `true` if processing succeeded, `false` on error (the
    /// same semantics as `process_feishu_chat`'s return value).
    pub async fn enqueue_scheduler(&self, ctx: SharedContext, event: ChatEvent) -> bool {
        let (tx, rx) = oneshot::channel();
        self.enqueue_inner(ctx, QueueMsg::Scheduler(event, tx)).await;
        rx.await.unwrap_or(false)
    }

    async fn enqueue_inner(&self, ctx: SharedContext, msg: QueueMsg) {
        let chat_id = match &msg {
            QueueMsg::Event(e) => e.chat_id.clone(),
            QueueMsg::Scheduler(e, _) => e.chat_id.clone(),
        };
        let mut map = self.consumers.lock().await;

        if let Some(tx) = map.get(&chat_id) {
            // Consumer already running — queue for later.
            if tx.send(msg).is_err() {
                warn!(chat_id = %chat_id, "chat consumer channel closed unexpectedly");
                map.remove(&chat_id);
            }
            return;
        }

        // First message for this chat — spawn a consumer.
        let (tx, rx) = mpsc::unbounded_channel();
        map.insert(chat_id.clone(), tx);
        drop(map);

        let consumers = self.consumers.clone();
        tokio::spawn(async move {
            info!(chat_id = %chat_id, "chat consumer started");
            Self::consumer_loop(&ctx, &consumers, &chat_id, rx, msg).await;
            consumers.lock().await.remove(&chat_id);
            info!(chat_id = %chat_id, "chat consumer exited");
        });
    }

    /// Run the consumer loop: process one event immediately, then wait
    /// for more on the channel with an idle timeout.
    async fn consumer_loop(
        ctx: &SharedContext,
        _consumers: &Arc<Mutex<HashMap<String, mpsc::UnboundedSender<QueueMsg>>>>,
        chat_id: &str,
        rx: mpsc::UnboundedReceiver<QueueMsg>,
        first: QueueMsg,
    ) {
        // Process the first event in-line.
        Self::process_one(ctx, first).await;

        // Then drain the channel.
        let mut rx = rx;
        loop {
            match tokio::time::timeout(IDLE_TIMEOUT, rx.recv()).await {
                Ok(Some(msg)) => {
                    Self::process_one(ctx, msg).await;
                }
                Ok(None) => break,
                Err(_) => {
                    info!(chat_id = %chat_id, idle_timeout = ?IDLE_TIMEOUT, "chat consumer idle, exiting");
                    break;
                }
            }
        }
    }

    async fn process_one(ctx: &SharedContext, msg: QueueMsg) {
        let (event, result_tx) = match msg {
            QueueMsg::Event(e) => (e, None),
            QueueMsg::Scheduler(e, tx) => (e, Some(tx)),
        };
        let ok = super::webhook::process_feishu_chat(
            ctx,
            &event.chat_id,
            &event.chat_type,
            &event.open_id,
            &event.message_id,
            &event.user_text,
        )
        .await;
        // Notify the scheduler if this was a scheduler-triggered event.
        if let Some(tx) = result_tx {
            let _ = tx.send(ok);
        }
    }
}
