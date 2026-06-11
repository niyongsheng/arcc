## Project Overview
ARCC (AI Rust Claude CLI) is a three-in-one personal AI assistant that operates as a TUI (interactive terminal UI like Claude Code), a CLI (one-shot prompt/pipe), and an HTTP server (axum with Feishu SSE). Built in Rust (edition 2024) with a workspace of five crates, it uses DeepSeek-V4 models (Pro/Flash) via reqwest, tokio for async, ratatui+crossterm for the TUI, rusqlite for session storage, and clap for argument parsing. Target audience: developers and power users who need a unified AI assistant embeddable in shell workflows, interactive coding environments, or chat services.

## Quick Start
```bash
cargo build --release
./target/release/arcc tui                # launch interactive terminal UI
./target/release/arcc cli "Hello"        # one-shot prompt
./target/release/arcc server --daemon    # start HTTP + SSE server
```
Copy `config/config.toml` to `~/.arcc/config.toml`, add your DeepSeek API key, and adjust settings.
Run tests: `cargo test --workspace`. Test a single crate: `cargo test -p arcc-core`.

## Repo Anatomy
- `src/main.rs` — binary entry point, dispatches subcommands (tui/cli/server) to the appropriate crate.
- `crates/arcc-core` — AI model providers, safety engine, session context compression, tool execution (MCP/shell).
- `crates/arcc-storage` — persistence: SQLite schema for sessions/messages, TOML configuration loader, JSON Lines audit log.
- `crates/arcc-server` — axum HTTP server with SSE endpoints for Feishu IM integration.
- `crates/arcc-tui` — terminal UI using ratatui+crossterm; handles keybindings, /commands, and streaming output.
- `crates/arcc-cli` — one-shot CLI mode; leverages portable-pty for interactive commands/ sudo password handling.
- `config/config.toml` — example configuration; runtime config read from `~/.arcc/config.toml`.
- `docs/` — design notes: DSML handling, interactive command and sudo password handling.

## Intelligent Conventions
- Error handling: libraries use `thiserror` for typed errors; application code uses `anyhow` for contextual fallible operations.
- Async: all IO is Tokio-based; `#[tokio::main]` throughout; `async-trait` for trait objects with async methods.
- Logging: `tracing` with `tracing-subscriber` (env-filter); JSON tracing for server mode.
- Git history follows conventional commits: `feat:`, `fix:`, `docs:`, `chore:`, `perf:`, `refactor:`.
- Testing: unit tests in `src/` alongside code (`#[cfg(test)]`), integration tests in `crates/arcc-core/tests/`. No mandatory CI test coverage gates currently.
- Config: use `toml` crate; validation enforced at start-up; missing `api_key` causes immediate exit.

## Design Decisions
- **Crate separation**: `arcc-core` is pure domain logic with no IO dependencies beyond model API; `arcc-storage` isolates all file/DB concerns. This allows swapping storage (e.g., from SQLite to Postgres) or model providers (only DeepSeek currently) without touching the UI layers.
- **TUI library**: `ratatui` + `crossterm` chosen for cross-platform terminal support and a rich set of widgets, despite larger binary size vs. a simpler curses wrapper.
- **Context window**: `context_max_tokens` defaults to 800k (80% of 1M) to fully leverage DeepSeek-V4’s large context; the session manager triggers automatic compression after each response if the token count exceeds the limit.
- **Portable-pty**: used in `arcc-cli` to handle interactive subprocesses (e.g., `sudo` prompts) transparently; otherwise stdin/stdout would block.
- **Feishu SSE integration**: the server exposes an SSE endpoint that forward AI responses to Feishu bots; requires a separate configuration key `[feishu]`.
- **ARCC.md generation**: the `/init` command in TUI mode invokes the AI to analyze the project and generate a project-level `ARCC.md`; content is streamed into the chat. This file is intentionally designed for AI comprehension (the very document you are reading follows that template).
- **Safety engine**: operates on an allowlist + risk-rating model; tool execution (shell commands, MCP plugins) is gated by this engine before running.

## Common Pitfalls
- **Missing API key**: Running any mode without a valid `model.api_key` in `~/.arcc/config.toml` will print an error and exit. Ensure the config file exists and is correctly formatted.
- **Rust edition 2024**: Requires Rust 1.85+ (stable). Building with an older compiler will fail with an edition error.
- **TUI terminal size**: The TUI requires a minimum terminal size (80x24); smaller terminals may panic or render incorrectly. `crossterm` normally handles resize but some old terminals may break.
- **SQLite bundled**: The `rusqlite` feature `bundled` compiles SQLite from source. This avoids system library mismatches but increases first build time.
- **Config hot-reload**: The `notify` crate watches `~/.arcc/config.toml` for changes; modifications while the server is running will **not** take effect until restart (notify is used only for the audit log tailing in TUI diagnostics).
- **Interactive sudo**: `arcc cli` using `portable-pty` expects a TTY; piping input can cause the PTY to hang if a password is requested. Prefer `sudo -S` or avoid commands requiring interactive authentication in CLI pipe mode.

## Notice
- Primary binary subcommands: `tui`, `cli <PROMPT>`, `server [--daemon]`.
- Server endpoints (when running): `POST /v1/chat/completions` (OpenAI‑compatible), `GET /v1/feishu/sse` (Feishu event stream), `GET /health`.
- TUI keybindings: `/init` to generate/update `ARCC.md`; `Ctrl+C` to abort; `Ctrl+D` to exit; `/model <pro|flash>` to switch.
- Configuration file location: `~/.arcc/config.toml`. Mandatory keys: `[model].api_key`; optional: `[model].model_name` (default `deepseek-chat`), `[server].port` (default 8080), `[feishu]` block for bot integration.
- Context management: `context_max_tokens` in config controls when compression triggers; aggressive summaries may lose detail—raise the limit if responses feel truncated.
- ARCC.md template: the project’s own AI comprehension file follows the layout defined in `/init`.