use serde::Deserialize;
use std::path::Path;
use tracing::info;

/// Complete ARCC configuration loaded from TOML.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ArccConfig {
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub feishu: FeishuConfig,
    #[serde(default)]
    pub safety: SafetyConfig,
    #[serde(default)]
    pub execution: ExecutionConfig,
    #[serde(default)]
    pub computer_use: ComputerUseConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_api_base")]
    pub api_base: String,
    /// Optional API key written directly in config file.
    /// If set, takes precedence over the env var specified by `api_key_env`.
    #[serde(default)]
    pub api_key: Option<String>,

    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_pro_model")]
    pub pro_model: String,
    #[serde(default = "default_flash_model")]
    pub flash_model: String,
    #[serde(default = "default_context_max")]
    pub context_max_tokens: usize,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_max_output")]
    pub max_output_tokens: u32,
    /// Enable strict function-calling mode (Beta endpoint).
    /// Forces the model to strictly follow JSON Schema for tool calls.
    #[serde(default)]
    pub use_strict_mode: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// API Key for HTTP endpoint authentication.
    /// When set, requests to protected endpoints (`/chat`, `/memory`)
    /// must include `Authorization: Bearer <key>`.
    /// When empty or unset, authentication is disabled (backwards compatible).
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_metrics_path")]
    pub metrics_path: String,
    #[serde(default = "default_health_path")]
    pub health_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeishuConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub app_secret: String,
    #[serde(default)]
    pub verification_token: String,
    #[serde(default = "default_feishu_webhook_path")]
    pub webhook_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SafetyConfig {
    /// Commands that need human confirmation before execution.
    /// All other commands are allowed to run without restriction.
    #[serde(default)]
    pub require_human_confirm: Vec<String>,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            require_human_confirm: vec![
                "rm".into(),
                "mv".into(),
                "dd".into(),
                "mkfs".into(),
                "shutdown".into(),
                "reboot".into(),
                "fdisk".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionConfig {
    #[serde(default = "default_command_timeout")]
    pub command_timeout_seconds: u64,
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            command_timeout_seconds: default_command_timeout(),
            max_output_bytes: default_max_output_bytes(),
        }
    }
}

/// Desktop computer-use settings (TUI, macOS).
///
/// Disabled by default. Enabling it lets the model take screenshots and
/// synthesize mouse/keyboard input; every input action is confirmed in the
/// TUI unless `require_confirmation = false`.
#[derive(Debug, Clone, Deserialize)]
pub struct ComputerUseConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub require_confirmation: bool,
    /// How long the TUI waits for a y/a/n answer before rejecting the action.
    #[serde(default = "default_confirm_timeout")]
    pub confirmation_timeout_seconds: u64,
    /// Maximum screenshot→action rounds per user prompt.
    #[serde(default = "default_computer_max_rounds")]
    pub max_rounds: u32,
    /// Screenshots are downscaled to this width before being sent to the model.
    #[serde(default = "default_max_screenshot_width")]
    pub max_screenshot_width: u32,
    /// How many historical screenshots stay attached to the request context.
    #[serde(default = "default_keep_screenshots")]
    pub keep_screenshots: usize,
    /// Capture a verification screenshot after each mutating action.
    #[serde(default = "default_true")]
    pub capture_after_action: bool,
    /// Delay before the post-action screenshot so the UI can settle.
    #[serde(default = "default_action_settle_ms")]
    pub action_settle_ms: u64,
    /// Provider used for image-bearing requests. Empty = flash_model.
    #[serde(default)]
    pub vision_model: String,
    /// Persist screenshots under ~/.arcc/screenshots for debugging.
    #[serde(default)]
    pub save_screenshots: bool,
}

impl Default for ComputerUseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            require_confirmation: true,
            confirmation_timeout_seconds: default_confirm_timeout(),
            max_rounds: default_computer_max_rounds(),
            max_screenshot_width: default_max_screenshot_width(),
            keep_screenshots: default_keep_screenshots(),
            capture_after_action: true,
            action_settle_ms: default_action_settle_ms(),
            vision_model: String::new(),
            save_screenshots: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_db_path")]
    pub db_path: String,
    #[serde(default = "default_config_watch_interval")]
    pub config_watch_interval: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_arcc_level")]
    pub arcc_level: String,
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
}

