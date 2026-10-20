# 定时任务 & 消息队列设计

## 整体架构

```
┌─────────────────────────────────────────────────────────┐
│                     scheduler_loop()                    │
│                   每 10s tick 一次                       │
│              查询 scheduled_tasks 表                    │
└──────────┬──────────────────────────────────────────────┘
           │
           │ executor.execute(&task)   ── TaskExecutor trait
           ▼
┌──────────────────┐
│ FeishuExecutor   │  ← 飞书实现（当前唯一实现）
│ ChatQueue        │  ← 每 chat 串行队列
└──────────┬───────┘
           │
           ▼
┌──────────────────────────────────────┐
│ process_feishu_chat()               │
│  LLM → tool calls → LLM → 回复用户    │
└──────────────────────────────────────┘
```

## 定时任务（Scheduler）

### 核心文件

| 文件 | 职责 |
|------|------|
| `arcc-core/src/task/executor.rs` | `TaskExecutor` trait 定义 |
| `arcc-server/src/feishu/executor.rs` | `FeishuTaskExecutor` 实现 |
| `arcc-server/src/scheduler.rs` | 调度器主循环 |
| `arcc-storage/src/db/queries.rs` | SQLite 查询（`list_due_tasks` 等） |
| `arcc-storage/src/db/models.rs` | `ScheduledTask` 数据模型 |
| `arcc-core/src/tools.rs` | 工具定义（`schedule_task_definition`） |

### 调度器循环

```rust
// scheduler.rs — 每 10s tick 一次
async fn scheduler_loop(ctx, executor: Arc<dyn TaskExecutor>) {
    loop {
        tick.tick().await;
        
        let tasks = ctx.storage.list_due_tasks().await;
        // 查询: WHERE status='pending' AND next_run_at <= datetime('now','localtime')
        
        for task in tasks {
            // 1. 标记 running（防重复执行）
            ctx.storage.update_task_status(&task.id, "running");
            
            // 2. 通过 trait 执行（不感知具体实现）
            let ok = executor.execute(&task).await;
            
            if !ok {
                // 3. 失败 → 累计重试次数，最多 10 次后标记 failed
                retry_count[task.id] += 1;
                if retries >= 10 {
                    update_task_status("failed");
                } else {
                    update_task_status("pending"); // 下次 tick 重试
                }
                continue;
            }
            
            // 4. 成功 → 更新状态
            retry_count.remove(task.id);
            if task.cron.is_some() {
                // 循环任务 → 计算下次触发时间
                let next = cron::Schedule::from_str(cron).upcoming(Local).next();
                update_task_next_run(task.id, next);
            } else {
                // 一次性任务 → 标记 completed
                complete_task(task.id);
            }
        }
    }
}
```

### TaskExecutor trait

```rust
#[async_trait]
pub trait TaskExecutor: Send + Sync {
    /// 执行一条到期的定时任务。
    /// 返回 true 成功，false 失败（调度器会重试）。
    async fn execute(&self, task: &ScheduledTask) -> bool;
}
```

**设计原则：**
- 调度器只依赖 `TaskExecutor` trait，不感知具体消息渠道
- 每个渠道（飞书、Slack 等）各自实现此 trait
- 新增渠道只需加一个实现文件 + 在 `lib.rs` 里注册

### 定时任务工具定义

| 参数 | 类型 | 用途 |
|------|------|------|
| `cron` | string (可选) | 循环任务：标准 6 字段 cron 表达式 |
| `delay_seconds` | integer (可选) | 一次性任务：从现在起延迟秒数 |
| `task` | string (必填) | 任务描述，触发时作为 LLM 输入 |

**交互流程：**

```
用户发 "一分钟后提醒我喝水"

① AI 调 get_current_time → 返回 "2026-06-13 14:47:01"
② AI 算 delay_seconds = 60
③ AI 调 schedule_task(delay_seconds=60, task="提醒用户喝水")
   → INSERT scheduled_tasks (cron=NULL, next_run_at=now+60s)
   → 回复用户 "✅ 已设置"
④ 60 秒后调度器触发 → 走 FeishuExecutor → LLM 执行并提醒用户
```

### 循环任务示例

