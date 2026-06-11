# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

ARCC (AI Rust Claude CLI) — 基于 Rust 2024 的终端常驻通用 AI Agent，以 DeepSeek-V4 为核心推理底座，全量兼容 MCP（Model Context Protocol）协议。目标：7×24 小时后台守护、不限编程语言场景的高性能自动化执行环境。

## Git 约定

- **Commit messages**: Must be written in **English**. Use imperative mood, concise and descriptive (e.g. `Fix model fallback on rate limit`, `Add session export endpoint`).
- **Branch naming**: `feature/`, `fix/`, `chore/` prefix followed by short kebab-case description.

## 主体技术选型

- **语言：** Rust 2024 edition
- **推理底座：** DeepSeek-V4（Pro 用于复杂推理 / Flash 用于高频对话与上下文压缩）
- **异步 Runtime：** tokio（多线程多任务调度）
- **HTTP 框架：** axum
- **TUI 框架：** ratatui 0.29 + crossterm 0.28
- **TUI Spinner：** tui-spinner 0.2（`FluxFrames` 预设帧集，覆盖 20+ 动画）
- **Markdown 渲染：** ratatui-markdown 0.3.6
- **命令行解析：** clap（Derive 架构）
- **HTTP 客户端：** reqwest
- **序列化：** serde + serde_json（零拷贝反序列化）
- **可观测性：** tracing + tracing-appender + metrics-exporter-prometheus
- **持久化：** rusqlite（bundled feature，零系统依赖）+ TOML 配置 + JSON Lines 审计
- **Token 计数：** tiktoken-rs
- **错误处理：** thiserror（库 crate）+ anyhow（binary 层）

## 三大运行模式（Cargo Workspace 多 crate）

| 模式 | 入口 | 关键依赖 |
|------|------|----------|
| **TUI** 交互式终端 | `arcc tui` | ratatui + crossterm + tui-spinner |
| **CLI** 批处理 / 管道嵌入 | `arcc cli "<prompt>"` | portable-pty |
| **Server** 后台守护 + 飞书 | `arcc server --daemon` | axum + tokio::signal |

```bash
cargo build                    # 编译全部
cargo build -p arcc-tui        # 仅编译 TUI
cargo build -p arcc-core       # 仅编译核心
cargo run -- tui               # 启动 TUI
cargo run -- cli "<prompt>"    # CLI 模式
```

## 项目目录结构

```
arcc/
├── Cargo.toml              # workspace root
├── crates/
│   ├── arcc-core/          # 核心抽象：模型 trait、工具执行、安全引擎
│   │   ├── src/
│   │   │   ├── model/      # ModelProvider trait + DeepSeek + 提示词模板
│   │   │   │   ├── prompts/ # 编译期嵌入的 .md 系统提示词模板
│   │   │   │   │   ├── cli.md / tui.md / plan.md / server.md / compress.md
│   │   │   │   │   └── mod.rs  # SystemPrompt + templates 模块
│   │   │   │   └── ...
│   │   │   ├── safety/     # 命令白名单、参数校验、风险评级
│   │   │   ├── session/    # 对话管理 + 上下文压缩
│   │   │   ├── tools.rs    # 命令执行（管道 + 交互模式）
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   ├── arcc-server/        # axum HTTP + 飞书 webhook + SSE
│   │   ├── src/
│   │   │   ├── routes/     # /chat, /metrics, /health
│   │   │   ├── feishu/     # 飞书消息卡片 + 交互回调
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   ├── arcc-tui/           # ratatui 终端交互
│   │   ├── src/
│   │   │   ├── ui/         # 渲染组件（components.rs 含动画/布局/输入）
│   │   │   ├── event/      # MPSC 事件循环 + crossterm 输入处理器
│   │   │   ├── commands.rs # 斜杠命令注册表
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   ├── arcc-cli/           # 命令行入口 + portable-pty
│   │   ├── src/
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   └── arcc-storage/       # SQLite + 配置读写 + 审计日志
│       ├── src/
│       │   ├── db/         # rusqlite 封装 + 迁移 + 查询
│       │   │   ├── queries.rs  # `/data` 命令的数据查询（sessions/messages/token）
│       │   │   └── ...
│       │   ├── config/     # TOML 读写
│       │   ├── audit/      # JSON Lines 追加写入 + 读取
│       │   │   ├── reader.rs  # 从文件尾部逆向读取最近 N 条审计事件
│       │   └── lib.rs
│       └── Cargo.toml
├── config/                 # 默认 TOML 模板
│   ├── config.toml
│   └── allowlist.toml
├── docs/
│   ├── arcc_optimized_design.pdf
│   └── interactive-command-and-sudo-password-handling.md
└── src/
    └── main.rs             # 二进制入口：clap 路由到各 crate
```

