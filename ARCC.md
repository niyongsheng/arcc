## Project Overview
ARCC (AI Rust Claude CLI) is a three-in-one personal AI assistant. It offers an interactive TUI clone of Claude Code, a single-shot CLI for pipe-friendly automation, and an Axum-based server with Feishu IM integration. The core uses DeepSeek-V4 (Pro and Flash) models for reasoning, shell command execution, and MCP tool use, guarded by an allowlist-based safety engine. Built in Rust with Tokio async, SQLite storage, and TOML configuration, it targets developers who want a fast, self-hosted AI coding sidekick.

## Quick Start
```bash
# build
cargo build --release

# run TUI (default)
./target/release/arcc tui

# one-shot CLI query
./target/release/arcc cli "explain this error"

# start server in daemon mode
./target/release/arcc server --daemon
```
Before running, create `~/.arcc/config.toml`:
```toml
[model]
api_key = "sk-your-deepseek-key"
```
Or export `ARCC_API_KEY=sk-...`.

## Repo Anatomy
- `src/main.rs` – entry point; dispatches to `tui`, `cli`, or `server` subcommands via `clap`.
- `crates/arcc-core` – AI logic: model providers, session manager, safety engine, tool executor.
- `crates/arcc-storage` – persistence: SQLite sessions/messages, TOML config, JSONL audit logs.
- `crates/arcc-server` – HTTP server (`axum`), SSE chat, Feishu bot webhook.
- `crates/arcc-tui` – Terminal UI (`ratatui` + `crossterm`) for interactive coding.
- `crates/arcc-cli` – One-shot CLI with PTY for sudo prompts, pipeable output.

## Intelligent Conventions
- **Error handling**: `thiserror` in libraries, `anyhow` in binaries; `?` for propagation.
- **Async**: Tokio multi-threaded runtime; `async_trait`, `tokio::spawn`, `tokio_stream::StreamExt`.
- **Naming**: modules/files snake_case, structs/enums CamelCase, functions snake_case.
- **Logging**: `tracing` with env-filter; set `RUST_LOG=arcc=debug` for verbose output.
- **Tests**: unit tests in `#[cfg(test)] mod`; integration tests in `tests/` directories. Run with `cargo test -p arcc-core`.
- **Git**: conventional commits (`feat:`, `fix:`, `refactor:`); feature branches; PRs get squash-merged.

## Design Decisions
- **Crate split**: `arcc-core` is the pure domain; `arcc-storage` handles I/O; UI and entry crates keep dependencies minimal.
- **DeepSeek model selection**: cost/performance sweet spot; model ID configurable via `config.toml` (`model.id`).
- **Safety**: tool allowlist (`crates/arcc-core/src/safety.rs`) blocks dangerous commands like `rm -rf /`; risk rating attached to each execution.
- **Session compression**: token-count-based truncation and summarisation (using `tiktoken-rs`) to fit 800k token window; configurable via `model.context_max_tokens`.
- **Hot reload**: `config.toml` is watched by `notify`; changes take effect without restart.
- **ARCC.md generation**: `/init` command sends codebase to the model and produces a custom `ARCC.md` with project-specific instructions (see template in `src/main.rs`).
- **Server daemon**: uses `--daemon` flag to detach; SSE endpoint `/chat` streams assistant responses; Feishu cards sent via webhook.

## Common Pitfalls
- **Missing API key**: panics at startup. Set env `ARCC_API_KEY` or `~/.arcc/config.toml`.
- **SQLite concurrency**: only one writer allowed; avoid running multiple server/TUI instances against the same database path.
- **TUI resizing**: ensure terminal supports true color and resize events; erratic rendering if unsupported.
- **PTY on Windows**: `arcc cli` may fail on non-Unix due to `portable-pty` limitations; prefer WSL.
- **Context overflow**: generating `ARCC.md` for large projects may exceed model context; raise `context_max_tokens` or pre-exclude heavy files.

## Notice
- `/init` in TUI creates/overwrites `ARCC.md` in the current working directory.
- Server health check: `GET /health`.
- CLI accepts `--file <path>` for context file and `--model pro|flash` to select variant.
- All model interactions are logged to `~/.arcc/audit.log` as JSON Lines.