use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// A generic user prompt. The caller awaits a response via the oneshot channel.
#[derive(Debug)]
pub struct PromptRequest {
    /// Markdown message / ASCII art to display in the chat area.
    pub message: String,
    /// Hint text below the message, e.g. "[y] yes · [n] no".
    pub hint: String,
    /// Channel to send the user's trimmed lowercase response (or None if dismissed).
    pub response_tx: oneshot::Sender<Option<String>>,
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
    /// Generic user prompt — caller awaits a response via the oneshot.
    Prompt(PromptRequest),
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
    /// Set the status text (e.g. "compressing...") for the status bar.
    Status(String),
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
