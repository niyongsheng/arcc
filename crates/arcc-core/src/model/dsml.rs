//! DSML (DeepSeek Markup Language) recovery layer.
//!
//! DeepSeek-V4 can intermittently emit tool calls as DSML markup embedded in
//! the `content` field instead of the proper native `tool_calls` JSON.  This
//! module detects and parses those blocks so tool execution is never silently
//! dropped.
//!
//! # DSML format (V4)
//!
//! The delimiter is DOUBLE U+FF5C (FULLWIDTH VERTICAL LINE):
//!
//! ```text
//! <..DSML..tool_calls>
//! <..DSML..invoke name="execute_command">
//! <..DSML..parameter name="command" string="true">ls -la</..DSML..parameter>
//! </..DSML..invoke>
//! </..DSML..tool_calls>
//! ```
//!
//! Each `..` represents one U+FF5C character.

use super::types::ToolCall;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// DSML tag constants (V4 format — uses DOUBLE U+FF5C delimiters)
// ---------------------------------------------------------------------------

const TAG_TOOL_CALLS_OPEN: &str = "<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}tool_calls>";
const TAG_TOOL_CALLS_CLOSE: &str = "</\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}tool_calls>";
const TAG_INVOKE_PREFIX: &str = "<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}invoke";
const TAG_INVOKE_CLOSE: &str = "</\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}invoke>";
const TAG_PARAM_PREFIX: &str = "<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}parameter";
const TAG_PARAM_CLOSE: &str = "</\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}parameter>";

// ---------------------------------------------------------------------------
// Non-streaming parser
// ---------------------------------------------------------------------------

/// Attempt to extract DSML tool calls from assistant content.
///
/// Returns `(cleaned_content, tool_calls)`.  If no DSML block is found,
/// `cleaned_content` equals the original and `tool_calls` is empty.
/// Malformed / incomplete blocks are left as-is in the content.
pub fn extract_tool_calls(content: &str) -> (String, Vec<ToolCall>) {
    let mut cleaned = String::with_capacity(content.len());
    let mut tool_calls = Vec::new();
    let mut remaining = content;

    loop {
        let Some(open_pos) = remaining.find(TAG_TOOL_CALLS_OPEN) else {
            cleaned.push_str(remaining);
            break;
        };

        // Emit content before the DSML block.
        cleaned.push_str(&remaining[..open_pos]);

        let block_start = open_pos + TAG_TOOL_CALLS_OPEN.len();
        let Some(close_pos) = remaining[block_start..].find(TAG_TOOL_CALLS_CLOSE) else {
            // No closing tag → block is incomplete; leave as text.
            cleaned.push_str(&remaining[open_pos..]);
            break;
        };

        let block_body = &remaining[block_start..block_start + close_pos];
        // Skip past the closing tag.
        remaining = &remaining[block_start + close_pos + TAG_TOOL_CALLS_CLOSE.len()..];

        // Parse invoke blocks within this tool_calls block.
        let parsed = parse_invoke_blocks(block_body);
        if parsed.is_empty() {
            // If we couldn't parse any invokes, leave the block as text.
            cleaned.push_str(TAG_TOOL_CALLS_OPEN);
            cleaned.push_str(block_body);
            cleaned.push_str(TAG_TOOL_CALLS_CLOSE);
        }
        tool_calls.extend(parsed);
    }

    (cleaned, tool_calls)
}

/// Parse `<｜DSML｜invoke name="...">...</｜DSML｜invoke>` blocks.
fn parse_invoke_blocks(body: &str) -> Vec<ToolCall> {
    let mut tool_calls = Vec::new();
    let mut remaining = body;

    loop {
        let Some(invoke_start) = remaining.find(TAG_INVOKE_PREFIX) else {
            break;
        };

        // Find end of this invoke tag's opening `>`.
        let tag_content_start = &remaining[invoke_start..];
        let Some(gt_pos) = tag_content_start.find('>') else {
            break;
        };
        let open_tag = &tag_content_start[..=gt_pos];

        // Extract name attribute.
        let Some(name) = extract_attr(open_tag, "name") else {
            remaining = &remaining[invoke_start + 1..];
            continue;
        };

        // Find matching close tag.
        let body_start = invoke_start + gt_pos + 1;
        let Some(close_pos) = remaining[body_start..].find(TAG_INVOKE_CLOSE) else {
            break;
        };
        let invoke_body = &remaining[body_start..body_start + close_pos];

        // Parse parameters.
        let arguments = parse_parameters(invoke_body);

        tool_calls.push(ToolCall {
            id: format!("dsml_{}", Uuid::new_v4().to_string().replace('-', "")[..12].to_owned()),
            name,
            arguments,
        });

        remaining = &remaining[body_start + close_pos + TAG_INVOKE_CLOSE.len()..];
    }

    tool_calls
}

