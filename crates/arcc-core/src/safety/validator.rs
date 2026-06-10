use serde::de::DeserializeOwned;

use super::allowlist::Allowlist;

/// Safety validation pipeline for user-facing inputs.
///
/// Run every externally-sourced action through this before execution.
#[derive(Debug, Clone)]
pub struct SafetyValidator {
    allowlist: Allowlist,
    max_cmd_length: usize,
}

impl SafetyValidator {
    pub fn new(allowlist: Allowlist) -> Self {
        Self {
            allowlist,
            max_cmd_length: 4096,
        }
    }

    /// Validate and type-check JSON input from the LLM.
    ///
    /// The LLM outputs structured JSON; this acts as a second boundary
    /// to catch injection or malformed data before it reaches execution.
    pub fn validate_json<T: DeserializeOwned>(&self, raw: &str) -> Result<T, ValidationError> {
        if raw.len() > self.max_cmd_length * 4 {
            return Err(ValidationError::InputTooLarge(raw.len()));
        }
        serde_json::from_str::<T>(raw).map_err(|e| ValidationError::Schema(e.to_string()))
    }

    /// Check a shell command against the allowlist.
    ///
    /// Returns `Ok(needs_human_confirm)` on success.
    /// Returns `Err(reason)` if the command is blocked.
    pub fn validate_command(&self, command: &str) -> Result<ValidationResult, ValidationError> {
        if command.len() > self.max_cmd_length {
            return Err(ValidationError::CommandTooLong(command.len()));
        }

        match self.allowlist.check(command) {
            Ok(needs_confirm) => {
                if needs_confirm {
                    Ok(ValidationResult::NeedsConfirmation)
                } else {
                    Ok(ValidationResult::Allowed)
                }
            }
            Err(reason) => Err(ValidationError::Blocked(reason)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationResult {
    Allowed,
    NeedsConfirmation,
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("command blocked: {0}")]
    Blocked(String),
    #[error("command too long ({0} bytes)")]
    CommandTooLong(usize),
    #[error("input too large ({0} bytes)")]
    InputTooLarge(usize),
    #[error("schema validation failed: {0}")]
    Schema(String),
}
