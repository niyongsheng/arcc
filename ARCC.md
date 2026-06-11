# ARCC.md

## Project Overview
ARCC (AI Rust Claude CLI) is a three-in-one personal AI assistant using the DeepSeek API. It runs in three modes:
- **TUI** (`arcc tui`): terminal-based interactive assistant (like Claude Code)
- **CLI** (`arcc cli "prompt"`): one-shot or pipe-friendly CLI
- **Server** (`arcc server --daemon`): auto-response service with Feishu SSE integration

The codebase is a Rust workspace (edition 2024) containing:
- `arcc-core`: model providers, safety engine, session manager, context compression, tool execution (MCP/Shell)
- `arcc-storage`: SQLite for sessions/messages, TOML config, JSON Lines audit logs
- `arcc-server`: axum-based server with Feishu SSE handler
- `arcc-tui`: ratatui + crossterm TUI
- `arcc-cli`: one-shot requests, PTY handling for interactive commands
- Root binary `arcc` that dispatches to the above modes

Key technologies: Rust, tokio, axum, ratatui, crossterm, portable-pty, rusqlite, clap, tiktoken-rs, DeepSeek V4 models.

## Conventions
- **Code style**: standard Rust; format with `cargo fmt` and lint with `cargo clippy` across the workspace.
- **Commit messages**: conventional commits (`feat:`, `fix:`, `chore:`, `docs:`, `refactor:`), as seen in git history.
- **Testing**: unit tests inside `src/` (e.g., `#[cfg(test)]` modules), integration tests in `crates/arcc-core/tests/`. Run `cargo test --all` before pushing.
- **Documentation**: keep `ARCC.md`, `CLAUDE.md`, and `docs/*.md` up to date with architectural decisions.
- **Workspace dependencies**: declare all external deps once in root `Cargo.toml` `[workspace.dependencies]`, reference with `workspace = true`.

## Common Tasks
- **Build**: `cargo build`
- **Run TUI**: `cargo run -- tui` (needs `~/.arcc/config.toml` with DeepSeek API key)
- **Run CLI**: `cargo run -- cli "Your prompt"`
- **Run server**: `cargo run -- server --daemon`
- **Test everything**: `cargo test --all`
- **Lint & format**: `cargo clippy --all-targets --all-features` and `cargo fmt --all -- --check`
- **Release build**: see `.github/workflows/release.yml`
- **Health check**: `bash scripts/health_check.sh`

## Notes
- User configuration lives in `~/.arcc/config.toml` (template in `config/config.toml`) – API key, model selection, token limits, etc.
- Context compression runs automatically after every AI response (managed by `arcc-core` session manager); `context_max_tokens` is set to 300k.
- The TUI’s `/init` command uses AI to generate a project-level `ARCC.md`; ensure the generated file is reviewed and is not overwritten by accident.
- Database files (SQLite) and audit logs are stored under `~/.arcc/data/` by default.
- TUI key handling filters `KeyEventKind::Repeat` for all events; uses rotating line animation during tool calls and compression.
- PTY-based command execution (e.g., interactive sudo) is handled by `arcc-cli` with `portable-pty`.
- Server mode’s Feishu integration lives in `arcc-server`; it pushes events over SSE.
- When modifying system prompts, note that markdown support now includes strikethrough, syntax highlighting, mermaid, collapsible trees, and image rendering.