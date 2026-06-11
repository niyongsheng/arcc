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
| **TUI** | `arcc tui` | 交互式终端（多轮对话 + 工具调用） |
| **CLI** | `arcc cli "<prompt>"` | 单轮执行（脚本/管道集成） |
| **Server** | `arcc server --daemon` | HTTP 后台服务（API + 飞书集成） |

## Quick Start

需要你只需一个 DeepSeek API Key：

```bash
# 安装
curl -fsSL https://raw.githubusercontent.com/niyongsheng/arcc/main/scripts/install.sh | bash

# 配置 API Key
echo '[model]
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"' > ~/.arcc/config.toml
```

---

## TUI 模式 — 交互式终端

启动全屏交互界面，适合日常使用：

```bash
arcc tui
```

### 界面布局

```
┌─ ARCC · a1b2c3d4 · tui  ✶─────────────────┐  ← 标题栏（含动画）
│                                              │
│  🧑 帮我看一下当前机器的资源占用              │  ← 聊天区（markdown渲染）
│  🤖 **系统资源概览**                          │
│                                              │
│     总内存   16 GiB    使用  43%             │
│     CPU     8 核      负载  0.8              │
│                                              │
│  ⚡ top -l 1 -n 5 -stats cpu,mem             │  ← 命令执行记录
│  exit=0                                      │
│                                              │
│  ◇ thinking  🧠 on  ──────────────────────── │  ← 状态栏（含动画）
│  ──────────────────────────────────────────── │  ← 分隔线
│  > /plan 帮我部署这个项目                     │  ← 输入行（Tab补全）
│  ──────────────────────────────────────────── │  ← 底部线
```

### 核心操作

| 操作 | 效果 |
|------|------|
| **输入文字 + Enter** | 向 AI 发送消息 |
| **↑/↓** | 浏览输入历史 |
| **PgUp/PgDn** | 滚动聊天内容 |
| **鼠标滚轮** | 滚动聊天内容（需 Shift+拖拽选择文本） |
| **Tab** | 命令补全 / 切换代码块焦点 |
| **Ctrl+C** | 退出 |

### 斜杠命令

| 命令 | 用途 |
|------|------|
| `/plan <任务>` | 多步骤任务规划分解 |
| `/exec <命令>` | 直接执行 shell 命令 |
| `/dashboard` 或 `/data` | 打开数据面板（会话/Token/审计） |
| `/skills` | 列出已注册 MCP 工具 |
| `/model` | 查看当前使用的 Pro/Flash 模型 |
| `/thinking` | 切换 DeepSeek 思维链显示 |
| `/clear` | 清空当前会话历史 |
| `/help` | 查看全部命令帮助 |
| `/stats` | 会话统计 |
| `/init` | 在项目根目录生成 ARCC.md |
| `/exit` | 退出 |

### 特色功能

- **双模型调度**：复杂任务自动用 Pro 模型，常规对话用 Flash 模型
- **上下文压缩**：达到 Token 阈值（默认 800k）时自动摘要压缩，节省上下文窗口
- **记忆系统**：AI 自动提取对话中的关键事实（偏好、项目信息），后续对话记得你
- **MCP 插件**：支持 MCP 协议的外部工具注册
- **思维链**：DeepSeek 的推理过程以灰色动画显示
- **命令安全**：`rm`/`dd`/`mkfs` 等危险命令需人工确认（y/a/n）

### 配置记忆系统

TUI 模式自动启用记忆功能。告诉 AI 你的信息：

```
> 我叫张三，我是 Rust 后端开发者
> 我的项目在 ~/work/my-project
```

后续 AI 会自动记住并引用这些信息。

---

## CLI 模式 — 单轮执行

适合脚本、管道、CI/CD 集成：

```bash
# 基本用法
arcc cli "用 python 写一个斐波那契函数"

# 原始命令（！前缀，绕过 LLM）
arcc cli "!ls -la"

# 管道输入
cat error.log | arcc cli "分析这些日志的错误原因"

# JSON 输出（给程序消费）
arcc cli --json "检查网络状态"
```

### 工作流程

```
用户输入 → LLM (DeepSeek-V4-Flash)
  → 输出文字（流式打印）
  → 或调用 execute_command 工具
    → 执行 shell 命令（30s 超时，4KB 截断）
    → 结果返回 LLM
  → LLM 综合输出最终回复
```

### 输出模式

| 模式 | 命令 | 用途 |
|------|------|------|
| **流式** | `arcc cli "..."` | 实时显示回复 token（默认） |
| **JSON** | `arcc cli --json "..."` | 一次性输出完整 JSON，适合程序解析 |

JSON 模式输出格式：

