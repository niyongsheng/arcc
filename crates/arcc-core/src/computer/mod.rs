//! Desktop computer-use tools (screenshot + synthetic mouse/keyboard).
//!
//! v1 targets macOS. The actual capture/input implementation lives in
//! [`capture`] and [`input`]; other platforms compile [`stub`] so the rest of
//! the workspace (and CI) still builds.
//!
//! DeepSeek accepts images only on `user` messages, so a screenshot is never
//! returned as a `tool` message. Instead the tool result is JSON text and the
//! TUI appends a follow-up user message carrying the PNG.

use serde::Deserialize;
use serde_json::json;

use crate::model::types::{ImageAttachment, ToolDefinition};
use arcc_storage::config::loader::ComputerUseConfig;

#[cfg(target_os = "macos")]
mod capture;
pub mod coord;
#[cfg(target_os = "macos")]
mod input;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod imp;
#[cfg(not(target_os = "macos"))]
#[path = "stub.rs"]
mod imp;

/// Public tool name exposed to the model.
pub const TOOL_NAME: &str = "computer";

/// Whether native desktop control is available in this build.
pub const fn is_supported() -> bool {
    cfg!(target_os = "macos")
}

/// Geometry of the most recent screenshot, used to map image-pixel
/// coordinates back to global screen coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenGeometryInfo {
    /// Index in `Monitor::all()` order.
    pub display: usize,
    pub display_count: usize,
    /// Global logical origin (points on macOS).
    pub screen_x: i32,
    pub screen_y: i32,
    /// Logical size (points on macOS).
    pub screen_width: u32,
    pub screen_height: u32,
    /// Dimensions of the PNG the model actually saw.
    pub image_width: u32,
    pub image_height: u32,
    pub scale_factor: f32,
}

/// Result of one `computer` tool call.
#[derive(Debug, Default)]
pub struct ComputerOutput {
    /// JSON text placed in the `tool`-role message.
    pub text: String,
    /// Images to attach to the following `user` message.
    pub images: Vec<ImageAttachment>,
    /// Updated screen geometry (after a screenshot).
    pub screen: Option<ScreenGeometryInfo>,
}

/// Per-conversation computer-use state.
///
/// Keeps the geometry of the last screenshot so click/type coordinates can be
/// mapped without re-capturing. The TUI owns one instance for the lifetime of
/// a tool-calling loop.
#[derive(Debug, Default)]
pub struct ComputerSession {
    pub last_screen: Option<ScreenGeometryInfo>,
}

/// Parse and validate tool arguments without executing them.
///
/// The TUI calls this before showing a confirmation prompt so malformed calls
/// are rejected without asking the user to approve an action that cannot run.
pub fn validate_arguments(arguments: &serde_json::Value) -> Result<(), ComputerError> {
    let args: ComputerArgs = serde_json::from_value(arguments.clone())
        .map_err(|e| ComputerError::InvalidArgs(e.to_string()))?;
    args.validate()
}

impl ComputerSession {
    /// Parse arguments, validate them, and execute the action.
    pub async fn execute(
        &mut self,
        arguments: &serde_json::Value,
        cfg: &ComputerUseConfig,
    ) -> Result<ComputerOutput, ComputerError> {
        let args: ComputerArgs = serde_json::from_value(arguments.clone())
            .map_err(|e| ComputerError::InvalidArgs(e.to_string()))?;
        args.validate()?;
        imp::execute(self, &args, cfg).await
    }
}

