use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use arcc_core::context::AppContext;
use arcc_core::model::deepseek::DeepSeekProvider;
use arcc_core::model::mock::MockProvider;
use arcc_core::model::registry::ProviderRegistry;

#[derive(Parser)]
#[command(name = "arcc", about = "AI Rust Claude CLI", version)]
struct Cli {
    /// Bypass command allowlist (DANGEROUS: allows all shell commands)
    #[arg(long, global = true, hide = true)]
    dangerously_skip_permissions: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start TUI interactive mode
    Tui,
    /// Execute a single prompt (headless, supports pipes)
    Cli {
        /// The prompt to execute (prefix with ! for raw shell command)
        prompt: Vec<String>,
    },
    /// Start background daemon (HTTP + Feishu)
    Server {
        /// Run as daemon with graceful shutdown
        #[arg(long)]
        daemon: bool,
    },
}

fn init_tracing(mode: &str) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,arcc=debug"));

    if mode == "tui" {
        let log_path = std::env::var("ARCC_LOG")
            .unwrap_or_else(|_| "/tmp/arcc-tui.log".into());
        let file = std::fs::File::create(&log_path).expect("create log file");
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_writer(file).with_ansi(false))
            .init();
        eprintln!("[arcc] logs → {log_path}");
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .init();
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mode = match cli.command {
        Command::Tui => "tui",
        Command::Cli { .. } => "cli",
        Command::Server { .. } => "server",
    };

    init_tracing(mode);

    // ---- bootstrap storage ----
    let storage = arcc_storage::ArccStorage::init_default()?;

    // ---- bootstrap model providers ----
    // Priority: config file api_key > env var > mock
    let api_key = storage
        .config
        .model
        .api_key
        .clone()
        .or_else(|| std::env::var("DEEPSEEK_API_KEY").ok());

    let (pro, flash): (
        Arc<dyn arcc_core::model::provider::ModelProvider>,
        Arc<dyn arcc_core::model::provider::ModelProvider>,
    ) = if let Some(ref key) = api_key {
        if key.is_empty() {
            tracing::warn!("DEEPSEEK_API_KEY is empty, using mock providers");
            (
                Arc::new(MockProvider::new("mock-pro")),
                Arc::new(MockProvider::new("mock-flash").with_delay(30)),
            )
        } else {
            let strict = storage.config.model.use_strict_mode;
            tracing::info!(
                strict_mode = strict,
                "using DeepSeek provider"
            );
            (
                Arc::new(DeepSeekProvider::with_strict_mode(
                    &storage.config.model.api_base,
                    key,
                    &storage.config.model.pro_model,
                    strict,
                )),
                Arc::new(DeepSeekProvider::with_strict_mode(
                    &storage.config.model.api_base,
                    key,
                    &storage.config.model.flash_model,
                    strict,
                )),
            )
        }
    } else {
        tracing::warn!("DEEPSEEK_API_KEY not set, using mock providers");
        (
            Arc::new(MockProvider::new("mock-pro")),
            Arc::new(MockProvider::new("mock-flash").with_delay(30)),
        )
    };

    let mut registry = ProviderRegistry::new(
        &storage.config.model.pro_model,
        &storage.config.model.flash_model,
    );
    registry.register(&storage.config.model.pro_model, pro);
    registry.register(&storage.config.model.flash_model, flash);

    // ---- build shared context ----
    let ctx = Arc::new(AppContext::new(registry, storage, cli.dangerously_skip_permissions));

    // ---- auto-detect ARCC.md in project root ----
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.clone();
        loop {
            let arcc_md = dir.join("ARCC.md");
            if arcc_md.exists() {
                if let Ok(content) = std::fs::read_to_string(&arcc_md) {
                    let mut instr = ctx.project_instructions.write().await;
                    *instr = Some(content);
                    tracing::info!(path = %arcc_md.display(), "loaded ARCC.md");
                }
                break;
            }
            if dir.join(".git").exists() || !dir.pop() {
                break;
            }
        }
    }

    // ---- dispatch ----
    match cli.command {
        Command::Tui => {
            tracing::info!("starting TUI mode");
            arcc_tui::run(ctx).await?;
        }
        Command::Cli { prompt } => {
            let input = prompt.join(" ");
            tracing::info!(%input, "starting CLI mode");
            arcc_cli::run(ctx, &input).await?;
        }
        Command::Server { daemon } => {
            tracing::info!(daemon, "starting server mode");
            arcc_server::run(ctx, daemon).await?;
        }
    }

    Ok(())
}
