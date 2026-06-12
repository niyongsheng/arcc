# Server 模式教程

ARCC Server 模式是一个 HTTP 后台服务，支持 API 调用和飞书机器人集成。
它运行在后台（类似 daemon），通过 webhook 接收飞书消息，经 LLM 处理后
返回回复，同时支持定时任务调度和持久记忆。

```bash
# 前台运行
arcc server

# 后台守护进程（Ctrl+C 优雅关闭）
arcc server --daemon
```

## 配置

配置文件位于 `~/.arcc/config.toml`：

```toml
[model]
api_base = "https://api.deepseek.com"
# api_key = "sk-xxx"                  # 直接填写，或设 DEEPSEEK_API_KEY 环境变量
pro_model = "deepseek-v4-pro"          # 复杂推理
flash_model = "deepseek-v4-flash"      # 高频对话 + 压缩

[server]
host = "127.0.0.1"
port = 9527

[feishu]
enabled = true                         # 启用飞书机器人
app_id = "cli_xxxxxxxxxxxx"
app_secret = "xxxxxxxxxxxxxxxxxxxxxxxxxxxx"
verification_token = "xxxxxxxxxxxx"

[safety]
require_human_confirm = ["rm", "mv", "dd", "mkfs", "shutdown"]
```

## API 端点

### POST /chat — AI 对话

```bash
curl -X POST localhost:9527/chat \
  -H "Content-Type: application/json" \
  -d '{"session_id":"user-123","prompt":"用 Python 写一个快速排序"}'
```

返回 SSE 流（包含 reasoning + content + finish）：

```
event: reasoning
data: 用户需要快速排序实现...

data: 下面是快速排序的实现：

event: finish
data: [DONE]
```

**参数：**

| 字段 | 类型 | 说明 |
|------|------|------|
| `session_id` | string | 用户标识（同一用户连续对话用相同 ID） |
| `prompt` | string | 用户输入 |
| `stream` | bool | 是否流式（默认 false） |

### GET /health — 健康检查

```bash
curl localhost:9527/health
# {"status":"ok","version":"0.6.0"}
```

### 记忆管理 — /memory/{user_id}[/{key}]

```bash
# 查看用户的所有记忆
curl localhost:9527/memory/user-123

# 手动添加记忆
curl -X POST localhost:9527/memory/user-123 \
  -H "Content-Type: application/json" \
  -d '{"key":"preferred-language","value":"Rust"}'

# 更新记忆
curl -X PUT localhost:9527/memory/user-123/preferred-language \
  -H "Content-Type: application/json" \
  -d '{"value":"Go"}'

# 删除记忆
curl -X DELETE localhost:9527/memory/user-123/preferred-language
```

### 飞书 Webhook（需配置 [feishu]）

| 端点 | 说明 |
|------|------|
| `POST /feishu/webhook` | 接收飞书事件回调（消息 + 卡片交互） |
| `POST /feishu/send` | 主动发送消息到飞书 |

```bash
# 主动发送测试消息（需要 open_id）
curl -X POST localhost:9527/feishu/send \
  -H "Content-Type: application/json" \
  -d '{"open_id":"ou_xxxxxxxx","text":"来自 ARCC 的消息"}'
```

## 飞书机器人功能

### 消息渲染（Markdown）

所有飞书消息以 `msg_type: "post"` + `tag: md` 格式发送，
支持 CommonMark/GFM 语法渲染：