/// Tool schema for the model.
pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_NAME.into(),
        description: "Control the user's desktop. Before clicking or typing, call action=screenshot \
                      to see the current screen. Coordinates are pixels in the most recent screenshot \
                      (origin top-left); each screenshot reports its image width/height. After a \
                      mutating action a fresh screenshot is returned unless include_screenshot=false. \
                      Prefer keyboard shortcuts. Do one action at a time and verify with a screenshot. \
                      Never type passwords, 2FA codes, or payment details; never interact with \
                      security/privacy dialogs unless the user explicitly asks."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "screenshot", "status", "click", "double_click", "right_click",
                        "move", "drag", "scroll", "type", "key", "wait"
                    ],
                    "description": "The desktop action to perform."
                },
                "x": { "type": "integer", "description": "X coordinate in the latest screenshot's pixels." },
                "y": { "type": "integer", "description": "Y coordinate in the latest screenshot's pixels." },
                "to_x": { "type": "integer", "description": "Drag end X (image pixels)." },
                "to_y": { "type": "integer", "description": "Drag end Y (image pixels)." },
                "button": { "type": "string", "enum": ["left", "right", "middle"] },
                "text": { "type": "string", "description": "Text to type." },
                "press_enter": { "type": "boolean", "description": "Press Return after typing." },
                "keys": { "type": "string", "description": "Key combo, e.g. cmd+shift+4, ctrl+c, Return, Escape, F5." },
                "direction": { "type": "string", "enum": ["up", "down", "left", "right"] },
                "amount": { "type": "integer", "description": "Scroll amount in lines (default 3)." },
                "milliseconds": { "type": "integer", "description": "Wait duration (max 10000 ms)." },
                "display": { "type": "integer", "description": "Monitor index; defaults to the primary display." },
                "include_screenshot": { "type": "boolean", "description": "Capture a verification screenshot after the action." }
            },
            "required": ["action"]
        }),
        strict: false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerAction {
    Screenshot,
    Status,
    Click,
    DoubleClick,
    RightClick,
    Move,
    Drag,
    Scroll,
    Type,
    Key,
    Wait,
}

impl ComputerAction {
    /// Read-only actions do not need a confirmation prompt.
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::Screenshot | Self::Status)
    }
}

/// Strongly-typed tool arguments. Unknown fields are ignored so model
/// hallucinations do not break an otherwise valid call.
#[derive(Debug, Clone, Deserialize)]
pub struct ComputerArgs {
    pub action: ComputerAction,
    #[serde(default)]
    pub x: Option<i64>,
    #[serde(default)]
    pub y: Option<i64>,
    #[serde(default)]
    pub to_x: Option<i64>,
    #[serde(default)]
    pub to_y: Option<i64>,
    #[serde(default)]
    pub button: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub press_enter: Option<bool>,
    #[serde(default)]
    pub keys: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub amount: Option<i64>,
    #[serde(default)]
    pub milliseconds: Option<u64>,
    #[serde(default)]
    pub display: Option<usize>,
    #[serde(default)]
    pub include_screenshot: Option<bool>,
}

impl ComputerArgs {
    pub fn validate(&self) -> Result<(), ComputerError> {
        let need_xy = |a: &Self| -> Result<(), ComputerError> {
            if a.x.is_none() || a.y.is_none() {
                Err(ComputerError::InvalidArgs(
                    "x and y are required for this action".into(),
                ))
            } else {
                Ok(())
            }
        };

        match self.action {
            ComputerAction::Screenshot | ComputerAction::Status | ComputerAction::Wait => Ok(()),
            ComputerAction::Click
            | ComputerAction::DoubleClick
            | ComputerAction::RightClick
            | ComputerAction::Move => need_xy(self),
            ComputerAction::Drag => {
                need_xy(self)?;
                if self.to_x.is_none() || self.to_y.is_none() {
                    Err(ComputerError::InvalidArgs(
                        "to_x and to_y are required for drag".into(),
                    ))
                } else {
                    Ok(())
                }
            }
            ComputerAction::Scroll => match self.direction.as_deref() {
                Some("up" | "down" | "left" | "right") => Ok(()),
                _ => Err(ComputerError::InvalidArgs(
                    "direction must be one of: up, down, left, right".into(),
                )),
            },
            ComputerAction::Type => match self.text.as_deref() {
                Some(t) if !t.is_empty() => Ok(()),
                _ => Err(ComputerError::InvalidArgs(
                    "text is required for the type action".into(),
                )),
            },
            ComputerAction::Key => match self.keys.as_deref() {
                Some(k) if !k.trim().is_empty() => Ok(()),
                _ => Err(ComputerError::InvalidArgs(
                    "keys is required for the key action".into(),
                )),
            },
        }
    }

