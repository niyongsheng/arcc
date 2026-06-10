![logo](./logo.svg)

# ARCC

**ARCC (AI Rust Claude CLI)** — A tri-mode AI agent powered by **DeepSeek-V4**, built with **Rust**.

[![Rust](https://img.shields.io/badge/Rust-2024-%23DEA584?logo=rust)](https://www.rust-lang.org)
[![DeepSeek](https://img.shields.io/badge/DeepSeek-V4-%234A90D9)](https://deepseek.com)

---

## Running Modes

| Mode | Command | Use Case |
|------|---------|----------|
| **TUI** | `arcc tui` | ClaudeCode-like tool |
| **CLI** | `arcc cli "<prompt>"` | A2A pipe-friendly |
| **Server** | `arcc server --daemon` | Auto CPIS with IM |

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

## Tech Stack

| Category | Technology |
|----------|-----------|
| **Language** | Rust 2024 edition |
| **Runtime** | tokio (async, multi-threaded) |
| **TUI** | ratatui 0.29 + crossterm 0.28 + tui-spinner 0.2 |
| **Markdown Rendering** | ratatui-markdown 0.3.6 |
| **HTTP (Server Mode)** | axum |
| **HTTP Client** | reqwest |
| **CLI Parsing** | clap (derive) |
| **Serialization** | serde + serde_json |
| **Observability** | tracing + tracing-appender + metrics-exporter-prometheus |
| **Storage** | rusqlite (bundled), TOML, JSON Lines |
| **Token Counting** | tiktoken-rs |
| **Error Handling** | thiserror (libraries) + anyhow (binaries) |

## License

MIT
