# ARCC CLI Skill — Claude Code MCP 集成

将 `arcc cli` 注册为 Claude Code 的 MCP 工具，使 Claude 能直接通过自然语言执行 shell 命令。

## 效果

注册后，Claude Code 会自动多出一个 `arcc` 工具。你只需说：

> "检查磁盘使用情况"
> "找出占用 80 端口的进程"
> "把当前分支 rebase 到 main"

Claude 就会调用 `arcc cli` 来执行，返回结果。

## 安装

### 1. 确保 arcc 在 PATH 中

```bash
which arcc
```

如果未安装，先安装 arcc。

### 2. 给 MCP 脚本执行权限

```bash
chmod +x /path/to/arcc/bin/arcc-mcp
```

### 3. 注册到 Claude Code

编辑 `~/.claude/settings.json`（如果不存在则创建）：

```json
{
  "mcpServers": {
    "arcc": {
      "type": "stdio",
      "command": "/absolute/path/to/arcc/bin/arcc-mcp",
      "args": []
    }
  }
}
```

> **注意**：`command` 必须是绝对路径，因为 Claude Code 的工作目录可能不是 arcc 项目目录。

### 4. 重启 Claude Code

重新打开 Claude Code，你会在 MCP 工具列表中看到 `arcc` 工具。

## 工具定义

| 属性 | 值 |
|------|-----|
| **名称** | `arcc` |
| **描述** | 将自然语言转换为 shell 命令并执行，返回结果 |
| **参数 1** | `prompt` (string, 必填) — 任务描述 |
| **参数 2** | `unsafe` (boolean, 可选) — 跳过安全限制 |

### 参数说明

**prompt**：自然语言描述你要做的事情。例如：

- `"列出 /tmp 下最大的 10 个文件"`
- `"检查 Docker 容器状态"`
- `"nginx 配置语法检查"`
- `"git 查看未推送的提交"`

**unsafe**：当需要执行危险命令（`rm`、`dd`、`mkfs`、`shutdown` 等）时设为 `true`。

## 工作原理

```
Claude Code → arcc-mcp (MCP stdio server)
                  ↓
            arcc cli --json [--unsafe] "prompt"
                  ↓
            JSON 响应 → 返回给 Claude Code
```

`arcc-mcp` 脚本：
1. 接收 MCP JSON-RPC 请求
2. 调用 `arcc cli --json "prompt"` 
3. 将 JSON 结果返回给 Claude Code

## 示例

### 系统管理

```
你: 检查内存使用情况
→ arcc cli --json "检查内存使用情况"
→ 返回: 总内存 16GB，已用 43% ...

你: 找出占用 CPU 最高的 5 个进程
→ arcc cli --json "找出占用CPU最高的5个进程"
```

### 文件操作（需 unsafe）

```
你: 删除 /tmp/old-logs 目录下的所有 .log 文件
→ arcc cli --json --unsafe "删除 /tmp/old-logs 目录下的所有 .log 文件"
```

### 网络诊断

```
你: 检查网络连通性
→ arcc cli --json "检查网络连通性"
```

### Git 操作

```
你: 查看当前分支状态
→ arcc cli --json "查看当前 git 分支状态"
```

## 在 CLAUDE.md 中引用

在项目根目录的 `CLAUDE.md` 中添加：

```markdown
## Available Tools

### arcc CLI

You have access to `arcc cli` for executing shell commands via natural
language. When you need to run terminal commands, invoke it as:

```json
{
  "tool": "arcc",
  "prompt": "your task description",
  "unsafe": false
}
```

Available as an MCP tool when registered in `~/.claude/settings.json`.
```

## 故障排除

**"arcc command not found"**：`arcc-mcp` 找不到 `arcc` 可执行文件。在脚本中修改 `ARCC` 变量为绝对路径，或确保 `arcc` 在 PATH 中。

**超时**：默认 60 秒超时，复杂任务可能需要更长时间。在 `subprocess.run` 的 `timeout` 参数中调整。

**JSON 解析失败**：`arcc cli --json` 版本过旧，不支持 `--json` 标志。更新到 v0.3.0 以上。