- `#` / `##` 标题
- `**加粗**`、`*斜体*`、`~~删除线~~`
- `` `行内代码` ``
- ` ``` ` 代码块（可指定语言）
- 有序/无序列表、表格、超链接

无需额外配置，AI 回复时直接使用 Markdown 即可。

### 连续对话

同一用户在同一个飞书会话中，对话历史会自动累积。
Server 使用 `chat_id` 作为 session key，每次消息都会带上历史上下文。

历史会话过长时（默认超过 80 万 tokens），系统每小时自动用
Flash 模型对旧消息做摘要压缩，保留最新对话和关键上下文。

### 主动通知（reply_to_user）

AI 在执行耗时操作（如命令执行、服务重启）时，可以自主调用
`reply_to_user(message)` 工具向用户发送进度通知，无需等全部完成。

例如用户说「重启一下 nginx」，AI 可能会：
1. 调 `reply_to_user("正在检查 nginx 状态…")`
2. 执行 `systemctl status nginx`
3. 执行 `systemctl restart nginx`
4. 调 `reply_to_user("nginx 已重启完成")`

### 定时任务（schedule_task）

支持 AI 自主创建和管理定时任务：

```text
用户：「每天凌晨 1 点帮我重启 nginx」
→ AI 调 schedule_task(cron="0 1 * * * *", task="重启 nginx 服务")
→ 回复「已安排，下次执行时间：2026-06-13 01:00:00」
```

**cron 格式：** 6 字段（秒 分 时 日 月 周）

| 示例 | 说明 |
|------|------|
| `0 1 * * * *` | 每天 1:00 |
| `0 */5 * * * *` | 每 5 分钟 |
| `0 0 * * * 0` | 每周日 0:00 |
| `@daily` | 每天 0:00（shorthand） |

**任务管理：**

| 工具 | 功能 |
|------|------|
| `list_scheduled_tasks` | 列出当前用户的所有活跃任务 |
| `cancel_scheduled_task` | 暂停（pause）或删除（delete）任务 |

用户可以说「看看我有哪些定时任务」「把那个 nginx 重启的任务取消掉」。

Scheduler 每 10 秒轮询一次，到期任务会以原用户的身份重新走完
LLM + 工具调用流程，结果自动推送给用户。

### 群聊 @ 过滤

群聊中仅当 bot 被 @提及 时才会响应，避免无关消息触发。
私聊不受限制，所有消息正常处理。

### 记忆系统

每次对话后，后台自动用 Flash 模型提取关键事实并存入 SQLite：

```text
用户：「我叫张三，是一名后端开发者」
→ 提取：name: 张三, user-role: 后端开发者
→ 下次对话 AI 会看到「## Known Facts」
```

- **私聊：** 记忆按 `chat_id` 隔离（每人独立）
- **群聊：** 记忆按 `open_id` 隔离（同群每人独立）

记忆通过 `/memory/{user_id}` API 也可手动管理。

## 自动记忆

Server 模式自动启用记忆系统。每次对话后，后台提取关键事实并存储：

```bash
# 第一次对话
curl ... -d '{"session_id":"user-123","prompt":"我是张三，用 Rust 写后端"}'

# 后续对话会自动记住
curl ... -d '{"session_id":"user-123","prompt":"你还记得我吗？"}'
# 回复会引用之前的信息
```

## 定时任务调度器

Server 启动时自动启动后台 scheduler（仅飞书模式下）：

- **轮询间隔：** 10 秒
- **到期触发：** 调用 `process_feishu_chat` 完整执行流程（LLM + 工具 + 回复）
- **循环任务：** 自动计算下次执行时间
- **一次性任务：** 执行后标记完成
- **会话压缩：** 每小时检查一次超长会话，自动摘要

## Prometheus 指标

```bash
curl localhost:9527/metrics
```

## 工作流示例

### 私聊：部署 + 定时

```
用户：帮我把新版本部署到服务器，然后每天凌晨 3 点检查一次健康状态
  → AI 执行部署命令（带 reply_to_user 报进度）
  → AI 调 schedule_task(cron="0 3 * * * *", task="检查服务健康状态")
  → AI 回复「已部署 v0.6.0，定时健康检查已安排」
```

### 群聊：多人协作

```
用户A @bot 帮我查一下磁盘使用情况
  → AI 执行 df -h，结果发到群里
  → 记忆按 A 的 open_id 存储（不影响群内其他人）

用户B @bot 你好
  → AI 回复，但看不到 A 的记忆
  → 群聊共享对话历史，但记忆隔离
```
