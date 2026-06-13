//! Feishu implementation of the `TaskExecutor` trait.
//!
//! Converts a due `ScheduledTask` into a chat event and enqueues it
//! through the per-chat serialization pipeline.

use async_trait::async_trait;

use arcc_core::context::SharedContext;
use arcc_core::task::executor::TaskExecutor;
use arcc_storage::db::models::ScheduledTask;

use super::chat_queue::{ChatEvent, ChatQueue};

/// Singleton access to the per-chat queue (shared with webhook).
fn chat_queue() -> &'static ChatQueue {
    static Q: std::sync::OnceLock<ChatQueue> = std::sync::OnceLock::new();
    Q.get_or_init(ChatQueue::new)
}

/// Executes scheduled tasks by forwarding them through the Feishu
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
        let trigger_prompt = format!(
            "[Scheduled task trigger] {}",
            task.task_description,
        );
        chat_queue().enqueue_scheduler(
            self.ctx.clone(),
            ChatEvent {
                chat_id: task.chat_id.clone(),
                chat_type: task.chat_type.clone(),
                open_id: task.open_id.clone(),
                message_id: String::new(),
                user_text: trigger_prompt,
            },
        ).await
    }
}
