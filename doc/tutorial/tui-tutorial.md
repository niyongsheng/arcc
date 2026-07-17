# TUI 模式教程

全屏交互式终端，适合日常使用。

```bash
arcc tui
```

## 界面布局

```
┌─ ARCC · a1b2c3d4 · tui  ✶─────────────────────┐  ← 标题栏（含动画）
│                                               │
│  🧑 帮我看一下当前机器的资源占用                   │  ← 聊天区（markdown渲染）
│  🤖 **系统资源概览**                            │
│                                               │
│     总内存   16 GiB    使用  43%                │
│     CPU     8 核      负载  0.8                │
│                                               │
│  ⚡ top -l 1 -n 5 -stats cpu,mem              │  ← 命令执行记录
│  exit=0                                       │
│                                               │
│  ◇ thinking  🧠 on  ────────────────────────  │  ← 状态栏（含动画）
│  ──────────────────────────────────────────── │  ← 分隔线
│  > /plan 帮我启动这个项目                        │  ← 输入行（Tab补全）
│  ──────────────────────────────────────────── │  ← 底部线
```

## 核心操作

| 操作 | 效果 |
|------|------|
| **输入文字 + Enter** | 向 AI 发送消息 |
| **↑/↓** | 浏览输入历史（AI 输出时也可提前打字） |
| **PgUp/PgDn** | 滚动聊天内容 |
| **鼠标滚轮** | 滚动聊天内容（Shift+拖拽选择文本） |
| **Tab** | 命令补全 / 切换代码块焦点 |
| **Ctrl+C** | 退出 |

## 斜杠命令

| 命令 | 用途 |
|------|------|
| `/plan <任务>` | 多步骤任务规划分解 |
| `/exec <命令>` | 直接执行 shell 命令 |
| `/dashboard` 或 `/data` | 打开数据面板（会话/Token/审计） |
| `/skills` | 列出已注册 MCP 工具 |
| `/model` | 查看当前使用的 Pro/Flash 模型 |
| `/thinking` | 切换 DeepSeek 思维链显示 |
| `/clear` | 清空当前会话历史 |
| `/help` | 查看全部命令帮助 |
| `/stats` | 会话统计 |
| `/init` | 在项目根目录生成 ARCC.md |
| `/exit` | 退出 |

## 特色功能

- **双模型调度**：复杂任务自动用 Pro 模型，常规对话用 Flash 模型
- **上下文压缩**：达到 Token 阈值（默认 800k）时自动摘要压缩，节省上下文窗口
- **项目指令**：通过 `/init` 在项目根目录生成 ARCC.md，AI 自动读取并遵循项目规范
- **MCP 插件**：支持 MCP 协议的外部工具注册
- **思维链**：DeepSeek 的推理过程以灰色动画显示
- **命令安全**：`rm`/`dd`/`mkfs` 等危险命令需人工确认（y/a/n）

---

## 安装

### Mac / Linux

```bash
# 一键安装（ARM Mac / Linux x86_64 自动下载预编译二进制）
curl -fsSL https://raw.githubusercontent.com/niyongsheng/arcc/main/scripts/install.sh | bash

# 验证
arcc -V

# 配置 API Key
mkdir -p ~/.arcc

cat > ~/.arcc/config.toml << 'EOF'
[model]
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
EOF

# 启动 TUI
arcc tui
```

### Windows
```powershell
# 安装（管理员权限）
irm https://raw.githubusercontent.com/niyongsheng/arcc/main/scripts/install.ps1 | iex

# 验证
arcc -V

# 配置 API Key
New-Item -ItemType Directory -Force -Path "$env:USERPROFILE\.arcc"

@'
[model]
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
'@ | Out-File -Encoding utf8 "$env:USERPROFILE\.arcc\config.toml"

# 启动 TUI
arcc tui
```