## 核心架构要点

### 1. 双模型分级调度
复杂 MCP 多步编排 → DeepSeek-V4-Pro（提取 `reasoning_content`）；常规巡检/流式对话 → DeepSeek-V4-Flash。上下文触达阈值（~8k tokens）自动触发 Flash 摘要压缩。

### 2. 工具执行与交互命令
`execute_command` 工具支持 `interactive` 参数，三层决策：

1. **AI 显式指定** `"interactive": true/false`
2. **自动检测** — 逐词分析命令（忽略大小写），匹配 `sudo`/`ssh`/`vim`/`nano`/`htop`/`top`/`less`/`more`/`passwd`/`telnet` 时自动启用交互模式
3. **默认管道模式**（30s 超时，4096 bytes 截断）

交互命令执行流程（`AppEvent::InteractiveCommand`）：
```
abort(crossterm 输入处理器) → LeaveAlternateScreen → disable_raw_mode
→ spawn(Stdio::inherit()) → wait → enable_raw_mode
→ EnterAlternateScreen → clear → respawn(crossterm 输入处理器)
```

### 3. TUI 事件循环
- **MPSC 通道**：输入处理器 → AppEvent → 主循环
- **crossterm 输入处理器**：返回 `JoinHandle`，交互命令前 abort，执行完后 respawn（防止与子进程抢 stdin）
- **绘制频率**：~60fps（16ms/tick）
- **状态管理**：`App.status` — `idle` / `thinking` / `streaming` / `executing` / `planning` / `waiting...` / `error`

### 4. TUI Spinner 动画（tui-spinner）
使用 `FluxFrames` 预设帧集，状态与动画映射：

| 状态 | 动画集 | 帧 |
|------|--------|-----|
| `thinking` / `loading` / `planning` | `FluxFrames::CLASSIC` | `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` |
| `streaming` | `FluxFrames::BOUNCE` | `⠉⠒⣀⠒` |
| `executing` | `FluxFrames::DICE` | `⚀⚁⚂⚃⚄⚅` |
| `waiting...` | `FluxFrames::DIAMOND` | `◇◈◆◈` |
| 标题栏（AI 活跃时） | `FluxFrames::STAR` | `✶✷✸✹` |
| idle | 绿色 ● 常亮 | — |

### 5. 多轮对话 + 工具调用循环
`submit()` 方法启动一个 `tokio::spawn` 异步任务，内含两阶段循环：

- **Phase 1**：LLM 可调用工具（`execute_command`），AI 返回 tool_calls 后执行之
- **Phase 2**：无工具权限，仅文本生成，LLM 根据工具结果整理最终回复
- 中间消息通过 `ArccStorage`（SQLite）持久化

### 6. I/O 隔离
所有同步阻塞/密集 CPU 操作通过 `tokio::task::spawn_blocking` 或 `tokio::task::block_in_place` 投递，禁止在主 Runtime 线程中直接运行可能阻塞的操作。

### 7. 安全三防线
命令白名单拦截 → 高危操作 TUI 交互式确认（y/a/n） → serde 强类型二次校验防 LLM 注入

### 8. 系统提示词管理（arcc-core/model/prompts/）

所有系统提示词统一为编译期嵌入的 `.md` 模板文件，通过 `include_str!` 零开销加载。

