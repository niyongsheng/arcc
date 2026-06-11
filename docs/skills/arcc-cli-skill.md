# ARCC CLI Skill — Claude Code MCP Integration

Register `arcc cli` as an MCP tool in Claude Code, enabling Claude to
execute shell commands directly through natural language.

## What It Does

Once registered, Claude Code gains an `arcc` tool. Just say:

> "Check disk usage"
> "Find processes on port 80"
> "Rebase current branch onto main"

Claude will invoke `arcc cli` to execute it and return the result.

## Setup

### 1. Verify arcc is in PATH

```bash
which arcc
```

If not found, install arcc first.

### 2. Make the MCP script executable

```bash
chmod +x /path/to/arcc/bin/arcc-mcp
```

### 3. Register with Claude Code

Edit `~/.claude/settings.json` (create if it doesn't exist):

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

> **Note:** `command` must be an absolute path — Claude Code's working
> directory may differ from the arcc project root.

### 4. Restart Claude Code

Reopen Claude Code. You should see the `arcc` tool in the MCP tool list.

## Tool Definition

| Property | Value |
|----------|-------|
| **Name** | `arcc` |
| **Description** | Converts natural language to shell commands, executes them, and returns results |
| **Arg 1** | `prompt` (string, required) — task description in natural language |
| **Arg 2** | `unsafe` (boolean, optional) — skip safety allowlist for dangerous commands |

### Parameter Guide

**prompt**: Describe what you want to do in natural language. Examples:

- `"List the 10 largest files in /tmp"`
- `"Check Docker container status"`
- `"Verify nginx configuration syntax"`
- `"Show unpushed commits"`

**unsafe**: Set to `true` when the task may involve dangerous commands
(`rm`, `dd`, `mkfs`, `shutdown`, etc.).

## How It Works

```
Claude Code → arcc-mcp (MCP stdio server)
                  ↓
            arcc cli --json [--unsafe] "prompt"
                  ↓
            JSON response → returned to Claude Code
```

The `arcc-mcp` script:
1. Receives MCP JSON-RPC requests via stdin
2. Calls `arcc cli --json "prompt"`
3. Returns the JSON result to Claude Code

## Examples

### System Administration

```
You: Check memory usage
→ arcc cli --json "Check memory usage"
→ Result: Total 16GB, 43% used ...

You: Find top 5 CPU-consuming processes
→ arcc cli --json "Find top 5 CPU-consuming processes"
```

### File Operations (requires --unsafe)

```
You: Delete all .log files in /tmp/old-logs
→ arcc cli --json --unsafe "Delete all .log files in /tmp/old-logs"
```

### Network Diagnostics

```
You: Check network connectivity
→ arcc cli --json "Check network connectivity"
```

### Git Operations

```
You: Show current branch status
→ arcc cli --json "Show current git branch status"
```

## Referencing in CLAUDE.md

Add to your project's `CLAUDE.md`:

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

## Troubleshooting

**"arcc command not found"** — The `arcc-mcp` script can't find the
`arcc` binary. Either set the `ARCC` variable in the script to an
absolute path, or ensure `arcc` is on your PATH.

**Timeout** — The default timeout is 60 seconds. For long-running tasks,
adjust the `timeout` parameter in `subprocess.run()` inside
`bin/arcc-mcp`.

**JSON parse error** — Your `arcc cli` version is too old and doesn't
support the `--json` flag. Upgrade to v0.3.0 or later.
