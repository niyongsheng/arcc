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
| **鼠标滚轮** | 滚动聊天内容（拖选文本时按住 Shift；Terminal.app/iTerm2 用 Option） |
| **`/mouse off`** | 关闭鼠标捕获，恢复终端原生拖拽选中/复制；用 PgUp/PgDn 滚动聊天 |
| **Tab** | 命令补全 / 切换代码块焦点 |
| **Ctrl+C** | 退出 |

## 斜杠命令

| 命令 | 用途 |
|------|------|
| `/plan <任务>` | 多步骤任务规划分解 |
| `/exec <命令>` | 直接执行 shell 命令 |
| `/computer [on/off/status]` | 桌面控制（截图 + 鼠标键盘，macOS） |
| `/dashboard` 或 `/data` | 打开数据面板（会话/Token/审计） |
| `/skills` | 列出已注册 MCP 工具 |
| `/model` | 查看当前使用的 Pro/Flash 模型 |
| `/thinking` | 切换 DeepSeek 思维链显示 |
| `/mouse [on/off/status]` | 开关终端鼠标捕获（`off` 恢复原生选中复制） |
| `/clear` | 清空当前会话历史 |
| `/help` | 查看全部命令帮助 |
| `/stats` | 会话统计 |
| `/init` | 在项目根目录生成 AGENTS.md |
| `/exit` | 退出 |

## 特色功能

- **双模型调度**：复杂任务自动用 Pro 模型，常规对话用 Flash 模型
- **上下文压缩**：达到 Token 阈值（默认 800k）时自动摘要压缩，节省上下文窗口
- **项目指令**：通过 `/init` 在项目根目录生成 AGENTS.md，AI 自动读取并遵循项目规范
- **MCP 插件**：支持 MCP 协议的外部工具注册
- **思维链**：DeepSeek 的推理过程以灰色动画显示
- **命令安全**：`rm`/`dd`/`mkfs` 等危险命令需人工确认（y/a/n）

## Computer Use（macOS）

让 ARCC 看到屏幕并操作鼠标/键盘，适合“打开某个应用、点击按钮、填写表单”等桌面任务。

1. 在系统设置 → 隐私与安全性中，为终端授予 **屏幕录制**（截图）和 **辅助功能**（鼠标键盘）权限，然后重启终端。
2. 在 TUI 中输入 `/computer on`。
3. 直接描述任务，例如：`打开 TextEdit，输入 hello world` 或 `截个屏，告诉我当前最前面的窗口是什么`。

行为说明：

- 默认关闭，每次启动都需要 `/computer on`；`/computer off` 立即停止。
- 截图会自动发送给视觉模型（默认 `deepseek-flash`）；文本推理仍可用 Pro。
- 每个鼠标/键盘动作都会弹出确认：
  - `y` 批准本次
  - `a` 本会话后续不再询问（谨慎使用）
  - `n` 拒绝
- 所有动作写入 `~/.arcc/logs/audit.jsonl`，可在 `/dashboard` 的审计区域查看。
- ARCC 会拒绝把密码、2FA、支付信息输入到桌面，也不会主动点击系统安全弹窗。

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