```
prompts/
├── mod.rs         # SystemPrompt struct + templates 模块
├── cli.md         # CLI 模式 — 聚焦 tool calling + 单轮应答
├── tui.md         # TUI 模式 — 多轮对话 + 结果解释
├── plan.md        # 计划模式 — 支持 {TASK} 插值
├── server.md      # Server 模式 — 无 shell 权限，纯文本回答
└── compress.md    # 上下文压缩 — 保留决策/路径/待办，丢弃冗余
```

设计模式（参考 Claude CLI 提示词风格）：

```
## Core Identity              → 你是谁 + 响应风格
## Available Tools             → 工具列表 + when/when-not-to-use
## Response Rules / Constraints → 编号 DO + DON'T
## Safety / Guardrails         → 不可越界的事
## Output Format（可选）        → 输出模板/结构要求
```

API 使用：

```rust
// 构造系统 ChatMessage
let msg = arcc_core::model::prompts::templates::tui().to_chat_message();
let msg = arcc_core::model::prompts::templates::plan("my task").to_chat_message();

// 直接取文本
let text: &str = templates::cli().as_str();
```

所有模板在构建时验证非空、插值正确。

### 9. `/data` 数据查询命令

TUI 中内置的数据查看命令，直接从 SQLite 和审计 JSONL 读取：

| 子命令 | 功能 | 数据源 |
|--------|------|--------|
| `/data sessions [limit]` | 最近会话列表 | SQLite `sessions` |
| `/data messages <id> [limit]` | 会话消息记录 | SQLite `messages` |
| `/data token [days]` | Token 消耗汇总 | SQLite `token_usage` |
| `/data audit [count]` | 最近审计事件 | `audit.jsonl`（尾部反向读取） |
| `/data summary <id>` | 会话压缩摘要 | SQLite `summaries` |

存储层新增：

- `arcc-storage/src/db/queries.rs` — `list_sessions` / `session_messages` / `token_usage_daily` / `latest_summary` / `total_tokens`
- `arcc-storage/src/audit/reader.rs` — `read_recent()` 从大文件尾部逆向 seek，避免全量扫描

## 模型抽象层（arcc-core/model/）

所有模型调用通过 `ModelProvider` trait，不直接依赖 DeepSeek 实现。

```rust
pub struct ChatMessage {
    pub role: String,                          // system / user / assistant / tool
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,          // tool 消息必须回传此 ID
    pub reasoning_content: Option<String>,     // DeepSeek 思考链
}

pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Option<Vec<ToolDefinition>>,
    pub tool_choice: Option<serde_json::Value>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stream: bool,
    pub thinking_mode: Option<String>,          // "enabled" / "disabled"
    pub reasoning_effort: Option<String>,        // "high" / "max"
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse>;
    async fn chat_stream(&self, req: ChatRequest) -> Result<impl Stream<Item = Result<StreamChunk>>>;
    fn count_tokens(&self, text: &str) -> usize;
    fn model_name(&self) -> &str;
}

pub enum StreamChunk {
    Content(String),
    Reasoning(String),
    ToolCallStart(ToolCall),
    ToolCallEnd { id: String, output: String },
    Finish(Usage),
}
```

### 注意事项
- Assistant 消息有 `tool_calls` 时，`content` 必须为非空字符串（DeepSeek API 要求 `content` 字段始终存在，不可为 null 或缺省）
- 带 `tool_calls` 的 assistant 消息后必须紧接对应的 tool 角色消息（`tool_call_id` 匹配），否则 API 返回 400
- `attach_reasoning()` 用于阶段转换后将累积的 reasoning_content 挂回 session

## 错误处理约定

```rust
// 库 crate（arcc-core / arcc-storage）：thiserror 定义结构化错误枚举
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("model provider error: {0}")]
    Model(String),
    #[error("mcp tool execution failed: {0}")]
    Mcp(String),
    #[error("safety violation: {0}")]
    Safety(String),
    #[error(transparent)]
    Storage(#[from] arcc_storage::StorageError),
}

// Binary 层（main.rs / CLI / Server）：anyhow::Result 传播，tracing::error! 记录上下文
```

