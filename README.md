![logo](./logo.svg)

# ARCC

**ARCC (AI Rust Claude CLI)** — Three-in-One Personal AI Assistant.

[![Rust](https://img.shields.io/badge/Rust-2024-%23DEA584?logo=rust)](https://www.rust-lang.org)
[![DeepSeek](https://img.shields.io/badge/DeepSeek-V4-%234A90D9)](https://deepseek.com)

![arcc tui demo](arcc_tui_demo.gif)

---

## Running Modes

| Mode | Command | Use Case |
|------|---------|----------|
| **TUI** | `arcc tui` | [交互式终端教程](docs/tui-tutorial.md) |
| **CLI** | `arcc cli "<prompt>"` | [单轮执行教程](docs/cli-tutorial.md) |
| **Server** | `arcc server --daemon` | [HTTP 后台教程](docs/server-tutorial.md) |

## Quick Start

需要你只需一个 DeepSeek API Key：

```bash
# 安装
curl -fsSL https://raw.githubusercontent.com/niyongsheng/arcc/main/scripts/install.sh | bash

# 配置 API Key
echo '[model]
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"' > ~/.arcc/config.toml
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
