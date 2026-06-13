//! Background scheduler — polls the `scheduled_tasks` table and triggers
//! due tasks via the feishu processing flow.
//!
//! The scheduler is spawned on server start (only when feishu is configured)
//! and runs for the lifetime of the process.

use std::str::FromStr;
use std::time::Duration;

use tokio::time::interval;
use tracing::{info, warn};

use arcc_core::context::SharedContext;

/// Interval between scheduler ticks (10 seconds).
const SCHEDULER_TICK: Duration = Duration::from_secs(10);

/// Run the scheduler loop forever. Expected to be spawned as a background
/// `tokio::spawn` task.
///
/// Every tick:
/// 1. Query SQLite for pending tasks whose `next_run_at` has passed.
/// 2. Mark each as `running`.
/// 3. Call `process_feishu_chat` (from the webhook module) to execute the
///    task — this reuses the full LLM + tool-calling loop.
/// 4. After execution, update `next_run_at` for recurring tasks or mark
///    as `completed` for one-shot tasks.
pub async fn scheduler_loop(ctx: SharedContext) {
    info!("scheduler started (tick = {SCHEDULER_TICK:?})");

    let mut tick = interval(SCHEDULER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut tick_count: u64 = 0;

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

            // Reuse the full feishu processing flow.  The task_description
            // is passed as the user's prompt — the LLM will re-read it,
            // plan the steps, and execute commands as needed.
            crate::feishu::webhook::process_feishu_chat(
                &ctx,
                &task.chat_id,
                &task.chat_type,
                &task.open_id,
                "",     // no original message_id
                &task.task_description,
            )
            .await;

            // Update scheduling state.
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