核心原则：
- **库 crate** → `thiserror` 枚举
- **二进制入口** → `anyhow::Result`
- **禁止** `unwrap()` / `expect()` 用于可能失败的操作（仅已知不变量可用）
- 网络超时、API 限流等必须有独立错误变体

## 持久化分层设计

全部嵌入式，无外部服务依赖。

### 配置层 — TOML
```
~/.arcc/
├── config.toml          # 主配置（API Key、模型参数等）
├── mcp_plugins.d/       # MCP 插件注册目录
└── allowlist.toml       # 命令白名单
```

### 会话层 — SQLite（rusqlite + bundled, WAL 模式）
表：`sessions` / `messages` / `summaries` / `token_usage`

### 审计层 — JSON Lines 追加写
`~/.arcc/logs/audit.jsonl` — 所有命令执行、MCP 调用、人工确认记录。

### Metrics — 内存 + /metrics scrape
`metrics-exporter-prometheus` 在内存维护计数器，Server 模式下 Prometheus 定期 scrape。

### 持久化总览

| 数据层 | 技术 | 依赖 |
|--------|------|:----:|
| 配置 | TOML | 无 |
| 会话 | SQLite（bundled） | 无 |
| 审计 | JSON Lines | 无 |
| Metrics | 内存 + scrape | 无 |

## TUI 界面布局（自顶向下）

```
[0] Session title     — ARCC · {UUID[..8]} · mode  + STAR spinner
[1] Chat area         — markdown 渲染，支持 scroll_offset
[2] Status bar        — spinner + status text + 🧠 on
[3] Divider           — ─── 分割线
[4] Input line        — > prompt（/命令高亮、Tab 补全、↑↓ 历史）
[5] Bottom divider    — ─── 分割线
```

输入特性：
- `/` 开头 → 斜杠命令（Tab 补全，命令名青色加粗/红色加粗）
- ↑/↓ → 输入框空时滚动聊天，有内容时浏览历史
- Ctrl+C → 退出

## TUI 斜杠命令一览

| 命令 | 分类 | 功能 |
|------|------|------|
| `/plan <task>` | tools | 多步骤任务规划 |
| `/clear` | view | 清空当前会话历史 |
| `/model` | view | 显示当前 Pro/Flash 模型 |
| `/skills` | tools | 列出已注册 MCP 工具 |
| `/exec <cmd>` | tools | 直接执行 shell 命令 |
| `/data <sub> [args]` | system | 查看持久化数据（sessions/messages/token/audit/summary） |
| `/stats` | system | 会话统计（消息数、历史记录数、会话 ID） |
| `/thinking` | system | 切换 DeepSeek 思维链模式 |
| `/exit` | system | 退出 ARCC |
| `/help [cmd]` | navigation | 命令帮助 |

## MCP 集成（Claude Code 调用 arcc）

arcc CLI 可以通过 MCP 协议注册为 Claude Code 的工具，让 Claude 直接通过自然语言执行 shell 命令。

### 注册方式

在 `~/.claude/settings.json` 中添加：

```json
{
  "mcpServers": {
    "arcc": {
      "type": "stdio",
      "command": "/path/to/arcc/bin/arcc-mcp",
      "args": []
    }
  }
}
```

`bin/arcc-mcp` 是一个 Python 脚本，实现 MCP stdio 协议，将请求转发给 `arcc cli --json`。

### 工具定义

- **名称**: `arcc`
- **参数**: `prompt` (string, 必填), `unsafe` (boolean, 可选)
- **输出**: JSON，包含 `response`(LLM回复)、`tool_calls`(执行记录)、`status`

详见 [docs/skills/arcc-cli.md](docs/skills/arcc-cli.md)

## 关键术语

- **MCP (Model Context Protocol)**：模型上下文协议，支持 stdio / SSE 通信
- **Skill**：即 MCP 插件，以子进程形式运行
- **FluxFrames**：tui-spinner 预设动画帧集（~20 种）
- **InteractiveCommand**：需要 TTY 的命令通过此事件让主循环代理执行
- **RawModeGuard** / **输入处理器生命周期**：交互命令时 abort/respawn 模式保障 stdin 独占
