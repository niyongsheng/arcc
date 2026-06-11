use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// User's choice when a command is blocked and needs permission.
#[derive(Debug)]
pub enum ConfirmChoice {
    /// Allow this one execution, don't save to allowlist.
    AllowOnce,
    /// Allow and add the command to the runtime allowlist.
    AllowAlways,
    /// Reject execution.
    Reject,
}

/// Events that flow through the MPSC channel from input/model to the renderer.
#[derive(Debug)]
pub enum AppEvent {
    /// User typed a line and pressed Enter.
    Input(String),
    /// Navigate input history backward (↑)
    HistoryPrev,
    /// Navigate input history forward (↓)
    HistoryNext,
    /// Tab key — trigger completion / cycle candidates
    Tab,
    /// A streaming token from the model or tool execution status.
    Token(String),
    /// Tool execution output line (rendered as a separate dimmed line).
    ToolExec(String),
    /// Model reasoning_content token.
    Reasoning(String),
    /// Model response finished.
    StreamDone,
    /// UI tick (e.g. for status bar clock).
    Tick,
    /// Terminal resize.
    Resize { cols: u16, rows: u16 },
    /// Scroll chat area up by N lines.
    ScrollUp(u16),
    /// Scroll chat area down by N lines.
    ScrollDown(u16),
    /// Request to quit.
    Quit,
    /// Command blocked by allowlist — user must choose AllowOnce / AllowAlways / Reject.
    /// The tool-execution task is suspended waiting on the oneshot sender.
    ConfirmCommand {
        command: String,
        tx: oneshot::Sender<ConfirmChoice>,
    },
    /// Interactive command — TUI should temporarily exit, let the user interact
    /// with the command in the real terminal, then re-enter TUI.
    /// The response_tx sends back the exit code as a string.
    InteractiveCommand {
        command: String,
        response_tx: oneshot::Sender<String>,
    },
    /// Live system metrics update (CPU, memory, network).
    LiveMetrics {
        cpu_pct: f64,
        mem_pct: f64,
        rx_rate: f64,
        tx_rate: f64,
    },
    /// Dismiss current overlay (dashboard, etc.)
    Dismiss,
}

/// Create the MPSC channel and spawn a tick generator.
pub fn create_event_loop() -> (mpsc::UnboundedSender<AppEvent>, mpsc::UnboundedReceiver<AppEvent>)
{
    let (tx, rx) = mpsc::unbounded_channel();
    let tick_tx = tx.clone();

    // Background tick — drives UI updates independent of model inference.
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if tick_tx.send(AppEvent::Tick).is_err() {
                break;
            }
        }
    });

    (tx, rx)
}
