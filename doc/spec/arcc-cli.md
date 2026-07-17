---
name: arcc-cli
description: AI sub-agent — delegate shell/system/git/docker tasks to LLM
---

`arcc cli` is an **LLM-powered sub-agent**. You give it a goal in natural language; it plans the steps, executes shell commands, interprets results, and returns a structured summary — all in one shot.

It handles messy, labor-intensive grunt work so you stay focused on higher-level reasoning.

## Core Identity

**arcc is an AI sub-agent, not a command runner.** It has its own:

- **Reasoning engine** — understands complex natural language instructions
- **Tool-calling loop** — can run multiple shell commands, see intermediate results, and adapt its plan
- **Response generation** — produces a natural language summary of what happened, not just raw output
- **Batch capability** — decompose a single high-level goal into multiple shell commands automatically

You delegate **what to do**; arcc figures out **how to do it**.

### Stateless Constraint

Each call is **stateless** — no conversation history between calls and no ability to ask clarifying questions. Formulate your prompt as a complete, self-contained task description.

## When to Use

| ✅ Delegate to arcc | ❌ Don't use — stay here |
|----------------|------------------------------|
| **System info** — disk, memory, CPU, network, processes | **Conversation** — multi-turn chat, planning, reasoning (you are better at this) |
| **File operations** — find, grep, sort, count, transform | **Interactive commands** — anything needing TTY (vim, nano, htop, ssh) |
| **Git queries** — status, log, diff, branch | **Long-running** — `tail -f`, `watch`, server processes |
| **Docker / container** — ps, logs, exec (non-interactive) | **Writing/editing code** — prefer Write/Edit tool directly |
| **Code analysis** — lint, test run, build check | **Package install** — unless explicitly asked |
| **Log analysis** — find patterns, count errors | **Anything you can do with Read/Grep/Glob tools** (faster) |
| **Bulk operations** — batch rename, cleanup, backup | |
| **Multi-step tasks** that chain several commands | |

**Cost optimization**: arcc runs on cheaper LLM (significantly cheaper per-token than Claude high-end models). Delegate token-intensive tasks — bulk grep/log analysis, multi-step shell scripts, large-file scanning — to arcc to burn cheaper tokens while your expensive Claude context stays focused on high-level reasoning.

**Rule of thumb**: If you'd need to chain 3+ shell commands to get the answer, delegate to arcc — it will plan the chain itself. If it's a single `ls`/`df`/`grep`, just run it directly.

## Calling Convention

```bash
# Delegate a task (arcc plans and executes)
arcc cli --json "check disk usage and identify top 5 largest directories"

# With unsafe flag for destructive operations
arcc cli --json --unsafe "delete all .log files older than 30 days"

# Pipe data for analysis
cat app.log | arcc cli --json "find error patterns and suggest root cause"
```

### Parameter Reference

| Parameter | Required | Description |
|-----------|----------|-------------|
| `prompt` | ✅ | Natural language **goal description**. Give context: file paths, targets, conditions |
| `--json` | recommended | Output single JSON object. Always use for AI-to-AI calling |
| `--unsafe` | optional | Skip safety allowlist for destructive operations |
| `!command` | — | Bypass arcc's LLM, run a raw shell command directly (see below) |

### `!command` — Raw Shell Bypass

Prefix a command with `!` to skip the DeepSeek-V4 reasoning entirely and execute it as a raw shell command:

```
arcc cli --json "!df -h"
arcc cli --json "!ls -la /tmp"
```

Use when:
- You know **exactly** what command to run (no planning needed)
- You want **raw stdout** in `tool_calls[].stdout` without AI interpretation overhead
- Debugging or diagnostic scenarios

Don't use for: complex tasks that benefit from arcc's reasoning, multi-step chains, or error recovery.

## Prompt Tips

arcc's response quality depends heavily on your prompt. Write it like you'd write a task for another engineer:

| Instead of… | Write… | Why |
|-------------|--------|-----|
| `"check disk"` | `"check disk usage on /dev/sda1, show percentage used and available space"` | Be specific about target and output |
| `"find large files"` | `"find files larger than 100MB under /home, sort by size descending, show top 10"` | Set thresholds and sorting |
| `"git status"` | `"show all unpushed commits, their authors, and the files changed in each"` | Ask for the derivative insight |
| `"check memory"` | `"check memory usage, show top 5 processes by RSS, highlight anything > 1GB"` | Ask for analysis, not just data |
| `"find errors"` | `"scan /var/log/crash.log for ERROR and FATAL entries, count by hour, identify the most common error message"` | Specify pattern and aggregation |

Key principles:
- **Provide context** — file paths, directories, targets, conditions
- **Specify output format** — "show top 5", "sort by size", "count by author"
- **Ask for insight** — "identify root cause", "highlight anomalies", "suggest fixes"
- **One goal per call** — stateless means you can't follow up, but don't stuff 3 unrelated tasks into one prompt either

## Response Format (--json)

arcc returns a structured JSON that includes both its AI-generated summary and the execution trace:

