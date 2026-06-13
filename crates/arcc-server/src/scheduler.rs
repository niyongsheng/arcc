//! Background scheduler — polls the `scheduled_tasks` table and triggers
//! due tasks via the configured `TaskExecutor`.
//!
//! The scheduler is spawned on server start and runs for the lifetime of
//! the process.  It is backend-agnostic — task execution is delegated to a
//! `TaskExecutor` implementation (Feishu, Slack, etc.).

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::interval;
use tracing::{info, warn};

use arcc_core::context::SharedContext;
use arcc_core::task::executor::TaskExecutor;

/// Interval between scheduler ticks (10 seconds).
const SCHEDULER_TICK: Duration = Duration::from_secs(10);

/// Maximum consecutive failures before a task is marked as `failed`
/// instead of retrying forever.  Resets to 0 on success.
const MAX_CONSECUTIVE_FAILURES: u32 = 10;

/// Run the scheduler loop forever. Expected to be spawned as a background
/// `tokio::spawn` task.
///
/// Every tick:
/// 1. Query SQLite for pending tasks whose `next_run_at` has passed.
/// 2. Mark each as `running`.
/// 3. Delegate execution to `executor.execute(&task)` — the implementation
///    handles dispatching to the appropriate messaging backend.
/// 4. After execution, update `next_run_at` for recurring tasks or mark
///    as `completed` for one-shot tasks.
pub async fn scheduler_loop(ctx: SharedContext, executor: Arc<dyn TaskExecutor>) {
    info!("scheduler started (tick = {SCHEDULER_TICK:?})");

    let mut tick = interval(SCHEDULER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut tick_count: u64 = 0;
    // Track consecutive failures per task to avoid infinite retry loops.
    let mut retry_count: HashMap<String, u32> = HashMap::new();

    loop {
        tick.tick().await;
        tick_count += 1;

        let tasks = match ctx.storage.list_due_tasks() {
            Ok(t) => t,
            Err(e) => {
                // DB errors are transient — log and retry next tick.
                warn!(err = %e, "scheduler: failed to list due tasks");
                continue;
            }
        };

        // Compress overgrown sessions every hour (~360 ticks).
        // This is an LLM call (flash model) so we don't want it on every tick.
        if tick_count.is_multiple_of(360) && let Some(flash) = ctx.providers.flash() {
            ctx.sessions.compress_all(flash.as_ref()).await;
        }

        if tasks.is_empty() {
            continue;
        }

        info!(count = tasks.len(), "scheduler: processing due tasks");

        for task in tasks {
            let task_id = task.id.clone();
            info!(task_id = %task_id, "scheduler: executing task");

            // Mark as running so other scheduler ticks don't pick it up.
            if let Err(e) = ctx.storage.update_task_status(&task_id, "running") {
                warn!(task_id, err = %e, "scheduler: failed to mark task as running");
                continue;
            }

            // Delegate execution to the configured backend.
            // The executor handles serialization per chat_id and returns
            // success/failure so the scheduler can decide retry/completion.
            let ok = executor.execute(&task).await;

            if !ok {
                // Execution failed — track retries.
                let fails = retry_count.entry(task_id.clone()).or_insert(0);
                *fails += 1;
                if *fails >= MAX_CONSECUTIVE_FAILURES {
                    warn!(task_id, retries = *fails, "scheduler: too many consecutive failures, marking failed");
                    let _ = ctx.storage.update_task_status(&task_id, "failed");
                    continue;
                }
                warn!(task_id, retries = *fails, "scheduler: task execution failed, will retry");
                if let Err(e) = ctx.storage.update_task_status(&task_id, "pending") {
                    warn!(task_id, err = %e, "scheduler: failed to reset task status");
                }
                continue;
            }

            // Execution succeeded — clear retry count.
            retry_count.remove(&task_id);
            if let Some(cron) = &task.cron {
                // Recurring: compute next run, reset to pending.
                match cron::Schedule::from_str(cron) {
                    Ok(schedule) => {
                        if let Some(next) = schedule.upcoming(chrono::Local).next() {
                            let next_str = next.format("%Y-%m-%d %H:%M:%S").to_string();
                            if let Err(e) = ctx.storage.update_task_next_run(&task_id, &next_str) {
                                warn!(task_id, err = %e, "scheduler: failed to update next_run");
                            }
                            info!(task_id, next_run = %next_str, "scheduler: recurring task updated");
                        } else {
                            warn!(task_id, "scheduler: cron has no future occurrences, marking completed");
                            let _ = ctx.storage.update_task_status(&task_id, "completed");
                        }
                    }
                    Err(e) => {
                        warn!(task_id, err = %e, "scheduler: invalid cron, marking failed");
                        let _ = ctx.storage.update_task_status(&task_id, "failed");
                    }
                }
            } else {
                // One-shot: mark completed with actual run time.
                if let Err(e) = ctx.storage.complete_task(&task_id) {
                    warn!(task_id, err = %e, "scheduler: failed to complete task");
                }
                info!(task_id, "scheduler: one-shot task completed");
            }
        }
    }
}
