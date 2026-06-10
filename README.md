![logo](./logo.svg)

# ARCC

**ARCC (AI Rust Claude CLI)** — Three-in-One Personal AI Assistant.

[![Rust](https://img.shields.io/badge/Rust-2024-%23DEA584?logo=rust)](https://www.rust-lang.org)
[![DeepSeek](https://img.shields.io/badge/DeepSeek-V4-%234A90D9)](https://deepseek.com)

---

## Running Modes

| Mode | Command | Use Case |
|------|---------|----------|
| **TUI** | `arcc tui` | ClaudeCode-like tool |
| **CLI** | `arcc cli "<prompt>"` | A2A pipe-friendly |
| **Server** | `arcc server --daemon` | Auto CPIS with IM |

## Installation

### Download binary (recommended)

Download the latest release for your platform from [GitHub Releases](https://github.com/niyongsheng/arcc/releases):

```bash
# macOS Apple Silicon
curl -sL https://github.com/niyongsheng/arcc/releases/latest/download/arcc-aarch64-apple-darwin.tar.gz | tar xz
sudo mv arcc /usr/local/bin/

# macOS Intel
curl -sL https://github.com/niyongsheng/arcc/releases/latest/download/arcc-x86_64-apple-darwin.tar.gz | tar xz
sudo mv arcc /usr/local/bin/

# Linux
curl -sL https://github.com/niyongsheng/arcc/releases/latest/download/arcc-x86_64-unknown-linux-gnu.tar.gz | tar xz
sudo mv arcc /usr/local/bin/
```

### Build from source

Requires [Rust](https://rustup.rs/) 2024 edition.

```bash
git clone https://github.com/niyongsheng/arcc.git
cd arcc
cargo build --release
sudo mv target/release/arcc /usr/local/bin/
```

### Configuration

Create `~/.arcc/config.toml` with your DeepSeek API key:

```toml
[model]
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
```

See [config/config.toml](config/config.toml) for all available options.

## Quick Start

```bash
cargo run -- tui
```

Build individual crates as needed:

```bash
cargo build -p arcc-core     # Core engine only
cargo build -p arcc-tui      # TUI only
```

## Architecture

```mermaid
flowchart TB
    Entry(["arcc"]) --> TUI["arcc tui<br/>ratatui + crossterm"]
    Entry --> CLI["arcc cli<br/>one-shot / pipe"]
    Entry --> Server["arcc server<br/>axum + Feishu SSE"]

    TUI --> Core["arcc-core"]
    CLI --> Core
    Server --> Core

    subgraph Core["arcc-core"]
        Model["ModelProvider<br/>DeepSeek-V4 Pro / Flash"]
        Safety["Safety Engine<br/>Allowlist + Risk Rating"]
        Session["Session Manager<br/>Context Compression"]
        Tools["Tool Executor<br/>MCP / Shell"]
    end

    Model --> DeepSeekPro["DeepSeek-V4-Pro<br/>Complex Reasoning"]
    Model --> DeepSeekFlash["DeepSeek-V4-Flash<br/>High-Freq Dialogue"]

    Core --> Storage["arcc-storage"]
    
    subgraph Storage["arcc-storage"]
        SQLite["SQLite<br/>Sessions / Messages"]
        Config["TOML<br/>Configuration"]
        Audit["JSON Lines<br/>Audit Log"]
    end

    Tools --> MCP["MCP Plugins<br/>Model Context Protocol"]
```

## License

MIT
