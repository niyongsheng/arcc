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

## 工作流程

```
用户输入 → LLM (DeepSeek-V4-Flash)
  → 输出文字（流式打印）
  → 或调用 execute_command 工具
    → 执行 shell 命令（30s 超时，4KB 截断）
    → 结果返回 LLM
  → LLM 综合输出最终回复
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

### Claude Code / MCP 集成

ARCC CLI 可以通过 MCP 协议注册为 Claude Code 的工具，让 Claude
直接通过自然语言执行 shell 命令。

```
你: 检查磁盘使用情况
→ Claude 自动调用 arcc 工具
→ arcc cli --json "检查磁盘使用情况"
→ JSON 结果返回给 Claude → 回复你
```

**配置方式**：在 `~/.claude/settings.json` 中添加：

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

详见 [ARCC CLI MCP Skill](skills/arcc-cli-skill.md)。