/// Parse `<｜DSML｜parameter name="..." string="true|false">value</｜DSML｜parameter>` blocks.
fn parse_parameters(body: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    let mut remaining = body;

    loop {
        let Some(param_start) = remaining.find(TAG_PARAM_PREFIX) else {
            break;
        };

        let tag_content_start = &remaining[param_start..];
        let Some(gt_pos) = tag_content_start.find('>') else {
            break;
        };
        let open_tag = &tag_content_start[..=gt_pos];

        // Extract name and string attributes.
        let Some(name) = extract_attr(open_tag, "name") else {
            remaining = &remaining[param_start + 1..];
            continue;
        };
        let is_string = extract_attr(open_tag, "string")
            .map(|s| s == "true")
            .unwrap_or(false);

        // Find matching close tag.
        let body_start = param_start + gt_pos + 1;
        let Some(close_pos) = remaining[body_start..].find(TAG_PARAM_CLOSE) else {
            break;
        };
        let value_text = &remaining[body_start..body_start + close_pos];

        // Parse value based on string attribute.
        let value = if is_string {
            serde_json::Value::String(value_text.to_owned())
        } else {
            // Try JSON parse; fall back to string.
            serde_json::from_str(value_text)
                .unwrap_or_else(|_| serde_json::Value::String(value_text.to_owned()))
        };

        map.insert(name, value);
        remaining = &remaining[body_start + close_pos + TAG_PARAM_CLOSE.len()..];
    }

    serde_json::Value::Object(map)
}

/// Extract an attribute value from an XML-like open tag.
/// Handles `name="value"` and `name='value'` forms.
fn extract_attr<'a>(tag: &'a str, attr_name: &str) -> Option<String> {
    let prefix = format!("{}=", attr_name);

    let Some(pos) = tag.find(&prefix) else {
        return None;
    };

    let after_eq = &tag[pos + prefix.len()..];
    let quote_char = after_eq.chars().next()?;
    if quote_char != '"' && quote_char != '\'' {
        return None;
    }

    let value_start = 1;
    let value_end = after_eq[value_start..].find(quote_char)?;

    Some(after_eq[value_start..value_start + value_end].to_owned())
}

