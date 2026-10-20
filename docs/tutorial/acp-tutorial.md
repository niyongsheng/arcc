# ACP 模式教程 — GUI Agent 后端

`arcc --acp` 以 **ACP（Agent Client Protocol v1）** stdio server 的形式运行：
通过标准输入输出与 ACP 客户端（如 [AionUI](https://github.com/nicepkg/aion)）
通信，让 GUI 客户端直接获得 ARCC 的完整能力——流式对话、shell 工具执行、
逐命令权限确认、会话取消与模型切换。

```bash
# 前台运行（Ctrl+C 退出）
arcc --acp

# 跳过权限确认（危险：放行所有 shell 命令，等价于 CLI 的 --unsafe）
arcc --acp --unsafe
```

## 在 ACP 客户端中使用

在支持 ACP v1 的 GUI 中，把 agent 的命令注册为：

| 字段 | 值 |
|------|-----|
| 命令 | `arcc` |
| 参数 | `--acp` |
| 工作目录 | 任意（会话默认 cwd，可被 `session/new` 覆盖） |

连接后客户端会自动完成握手（`initialize`），然后逐会话发起对话。
ARCC 端到端能力一览：

- **流式输出** — `agent_message_chunk`（正文）+ `agent_thought_chunk`（推理链）
- **工具执行** — 模型可调用 `execute_command`，客户端实时看到
  `tool_call` → `tool_call_update` 生命周期
- **权限确认** — 高危命令（白名单 `require_human_confirm` 命中）会以
  `session/request_permission` 弹给客户端，用户点「Allow once / Reject」决定
- **模型切换** — 客户端可在 Flash / Pro 之间切换（`session/set_config_option`）
- **取消** — `session/cancel` 中断当前回合（含正在执行的子进程，`kill_on_drop` 收割）

## 协议概览

**传输**：JSON-RPC 2.0 over stdio，每行一个 JSON 文档（`\n` 分隔，UTF-8）。
只有 ACP 消息走 stdout，所有日志走 stderr，互不污染。

**流程**：`initialize` 握手 → `session/new` 建会话 → `session/prompt` 发起回合
→ 回合中持续收到 `session/update` 通知 → 回合结束返回 `{stopReason, usage}`。

### 支持的 RPC 方法

| 方法 | 方向 | 说明 |
|------|:----:|------|
| `initialize` | 客户端 → 服务端 | 握手，返回协议版本 1、能力与 agent 信息 |
| `session/new` | 客户端 → 服务端 | 建会话（`cwd` 可选），返回 sessionId + 模型列表 |
| `session/prompt` | 客户端 → 服务端 | 发起回合（prompt 为 text block 数组） |
| `session/update` | 服务端 → 客户端 | 通知：消息 chunk / 工具状态 / token 用量 |
| `session/request_permission` | 服务端 → 客户端 | 请求权限（携带 `allow_once` / `reject_once` 选项） |
| `session/cancel` | 客户端 → 服务端 | 取消当前回合（请求或通知皆可） |
| `session/set_mode` | 客户端 → 服务端 | 仅支持 `"default"` |
| `session/set_config_option` | 客户端 → 服务端 | 仅支持 `model` = `pro` / `flash` |
| `session/close` | 客户端 → 服务端 | 关闭会话（同时取消进行中的回合） |
| `session/load` / `session/resume` | — | 不支持（返回 `METHOD_NOT_FOUND`） |

### 错误码

| 代码 | 含义 |
|------|------|
| `-32700` | JSON 解析失败 |
| `-32600` / `-32601` / `-32602` | 非法请求 / 方法不存在 / 参数错误 |
| `-32001` | 会话不存在（`SESSION_NOT_FOUND`） |
| `-32004` | 会话已有进行中的 prompt（`SESSION_BUSY`，同一会话不允许并发回合） |

## 一次完整对话的 wire 流程

```text
→ {"jsonrpc":"2.0","id":1,"method":"initialize"}
← {"jsonrpc":"2.0","id":1,"result":{
     "protocolVersion":1,
     "agentCapabilities":{"loadSession":false,"promptCapabilities":{},
                          "sessionCapabilities":{"close":{}}},
     "agentInfo":{"name":"arcc","title":"ARCC","version":"0.9.0"},
     "authMethods":[]}}

→ {"jsonrpc":"2.0","id":2,"method":"session/new"}
← {"jsonrpc":"2.0","id":2,"result":{
     "sessionId":"<uuid>",
     "modes":["default"],
     "configOptions":[{"key":"model","type":"select",...}],
     "models":{"currentModelId":"flash",
               "availableModels":[
                 {"modelId":"flash","name":"DeepSeek-V4-Flash"},
                 {"modelId":"pro","name":"DeepSeek-V4-Pro"}]}}

→ {"jsonrpc":"2.0","id":3,"method":"session/prompt",
   "params":{"sessionId":"<uuid>",
             "prompt":[{"type":"text","text":"检查磁盘使用情况"}]}}

← {"jsonrpc":"2.0","method":"session/update","params":{
     "sessionId":"<uuid>",
     "update":{"sessionUpdate":"agent_thought_chunk",
               "content":{"type":"text","text":"用户想查磁盘…"}}}}
← {"jsonrpc":"2.0","method":"session/update","params":{
     "sessionId":"<uuid>",
     "update":{"sessionUpdate":"agent_message_chunk",
               "content":{"type":"text","text":"好的，正在检查…"}}}}
← {"jsonrpc":"2.0","method":"session/update","params":{
     "sessionId":"<uuid>",
     "update":{"sessionUpdate":"tool_call",
               "toolCallId":"<id>","title":"df -h",
               "rawInput":"df -h"}}}
← {"jsonrpc":"2.0","method":"session/update","params":{
     "sessionId":"<uuid>",
     "update":{"sessionUpdate":"tool_call_update",
               "toolCallId":"<id>","status":"in_progress"}}}
← {"jsonrpc":"2.0","method":"session/update","params":{
     "sessionId":"<uuid>",
     "update":{"sessionUpdate":"tool_call_update",
               "toolCallId":"<id>","status":"completed",
               "rawOutput":{"stdout":"...","stderr":"","exit_code":0}}}}
← {"jsonrpc":"2.0","method":"session/update","params":{
     "sessionId":"<uuid>",
     "update":{"sessionUpdate":"usage_update","used":1234,"size":800000}}}

← {"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn",
     "usage":{"inputTokens":900,"outputTokens":334}}}
```

`session/update` 通知统一挂在 `sessionUpdate` 键上（值为蛇形变体名，
字段平铺在同一对象中）——这是官方 ACP v1 schema 的 tagged-union 形态。

## 权限确认流程

命中 `~/.arcc/config.toml` 中 `[safety].require_human_confirm` 的命令
（默认 `rm` / `mv` / `dd` / `mkfs` / `shutdown` / `reboot` / `fdisk`），
服务端不会直接执行，而是向客户端发起请求：

```text
→ {"jsonrpc":"2.0","id":"<uuid>","method":"session/request_permission",
   "params":{"sessionId":"<uuid>",
             "toolCall":{"toolCallId":"<id>","title":"rm -rf /tmp/cache",
                         "kind":"shell","status":"pending",
                         "rawInput":"rm -rf /tmp/cache"},
             "options":[
               {"optionId":"allow_once","name":"Allow once","kind":"allow_once"},
               {"optionId":"reject_once","name":"Reject","kind":"reject_once"}]}}

← {"jsonrpc":"2.0","id":"<uuid>",
   "result":{"outcome":{"outcome":"selected","optionId":"allow_once"}}}
```

- 选 **Allow once** → 执行该命令
- 选 **Reject**（或客户端直接取消权限弹窗）→ 该工具调用标记 `failed`，
  LLM 收到「execution rejected by user」后自行调整策略
- 客户端不回包时该回合会一直等待；`session/cancel` 可中断等待

## 模型切换

默认使用 Flash（高频对话）。客户端可通过配置选项切换：

```text
→ {"jsonrpc":"2.0","id":4,"method":"session/set_config_option",
   "params":{"sessionId":"<uuid>","key":"model","value":"pro"}}
← {"jsonrpc":"2.0","id":4,"result":{
     "configOptions":[{"key":"model","name":"Model","type":"select",
                       "options":[
                         {"label":"DeepSeek-V4-Flash","value":"flash"},
                         {"label":"DeepSeek-V4-Pro","value":"pro"}],
                       "defaultValue":"pro"}]}}
```

选择在会话内生效，只影响后续回合。复杂推理（规划、多步工具编排）切 Pro，
常规对话保持 Flash。

## 与其它模式的差异

| 特性 | ACP | TUI | CLI | Server |
|------|:---:|:---:|:---:|:------:|
| 交互终端 | — | ✅ | — | — |
| 权限确认 | 客户端弹窗 | 终端 y/a/n | 拦截报错 | 白名单拦截 |
| 会话持久化 | ✅（SQLite） | ✅ | — | ✅ |
| 上下文压缩 | ✅ | ✅ | — | ✅ |
| 多轮对话 | ✅ | ✅ | — | ✅ |

**限制**：

- **无 TTY** — ACP 是纯管道传输，模型的 `interactive` 标志被忽略，
  所有命令一律以管道模式执行（30s 超时、4096 bytes 截断，可在
  `config.toml` 的 `[execution]` 调整）
- **无会话恢复** — `session/load` / `session/resume` 不支持：
  SQLite 只存 role/content，无法无损重放工具调用
- **单回合并发** — 同一会话同时发两个 `session/prompt` 会得到 `-32004`；
  多个会话（多个 sessionId）之间互不干扰

## 手工调试

用一条管道即可验证握手与建会话：

```bash
{
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize"}'
  printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"session/new"}'
} | arcc --acp
```

也可直接跑仓库里的集成测试：

```bash
cargo test -p arcc-acp
```

## 常见问题

**Q：客户端显示「连接失败」？**
A：确认 `arcc --acp` 能被直接启动（`which arcc`），且客户端注册时
命令/参数正确。ACP 消息只走 stdout，若 stdout 混入日志说明版本或
环境变量异常（`RUST_LOG` 仅影响 stderr）。

**Q：命令执行结果被截断？**
A：管道模式默认上限 4096 bytes，可在 `~/.arcc/config.toml`
`[execution].max_output_bytes` 调大。

**Q：为什么有的命令不弹权限确认？**
A：只有命中 `require_human_confirm` 的命令才需要确认；白名单内的
安全命令直接放行。`--unsafe` 启动则全部放行（不推荐）。
