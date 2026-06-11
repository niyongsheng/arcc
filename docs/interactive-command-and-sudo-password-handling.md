# 交互式命令执行与 `interactive` 机制设计

## 概述

ARCC 通过 `execute_command` 工具执行 shell 命令。部分命令需要 TTY 交互
（如 `sudo xxx`、`ssh`、`vim`、`mole clean` 等），这类命令不能通过管道
stdin/stdout 执行，必须直接绑定用户的终端。

ARCC 的 `interactive` 机制解决两个核心问题：
1. **谁决定**命令是否需要 TTY — 由 AI 根据系统提示词自主判断
2. **如何执行**需要 TTY 的命令 — TUI 临时交出终端控制权

## 设计原则：AI 自主决策

### 不再靠硬编码名单

早期版本维护一个 Rust 硬编码的命令名列表（`sudo`、`ssh`、`vim`、`nano`、
`htop`、`top`、`less`、`more`、`passwd`、`telnet`...），匹配到则强制启用
交互模式。这种方式存在明显缺陷：

- **遗漏**：每个新工具（`mole`、`brew`、`apt`、`pip`…）都要改代码
- **AI 偷懒**：系统提示词告诉 AI "系统会自动检测" → AI 不再认真思考
- **维护成本**：名单越加越长，但仍追不上真实场景

### 当前设计：AI 自主决定 + 最小安全网

`interactive` 是工具定义的 **required** 参数（tools.rs:45），AI **每次调用**
都必须填写 `true` 或 `false`。决策链简化为：

```
AI 显式指定 interactive=true/false
    │
    ├── 值为 true  → 交互模式（继承 TTY）
    │
    └── 值为 false → 管道模式（30s 超时，4096B 截断）
         │
         └── 安全网：命令含 sudo → 强制 interactive=true
```

```rust
let ai_interactive = tc.arguments.get("interactive").and_then(|v| v.as_bool());
let auto_interactive = first == "sudo" || words.contains(&"sudo"); // 仅 sudo 兜底
let interactive = ai_interactive.unwrap_or(auto_interactive);
```

只有 `sudo` 保留为自动检测项（太常见、后果严重、AI 判断错误的代价高）。

### 系统提示词引导

三个模板文件统一给出指引，帮助 AI 做出正确判断：

| 模板 | 生效命令 |
|------|---------|
| `tui.md` | 普通对话输入 |
| `plan.md` | `/plan <task>` |
| `cli.md` | `arcc cli "<prompt>"` |

核心指引：
```
interactive: true  — 任何可能弹提示、要密码、提权、交互式界面的命令
interactive: false — 纯批处理，无任何交互
不确定 → 优先 true
```

## 执行流程

```text
┌────────────────────────────────────────────────────────────┐
│  工具执行任务（tokio::spawn）                                │
│  1. 检测到 interactive = true                               │
│  2. 创建 oneshot channel                                    │
│  3. 发送 AppEvent::InteractiveCommand{command, response_tx} │
│  4. await resp_rx                                           │
│  5. 收到退出码，格式化为 tool response                        │
│  6. 继续 phase 2 API 请求                                    │
└────────────────────────────────────────────────────────────┘
                      │
                      ▼
┌────────────────────────────────────────────────────────────┐
│  主事件循环                                                  │
│  1. abort(crossterm 输入处理器) — 停止抢 stdin               │
│  2. LeaveAlternateScreen        — 退出备屏幕                 │
│  3. disable_raw_mode            — 恢复 cooked 模式           │
│  4. spawn(command, Stdio::inherit()) — 子进程继承终端        │
│  5. wait()                      — 阻塞等待完成               │
│  6. enable_raw_mode             — 恢复 raw 模式              │
│  7. EnterAlternateScreen        — 重新进入备屏幕             │
│  8. terminal.clear()            — 清屏                       │
│  9. respawn(crossterm 输入处理器) — 恢复输入监听              │
│  10. response_tx.send(exit_code) — 返回退出码                │
└────────────────────────────────────────────────────────────┘
```

## 问题排查历史

### 问题 1：sudo 提示密码错误 + 需要按两次回车

**现象**：在 TUI 中执行 `sudo xxx`，输入密码后需要按两次回车，且提示"Sorry, try again"。

**排查过程**：

| 尝试的方案 | 结果 | 原因 |
|-----------|------|------|
| 仅 `disable_raw_mode` + `Stdio::inherit()` | ❌ 仍需要两次回车 | crossterm 还在抢 stdin |
| + `LeaveAlternateScreen` | ❌ 仍需要两次回车 | 同上 |
| + `stty sane` + `tcflush` + `/dev/tty` | ❌ 未完全解决 | 复杂但未命中根因 |
| **abort crossterm 输入处理器** | ✅ 一次性解决 | 根因在这里 |

