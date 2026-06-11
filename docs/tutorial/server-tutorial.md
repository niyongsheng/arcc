# Server 模式教程

HTTP 后台服务，支持 API 调用和飞书机器人集成。

```bash
# 前台运行
arcc server

# 后台守护进程
arcc server --daemon
```

## 配置

在 `~/.arcc/config.toml` 中：

```toml
[server]
host = "127.0.0.1"
port = 9527

[feishu]
enabled = false
app_id = "cli_xxxxxxxxxxxx"
app_secret = "xxxxxxxxxxxxxxxxxxxxxxxxxxxx"
verification_token = "xxxxxxxxxxxx"
```

## API 端点

### POST /chat — AI 对话

```bash
curl -X POST localhost:9527/chat \
  -H "Content-Type: application/json" \
  -d '{"session_id":"user-123","prompt":"用 Python 写一个快速排序"}'
```

返回 SSE 流：

```
data: 当然，下面是快速排序的实现
data: ：

event: reasoning
data: 用户需要快速排序实现...

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
# {"status":"ok","version":"0.1.0"}
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

## 自动记忆

Server 模式自动启用记忆系统。每次对话后，后台提取关键事实并存储：

```bash
# 第一次对话
curl ... -d '{"session_id":"user-123","prompt":"我是张三，用 Rust 写后端"}'

# 后续对话会自动记住
curl ... -d '{"session_id":"user-123","prompt":"你还记得我吗？"}'
# 回复会引用之前的信息
```

## Prometheus 指标

```bash
curl localhost:9527/metrics
```