// --- Default implementations ---

fn default_provider() -> String {
    "deepseek".into()
}
fn default_api_base() -> String {
    "https://api.deepseek.com".into()
}
fn default_api_key_env() -> String {
    "DEEPSEEK_API_KEY".into()
}
fn default_pro_model() -> String {
    "deepseek-v4-pro".into()
}
fn default_flash_model() -> String {
    "deepseek-flash".into()
}
fn default_context_max() -> usize {
    8000
}
fn default_temperature() -> f32 {
    0.7
}
fn default_max_output() -> u32 {
    4096
}
fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    9527
}
fn default_metrics_path() -> String {
    "/metrics".into()
}
fn default_health_path() -> String {
    "/health".into()
}
fn default_feishu_webhook_path() -> String {
    "/feishu/webhook".into()
}
fn default_db_path() -> String {
    "arcc.db".into()
}
fn default_config_watch_interval() -> u64 {
    5
}
fn default_command_timeout() -> u64 {
    30
}
fn default_max_output_bytes() -> usize {
    4096
}
fn default_true() -> bool {
    true
}
fn default_confirm_timeout() -> u64 {
    120
}
fn default_computer_max_rounds() -> u32 {
    20
}
fn default_max_screenshot_width() -> u32 {
    1280
}
fn default_keep_screenshots() -> usize {
    1
}
fn default_action_settle_ms() -> u64 {
    250
}
fn default_log_level() -> String {
    "info".into()
}
fn default_arcc_level() -> String {
    "debug".into()
}
fn default_log_dir() -> String {
    "logs".into()
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            api_base: default_api_base(),
            api_key: None,
            api_key_env: default_api_key_env(),
            pro_model: default_pro_model(),
            flash_model: default_flash_model(),
            context_max_tokens: default_context_max(),
            temperature: default_temperature(),
            max_output_tokens: default_max_output(),
            use_strict_mode: false,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            api_key: None,
            metrics_path: default_metrics_path(),
            health_path: default_health_path(),
        }
    }
}

impl Default for FeishuConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: String::new(),
            app_secret: String::new(),
            verification_token: String::new(),
            webhook_path: default_feishu_webhook_path(),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            config_watch_interval: default_config_watch_interval(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            arcc_level: default_arcc_level(),
            log_dir: default_log_dir(),
        }
    }
}

/// Load configuration from a TOML file.
pub fn load(path: &Path) -> Result<ArccConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    let config: ArccConfig =
        toml::from_str(&contents).map_err(|e| ConfigError::Parse(e.to_string()))?;

    info!(path = %path.display(), "configuration loaded");
    Ok(config)
}

/// Load an allowlist TOML file (separate file for operational convenience).
pub fn load_allowlist(path: &Path) -> Result<Vec<String>, ConfigError> {
    #[derive(Deserialize)]
    struct AllowlistFile {
        allowlist: Vec<String>,
    }

    let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    let file: AllowlistFile =
        toml::from_str(&contents).map_err(|e| ConfigError::Parse(e.to_string()))?;

    Ok(file.allowlist)
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computer_use_defaults_are_safe() {
        let config = ArccConfig::default();
        assert!(!config.computer_use.enabled);
        assert!(config.computer_use.require_confirmation);
        assert_eq!(config.computer_use.confirmation_timeout_seconds, 120);
        assert_eq!(config.computer_use.max_rounds, 20);
        assert_eq!(config.computer_use.max_screenshot_width, 1280);
        assert_eq!(config.computer_use.keep_screenshots, 1);
        assert_eq!(config.computer_use.action_settle_ms, 250);
        assert!(!config.computer_use.save_screenshots);
    }

    #[test]
    fn parses_computer_use_section() {
        let config: ArccConfig = toml::from_str(
            r#"
            [computer_use]
            enabled = true
            max_rounds = 5
            vision_model = "deepseek-flash"
            "#,
        )
        .expect("valid config");
        assert!(config.computer_use.enabled);
        assert_eq!(config.computer_use.max_rounds, 5);
        assert_eq!(config.computer_use.vision_model, "deepseek-flash");
        // Unspecified fields keep their safe defaults.
        assert!(config.computer_use.require_confirmation);
    }
}