```json
{
  "response": "Disk usage is 47% (120G/256G). The largest directory outside system folders is /home/user/projects (23G) followed by /home/user/.cache (8G).",
  "tool_calls": [
    {
      "command": "df -h / | tail -1",
      "status": "ok",
      "stdout": "/dev/sda1  256G  120G  136G  47% /",
      "stderr": "",
      "exit_code": 0,
      "error": null
    },
    {
      "command": "du -sh /home/user/* 2>/dev/null | sort -rh | head -5",
      "status": "ok",
      "stdout": "23G\t/home/user/projects\n8G\t/home/user/.cache\n...",
      "stderr": "",
      "exit_code": 0,
      "error": null
    }
  ],
  "status": "ok"
}
```

| Field | Type | Meaning |
|-------|------|---------|
| `response` | string | arcc's AI-generated summary of findings (natural language) |
| `tool_calls[]` | array | Each shell command arcc chose to execute (its reasoning trace) |
| `tool_calls[].status` | string | `"ok"` / `"blocked"` / `"error"` |
| `tool_calls[].exit_code` | int | 0 = success, non-zero = failure |
| `status` | string | `"ok"` (all succeeded) / `"partial"` / `"error"` |


## Response Rules

After receiving the JSON result:

1. **Always mention what was done** — reference `tool_calls[].command` so the user knows what ran
2. **Summarize** the `response` field in your own words, don't just echo it
3. **Show stdout** when it's the primary output (e.g. `df -h`, `git log`)
4. **Hide stdout** when it's noise (e.g. `mkdir -p`, `mv` succeeded)
5. **Surface errors** — if `tool_calls[].status` is `"error"` or `"blocked"`, tell the user

```markdown
<!-- Good -->
Disk usage: 120 GB / 256 GB (47%) on `/dev/sda1`.

<!-- Bad — just echoing JSON -->
The response says "Disk usage: 120GB / 256GB (47%)".
```

## Safety & Guardrails

### --unsafe Rule

| Condition | Action |
|-----------|--------|
| Read-only commands (`ls`, `df`, `cat`, `grep`) | Never need `--unsafe` |
| Mutating but safe (`mkdir`, `touch`, `cp`, `mv`) | Don't use `--unsafe` unless file is critical |
| Destructive (`rm -rf`, `dd`, `mkfs`, `shutdown`, `kill -9`) | **Always ask user first**, then use `--unsafe` |
| User explicitly said "delete", "remove", "clean up" | Safe to use `--unsafe` — but still mention it |

⚠ **Never silently use `--unsafe`.** Always inform the user: "I'll use --unsafe to proceed."

### Stateless Limitation

arcc cli has **no memory** between calls. If a task requires multiple steps:
 1. Break it into independent calls
 2. Each call must contain all context needed
 3. Pass intermediate data via files or pipe: `arcc cli --json "step N"`

## Error Recovery

| Error | Cause | Action |
|-------|-------|--------|
| `"status": "blocked"` | Command matched deny-list | Retry with `--unsafe` after user approval |
| `command not found` | arcc not on PATH | Suggest installing or provide absolute path |
| `unrecognized option '--json'` | Outdated arcc | `arcc` version too old, suggest update |
| `"status": "error"` with `exit_code != 0` | Command failed | Check stderr, fix parameters, retry |
| `"status": "partial"` | Some commands succeeded, some failed | Report both, retry only failed ones |
| `sudo` hangs or times out | No TTY to input password | See **Sudo & Privilege** below |

## Sudo & Privilege

arcc cli runs commands through a pseudo-terminal (PTY). When called by
another AI agent (via MCP / pipe), there is no human to type a sudo
password — commands like `sudo systemctl restart nginx` will **hang**
waiting for password input.

### AI-agent behaviour

When you (the calling agent) need a command that requires `sudo`:

1. **Try `sudo -n` first** — non-interactive mode. If the user has
   NOPASSWD configured, it succeeds immediately. If not, it fails
   with `sudo: a password is required` instead of hanging.
2. **Report the failure** — tell the user which command needs sudo
   and ask them to set up passwordless sudo for it.

### Recommended setup

**Passwordless sudo for specific commands** (safest):

```bash
# As root or via sudo visudo -f /etc/sudoers.d/arcc
echo "$USER ALL=(ALL) NOPASSWD: /usr/bin/systemctl, /usr/bin/docker" | sudo tee /etc/sudoers.d/arcc
```

This lets arcc run `sudo systemctl restart nginx` and `sudo docker ps`
without a password, while other sudo commands still prompt normally.

**Full passwordless sudo** (convenient, less secure):

```bash
echo "$USER ALL=(ALL) NOPASSWD: ALL" | sudo tee /etc/sudoers.d/arcc


| Category | Example Prompt |
|----------|---------------|
| System | `"check memory and cpu usage, show top 5 processes by memory"` |
| Disk | `"find directories larger than 1GB in /home"` |
| Network | `"show what's listening on port 80 and 443"` |
| Docker | `"list all running containers and their resource usage"` |
| Git | `"show commits since last tag, grouped by author"` |
| Files | `"find all .tmp files older than 7 days and count their total size"` |
| Logs | `"find the 10 most frequent error messages in /var/log/system.log"` |
| Code | `"count lines of Rust code, excluding comments and blank lines, in src/"` |

This exposes a single tool `arcc(prompt, unsafe?)` — same as calling `arcc cli --json`.