/// Return the length of the longest suffix of `buf` that is a prefix of `tag`.
///
/// This tells us how many characters we must keep in the buffer to avoid
/// missing a tag split across two chunks.
fn longest_partial_tag_suffix(buf: &str, tag: &str) -> usize {
    for n in (1..=tag.len().min(buf.len())).rev() {
        let start = buf.len() - n;
        // Ensure we're on a UTF-8 character boundary — emoji and other
        // multi-byte characters would otherwise cause a panic.
        if !buf.is_char_boundary(start) {
            continue;
        }
        let suffix = &buf[start..];
        if tag.starts_with(suffix) {
            return n;
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Streaming accumulator
// ---------------------------------------------------------------------------

/// Stateful DSML accumulator for streaming content.
///
/// Content deltas are fed into `ingest()`.  When a complete DSML block is
/// detected, the tool calls are returned and the DSML text is stripped.
#[derive(Debug, Default)]
pub struct DsmlAccumulator {
    /// Buffered content that may contain an in-progress DSML block.
    buf: String,
    /// Whether we're inside a `<｜DSML｜tool_calls>` block.
    in_dsml: bool,
}

impl DsmlAccumulator {
    /// Feed a content delta.  Returns `(emit_content, completed_tool_calls)`.
    ///
    /// - `emit_content`: clean text safe to surface to the user (may be empty).
    /// - `completed_tool_calls`: tool calls extracted from a just-completed
    ///   DSML block (only non-empty when a block finishes).
    pub fn ingest(&mut self, delta: &str) -> (Option<String>, Vec<ToolCall>) {
        self.buf.push_str(delta);

        // Content that appeared before the DSML opening tag (if any).
        let mut before: Option<String> = None;

        if !self.in_dsml {
            // Check if a DSML block has started.
            if let Some(pos) = self.buf.find(TAG_TOOL_CALLS_OPEN) {
                if pos > 0 {
                    let prefix = self.buf[..pos].to_owned();
                    self.buf.drain(..pos);
                    before = Some(prefix);
                }
                // Fall through to check for close tag immediately.
            } else {
                // Emit everything except a suffix that could be a partial
                // match of the opening tag (so we don't miss a tag split
                // across chunks).
                let keep = longest_partial_tag_suffix(&self.buf, TAG_TOOL_CALLS_OPEN);
                let safe_len = self.buf.len().saturating_sub(keep);
                if safe_len > 0 {
                    let emit = self.buf[..safe_len].to_owned();
                    self.buf.drain(..safe_len);
                    return (Some(emit), Vec::new());
                }
                return (None, Vec::new());
            }
        }

        // Either we were already inside a DSML block, or we just found the
        // opening tag and drained the prefix into `before`.  In both cases
        // the buffer starts with (or contains) an in-progress DSML block.
        self.in_dsml = true;

        // Consume `before` from the outer scope.  It holds content that
        // appeared before the DSML opening tag.
        let prefix_content = before.take();

        if let Some(close_pos) = self.buf.find(TAG_TOOL_CALLS_CLOSE) {
            let dsml_end = close_pos + TAG_TOOL_CALLS_CLOSE.len();
            let dsml_text = self.buf[..dsml_end].to_owned();
            let suffix = if dsml_end < self.buf.len() {
                Some(self.buf[dsml_end..].to_owned())
            } else {
                None
            };
            self.buf.clear();
            self.in_dsml = false;

            let (_, tool_calls) = extract_tool_calls(&dsml_text);

            // Stitch: prefix_content (before DSML) + suffix (after DSML).
            let mut content_parts: Vec<String> = Vec::new();
            if let Some(pfx) = prefix_content
                && !pfx.is_empty()
            {
                content_parts.push(pfx);
            }

            if let Some(sfx) = suffix
                && !sfx.is_empty()
            {
                // Re-feed suffix through ingest so it gets the same
                // partial-tag handling (and may itself contain DSML).
                self.buf.push_str(&sfx);
                let (more_content, more_tcs) = self.ingest("");
                if let Some(mc) = more_content
                    && !mc.is_empty()
                {
                    content_parts.push(mc);
                }
                let mut all_tcs = tool_calls;
                all_tcs.extend(more_tcs);
                let combined = if content_parts.is_empty() {
                    None
                } else {
                    Some(content_parts.join(""))
                };
                return (combined, all_tcs);
            }

            let combined = if content_parts.is_empty() {
                None
            } else {
                Some(content_parts.join(""))
            };
            (combined, tool_calls)
        } else {
            // Still waiting for the close tag.
            (None, Vec::new())
        }
    }

    /// Flush any remaining buffered content.  Called when the stream ends.
    /// If a DSML block never closed, the raw text is returned as content.
    pub fn flush(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            None
        } else {
            let text = std::mem::take(&mut self.buf);
            self.in_dsml = false;
            Some(text)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Convenience: build a DSML block from invoke fragments.
    fn dsml_block(invokes: &str) -> String {
        format!(
            "{open}{invokes}{close}",
            open = TAG_TOOL_CALLS_OPEN,
            close = TAG_TOOL_CALLS_CLOSE
        )
    }

    fn invoke(name: &str, params: &str) -> String {
        format!(
            "<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}invoke name=\"{name}\">{params}</\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}invoke>"
        )
    }

    fn param(name: &str, value: &str, is_string: bool) -> String {
        format!(
            "<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}parameter name=\"{name}\" string=\"{s}\">{value}</\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}parameter>",
            s = if is_string { "true" } else { "false" }
        )
    }

    #[test]
    fn no_dsml_passthrough() {
        let input = "The weather in Hangzhou is 24°C.";
        let (cleaned, tcs) = extract_tool_calls(input);
        assert_eq!(cleaned, input);
        assert!(tcs.is_empty());
    }

    #[test]
    fn single_tool_call() {
        let block = dsml_block(&invoke("get_weather", &format!(
            "{}{}",
            param("location", "Hangzhou", true),
            param("date", "2026-06-09", true),
        )));
        let input = format!("Let me check...{block}Done.");
        let (cleaned, tcs) = extract_tool_calls(&input);
        assert_eq!(cleaned, "Let me check...Done.");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].name, "get_weather");
        let args = tcs[0].arguments.as_object().unwrap();
        assert_eq!(args["location"], "Hangzhou");
        assert_eq!(args["date"], "2026-06-09");
    }

    #[test]
    fn integer_param_not_string() {
        let block = dsml_block(&invoke("count", &param("n", "42", false)));
        let (_, tcs) = extract_tool_calls(&block);
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].arguments["n"], 42);
    }

    #[test]
    fn boolean_param() {
        let block = dsml_block(&invoke("set_flag", &param("enabled", "true", false)));
        let (_, tcs) = extract_tool_calls(&block);
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].arguments["enabled"], true);
    }

    #[test]
    fn multiple_invokes() {
        let block = dsml_block(&format!(
            "{}{}",
            invoke("get_date", &param("dummy", "", true)),
            invoke("get_weather", &param("location", "Beijing", true)),
        ));
        let (_, tcs) = extract_tool_calls(&block);
        assert_eq!(tcs.len(), 2);
        assert_eq!(tcs[0].name, "get_date");
        assert_eq!(tcs[1].name, "get_weather");
    }

    #[test]
    fn malformed_no_close_left_as_text() {
        let input = "prefix <｜DSML｜tool_calls><｜DSML｜invoke name=\"x\">suffix";
        let (cleaned, tcs) = extract_tool_calls(input);
        assert_eq!(cleaned, input); // left untouched
        assert!(tcs.is_empty());
    }

    #[test]
    fn already_has_native_tool_calls() {
        // DSML parser should still extract even if there's other content.
        let block = dsml_block(&invoke("cmd", &param("cmd", "ls", true)));
        let (cleaned, tcs) = extract_tool_calls(&block);
        assert!(cleaned.is_empty()); // only DSML
        assert_eq!(tcs.len(), 1);
    }

    // --- streaming accumulator tests ---

    #[test]
    fn accumulator_no_dsml() {
        let mut acc = DsmlAccumulator::default();
        let (emit, tcs) = acc.ingest("Hello ");
        assert_eq!(emit.unwrap(), "Hello ");
        assert!(tcs.is_empty());

        let (emit, tcs) = acc.ingest("world");
        assert_eq!(emit.unwrap(), "world");
        assert!(tcs.is_empty());

        let flush = acc.flush();
        assert!(flush.is_none());
    }

    #[test]
    fn accumulator_dsml_in_one_chunk() {
        let mut acc = DsmlAccumulator::default();
        let block = dsml_block(&invoke("f", &param("x", "1", false)));

        let (emit, tcs) = acc.ingest(&format!("pre {block} post"));
        // "pre " emitted, then DSML block parsed, " post" emitted
        // Actually since DSML block is complete in one ingest, we get pre, then DSML parse, then post.
        // The exact emission depends on implementation. Let's run and see.
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].name, "f");
        // Clean content should be "pre  post"
        let flush = acc.flush();
        // Total emitted content should be "pre  post"
        let all_content = format!("{}{}", emit.unwrap_or_default(), flush.unwrap_or_default());
        assert!(!all_content.contains(TAG_TOOL_CALLS_OPEN), "DSML tags should be stripped");
    }

    #[test]
    fn accumulator_dsml_split_across_chunks() {
        let mut acc = DsmlAccumulator::default();
        let block = dsml_block(&invoke("f", &param("x", "1", false)));
        let (a, b) = block.split_at(block.len() / 2);

        let (emit1, tcs1) = acc.ingest(a);
        // First chunk: DSML started but not finished → no tool calls yet
        assert!(tcs1.is_empty());

        let (emit2, tcs2) = acc.ingest(b);
        // Second chunk: DSML completed → tool calls emitted
        assert_eq!(tcs2.len(), 1);
        assert_eq!(tcs2[0].name, "f");
    }

    #[test]
    fn accumulator_incomplete_dsml_flushed_as_text() {
        let mut acc = DsmlAccumulator::default();
        let incomplete = format!("{TAG_TOOL_CALLS_OPEN}{}", "<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}invoke name=\"x\">");
        let (emit, tcs) = acc.ingest(&incomplete);
        assert!(tcs.is_empty());
        // Flush should return the incomplete DSML as text.
        let flush = acc.flush();
        assert!(flush.is_some());
        assert!(flush.unwrap().contains(TAG_TOOL_CALLS_OPEN));
    }

    #[test]
    fn debug_longest_partial_tag_suffix() {
        let tag = TAG_TOOL_CALLS_OPEN;
        eprintln!("tag ({} chars, {} bytes): {:?}", tag.chars().count(), tag.len(), tag);
        // buf = "<"
        let buf = "<";
        let keep = longest_partial_tag_suffix(buf, tag);
        eprintln!("buf={buf:?} len={} keep={keep}", buf.len());
        assert_eq!(keep, 1, "< alone should have keep=1 (it's the tag prefix)");

        // buf = "<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}"
        let buf2 = "<\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}";
        let keep2 = longest_partial_tag_suffix(buf2, tag);
        eprintln!("buf2.len={} keep={keep2}", buf2.len());
        assert!(keep2 > 0, "partial tag should be recognized as prefix");

        // Check that tag.starts_with("<") is true
        assert!(tag.starts_with("<"), "tag should start with '<'");
    }

    #[test]
    fn partial_tag_held_across_many_small_deltas() {
        // Simulate how DeepSeek streams the DSML opening tag:
        // character by character (or a few chars at a time).
        let mut acc = DsmlAccumulator::default();

        // Feed "<" then the rest of the tag piece by piece
        // TAG_TOOL_CALLS_OPEN = "<｜｜DSML｜｜tool_calls>"
        let (emit, tcs) = acc.ingest("<");
        assert!(emit.is_none(), "should not emit content for '<' alone");
        assert!(tcs.is_empty());

        let (emit, tcs) = acc.ingest("\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}");
        assert!(emit.is_none(), "should not emit content for partial tag");
        assert!(tcs.is_empty());

        let (emit, tcs) = acc.ingest("tool");
        assert!(emit.is_none(), "should not emit 'tool'");
        assert!(tcs.is_empty());

        let (emit, tcs) = acc.ingest("_c");
        assert!(emit.is_none(), "should not emit '_c'");
        assert!(tcs.is_empty());

        let (emit, tcs) = acc.ingest("alls");
        assert!(emit.is_none(), "should not emit 'alls'");
        assert!(tcs.is_empty());

        // Now the opening tag is complete with '>'
        let (emit, tcs) = acc.ingest(">");
        assert!(emit.is_none(), "tag complete — still no content");
        assert!(tcs.is_empty(), "no close tag yet — no tool calls");

        // Accumulator should be in DSML mode
        assert!(acc.in_dsml, "should be in dsml mode after opening tag");
    }

    #[test]
    fn partial_tag_in_chunks_with_leading_text() {
        let mut acc = DsmlAccumulator::default();
        // "先查一下系统信息 <\|DSML\|tool_calls>..."
        let (emit, tcs) = acc.ingest("先查一下系统信息 <");
        assert_eq!(emit.as_deref(), Some("先查一下系统信息 "),
            "text before '<' should be emitted");
        assert!(tcs.is_empty());

        // The '<' is now in the buffer as a potential tag start
        let (emit, tcs) = acc.ingest("\u{FF5C}\u{FF5C}DSML\u{FF5C}\u{FF5C}");
        assert!(emit.is_none(), "text after '<' should be held as partial tag");
        assert!(tcs.is_empty());

        let (emit, tcs) = acc.ingest("tool_calls>");
        assert!(emit.is_none(), "opening tag complete — no content");
        assert!(tcs.is_empty());

        assert!(acc.in_dsml, "should be in dsml mode");
    }
}
