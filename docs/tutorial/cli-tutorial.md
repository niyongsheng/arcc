# CLI 模式教程 — Shell 子代理

`arcc cli` 是一个**pipe-line & Agent friendly shell 执行器**：接收自然语言描述，自主规划并执行
shell 命令，支持结构化返回JSON。

```bash
# 自然语言 → shell 命令
arcc cli "找出最大的 5 个文件"

# 原始命令（！前缀，不走 LLM）
arcc cli "!ls -la"

# 管道输入分析
cat error.log | arcc cli "统计 500 错误占比"

# JSON 模式（给程序/AI 解析）
arcc cli --json "检查网络状态"
```

## 作为子代理的优势

| 特性 | 说明 |
|------|------|
| **单一职责** | 只做自然语言→shell，不分散注意力 |
| **结构化输出** | `--json` 返回 `{response, tool_calls[], status}` |
| **自主规划** | 描述目标即可，自主决定命令和顺序 |
| **安全可控** | 危险命令被拦截，需 `--unsafe` 显式放行 |
| **轻量无状态** | 每次调用独立，秒级完成 |

## JSON 输出

```json
{
  "response": "磁盘使用 120GB / 256GB (47%)",
  "tool_calls": [
    { "command": "df -h / | tail -1", "status": "ok",
      "stdout": "/dev/sda1  256G  120G  136G  47% /", "exit_code": 0 }
  ],
  "status": "ok"
}
```

父 agent 用 `jq` 提取结果：

```bash
result=$(arcc cli --json "检查 Docker 容器状态")
reply=$(echo "$result" | jq -r '.response')
```

## ClaudeCode 调用

在 `CLAUDE.md` 或系统提示词中注册：

```markdown
## 可用工具

### arcc CLI — Shell 执行子代理

调用方式：`arcc cli --json "任务描述"`
返回 JSON：{ response: string, tool_calls: array, status: string }

- 普通操作直接调用
- 危险操作加 `--unsafe`（rm / dd / mkfs 等）
- 管道分析：`cat file | arcc cli --json "分析内容"`
```

## 安全机制

```bash
# 危险命令被拦截
arcc cli "rm -rf /tmp/cache"
# → error: requires human confirmation

# --unsafe 放行
arcc cli --unsafe "rm -rf /tmp/cache"
```

默认拦截：`rm`, `mv`, `dd`, `mkfs`, `shutdown`, `reboot`, `fdisk`。
可在 `~/.arcc/config.toml` 的 `[safety]` 中自定义。

## 典型场景

| 场景 | 命令 |
|------|------|
| DevOps | `arcc cli --json "检查所有 Docker 容器"` |
| 网络 | `arcc cli --json "找出 80 端口的进程"` |
| 日志 | `cat access.log \| arcc cli --json "统计 500 错误"` |
| 存储 | `arcc cli --json "最大的 10 个目录"` |
| Git | `arcc cli "把当前分支 rebase 到 main"` |

详见 [ARCC CLI Skill](../skills/arcc-cli.md)。
