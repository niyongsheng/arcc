## Project Overview
ARCC is a three-in-one AI assistant for the terminal: a **ClaudeCode‑like TUI**, a **pipe‑friendly CLI**, and a **server** (A2A/IM bridge). It targets developers who want an interactive coding agent or a lightweight, local‑first AI interface. The tech stack is Rust 2024, with `ratatui`/`crossterm` for the TUI, `axum` for the HTTP server, `rusqlite` for session storage, and the DeepSeek API (V4 Pro & Flash) as the model backend.

## Quick Start
```bash
# Install
curl -fsSL https://raw.githubusercontent.com/niyongsheng/arcc/main/scripts/install.sh | bash

# Set API key (edit ~/.arcc/config.toml or copy config/config.toml)
mkdir -p ~/.arcc
echo '[model]' > ~/.arcc/config.toml
echo 'api_key = "sk-..."' >> ~/.arcc/config.toml

# Build from source (if you cloned the repo)
cargo build --release

# Run
cargo run -- tui          # TUI agent
cargo run -- cli "Hi"     # one‑shot chat
cargo run -- server --daemon   # HTTP server
```

## Repo Anatomy
- `crates/arcc-core` — Domain logic: model provider, safety engine, session management, tool executor.
- `crates/arcc-storage` — Persistence layer: SQLite schema, config loader, audit logging.
- `crates/arcc-server` — `axum` HTTP server, exposes chat endpoints and Feishu SSE integration.
- `crates/arcc-tui` / `crates/arcc-cli` — User‑facing interfaces; `arcc-tui` holds the full TUI app, `arcc-cli` a lightweight one‑shot runner.
- `src/main.rs` — Entry point binary `arcc` that dispatches to the correct mode via `clap`.
- `config/` — Reference configuration; `docs/` — detailed design notes (e.g., DSML handling, interactive password handling).

## Intelligent Conventions
- **Async runtime**: `tokio` (full features) – all I/O runs on a multi‑threaded scheduler.
- **Error handling**: crate‑specific errors use `thiserror`; binaries and higher‑level glue use `anyhow` for ergonomic context.
- **Naming**: internal crates use the `arcc-*` prefix; module structure mirrors crate boundaries.
- **Testing**: tests live in `<crate>/tests/` (e.g., `crates/arcc-core/tests`). Run a subset with `cargo test -p arcc-core`.
- **Observability**: structured logging via `tracing` with `tracing-subscriber` (env‑filter for RUST_LOG, JSON output). Prometheus metrics are exported by the server using `metrics-exporter-prometheus`.
- **Git conventions**: Commits follow `<type>: <description>`; `feat:`, `fix:`, `chore:`, `docs:`, `perf:`, `refactor:`. No formal PR template.

## Design Decisions
- **Dual‑model dispatch**: The system prompt instructs the AI to classify user intent, then route to DeepSeek‑V4‑Flash for fast dialogue or Pro for complex reasoning. This is implemented inside `arcc-core`’s model provider.
- **Context compression**: After every AI response, the session manager trims message history to stay under `context_max_tokens` (currently 800k). This avoids unbounded token growth and is triggered automatically (see `core` session logic).
- **ARCC.md generation (`/init` command)**: The assistant analyzes the working directory and writes a project‑specific `ARCC.md`. The content is streamed in‑stream to the TUI to show progress.
- **SQLite with bundled**: The `rusqlite` feature `bundled` compiles SQLite statically, eliminating external library dependencies.
- **Server mode** is meant to run as a daemon (systemd/launchd) and integrates with Feishu IM via SSE; the `arcc server --daemon` flag writes a PID file and logs to `~/.arcc/server.log`.
- **PTY handling**: The CLI mode uses `portable-pty` for true terminal emulation when running interactive commands.
- **Safety engine**: A built‑in allowlist and risk‑rating filter all tool calls; this is configured via `config.toml`.

## Common Pitfalls
- **Config path**: The default config file is `~/.arcc/config.toml`. The example in `config/config.toml` must be copied and edited – forgetting to set `api_key` causes a runtime panic.
- **High token limits**: `context_max_tokens` defaulted to 300k then bumped to 800k; ensure your DeepSeek account supports 1M tokens (the allocator reserves 20% headroom).
- **TUI I/O during waiting states**: The TUI now accepts character input (e.g., y/n prompts) while the AI is generating, but the rendering layer requires that newline‑heavy content (reasoning traces) be collapsed to prevent display glitches.
- **Flaky environment detection**: In CI, some tests that spawn subprocesses may fail on macOS due to SIP restrictions; run `cargo test -- --test-threads=1` in those cases.
- **ARCC.md overwrite**: `/init` will ask for confirmation before replacing an existing `ARCC.md`. The prompt only appears during an active TUI session with a waiting state.

## Notice
- **CLI interface**: `arcc cli "<prompt>"` writes the final answer to stdout (pipeable) and logs to stderr. It reads stdin if no prompt is given.
- **Server endpoints**: By default `arcc server` listens on `0.0.0.0:8080`. Key endpoints are `/api/chat` (JSON POST) and `/api/chat/stream` (SSE). A health check is at `/health`.
- **TUI keyboard shortcuts**: `Ctrl+N` new session, `Ctrl+C` exit (confirm required), `/init` triggers project analysis, `/compress` manually compresses context.
- **ARCC.md schema**: The assistant will ask you to modify `ARCC.md` – the document structure is the same as this file, with sections for Overview, Quick Start, Repo Anatomy, etc.