//! Feishu implementation of the `TaskExecutor` trait.
//!
//! Converts a due `ScheduledTask` into a chat event and enqueues it
//! through the per-chat serialization pipeline.

use async_trait::async_trait;

use arcc_core::context::SharedContext;
use arcc_core::task::executor::TaskExecutor;
use arcc_storage::db::models::ScheduledTask;

use crate::im::{ChatEvent, chat_queue};

/// Executes scheduled tasks by forwarding them through the shared
/// per-chat message pipeline.
#[derive(Clone)]
pub struct FeishuTaskExecutor {
    ctx: SharedContext,
}

impl FeishuTaskExecutor {
    pub fn new(ctx: SharedContext) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl TaskExecutor for FeishuTaskExecutor {
    async fn execute(&self, task: &ScheduledTask) -> bool {
        let trigger_prompt = format!("[Scheduled task trigger] {}", task.task_description);
        chat_queue()
            .enqueue_scheduler(
                self.ctx.clone(),
                ChatEvent {
                    platform: "feishu",
                    chat_id: task.chat_id.clone(),
                    chat_type: task.chat_type.clone(),
                    user_id: task.open_id.clone(),
                    message_id: String::new(),
                    reply_id: task.reply_id.clone(),
                    reply_id_type: task.reply_id_type.clone(),
                    session_webhook: None,
                    user_text: trigger_prompt,
                },
            )
            .await
    }
}
