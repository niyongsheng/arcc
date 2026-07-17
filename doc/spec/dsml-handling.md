# DSML 标签处理：DeepSeek-V4 Thinking Mode

## 概述

DSML（DeepSeek Markup Language）是 DeepSeek 模型在思考模式（thinking mode）下用于表达工具调用的标记语言。当 `thinking_mode = enabled` 时，模型可能在 `reasoning_content`（思考链）和 `content`（最终输出）中同时或分别嵌入 DSML 格式的工具调用。

ARCC 需要在流式处理中完成两件事：
1. **提取** DSML 工具调用，交给执行引擎
2. **剥离** DSML 标签，避免用户看到原始标记

## DSML 标签格式

DeepSeek-V4 使用 **双 U+FF5C**（FULLWIDTH VERTICAL LINE）作为标签定界符：

```
<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}tool_calls>
  <\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}invoke name="execute_command">
    <\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}parameter name="command" string="true">ls -la</\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}parameter>
  </\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}invoke>
</\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}tool_calls>
```

**注意事项：** 早期版本的 DeepSeek API 使用单 U+FF5C 定界符，V4 版本已升级为双 U+FF5C。如果版本升级，需要同步更新标签常量。

相关代码：`crates/arcc-core/src/model/dsml.rs`

```rust
const TAG_TOOL_CALLS_OPEN: &str =
    "<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}tool_calls>";
const TAG_TOOL_CALLS_CLOSE: &str =
    "</\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}tool_calls>";
const TAG_INVOKE_PREFIX: &str =
    "<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}invoke";
const TAG_INVOKE_CLOSE: &str =
    "</\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}invoke>";
const TAG_PARAM_PREFIX: &str =
    "<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}parameter";
const TAG_PARAM_CLOSE: &str =
    "</\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}parameter>";
```

## 流式处理的挑战

### 1. 双通道：content 与 reasoning_content

流式 SSE 响应中，模型输出可能分布在两个通道：

```json
// 通道 A：reasoning_content（模型思考过程）
{"choices":[{"delta":{"reasoning_content":"...可能包含 DSML..."}}]}

// 通道 B：content（最终回复内容）
{"choices":[{"delta":{"content":"...也可能包含 DSML..."}}]}
```

两路都需要独立的 DSML 累加器，互不干扰。

**对应代码：** `crates/arcc-core/src/model/deepseek.rs`

```rust
let mut dsml_acc = dsml::DsmlAccumulator::default();
let mut reasoning_dsml_acc = dsml::DsmlAccumulator::default();

// reasoning_content 清洗
if let Some(reasoning) = delta.reasoning_content {
    let (clean_reasoning, _) = reasoning_dsml_acc.ingest(&reasoning);
    // 发送 clean_reasoning 作为 Reasoning 块
}

// content 清洗 + 工具调用提取
if let Some(content) = delta.content {
    let (clean_content, dsml_tcs) = dsml_acc.ingest(&content);
    // 发送 clean_content 作为 Content 块
    // dsml_tcs 作为 ToolCallStart 块
}
```

### 2. 逐字符流式到达

DeepSeek 流式接口可能将 DSML 标签拆分为极小的 SSE delta 逐个发送。一个 `<\u{FF5C}\u{FF5C}DSML...>` 标签可能分为 6-7 个独立 chunk 到达：

```python
# 实际观测到的流式拆分
chunks = [
    "<",                          # 1 byte
    "\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}",  # 17 bytes
    "tool",                       # 4 bytes
    "_c",                         # 2 bytes
    "alls",                       # 4 bytes
    ">",                          # 1 byte
]
```

### 3. finish_reason 与 content 同帧

工具调用完成的标志 `finish_reason: "tool_calls"` 可能与 DSML 关闭标签在同一 SSE chunk 中到达：

```json
{"choices":[{"delta":{"content":"</\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}tool_calls>"},
  "finish_reason":"tool_calls"}]}
```

处理顺序至关重要：**必须先处理 content（喂入 DSML 累加器），再处理 finish_reason（flush 累加器）**。如果顺序颠倒，DSML 关闭标签将丢失，累加器在 flush 时吐出未闭合的原始 DSML。

## 已修复的 Bug

### Bug 1：双 U+FF5C 写成了单 U+FF5C（dsml.rs）

原始代码使用单 `\u{FF5C}` 定界符，但 DeepSeek-V4 API 实际输出的是双 `\u{FF5C}\u{FF5C}`。累加器从未匹配到任何 DSML 标签，所有 DSML 内容作为普通文本透传。

### Bug 2：finish_reason 跳过 content 处理（deepseek.rs）

原始的 `continue` 语句在 finish_reason 分支内，导致同一 SSE chunk 的 content 永远到不了 DSML 累加器。当 DSML 关闭标签和 finish_reason 在同一 chunk 时，标签未被消化，flush 吐出未闭合的原始 DSML。

修复：将 content 处理移到 finish_reason 检查之前。

### Bug 3：longest_partial_tag_suffix 的 off-by-one（dsml.rs）

```diff
- for n in (1..tag.len().min(buf.len())).rev() {
+ for n in (1..=tag.len().min(buf.len())).rev() {
```

