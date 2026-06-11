## Project Overview
ARCC is a multi-modal AI assistant written in Rust, offering a TUI (ratatui + crossterm), a pipe‑friendly CLI, and a Feishu‑SSI‑compatible HTTP server. It targets developers who want a local, context‑aware coding companion powered by DeepSeek models (V4 Pro / Flash). Core features include session persistence (SQLite), automatic context compression, a shell tool executor with PTY support, and a safety engine with allowlist/risk rating.

## Quick Start
```bash
# ensure Rust ≥ 1.85 (stable)
git clone https://github.com/niyongsheng/arcc
cd arcc

# build and test
cargo build --release
cargo test --workspace

# run
cargo run -- tui                          # TUI mode
cargo run -- cli "what is 2+2?"          # one-shot CLI
echo "explain this error" | cargo run -- cli  # pipe input
cargo run -- server --daemon             # daemon mode

# configuration
mkdir -p ~/.arcc
cp config/config.toml ~/.arcc/config.toml
# edit ~/.arcc/config.toml to set model.api_key
```

## Repo Anatomy
- **`src/main.rs`** – Binary entry point; uses `clap` to dispatch to `tui / cli / server` subcommands.
- **`crates/arcc-core`** – AI provider abstraction (DeepSeek), session manager, compression, tool executor (MCP/shell).
- **`crates/arcc-storage`** – Persistence: SQLite for sessions, TOML config loader, JSON‑Lines audit log.
- **`crates/arcc-server`** – Axum HTTP server with SSE streaming (Feishu IM integration).
- **`crates/arcc-tui`** – Terminal UI: ratatui + crossterm, interactive slash commands, tree rendering.
- **`crates/arcc-cli`** – Non‑interactive command: takes prompt from args or stdin, prints response.
- **`config/config.toml`** – Reference configuration with all available options.
- **`docs/`** – Design documentation (DSML handling, PTY sudo password, etc.).

## Intelligent Conventions
- **Error handling:** `thiserror` for library‑level error types, `anyhow` for application‑level error propagation.
- **Async:** `tokio` runtime (multi‑thread, full features); `async_trait` used for trait objects, `tokio_stream` for streaming responses.
- **Logging:** `tracing` + `tracing-subscriber`; env filter (`RUST_LOG`) controls verbosity; JSON log output supported.
- **Testing:**
  - Unit tests use `#[cfg(test)] mod tests` inside `src/`.
  - Integration tests live in `crates/arcc-core/tests/` (and possibly other crates’ `tests/` dirs).
  - Run subsets with `cargo test -p <crate>`.
- **Git / PR conventions:** Conventional commits (`feat:`, `fix:`, `docs:`, `perf:`, `refactor:`); feature branches, squash merge.

## Design Decisions
- **Separation of concerns:** Five crates (`core`, `storage`, `server`, `tui`, `cli`) keep responsibilities isolated.
- **SQLite for sessions** (via `rusqlite` with `bundled` feature) instead of flat files: transactional safety, queries for session history.
- **PTY for shell tool** (`portable-pty`): Enables interactive commands (e.g., sudo) by allocating a real pseudo‑terminal.
- **Context compression:** Automatically triggered after every AI response; uses `tiktoken-rs` for accurate token counting; configurable `context_max_tokens` (default 800k, 80% of DeepSeek’s 1M context window).
- **Configuration:** Single TOML file at `~/.arcc/config.toml` (not overridable by env vars directly, but API key can be set via `DEEPSEEK_API_KEY` env var). The server mode can be run as daemon with `--daemon`.
- **TUI tree rendering:** JSON/TOML blocks are rendered as collapsible trees, with raw view toggle; mermaid diagrams are marked with alignment warnings in system prompts.

## Common Pitfalls
- **Rust edition 2024:** Requires Rust **≥ 1.85**; builds will fail on older compilers.
- **Config not found:** The binary expects `~/.arcc/config.toml`; missing file causes startup error. Use `config/config.toml` as template.
- **API key:** If neither the config file nor the `DEEPSEEK_API_KEY` env var is set, API requests will fail with auth errors.
- **PTY on Windows:** `portable-pty` works with ConPTY (Windows 10 1809+); older Windows or non‑terminal hosts may cause tool execution failures.
- **Context limits:** Flaky behaviour (truncated replies) may arise if `context_max_tokens` is set too low; ensure it matches the model’s actual context window.
- **Workspace builds:** Always run `cargo build/test` from the workspace root (`/arcc`); building individual crates may miss dependencies if not via `-p` flag.

## Notice
- **CLI subcommands:** `tui`, `cli <prompt>`, `server` (with `--daemon` to fork).
- **TUI slash commands:** `/init` generates a project‑level `ARCC.md`; `/help` lists all commands; `/exit` quits.
- **Server API:** The server exposes POST `/chat` (and `/chat/stream` for SSE) with request format `{ "messages": [...] }`; integrates with Feishu bot via SSE endpoint.
- **Configuration keys:** `model.api_key`, `model.context_max_tokens`, `model.default_model` (Pro/Flash), `storage.db_path`, `safety.allowlist`, etc. See `config/config.toml` for all.