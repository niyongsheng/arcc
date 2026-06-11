# ARCC.md

## Project Overview
ARCC (AI Rust Claude CLI) is a three-in-one personal AI assistant built in Rust.  
It provides:
- **TUI** (ratatui + crossterm) – interactive terminal interface
- **CLI** – one-shot / pipe-friendly invocations
- **Server** (axum) – daemon mode with Feishu SSE integration

The backend uses **DeepSeek V4** (Pro/Flash) via a dedicated `arcc-core` crate that handles model interaction, safety checks, session management, context compression, and tool execution (shell commands, MCP plugins). Data persistence is handled by `arcc-storage` (SQLite for sessions/messages, TOML config, JSON Lines audit log).  
The workspace also includes dedicated crates for each interface (`arcc-tui`, `arcc-cli`, `arcc-server`) and a root binary that dispatches to the selected mode.

## Conventions
- **Rust 2024 edition**, workspace resolver 2.
- **Commit messages** follow conventional commits: `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`.
- Code formatting with `cargo fmt`, linting with `cargo clippy`.
- Configuration lives in `~/.arcc/config.toml` (defaults in `/config/config.toml`).
- Tests are run workspace-wide with `cargo test`.
- Use `anyhow` for application-level errors, `thiserror` for library crates.

## Common Tasks
```bash
# Build (debug)
cargo build

# Build release
cargo build --release

# Run all tests
cargo test --workspace

# Format check
cargo fmt --check

# Lint (deny warnings)
cargo clippy -- -D warnings

# Run TUI
cargo run -- tui

# Run CLI one-shot
cargo run -- cli "your prompt"

# Run server in daemon mode
cargo run -- server --daemon
```

## Notes
- The `/init` slash-command inside the TUI invokes AI to generate a project-level `ARCC.md` – the very file you are reading.
- **Context compression** automatically runs after every AI response; maximum token budget is 300k.
- **Interactive tree blocks** are rendered for JSON/TOML code blocks – they support expand/collapse and a raw toggle.
- Markdown rendering supports strikethrough, syntax highlighting, Mermaid diagrams, tree views, and images.
- `Esc` key aborts an in-progress AI streaming response.
- Stray input from child processes during tool execution is discarded to keep the UI clean.
- Key-repeat events (`KeyEventKind::Repeat`) are explicitly handled for backspace and character input.
- Pasted content has CR (`\r`) stripped automatically.
- For DeepSeek’s custom markup (`DSML`), see `docs/dsml-handling.md`.
- Handling of interactive commands and sudo password prompts is documented in `docs/interactive-command-and-sudo-password-handling.md`.
- The health check script is at `scripts/health_check.sh`, and installers are provided in `scripts/`.
- CI workflows (format, clippy, test, release) are in `.github/workflows/`.