**根因**：crossterm 的 `spawn_input_handler` 在后台 tokio 任务中持续轮询 stdin。  
当子进程（sudo）也通过 `Stdio::inherit()` 读取同一 stdin 时，两者争抢输入数据。  
用户输入的密码可能被 crossterm 吞掉，sudo 读到不完整的数据 → 密码错误。

**修复**：执行交互命令前 `abort()` 输入处理器的 `JoinHandle`，执行完后重新 `spawn`。

```rust
input_handle.abort();                          // 暂停 crossterm
let _ = input_handle.await;
// ... 运行子进程 ...
input_handle = spawn_input_handler(tx.clone()); // 重启 crossterm
```

### 问题 2：子进程输出与 TUI 界面混乱

**现象**：子进程的输出直接打在 alternate screen 上，和 TUI 内容混合，界面错乱。

**修复**：执行子进程前离开 alternate screen（`LeaveAlternateScreen`），输出进入主屏幕。  
执行完毕后重新进入（`EnterAlternateScreen`）+ `clear()`。

```rust
execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
// ... spawn/wait ...
execute!(terminal.backend_mut(), EnterAlternateScreen)?;
terminal.clear()?;
```

### 问题 3：屏幕闪烁

**现象**：子进程结束后 TUI 重新进入 alternate screen 时有可见闪烁。

**原因**：`clear()` 瞬间清屏造成的。

**修复**：移除 `clear()` 调用，让下一次 `terminal.draw()` 自然覆盖即可。  
但经过验证，在 `EnterAlternateScreen` 后执行 `clear()` 不会引起可见闪烁，恢复使用。

### 问题 4：mole 内部调 sudo 未进入交互模式 → 密码被截获 + API 400

**现象**：`mo clean --dry-run` 在 pipe 模式运行，mole 内部调用 sudo 等待密码。
用户在 TUI 输入密码后，密码被当作新用户消息提交，导致消息序列被破坏
（`assistant(tool_calls)` → `user(password)` 中间缺少 tool 响应），
DeepSeek API 返回 400。

**根因**：`mole` 命令名不在 auto_interactive 列表中，AI 也未设 `interactive: true`。
命令在 pipe 模式运行（30s 超时），sudo 无 TTY 无法读取密码。

**修复**：将决策权交还给 AI（见上方"设计原则"），改进系统提示词给出清晰指引。
移除硬编码命令列表，仅保留 `sudo` 作为安全网。

## 关键代码路径

| 文件 | 函数/事件 | 作用 |
|------|----------|------|
| `crates/arcc-tui/src/ui/app.rs` | `AppEvent::InteractiveCommand` | 主循环处理交互命令 |
| `crates/arcc-tui/src/ui/app.rs` | `submit()` / `plan_submit()` | 工具执行循环，检测 `interactive` |
| `crates/arcc-tui/src/ui/app.rs` | `auto_interactive` | 安全网：仅 `sudo` 强制交互 |
| `crates/arcc-tui/src/event/handler.rs` | `spawn_input_handler()` | 返回 `JoinHandle` 支持 abort |
| `crates/arcc-tui/src/event/loop_event.rs` | `AppEvent::InteractiveCommand` | 事件定义 |
| `crates/arcc-core/src/tools.rs` | `command_tool_definition()` | 工具定义参数 `interactive` |
| `crates/arcc-core/src/model/prompts/tui.md` | 系统提示词 | 指导 AI 判断 `interactive` |
| `crates/arcc-core/src/model/prompts/plan.md` | 系统提示词 | `/plan` 模式的指引 |
| `crates/arcc-core/src/model/prompts/cli.md` | 系统提示词 | CLI 模式的指引 |

## 对比参考：CodeWhale 的实现

CodeWhale 使用更简单的方案——**不离开始备屏幕**，仅 `disable_raw_mode` → spawn → `enable_raw_mode`。  
但这一方案依赖 crossterm 不与子进程竞争 stdin（它们没有背景输入处理器），因此不适用于 ARCC 的架构。

ARCC 的最终方案结合了两者优点：
- CodeWhale 的 `Stdio::inherit()` 思路（简单可靠）
- 主动管理 crossterm 输入处理器生命周期（解决 stdin 竞争）
- alternate screen 进出（解决界面混乱）