`1..len` 是半开区间，不包括 `len`。当缓冲区长度恰好等于标签前缀长度（例如缓冲区只有 `"<"` 时），循环体一次都不执行——因为 `1..1` 是空区间。函数返回 0，累加器认为 `"<"` 不是 DSML 标签开头，直接当作普通内容吐出。

**效果：** 每个 DSML 标签的第一个字符都在这里泄漏，显示了原始标记的全部文本。

### Bug 4：用户输入重复导致 API 400（app.rs）

在为用户输入添加 SQLite 持久化时，将用户消息推入 session，但 `prepare_for_request()` 已从 session 返回该消息，随后又在 `initial_messages` 中重复追加。API 收到 `[system, user, user]` 双用户消息，破坏了消息交替顺序，触发 "insufficient tool messages following tool_calls message" 错误。

## DSML 累加器架构

`DsmlAccumulator` 是一个状态机，设计用于流式内容：

```
状态流转：
  in_dsml = false
    → 收到 content delta
    → 缓存，检查是否匹配 TAG_TOOL_CALLS_OPEN
    → 不匹配：用 longest_partial_tag_suffix 判断是否可能为标签前缀
      → 是前缀：继续缓存，不吐出内容
      → 非前缀：吐出安全部分，丢弃非匹配部分
    → 匹配：in_dsml = true

  in_dsml = true
    → 收到后续 content delta
    → 缓存，检查是否匹配 TAG_TOOL_CALLS_CLOSE
    → 不匹配：继续缓存
    → 匹配：提取工具调用，吐出标签前后的干净内容，in_dsml = false
```

`flush()` 方法在流结束时调用。如果仍有未闭合的 DSML 块，缓冲区内容作为普通文本返回。

## 关键代码位置

| 模块 | 文件 | 职责 |
|------|------|------|
| DSML 标签常量 | `crates/arcc-core/src/model/dsml.rs:29-34` | 定义所有 DSML 标签模式 |
| 非流式解析器 | `crates/arcc-core/src/model/dsml.rs:43-80` | 用于完整文本的 DSML 提取 |
| 流式累加器 | `crates/arcc-core/src/model/dsml.rs:238-360` | 状态机，逐 delta 处理 |
| 部分标签匹配 | `crates/arcc-core/src/model/dsml.rs:202-214` | 防跨 chunk 标签拆分 |
| 流式 content 处理 | `crates/arcc-core/src/model/deepseek.rs:585-605` | content 通道的 DSML 清洗 |
| 流式 reasoning 处理 | `crates/arcc-core/src/model/deepseek.rs:569-579` | reasoning 通道的 DSML 清洗 |
| finish_reason 处理 | `crates/arcc-core/src/model/deepseek.rs:617-631` | 工具调用完成后的累加器 flush |

## 测试覆盖

`crates/arcc-core/src/model/dsml.rs` 中的测试用例：

| 测试 | 覆盖场景 |
|------|---------|
| `no_dsml_passthrough` | 无 DSML 的纯文本不触发解析 |
| `single_tool_call` | 完整 DSML 块提取单个工具调用 |
| `multiple_invokes` | 同一块内多个工具调用 |
| `integer_param_not_string` | 非字符串参数类型解析 |
| `boolean_param` | 布尔参数类型解析 |
| `malformed_no_close_left_as_text` | 不完整 DSML 块保留为文本 |
| `already_has_native_tool_calls` | 同时存在原生 JSON 和 DSML 工具调用 |
| `accumulator_no_dsml` | 流式模式：无 DSML 内容直接透传 |
| `accumulator_dsml_in_one_chunk` | 流式模式：DSML 块单 chunk 到达 |
| `accumulator_dsml_split_across_chunks` | 流式模式：DSML 块跨多个 chunk |
| `accumulator_incomplete_dsml_flushed_as_text` | 流式模式：不完整 DSML flush 为文本 |
| `debug_longest_partial_tag_suffix` | 部分标签匹配函数的准确性验证 |
| `partial_tag_held_across_many_small_deltas` | 逐字符流式拆分的正确 hold（Bug 3 专用） |
| `partial_tag_in_chunks_with_leading_text` | 前导文本 + 逐 chunk DSML 标签 |

## Debug 方法

当 DSML 显示异常时，在 `deepseek.rs` 的 content 处理路径中添加：

```rust
debug!(
    raw_len = content.len(),
    raw_first = %content.chars().take(8).collect::<String>(),
    clean = clean_content.as_deref().unwrap_or("(none)"),
    dsml_count = dsml_tcs.len(),
    "dsml accumulator ingested content"
);
```

日志输出示例（正常）：
```
dsml_acc flush clean (no remnant)
dsml accumulator ingested content raw_len=6 raw_first=好的 clean="好的" dsml_count=0
dsml accumulator ingested content raw_len=1 raw_first=< clean="(none)" dsml_count=0
dsml accumulator ingested content raw_len=17 raw_first=  clean="(none)" dsml_count=0
dsml accumulator ingested content raw_len=1 raw_first=> clean="(none)" dsml_count=0
clean 为 "(none)" 表示 DSML 已被正确 hold 在缓冲中
```

日志输出示例（异常——off-by-one bug 时的泄漏）：
```
dsml accumulator ingested content raw_len=1 raw_first=< clean="<" dsml_count=0
                                                                  ^^ DSML 标签标签泄漏为内容
```
