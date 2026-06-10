pub mod commands;
pub mod event;
pub mod ui;

use arcc_core::context::SharedContext;

/// Run the TUI interactive mode.
pub async fn run(ctx: SharedContext) -> anyhow::Result<()> {
    crate::ui::app::run(ctx).await
}
