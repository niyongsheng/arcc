# ARCC CLI Tool — AI Agent Integration

`arcc cli` converts natural language into shell commands, executes them,
and returns structured results. Any AI coding agent (Claude Code, Codex,
Cursor, Windsurf, etc.) can invoke it via a simple shell call — no MCP
server or special configuration required.

## Quick Reference

```bash
# Basic — ask and execute
arcc cli --json "check disk usage"

# Skip safety blocks (rm, dd, mkfs, etc.)
arcc cli --json --unsafe "delete all .log files in /tmp"

# Pipe data for analysis
cat error.log | arcc cli --json "find the most frequent error"
```

## Output Format

Every call with `--json` returns a single JSON object on stdout:

```json
{
  "response": "Disk usage: 120GB / 256GB (47%)",
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

| Field | Type | Description |
|-------|------|-------------|
| `response` | string | AI's natural-language summary |
| `tool_calls[]` | array | Each shell command executed |
| `tool_calls[].status` | string | `"ok"` / `"blocked"` / `"error"` |
| `status` | string | `"ok"` / `"partial"` / `"error"` |

## Integration Examples

### In CLAUDE.md / Agent Instructions

```markdown
## Available Tools

### arcc CLI

You can execute shell commands via `arcc cli --json "description"`.
Parse the JSON result to get the AI summary and command outputs.

- Basic: `arcc cli --json "check disk usage"`
- With unsafe flag: `arcc cli --json --unsafe "remove old files"`
- Parsing: `arcc cli --json "..." | jq .response`
```

### Calling from Shell (any agent)

```bash
# Get structured result
result=$(arcc cli --json "find the largest files in /tmp")
response=$(echo "$result" | jq -r '.response')
```

### As an MCP Tool (Claude Code / Cursor / Windsurf)

`bin/arcc-mcp` implements the MCP stdio protocol, wrapping `arcc cli`.
Register it in your agent's MCP configuration:

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

Supported tools: `arcc` with parameters `prompt` (string, required) and
`unsafe` (boolean, optional).

## Parameter Reference

| Argument | Description |
|----------|-------------|
| `prompt` | Natural language task description (e.g. `"check disk usage"`) |
| `--json` | Output a single JSON object instead of streaming tokens |
| `--unsafe` | Skip safety allowlist — allows `rm`, `dd`, `mkfs`, `shutdown` |
| `!command` | Run a raw shell command without LLM involvement |

## Typical Use Cases

| Category | Example |
|----------|---------|
| System | `arcc cli --json "check memory and cpu usage"` |
| Network | `arcc cli --json "find what's listening on port 80"` |
| Docker | `arcc cli --json "list all running containers"` |
| Git | `arcc cli --json "show unpushed commits"` |
| Files | `arcc cli --json --unsafe "find and delete empty directories"` |
| Logs | `cat app.log \| arcc cli --json "find error patterns"` |
| Code | `arcc cli --json "generate a rust function to read toml files"` |

## Troubleshooting

**`arcc: command not found`** — arcc is not installed or not on PATH.

**`unrecognized option '--json'`** — Version too old. Update to v0.3.0+.

**Commands blocked** — Add `--unsafe` to skip the safety allowlist.
