//! DingTalk implementation of the `TaskExecutor` trait.

use async_trait::async_trait;

use arcc_core::context::SharedContext;
use arcc_core::task::executor::TaskExecutor;
use arcc_storage::db::models::ScheduledTask;

use crate::im::{ChatEvent, chat_queue};

/// Executes DingTalk scheduled tasks through the shared per-chat pipeline.
///
/// There is no live `sessionWebhook` for a scheduler trigger, so replies go
/// through the robot API using the persisted reply target.
#[derive(Clone)]
pub struct DingtalkTaskExecutor {
    ctx: SharedContext,
}

impl DingtalkTaskExecutor {
    pub fn new(ctx: SharedContext) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl TaskExecutor for DingtalkTaskExecutor {
    async fn execute(&self, task: &ScheduledTask) -> bool {
        let trigger_prompt = format!("[Scheduled task trigger] {}", task.task_description);
        chat_queue()
            .enqueue_scheduler(
                self.ctx.clone(),
                ChatEvent {
                    platform: "dingtalk",
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
