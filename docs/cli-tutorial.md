# CLI 模式教程 — Shell 子代理

`arcc cli` 是一个**专用的 shell 执行子代理**。它接收自然语言描述，自主规划
并执行 shell 命令，返回结构化 JSON 结果。

它不擅长复杂推理或长对话，但非常擅长一件事：**把自然语言变成 shell 命令并执行**。
所以适合作为"工具人"被更强大的 AI Agent 调用。

## 基本用法

```bash
# 自然语言 → shell 命令
arcc cli "找出最大的 5 个文件"

# 原始命令（！前缀，绕过 LLM，直接执行）
arcc cli "!ls -la"

# 管道输入分析
cat error.log | arcc cli "统计 500 错误占比"

# JSON 模式（给程序/AI 消费）
arcc cli --json "检查网络状态"
```

## 作为子代理的优势

| 特性 | 说明 |
|------|------|
| **专注单一职责** | 只做自然语言→shell 的转换和执行，不分散注意力 |
| **结构化输出** | `--json` 模式返回 `{response, tool_calls[], status}`，可直接被父 agent 解析 |
| **自主规划** | 你只需描述目标，它自己决定用什么命令、按什么顺序执行 |
| **安全可控** | 危险命令被 allowlist 拦截，需显式 `--unsafe` 放行 |
| **轻量无状态** | 每次调用独立启动，秒级完成，无上下文负担 |

## 输出模式

| 模式 | 命令 | 说明 |
|------|------|------|
| **流式** | `arcc cli "..."` | Token 逐个打印到 stdout，适合终端交互 |
| **JSON** | `arcc cli --json "..."` | 一次性输出完整 JSON，适合程序化消费 |

JSON 模式输出结构：

```json
{
  "response": "当前磁盘使用 120GB / 256GB (47%)",
  "tool_calls": [
    {
      "command": "df -h / | tail -1",
      "status": "ok",
      "stdout": "/dev/sda1  256G  120G  136G  47% /",
      "stderr": "",
      "exit_code": 0,
      "error": null
    }
  ],
  "status": "ok"
}
```

父 agent 通过 `jq` 或 JSON 解析库提取 `.response` 获取 AI 总结，提取 `.tool_calls[]`
获取每条命令的详细执行结果。

## 从父 Agent 调用

任何能执行 shell 命令的 AI Agent 都可以调用 arcc CLI：

```bash
# 最基本的方式（任何 agent 都支持）
result=$(arcc cli --json "检查 Docker 容器状态")

# 提取回复
reply=$(echo "$result" | jq -r '.response')

# 检查是否有命令被阻止
if echo "$result" | jq -e '.tool_calls[] | select(.status == "blocked")' > /dev/null; then
  # 以 --unsafe 重试
  result=$(arcc cli --json --unsafe "检查 Docker 容器状态")
fi
```

### 在 Agent 指令中注册

在 `CLAUDE.md` 或 agent 的系统提示词中添加：

```markdown
## 可用工具

### arcc CLI — Shell 执行子代理

当需要执行 shell 命令时，调用 `arcc cli --json "任务描述"`。
返回 JSON 结构：{ response: string, tool_calls: array, status: string }。

- 普通操作：`arcc cli --json "查看磁盘使用"`
- 危险操作：`arcc cli --json --unsafe "删除旧日志"`
- 管道分析：`cat file | arcc cli --json "分析内容"`
```

## 安全选项

```bash
# 默认模式：危险命令被拦截（rm / dd / mkfs / shutdown 等）
arcc cli --json "删除 /tmp/cache"

# --unsafe 模式：允许所有命令
arcc cli --json --unsafe "删除 /tmp/cache"
```

当子代理被父 agent 调用时，父 agent 可以根据任务风险级别决定是否加 `--unsafe`。

## 典型场景

| 场景 | 命令 |
|------|------|
| DevOps | `arcc cli --json "检查所有 Docker 容器状态"` |
| 网络 | `arcc cli --json "找出 80 端口的进程"` |
| 日志 | `cat access.log \| arcc cli --json "统计 500 错误占比"` |
| 存储 | `arcc cli --json "找出占用空间最大的 10 个目录"` |
| 代码 | `arcc cli "写一个 Rust LRU Cache" > lru.rs` |
| Git | `arcc cli "把当前分支 rebase 到 main"` |
| 安全 | `arcc cli --json --unsafe "查找并修复泄露的密钥文件"` |

## 与 Server 模式配合

对于需要多轮对话或持久化的场景，使用 Server 模式的 `/chat` 端点代替：

```bash
# 单轮 → arcc cli
# 多轮 → curl server:9527/chat
```

详见 [ARCC CLI Skill](skills/arcc-cli.md)。
