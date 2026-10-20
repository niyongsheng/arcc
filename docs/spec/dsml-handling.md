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

**注意事项：** 定界符每侧可能是 **1 个或 2 个** U+FF5C，且实际流式输出里两侧数量不一定对称。V4 官方规范是单 `｜`，但线上 SSE 常见双 `｜｜`。解析层不能只匹配一种固定字符串。

实测还出现过以下**变形（mangled）**形式，解析层必须全部兼容：

```text
<｜｜DSML｜｜tool_calls>                  # 双 U+FF5C
<｜｜DSML｜｜ calls>                       # 双 U+FF5C + 空格 + 裸 calls（2026-10 线上实测）
<｜｜DSML｜｜ invoke name="...">          # 标记后带空格
</｜｜DSML｜｜ parameter>                  # 闭合标签同样带空格
<｜DSML｜tool_calls>                      # 单 U+FF5C（官方规范）
<｜｜DSML｜｜function_calls>               # V3.2 外层名
```

相关代码：`crates/arcc-core/src/model/dsml.rs`

```rust
/// 外层 tool-call 块可接受的标签名
const TOOL_CALL_TAGS: [&str; 3] = ["tool_calls", "calls", "function_calls"];

/// 每侧 1~2 个 U+FF5C 的标记形式，长形式在前
const MARKER_FORMS: [&str; 4] = [
    "\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}",
    "\u{FF5C}\u{FF5C}DSML\u{FF5C}",
    "\u{FF5C}DSML\u{FF5C}\u{FF5C}",
    "\u{FF5C}DSML\u{FF5C}",
];
```

标签匹配由 `match_open_tag_at` / `match_close_tag_at` 完成：跳过标记后允许任意空白，再匹配标签名；因此 `<｜｜DSML｜｜ calls>`、`<｜｜DSML｜｜ invoke ...>` 与规范形式走同一条解析路径。

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

### Bug 5：解析层只匹配规范标签，漏掉线上 mangled 形式（dsml.rs）

2026-10-20 的 TUI 会话（`1cc24686-877c-46b8-ad81-ca0c11e8a71a`）中，DeepSeek 实际输出的标签是：

```text
<｜｜DSML｜｜ calls>
<｜｜DSML｜｜ invoke name="execute_command">
<｜｜DSML｜｜ parameter name="command" string="true">open -a "/Applications/Doubao.app"</｜｜DSML｜｜ parameter>
</｜｜DSML｜｜ invoke>
</｜｜DSML｜｜ calls>
```

旧实现用固定常量 `<｜｜DSML｜｜tool_calls>` 做 `str::find`，与实际的 **空格 + 裸 `calls`** 不匹配：

1. 累加器在收到 `" calls"` 时判定 `<｜｜DSML｜｜ calls` 不是标签前缀，把整段原始标签当作正文吐出；
2. 由于始终没有进入 `in_dsml`，工具调用也不会被提取，命令根本没执行（审计日志无记录）；
3. DSML 文本最终被持久化进 `messages` 表，用户看到原始标记。

修复：改为结构化标签匹配器（`MARKER_FORMS` + `TOOL_CALL_TAGS` + 可选空白），流式部分用 `could_be_open_tag_prefix` 判断是否继续 hold，覆盖 1/2 个 U+FF5C、空格、`calls` / `tool_calls` / `function_calls` 等变体。

## DSML 累加器架构

`DsmlAccumulator` 是一个状态机，设计用于流式内容：

```
状态流转：
  in_dsml = false
    → 收到 content delta
    → 缓存，用 find_open_tag 检查是否出现 tool-call 起始标签
    → 未出现：用 longest_partial_open_tag_suffix / could_be_open_tag_prefix
      判断尾部是否可能为标签前缀
      → 是前缀：继续缓存，不吐出内容
      → 非前缀：吐出安全部分
    → 出现：in_dsml = true

  in_dsml = true
    → 收到后续 content delta
    → 缓存，用 find_close_tag 检查是否出现外层闭合标签
    → 未出现：继续缓存
    → 出现：extract_tool_calls 提取工具调用，吐出标签前后的干净内容，
      in_dsml = false
```

`flush()` 方法在流结束时调用。如果仍有未闭合的 DSML 块，缓冲区内容作为普通文本返回。

## 关键代码位置

| 模块 | 文件 | 职责 |
|------|------|------|
| DSML 标记/标签常量 | `crates/arcc-core/src/model/dsml.rs:51-64` | `MARKER_FORMS` + `TOOL_CALL_TAGS` |
| 标签匹配器 | `crates/arcc-core/src/model/dsml.rs:79-157` | `match_open_tag_at` / `match_close_tag_at` / `find_*_tag` |
| 非流式解析器 | `crates/arcc-core/src/model/dsml.rs:193-230` | `extract_tool_calls` |
| 参数/invoke 解析 | `crates/arcc-core/src/model/dsml.rs:234-305` | `parse_invoke_blocks` / `parse_parameters` |
| 部分标签匹配 | `crates/arcc-core/src/model/dsml.rs:160-190,338` | `could_be_open_tag_prefix` / `longest_partial_open_tag_suffix` |
| 流式累加器 | `crates/arcc-core/src/model/dsml.rs:363-455` | 状态机，逐 delta 处理 |
| 流式 content 处理 | `crates/arcc-core/src/model/deepseek.rs:646-668` | content 通道的 DSML 清洗 |
| 流式 reasoning 处理 | `crates/arcc-core/src/model/deepseek.rs:627-637` | reasoning 通道的 DSML 清洗 |
| finish_reason 处理 | `crates/arcc-core/src/model/deepseek.rs:691-724` | 工具调用完成后的累加器 flush |

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
| `mangled_v4_live_format` | `<｜｜DSML｜｜ calls>` + 空格标签（Bug 5 线上原文） |
| `mangled_v4_streaming_split_chunks` | 按 tui.log 实测 chunk 序列回放（Bug 5 专用） |
| `canonical_single_bar_format` | 官方单 `｜` 规范形式 |
| `function_calls_wrapper` | V3.2 `function_calls` 外层名 |
| `less_than_in_text_not_held` | 普通文本中的 `<` 不被误 hold |

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
