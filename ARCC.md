## Project Overview
ARCC (AI Rust Claude CLI) is a personal AI assistant that provides three interfaces—TUI, CLI, and HTTP server—for interacting with DeepSeek language models. Built in Rust (edition 2024), it focuses on developer workflows: interactive coding assistance, one-shot pipelines, and automated agent-to-agent communication. The system includes a configurable safety engine, session context compression, a model-agnostic tool executor (MCP and shell), and persistent storage via SQLite. Target audience: developers wanting a local, offline-capable AI coding partner with command-line and TUI integration.

## Quick Start
Copy-paste to build, test, and run:

```bash
# Build all crates
cargo build --release

# Run tests
cargo test

# Run the TUI (after setting API key)
cargo run -- tui
```

Configuration: create `~/.arcc/config.toml` with your DeepSeek API key (see `config/config.toml` for all options):

```toml
[model]
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
```

## Repo Anatomy
- `src/main.rs` – Binary entrypoint; dispatches subcommands (`tui`, `cli`, `server`) to workspace crates.
- `crates/` – Workspace members:
  - `arcc-core` – Core logic: model providers, safety engine, session manager, tool executor.
  - `arcc-storage` – SQLite + TOML config + JSON-L audit log.
  - `arcc-server` – Axum server with Feishu SSE integration (`server --daemon`).
  - `arcc-tui` – Interactive terminal UI (ratatui + crossterm).
  - `arcc-cli` – One-shot CLI with pipe-friendly output.
- `config/` – Reference `config.toml`.
- `docs/` – Design notes (DSML handling, sudo password prompts).
- `scripts/` – Install scripts, health check.

## Intelligent Conventions
- **Error handling**: Libraries (`arcc-core`, `arcc-storage`) use `thiserror`; application code uses `anyhow` for ergonomic propagation.
- **Async**: All I/O is `tokio`-based; `async-trait` for trait objects in core.
- **CLI**: `clap` derive macros for argument parsing; subcommands defined in respective crate roots.
- **Logging**: `tracing` with `env-filter`; set `RUST_LOG=arcc=debug` to enable.
- **Testing**: Unit tests in `src/` files, integration tests in `tests/` directories (e.g., `crates/arcc-core/tests`). Run a single crate’s tests: `cargo test -p arcc-core`.
- **Git history** uses conventional commits (`feat:`, `fix:`, `refactor:`); no strict branch strategy evident.
- **Workspace dependency injection**: internal crates aliased as `arcc-<name>` in root `Cargo.toml`; use `arcc_core::*` in code.

## Design Decisions
- **Crate separation**: `arcc-core` is model-agnostic, allowing different providers. `arcc-storage` isolates persistence; swapping SQLite for another backend only touches that crate.
- **Model selection**: DeepSeek-V4 Pro (complex reasoning) and Flash (high-frequency dialogue) chosen via config; token counting uses `tiktoken-rs`.
- **TUI framework**: `ratatui` + `crossterm` for wide platform support and minimal dependencies over immediate-mode alternatives.
- **MCP integration**: Tool executor implements a lightweight Model Context Protocol; external plugins can be loaded as subprocesses.
- **ARCC.md generation (`/init` command)**: calls the AI to analyze the current project and writes `ARCC.md`; streaming output shown in chat. Implementation is spread across `arcc-tui` (Prompt component) and `arcc-core` (session interaction).
- **Config location**: `~/.arcc/config.toml`; environment variable `ARCC_API_KEY` overrides the config file’s `api_key`.
- **FEATURE FLAGS**: `portable-pty` is used for shell tool execution, may require `libutil` or `libc` depending on OS; compiled unconditionally.

## Common Pitfalls
- **API key missing**: TUI starts but any model call fails; check `~/.arcc/config.toml` or `ARCC_API_KEY`.
- **Input buffer leaks**: Fixed in `79ae715`, but if the TUI seems to read stale input after streaming, ensure you are on latest commit.
- **Rust edition 2024**: Requires Rust >= 1.85 (nightly/beta as of early 2025) or latest stable if edition stabilized.
- **`portable-pty`** can panic on Windows without a proper conpty host; prefer Linux/macOS for full tool functionality.
- **Large context tokens**: setting `context_max_tokens` above 800k may exhaust memory on low-RAM machines.
- **Streaming in TUI**: certain escape sequences from the model output may disrupt the UI; we sanitize most, but raw binary STDOUT from shell tools can cause rendering glitches.

## Notice
Key interfaces and flags an AI will likely interact with:

- **TUI `/init`** – Generates an `ARCC.md` for the current working directory. The assistant will receive this command and must produce a Markdown document following the structure shown in ARCC.md template (Project Overview, Quick Start, etc.). The AI’s response is streamed into the chat.
- **CLI** – `arcc cli "<prompt>"`; non-interactive, prints response to stdout. Use for scripting.
- **Server** – `arcc server --daemon` starts HTTP API (Axum) on `127.0.0.1:3030` by default; `POST /v1/chat` with JSON payload. Feishu SSE callback endpoint available at `/feishu/event`.
- **Config** – TOML file; model parameters, safety rules, tool allowlists. Reloaded on SIGHUP or when `notify` detects changes.
- **Environment** – `ARCC_API_KEY` overrides config file API key; `RUST_LOG` controls tracing verbosity.