use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEventKind};
use tokio::sync::mpsc;

use super::loop_event::AppEvent;

/// Spawn a task that reads terminal input and forwards it to the event channel.
///
/// Returns a `JoinHandle` that can be aborted to stop the handler (e.g. before
/// running an interactive child process that needs exclusive stdin access).
///
/// Mouse capture is NOT enabled — text selection in the terminal works normally.
/// Most terminals convert mouse scroll-wheel events into ↑/↓ or PgUp/PgDn key
/// events, which the App handles contextually (scroll chat when input is empty,
/// navigate history when input has content).
pub fn spawn_input_handler(tx: mpsc::UnboundedSender<AppEvent>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::task::yield_now().await;

            if !event::poll(std::time::Duration::from_millis(10)).unwrap_or(false) {
                continue;
            }

            match event::read() {
                Ok(CrosstermEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                    match key.code {
                        KeyCode::Char('c')
                            if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            let _ = tx.send(AppEvent::Quit);
                            break;
                        }
                        KeyCode::Enter => {
                            let _ = tx.send(AppEvent::Input("\n".into()));
                        }
                        KeyCode::Tab => {
                            let _ = tx.send(AppEvent::Tab);
                        }
                        KeyCode::PageUp => {
                            let _ = tx.send(AppEvent::ScrollUp(10));
                        }
                        KeyCode::PageDown => {
                            let _ = tx.send(AppEvent::ScrollDown(10));
                        }
                        KeyCode::Home => {
                            let _ = tx.send(AppEvent::ScrollUp(u16::MAX));
                        }
                        KeyCode::End => {
                            let _ = tx.send(AppEvent::ScrollDown(u16::MAX));
                        }
                        KeyCode::Left => {
                            let _ = tx.send(AppEvent::Input("\x1b[D".into()));
                        }
                        KeyCode::Right => {
                            let _ = tx.send(AppEvent::Input("\x1b[C".into()));
                        }
                        KeyCode::Up => {
                            let _ = tx.send(AppEvent::HistoryPrev);
                        }
                        KeyCode::Down => {
                            let _ = tx.send(AppEvent::HistoryNext);
                        }
                        KeyCode::Backspace if key.kind == KeyEventKind::Press
                            || key.kind == KeyEventKind::Repeat =>
                        {
                            let _ = tx.send(AppEvent::Input("\x08".into()));
                        }
                        KeyCode::Esc => {
                            let _ = tx.send(AppEvent::Dismiss);
                        }
                        KeyCode::Char(ch) if key.kind == KeyEventKind::Press
                            || key.kind == KeyEventKind::Repeat =>
                        {
                            let _ = tx.send(AppEvent::Input(ch.to_string()));
                        }
                        _ => {}
                    }
                }
                Ok(CrosstermEvent::Paste(content)) => {
                    // Bracketed paste: send entire pasted content as one event.
                    // Embedded \n are part of the text, not Enter key presses.
                    // Strip \r (CR) from CRLF line endings that macOS terminals
                    // may include when pasting from other applications.
                    let cleaned = content.replace('\r', "");
                    let _ = tx.send(AppEvent::Input(cleaned));
                }
                Ok(CrosstermEvent::Resize(cols, rows)) => {
                    let _ = tx.send(AppEvent::Resize { cols, rows });
                }
                Err(e) => {
                    tracing::error!(err = %e, "terminal input error");
                    break;
                }
                _ => {}
            }
        }
    })
}
