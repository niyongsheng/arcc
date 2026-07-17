# CLI 模式教程 — Shell 子代理

`arcc cli` 是一个**pipe-line & Agent friendly shell 执行器**：接收自然语言描述，自主规划并执行，安全机制。
shell 命令，支持结构化返回JSON。

```bash
# 自然语言 → shell 命令
arcc cli "今天天气"
arcc cli "检查网络状态"

# 原始命令（！前缀，不走 LLM；用单引号避免 zsh 历史扩展）
arcc cli '!ls -la'

# 管道输入分析
cat error.log | arcc cli "统计 500 错误占比"
cat slow.sql.log | arcc cli "找出执行最慢的3条SQL"

# JSON 模式（给程序/AI 解析）
arcc cli --json "最近一周上证50走势分析"
# JSON → 格式美化（用 Python 内置 json 模块）
arcc cli --json "最近一周上证50走势分析" | python3 -m json.tool

# JSON → 提取 AI 摘要
arcc cli --json "磁盘使用情况" | grep '"response"' | head -1

# JSON → 查看 tool_calls 的执行情况
arcc cli --json "找出最大的 3 个进程" | grep -o '"command":"[^"]*"[^}]*}'

# JSON → 传给下一个命令（提取 response 作为下一个输入）
arcc cli --json "检查 Docker nginx容器状态" | grep -o '"response":"[^"]*"' | cut -d'"' -f4 | arcc cli "服务中断原因"
```

## 作为子代理的优势

| 特性 | 说明 |
|------|------|
| **单一职责** | 只做自然语言→shell，不分散注意力 |
| **结构化输出** | `--json` 返回 `{response, tool_calls[], status}` |
| **自主规划** | 描述目标即可，自主决定命令和顺序 |
| **安全可控** | 危险命令被拦截，需 `--unsafe` 显式放行 |
| **轻量无状态** | 每次调用独立，用完即走 |

## ClaudeCode/Codex 调用

方式一：在 `CLAUDE.md`|`AGENTS.md` 或系统提示词中注册：

```markdown
## 可用工具

### arcc CLI — Shell 执行子代理

调用方式：`arcc cli --json "任务描述"`
返回 JSON：{ response: string, tool_calls: array, status: string }

- 普通操作直接调用
- 危险操作加 `--unsafe`（rm / dd / mkfs 等）
- 管道分析：`cat file | arcc cli --json "分析内容"`
```

方式二：独立的arcc skill

详见 [ARCC CLI Skill](../skills/arcc-cli.md)

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