```json
{
  "response": "您的网络状态：IP 192.168.1.x，连通性正常",
  "tool_calls": [
    {
      "command": "curl -s -o /dev/null -w '%{http_code}' https://baidu.com",
      "status": "ok",
      "stdout": "200",
      "stderr": "",
      "exit_code": 0,
      "error": null
    }
  ],
  "status": "ok"
}
```

### 安全选项

```bash
# 绕过安全限制（允许所有命令执行）
arcc cli --unsafe "检查系统状态"

# 或完整参数名
arcc cli --dangerously-skip-permissions "检查系统状态"
```

### 典型场景

```bash
# DevOps 操作
arcc cli "检查所有 Docker 容器状态"
arcc cli "找出占用 80 端口的进程"

# 数据处理
cat access.log | arcc cli "统计返回 500 的请求占比"

# 代码生成
arcc cli "写一个 Rust 的 LRU Cache 实现" > lru.rs

# Claude Code 集成
# 在 CLAUDE.md 中添加：
# 你可以使用 arcc cli --json --unsafe 来执行 shell 操作
```

---

## Server 模式 — HTTP 后台服务

启动 API 服务，支持 HTTP 调用和飞书机器人集成：

```bash
# 前台运行
arcc server

# 后台守护进程
arcc server --daemon
```

### 配置

在 `~/.arcc/config.toml` 中：

```toml
[server]
host = "127.0.0.1"
port = 9527

[feishu]
enabled = false
app_id = "cli_xxxxxxxxxxxx"
app_secret = "xxxxxxxxxxxxxxxxxxxxxxxxxxxx"
verification_token = "xxxxxxxxxxxx"
```

### API 端点

#### `POST /chat` — AI 对话

```bash
curl -X POST localhost:9527/chat \
  -H "Content-Type: application/json" \
  -d '{"session_id":"user-123","prompt":"用 Python 写一个快速排序"}'
```

返回 SSE 流：
```
data: 当然，下面是快速排序的实现
data: ：

event: reasoning
data: 用户需要快速排序实现...

event: finish
data: [DONE]
```

**参数**：

| 字段 | 类型 | 说明 |
|------|------|------|
| `session_id` | string | 用户标识（同一用户连续对话用相同 ID） |
| `prompt` | string | 用户输入 |
| `stream` | bool | 是否流式（默认 false） |

#### `GET /health` — 健康检查

```bash
curl localhost:9527/health
# {"status":"ok","version":"0.1.0"}
```

#### **记忆管理** — `/memory/{user_id}[/{key}]`

```bash
# 查看用户的所有记忆
curl localhost:9527/memory/user-123

# 手动添加记忆
curl -X POST localhost:9527/memory/user-123 \
  -H "Content-Type: application/json" \
  -d '{"key":"preferred-language","value":"Rust"}'

# 更新记忆
curl -X PUT localhost:9527/memory/user-123/preferred-language \
  -H "Content-Type: application/json" \
  -d '{"value":"Go"}'

# 删除记忆
curl -X DELETE localhost:9527/memory/user-123/preferred-language
```

#### **飞书 Webhook** (需配置 `[feishu]`)

| 端点 | 说明 |
|------|------|
| `POST /feishu/webhook` | 接收飞书事件回调（消息 + 卡片交互） |
| `POST /feishu/send` | 主动发送消息到飞书 |

```bash
# 主动发送测试消息（需要 open_id）
curl -X POST localhost:9527/feishu/send \
  -H "Content-Type: application/json" \
  -d '{"open_id":"ou_xxxxxxxx","text":"来自 ARCC 的消息"}'
```

### 自动记忆

Server 模式自动启用记忆系统。每次对话后，后台提取关键事实并存储：

```bash
# 第一次对话
curl ... -d '{"session_id":"user-123","prompt":"我是张三，用 Rust 写后端"}'

# 后续对话会自动记住
curl ... -d '{"session_id":"user-123","prompt":"你还记得我吗？"}'
# 回复会引用之前的信息
```

### Prometheus 指标

```bash
curl localhost:9527/metrics
```

---

## 三种模式对比

| 特性 | TUI | CLI | Server |
|------|:---:|:---:|:------:|
| 多轮对话 | ✅ | ❌ | ✅* |
| 工具调用 | ✅ | ✅ | ❌ |
| 记忆系统 | ✅ | ❌ | ✅ |
| 流式输出 | ✅ | ✅ | ✅ |
| 飞书集成 | ❌ | ❌ | ✅ |
| 会话持久化 | ✅ | ❌ | ✅ |
| 脚本/管道 | ❌ | ✅ | ❌ |
| 聊天滚动 | ✅ | - | - |
| 命令安全确认 | ✅ | `--unsafe` | - |

\* Server 通过 `session_id` 维持上下文，但同一 session 的每次请求会累积历史。

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