```
用户发 "每天早上8点重启nginx"

① AI 调 get_current_time → 确认当前时间
② AI 生成 cron = "0 8 * * *"（6 字段：秒 分 时 日 月 周）
③ AI 调 schedule_task(cron="0 8 * * *", task="重启nginx")
   → INSERT scheduled_tasks (cron="0 8 * * *", next_run_at=明天 08:00)
④ 调度器每天 08:00 触发 → LLM 执行重启 → 计算下次 next_run_at = 后天 08:00
```

## 消息队列（ChatQueue）

### 为什么需要

```
问题：多条消息并发处理同一个 Session
  消息A → tokio::spawn 任务A → 写 user → 等LLM(3s) → 写 asst
  消息B → tokio::spawn 任务B →  写 user → 等LLM → ...
                              ↑ 两个任务同时写 VecDeque → 交叉错乱
  
结果：VecDeque 中出现:
  [user, user, tool, tool, asst, asst, ...]
  ↑ tool 消息前面没有匹配的 asst(tool_calls) → DeepSeek API 400
```

### 设计方案

每个 chat（飞书会话）一个 **独立的 tokio mpsc channel + 单消费者**，按 FIFO 顺序逐条处理。

```
Chat A 第一条消息 → channel A → 消费者 A → 处理 msg1 → 处理 msg2 → 空闲120s退出
Chat A 第二条消息                           ↑ 排队等待

Chat B 第一条消息 → channel B → 消费者 B → 处理 msg1 → 空闲退出
                    ↑ 与 Chat A 完全隔离
```

### 核心文件

| 文件 | 职责 |
|------|------|
| `arcc-server/src/feishu/chat_queue.rs` | ChatQueue 实现 |
| `arcc-server/src/feishu/webhook.rs` | 用户消息走 ChatQueue.enqueue() |
| `arcc-server/src/feishu/executor.rs` | 调度器触发也走 ChatQueue.enqueue_scheduler() |

### 关键数据结构

```rust
// chat_queue.rs
enum QueueMsg {
    Event(ChatEvent),                    // 普通用户消息
    Scheduler(ChatEvent, oneshot::Sender<bool>),  // 调度器触发 + 结果通知
}

struct ChatEvent {
    chat_id: String,
    chat_type: String,
    open_id: String,
    message_id: String,
    user_text: String,
}

struct ChatQueue {
    consumers: Arc<Mutex<HashMap<String, UnboundedSender<QueueMsg>>>>,
}
```

### 消费者生命周期

```
1. 第一条消息 → 创建 channel + spawn 消费者
2. 消费者 process_one(first_event)
3. 循环 recv() + process_one()
4. 120 秒没新消息 → 超时退出 → 从 HashMap 清理
5. 新消息到来 → 创建新消费者（回到 1）
```

### 消息来源统一

**两种消息来源都走同一个 ChatQueue：**

```
webhook (用户发消息)     ─┐
                          ├─ ChatQueue.enqueue() → per-chat 消费者 → process_feishu_chat()
scheduler (定时触发)      ─┘
```

保证同一个 chat 的所有消息（无论来源）按顺序处理。

### 定时触发 prompt 设计

```
调度器发送的 user_text 格式：
  "[Scheduled task trigger] 提醒用户去上厕所"

系统提示词 (server.md) 规则 #7：
  看到 [Scheduled task trigger] 开头 → 立即执行任务并用
  reply_to_user 通知用户，禁止调 list_scheduled_tasks、
  schedule_task 等工具
```

## Session 管理

### 延迟持久化

```
改前：用户输入 → ensure_session() → INSERT 空 session → 调 LLM
                                        ↑ 如果断网/退出 → 孤儿 session

改后：用户输入 → 内存 VecDeque → 调 LLM → 收到第一条回复
                                          → enable_persistence() → INSERT session + 消息
                                          ↑ 有内容才落库，永不孤儿
```

### 消息序列清洗

```rust
// sanitize_history() — 加载 session 历史时过滤非法序列
// 场景: asst(tool_calls) 后面没有足够的 tool 消息
// 处理: 去掉 asst 上的 tool_calls，变成纯文本
// 
// 场景: tool 消息前面没有匹配的 asst(tool_calls)
// 处理: 删除孤立 tool 消息
```

## 配置参考

```toml
# config.toml
[execution]
command_timeout_seconds = 30    # 命令执行超时
max_output_bytes = 4096         # 命令输出截断大小

[safety]
require_human_confirm = ["rm", "mv", "dd", "mkfs", "shutdown", "reboot", "fdisk"]
```
