//! Task executor trait — abstraction over how scheduled tasks are
//! delivered to the user.  Each channel (Feishu, Slack, etc.)
//! implements this trait so the scheduler never depends on a
//! specific messaging backend.

use async_trait::async_trait;

use arcc_storage::db::models::ScheduledTask;

/// A backend that can execute a scheduled task when it fires.
///
/// The scheduler calls `execute` for each due task.  Implementations
/// convert the `ScheduledTask` into a channel-specific action (e.g.
/// send a Feishu message, post to Slack, trigger a webhook) and
/// return `true` on success or `false` on failure.
#[async_trait]
pub trait TaskExecutor: Send + Sync {
    /// Execute a due scheduled task.
    ///
    /// Returns `true` if the task was completed successfully, `false`
    /// if execution failed (the scheduler may retry).
    async fn execute(&self, task: &ScheduledTask) -> bool;
}