    /// Whether a verification screenshot should be taken after the action.
    pub fn wants_screenshot(&self, default_after_action: bool) -> bool {
        if self.action == ComputerAction::Screenshot {
            return true;
        }
        if let Some(explicit) = self.include_screenshot {
            return explicit;
        }
        match self.action {
            ComputerAction::Move | ComputerAction::Wait | ComputerAction::Status => false,
            ComputerAction::Screenshot => true,
            _ => default_after_action,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ComputerError {
    #[error("computer use is not supported on this platform")]
    Unsupported,
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("screen capture failed: {0}")]
    Capture(String),
    #[error("input simulation failed: {0}")]
    Input(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl ComputerError {
    /// User-facing remediation hint, if any.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::Unsupported => {
                Some("Desktop control is currently available on macOS only (TUI mode).")
            }
            Self::Capture(_) => Some(
                "Grant Screen Recording to your terminal in System Settings → Privacy & Security \
                 → Screen Recording, then restart the terminal.",
            ),
            Self::Input(_) => Some(
                "Grant Accessibility to your terminal in System Settings → Privacy & Security \
                 → Accessibility, then restart the terminal.",
            ),
            Self::InvalidArgs(_) | Self::Internal(_) => None,
        }
    }
}

/// Build the JSON error text returned to the model.
pub fn error_text(err: &ComputerError) -> String {
    let mut value = json!({
        "ok": false,
        "error": err.to_string(),
    });
    if let Some(hint) = err.hint() {
        value["hint"] = json!(hint);
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(value: serde_json::Value) -> ComputerArgs {
        serde_json::from_value(value).expect("args should parse")
    }

    #[test]
    fn click_requires_xy() {
        let a = args(json!({"action": "click"}));
        assert!(a.validate().is_err());
        let a = args(json!({"action": "click", "x": 1, "y": 2}));
        assert!(a.validate().is_ok());
    }

    #[test]
    fn drag_requires_end_points() {
        let a = args(json!({"action": "drag", "x": 1, "y": 2}));
        assert!(a.validate().is_err());
        let a = args(json!({"action": "drag", "x": 1, "y": 2, "to_x": 3, "to_y": 4}));
        assert!(a.validate().is_ok());
    }

    #[test]
    fn scroll_requires_valid_direction() {
        let a = args(json!({"action": "scroll", "direction": "sideways"}));
        assert!(a.validate().is_err());
        let a = args(json!({"action": "scroll", "direction": "down"}));
        assert!(a.validate().is_ok());
    }

    #[test]
    fn type_and_key_require_content() {
        assert!(
            args(json!({"action": "type", "text": ""}))
                .validate()
                .is_err()
        );
        assert!(
            args(json!({"action": "type", "text": "hi"}))
                .validate()
                .is_ok()
        );
        assert!(
            args(json!({"action": "key", "keys": "  "}))
                .validate()
                .is_err()
        );
        assert!(
            args(json!({"action": "key", "keys": "cmd+c"}))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn screenshot_defaults() {
        let a = args(json!({"action": "screenshot"}));
        assert!(a.wants_screenshot(false));
        let a = args(json!({"action": "move", "x": 1, "y": 2}));
        assert!(!a.wants_screenshot(true));
        let a = args(json!({"action": "click", "x": 1, "y": 2}));
        assert!(a.wants_screenshot(true));
        let a = args(json!({"action": "click", "x": 1, "y": 2, "include_screenshot": false}));
        assert!(!a.wants_screenshot(true));
    }

    #[test]
    fn read_only_actions() {
        assert!(ComputerAction::Screenshot.is_read_only());
        assert!(ComputerAction::Status.is_read_only());
        assert!(!ComputerAction::Click.is_read_only());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires macOS Screen Recording permission"]
    async fn captures_a_real_screenshot() {
        let cfg = ComputerUseConfig {
            max_screenshot_width: 640,
            ..Default::default()
        };
        let mut session = ComputerSession::default();
        let output = session
            .execute(&json!({"action": "screenshot"}), &cfg)
            .await
            .expect("screenshot should succeed with Screen Recording permission");

        assert_eq!(output.images.len(), 1);
        assert!(output.images[0].width > 0);
        assert!(!output.images[0].data_base64.is_empty());
        assert!(output.screen.is_some());
        assert!(output.text.contains("image_width"));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires a live macOS GUI session"]
    async fn reads_monitor_and_cursor_status() {
        let cfg = ComputerUseConfig::default();
        let mut session = ComputerSession::default();
        let output = session
            .execute(&json!({"action": "status"}), &cfg)
            .await
            .expect("status should succeed");
        assert!(output.images.is_empty());
        assert!(output.text.contains("monitors"));
    }
}
