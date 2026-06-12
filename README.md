![logo](./logo.svg)

# ARCC

**ARCC (AI Rust Claude CLI)** — Three-in-One Personal AI Assistant.

[![Rust](https://img.shields.io/badge/Rust-2024-%23DEA584?logo=rust)](https://www.rust-lang.org)
[![DeepSeek](https://img.shields.io/badge/DeepSeek-V4-%234A90D9)](https://deepseek.com)

![arcc tui demo](docs/arcc_tui_demo.gif)

---

## Running Modes

| Mode | Command | Multi-Turn | Memory | Tool Call | Session Persist | Script/Pipe | IM Bot |
|------|---------|:----------:|:------:|:---------:|:---------------:|:-----------:|:------:|
| [**TUI**](docs/tutorial/tui-tutorial.md) | `arcc tui` | ✅ | — | ✅ | ✅ | — | — |
| [**CLI**](docs/tutorial/cli-tutorial.md) | `arcc cli "<prompt>"` | — | — | ✅ | — | ✅ | — |
| [**Server**](docs/tutorial/server-tutorial.md) | `arcc server --daemon` | ✅ | ✅ | ✅ | ✅ | — | ✅ |

## Quick Start

You only need one [DeepSeek API Key](https://platform.deepseek.com)：

```bash
# install
curl -fsSL https://raw.githubusercontent.com/niyongsheng/arcc/main/scripts/install.sh | bash

# API Key
echo '[model]
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"' > ~/.arcc/config.toml
```
See [config/config.toml](config/config.toml) for all available options.

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
        Tools["Tool Executor<br/>MCP / Skill"]
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

[MIT](./LICENSE)