# CLI 模式教程

单轮执行模式，适合脚本、管道、CI/CD 集成。

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

## 输出模式

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

## 安全选项

```bash
# 绕过安全限制（允许所有命令执行）
arcc cli --unsafe "检查系统状态"

# 或完整参数名
arcc cli --dangerously-skip-permissions "检查系统状态"
```

## 典型场景

```bash
# DevOps 操作
arcc cli "检查所有 Docker 容器状态"
arcc cli "找出占用 80 端口的进程"

# 数据处理
cat access.log | arcc cli "统计返回 500 的请求占比"

# 代码生成
arcc cli "写一个 Rust 的 LRU Cache 实现" > lru.rs

```

### AI Agent 集成

ARCC CLI 可以被任何 AI 编程助手（Claude Code、Codex、Cursor 等）直接调用。
只需让 agent 执行 shell 命令即可：

```bash
# agent 内部调用（任何支持 shell 的 agent）
arcc cli --json "检查磁盘使用情况"
```

返回的 JSON 可被 agent 解析提取结果。详见 [ARCC CLI Skill](skills/arcc-cli.md)。
