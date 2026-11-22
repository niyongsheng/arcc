//! DeepSeek Web Tools API — web search, URL fetch, and OCR.
//!
//! These client-side built-in tools call the DeepSeek Tools API directly:
//!
//! - `POST {api_base}/v1/tools/search` — structured web search results
//! - `POST {api_base}/v1/tools/fetch`  — one URL as Markdown
//! - `POST {api_base}/v1/tools/ocr`    — per-page text from PDF/Office/images
//!
//! Billing is per successful request (see the DeepSeek pricing page), so the
//! tools are gated by `[web] enabled` in the config.  The API key is shared
//! with the chat completions endpoint: `[model].api_key` first, then the env
//! var named by `[model].api_key_env`.
//!
//! The module also exposes the server-side tool declarations
//! (`deepseek_web_search` / `deepseek_read_url`).  Those are executed by
//! DeepSeek itself — the client only declares `{"type": "..."}` — and the
//! model must be `deepseek-flash`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tracing::debug;

use crate::context::AppContext;
use crate::model::types::ToolDefinition;
use arcc_storage::audit::types::{AuditEvent, ExecResult};
use arcc_storage::config::loader::{ModelConfig, WebToolsConfig};

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// Client-side search tool name.
pub const SEARCH_TOOL: &str = "web_search";
/// Client-side URL fetch tool name.
pub const FETCH_TOOL: &str = "web_fetch";
/// Client-side OCR tool name.
pub const OCR_TOOL: &str = "ocr";

/// Server-side DeepSeek web search tool type.
pub const SERVER_SEARCH_TOOL: &str = "deepseek_web_search";
/// Server-side DeepSeek read-URL tool type.
pub const SERVER_READ_URL_TOOL: &str = "deepseek_read_url";

const MAX_QUERY_CHARS: usize = 128;
const MAX_URL_CHARS: usize = 8192;
/// Per-result body cap before the overall budget is applied.
const MAX_CONTENT_CHARS: usize = 4_000;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum WebToolError {
    #[error("missing DeepSeek API key — set [model].api_key or the {0} env var")]
    MissingApiKey(String),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("file error: {0}")]
    File(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error {code}: {message}")]
    Api { code: u16, message: String },
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// `web_search` — structured web search via the DeepSeek Search API.
pub fn search_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: SEARCH_TOOL.into(),
        description: "Search the web through DeepSeek's Search API. Returns ranked results \
                      with title, URL, snippets and summaries. Use it for current events, \
                      documentation, or facts beyond your knowledge cutoff. The query supports \
                      operators: site:example.com, -site:example.com, \"exact phrase\", \
                      lang:zh,en, after:2025-01-01, before:2026-06-01. Follow up with web_fetch \
                      to read a specific result."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query, 1-128 characters. Search operators are supported."
                },
                "count": {
                    "type": "integer",
                    "description": "Number of results to return, 1-50. Defaults to the configured value."
                },
                "need_content": {
                    "type": "boolean",
                    "description": "Include page bodies in each result's `content`. Costs more tokens; \
                                    prefer web_fetch when you only need one page."
                },
                "ori_question": {
                    "type": "string",
                    "description": "The user's original question (1-128 chars), helps the search \
                                    engine understand intent."
                }
            },
            "required": ["query"]
        }),
        strict: false,
        builtin: None,
    }
}

/// `web_fetch` — fetch one URL as Markdown via the DeepSeek Fetch API.
pub fn fetch_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: FETCH_TOOL.into(),
        description: "Fetch a single web page through DeepSeek's Fetch API and return its title \
                      and main content as Markdown. Use it after web_search to read a promising \
                      result, or when the user gives you a link. Only http/https URLs are allowed."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "HTTP/HTTPS URL to fetch."
                }
            },
            "required": ["url"]
        }),
        strict: false,
        builtin: None,
    }
}

/// `ocr` — extract text from a local document/image via the DeepSeek OCR API.
pub fn ocr_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: OCR_TOOL.into(),
        description: "Extract text from a local PDF, Word, PowerPoint, or image file through \
                      DeepSeek's OCR API. Returns per-page text plus bounding-box markers. Use \
                      `text_recognition` for text-heavy documents and `rich_layout_recognition` \
                      when layout/figures matter. Files larger than the configured limit are rejected."
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Local file path (PDF, DOC/DOCX, PPT/PPTX, PNG, JPEG, WebP, GIF, BMP, TIFF). `~/` is expanded."
                },
                "purpose": {
                    "type": "string",
                    "enum": ["text_recognition", "rich_layout_recognition"],
                    "description": "text_recognition (default): text only. rich_layout_recognition: \
                                    also recognizes figures/tables and their positions."
                }
            },
            "required": ["path"]
        }),
        strict: false,
        builtin: None,
    }
}

/// Server-side tool declarations for the DeepSeek Chat Completions API.
///
/// These serialize as `{"type": "deepseek_web_search"}` and run entirely on
/// DeepSeek's side; the client never receives a function call for them.
/// Requires the flash model.
pub fn server_tool_definitions() -> Vec<ToolDefinition> {
    [SERVER_SEARCH_TOOL, SERVER_READ_URL_TOOL]
        .into_iter()
        .map(|kind| ToolDefinition {
            name: kind.into(),
            description: String::new(),
            parameters: json!({ "type": "object", "properties": {} }),
            strict: false,
            builtin: Some(kind.into()),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Execute one client-side web tool. Returns the tool result text (JSON).
pub async fn execute(
    model_cfg: &ModelConfig,
    web_cfg: &WebToolsConfig,
    tool: &str,
    args: &Value,
) -> Result<String, WebToolError> {
    match tool {
        SEARCH_TOOL => search(model_cfg, web_cfg, args).await,
        FETCH_TOOL => fetch(model_cfg, web_cfg, args).await,
        OCR_TOOL => ocr(model_cfg, web_cfg, args).await,
        other => Err(WebToolError::InvalidArguments(format!(
            "unknown web tool `{other}`"
        ))),
    }
}

/// Human-readable one-liner for progress events.
pub fn describe_call(tool: &str, args: &Value) -> String {
    match tool {
        SEARCH_TOOL => format!("🔎 {}", args["query"].as_str().unwrap_or("searching...")),
        FETCH_TOOL => format!("🌐 {}", args["url"].as_str().unwrap_or("fetching...")),
        OCR_TOOL => format!("📄 {}", args["path"].as_str().unwrap_or("reading file...")),
        other => other.to_owned(),
    }
}

/// Append a `web_tool` audit event (best effort).
pub fn audit(
    ctx: &AppContext,
    session: &str,
    tool: &str,
    arguments: &Value,
    outcome: &Result<String, WebToolError>,
    elapsed_ms: u64,
) {
    let result = match outcome {
        Ok(_) => ExecResult::Ok,
        Err(e) => ExecResult::Error(e.to_string()),
    };
    ctx.storage.audit.write(&AuditEvent::WebToolCall {
        ts: chrono::Utc::now().to_rfc3339(),
        session: session.to_owned(),
        tool: tool.to_owned(),
        arguments: arguments.to_string(),
        result,
        elapsed_ms,
    });
}

async fn search(
    model_cfg: &ModelConfig,
    web_cfg: &WebToolsConfig,
    args: &Value,
) -> Result<String, WebToolError> {
    let query = args["query"]
        .as_str()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .ok_or_else(|| WebToolError::InvalidArguments("`query` is required".into()))?;
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(WebToolError::InvalidArguments(format!(
            "`query` must be at most {MAX_QUERY_CHARS} characters"
        )));
    }

    let count = args["count"]
        .as_u64()
        .map(|c| c.clamp(1, 50) as u32)
        .unwrap_or(web_cfg.search_count.clamp(1, 50));
    let need_content = args["need_content"]
        .as_bool()
        .unwrap_or(web_cfg.search_need_content);

    let mut body = json!({
        "query": query,
        "count": count,
        "need_content": need_content,
    });
    if let Some(question) = args["ori_question"]
        .as_str()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    {
        body["ori_question"] = json!(truncate_chars(question, MAX_QUERY_CHARS));
    }

    let (base, key) = credentials(model_cfg)?;
    let client = http_client(web_cfg.timeout_seconds.max(10))?;
    let req = client
        .post(format!("{base}/v1/tools/search"))
        .bearer_auth(&key)
        .json(&body);
    let value = send_json(req).await?;

    let results = value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let budget = web_cfg.max_result_chars.max(1_000);
    let mut items: Vec<Value> = Vec::new();
    let mut used = 0usize;
    for r in results {
        let mut item = json!({
            "title": r.get("title").cloned().unwrap_or(Value::Null),
            "url": r.get("url").cloned().unwrap_or(Value::Null),
            "summary": r.get("summary").cloned().unwrap_or(Value::Null),
            "snippet": r.get("snippet").cloned().unwrap_or(Value::Null),
        });
        for key in ["site_name", "date", "authority", "rerank_score"] {
            if let Some(v) = r.get(key)
                && !v.is_null()
            {
                item[key] = v.clone();
            }
        }
        if let Some(content) = r.get("content").and_then(Value::as_str)
            && !content.is_empty()
        {
            item["content"] = json!(truncate_chars(content, MAX_CONTENT_CHARS));
        }

        let encoded_len = item.to_string().chars().count() + 1;
        if used + encoded_len > budget && !items.is_empty() {
            debug!(
                kept = items.len(),
                total = maybe_len(&items),
                "search results truncated to fit budget"
            );
            break;
        }
        used += encoded_len;
        items.push(item);
    }

    Ok(json!({
        "query": query,
        "returned": items.len(),
        "results": items,
    })
    .to_string())
}

async fn fetch(
    model_cfg: &ModelConfig,
    web_cfg: &WebToolsConfig,
    args: &Value,
) -> Result<String, WebToolError> {
    let url = args["url"]
        .as_str()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| WebToolError::InvalidArguments("`url` is required".into()))?;
    validate_url(url)?;

    let (base, key) = credentials(model_cfg)?;
    let client = http_client(web_cfg.timeout_seconds.max(10))?;
    let req = client
        .post(format!("{base}/v1/tools/fetch"))
        .bearer_auth(&key)
        .json(&json!({ "url": url }));
    let value = send_json(req).await?;

    let title = value.get("title").and_then(Value::as_str).unwrap_or("");
    let content = value.get("content").and_then(Value::as_str).unwrap_or("");
    Ok(json!({
        "url": url,
        "title": title,
        "content": truncate_chars(content, web_cfg.max_result_chars.max(1_000)),
    })
    .to_string())
}

async fn ocr(
    model_cfg: &ModelConfig,
    web_cfg: &WebToolsConfig,
    args: &Value,
) -> Result<String, WebToolError> {
    let raw_path = args["path"]
        .as_str()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| WebToolError::InvalidArguments("`path` is required".into()))?;
    let path = expand_tilde(raw_path);

    let purpose = args["purpose"].as_str().unwrap_or("text_recognition");
    if purpose != "text_recognition" && purpose != "rich_layout_recognition" {
        return Err(WebToolError::InvalidArguments(
            "`purpose` must be text_recognition or rich_layout_recognition".into(),
        ));
    }

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| WebToolError::File(format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(WebToolError::File(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let max_bytes = web_cfg.ocr_max_file_mb.saturating_mul(1024 * 1024);
    if max_bytes > 0 && meta.len() > max_bytes {
        return Err(WebToolError::InvalidArguments(format!(
            "file is {} MiB; the limit is {} MiB",
            meta.len() / (1024 * 1024),
            web_cfg.ocr_max_file_mb
        )));
    }

    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| WebToolError::File(format!("{}: {e}", path.display())))?;
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "upload".into());
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str(mime_for(&path))
        .map_err(|e| WebToolError::InvalidArguments(format!("invalid filename: {e}")))?;
    // `purpose` must precede `file` (see the OCR API docs).
    let form = reqwest::multipart::Form::new()
        .text("purpose", purpose.to_owned())
        .part("file", part);

    let (base, key) = credentials(model_cfg)?;
    let client = http_client(web_cfg.ocr_timeout_seconds.max(180))?;
    let req = client
        .post(format!("{base}/v1/tools/ocr"))
        .bearer_auth(&key)
        .multipart(form);
    let value = send_json(req).await?;

    if let Some(err) = value.get("error") {
        return Err(WebToolError::Api {
            code: 200,
            message: err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("OCR failed")
                .to_owned(),
        });
    }

    let pages = value
        .get("pages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let page_count = value
        .get("page_count")
        .and_then(Value::as_u64)
        .unwrap_or(pages.len() as u64);

    let budget = web_cfg.max_result_chars.max(1_000);
    let per_page = (budget / pages.len().max(1) as usize).max(200);
    let mut out_pages = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;
    for page in &pages {
        let text = page.get("text").and_then(Value::as_str).unwrap_or("");
        let remaining = budget.saturating_sub(used).max(200);
        let cap = per_page.min(remaining);
        let (text, was_truncated) = truncate_chars_with_flag(text, cap);
        truncated |= was_truncated;
        used += text.chars().count();

        let mut item = json!({
            "index": page.get("index").cloned().unwrap_or(Value::Null),
            "text": text,
        });
        if let Some(reason) = page.get("finish_reason").and_then(Value::as_str)
            && reason != "stop"
        {
            item["finish_reason"] = json!(reason);
        }
        if was_truncated {
            item["truncated"] = json!(true);
        }
        out_pages.push(item);
    }

    Ok(json!({
        "file": path.display().to_string(),
        "page_count": page_count,
        "pages": out_pages,
        "truncated": truncated,
    })
    .to_string())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn credentials(cfg: &ModelConfig) -> Result<(String, String), WebToolError> {
    let key = cfg
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            std::env::var(&cfg.api_key_env)
                .ok()
                .map(|k| k.trim().to_owned())
                .filter(|k| !k.is_empty())
        })
        .ok_or_else(|| WebToolError::MissingApiKey(cfg.api_key_env.clone()))?;
    Ok((cfg.api_base.trim_end_matches('/').to_owned(), key))
}

fn http_client(timeout_secs: u64) -> Result<reqwest::Client, WebToolError> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(10))
        .build()?)
}

/// Send a JSON request and decode the JSON body, mapping API errors.
async fn send_json(req: reqwest::RequestBuilder) -> Result<Value, WebToolError> {
    let resp = req.send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(WebToolError::Api {
            code: status.as_u16(),
            message: api_error_message(&text),
        });
    }
    let value: Value = serde_json::from_str(text.trim_start()).map_err(|e| WebToolError::Api {
        code: status.as_u16(),
        message: format!("invalid JSON response: {e}"),
    })?;
    if let Some(err) = value.get("error") {
        return Err(WebToolError::Api {
            code: status.as_u16(),
            message: err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("API returned an error")
                .to_owned(),
        });
    }
    Ok(value)
}

fn api_error_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(body)
        && let Some(message) = value
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
    {
        return message.to_owned();
    }
    truncate_chars(body.trim(), 500)
}

fn validate_url(url: &str) -> Result<(), WebToolError> {
    if url.chars().count() > MAX_URL_CHARS {
        return Err(WebToolError::InvalidArguments(format!(
            "`url` must be at most {MAX_URL_CHARS} characters"
        )));
    }
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| WebToolError::InvalidArguments(format!("invalid URL: {e}")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(WebToolError::InvalidArguments(
            "only http/https URLs are supported".into(),
        ));
    }
    if parsed.host_str().is_none() {
        return Err(WebToolError::InvalidArguments(
            "URL must contain a host".into(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(WebToolError::InvalidArguments(
            "URLs with embedded credentials are not allowed".into(),
        ));
    }
    Ok(())
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("doc") => "application/msword",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("ppt") => "application/vnd.ms-powerpoint",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        Some("tif") | Some("tiff") => "image/tiff",
        _ => "application/octet-stream",
    }
}

/// Truncate a string to `max` Unicode scalar values.
fn truncate_chars(s: &str, max: usize) -> String {
    truncate_chars_with_flag(s, max).0
}

/// Truncate a string to `max` chars, returning whether anything was dropped.
fn truncate_chars_with_flag(s: &str, max: usize) -> (String, bool) {
    if max == 0 {
        return (String::new(), !s.is_empty());
    }
    let mut out = String::new();
    for c in s.chars().take(max) {
        out.push(c);
    }
    if out.chars().count() < s.chars().count() {
        out.push_str("\n...(truncated)");
        (out, true)
    } else {
        (out, false)
    }
}

fn maybe_len(items: &[Value]) -> usize {
    items.iter().map(|v| v.to_string().len()).sum()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Spin up a one-shot HTTP server that records the request and replies
    /// with `body`. Returns the base URL and a join handle with the raw
    /// request text.
    async fn mock_server(body: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut tmp = [0u8; 8192];
            loop {
                let n = socket.read(&mut tmp).await.expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
                    let content_len = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buf.len() >= header_end + 4 + content_len {
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
            String::from_utf8_lossy(&buf).to_string()
        });
        (format!("http://{addr}"), handle)
    }

    fn test_model(api_base: String) -> ModelConfig {
        ModelConfig {
            api_base,
            api_key: Some("test-key".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn search_posts_to_tools_endpoint_and_formats_results() {
        let (base, server) = mock_server(
            r#"{"results":[{"title":"Rust","url":"https://example.com/rust","summary":"A language","snippet":"snip","rerank_score":2.4}]}"#,
        )
        .await;
        let out = execute(
            &test_model(base),
            &WebToolsConfig::default(),
            SEARCH_TOOL,
            &json!({"query": "rust", "count": 3}),
        )
        .await
        .expect("search succeeds");
        let request = server.await.expect("server joined");

        assert!(request.starts_with("POST /v1/tools/search "), "{request}");
        assert!(request.contains("authorization: Bearer test-key"));
        let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
        assert!(body.contains("\"query\":\"rust\""), "{body}");
        assert!(body.contains("\"count\":3"), "{body}");

        let parsed: Value = serde_json::from_str(&out).expect("valid JSON output");
        assert_eq!(parsed["returned"], 1);
        assert_eq!(parsed["results"][0]["title"], "Rust");
        assert_eq!(parsed["results"][0]["url"], "https://example.com/rust");
    }

    #[tokio::test]
    async fn fetch_posts_url_and_returns_markdown() {
        let (base, server) = mock_server(r##"{"title":"Doc","content":"# Hello"}"##).await;
        let out = execute(
            &test_model(base),
            &WebToolsConfig::default(),
            FETCH_TOOL,
            &json!({"url": "https://example.com/doc"}),
        )
        .await
        .expect("fetch succeeds");
        let request = server.await.expect("server joined");

        assert!(request.starts_with("POST /v1/tools/fetch "), "{request}");
        assert!(request.contains("\"url\":\"https://example.com/doc\""));

        let parsed: Value = serde_json::from_str(&out).expect("valid JSON output");
        assert_eq!(parsed["title"], "Doc");
        assert_eq!(parsed["content"], "# Hello");
    }

    #[tokio::test]
    async fn ocr_uploads_multipart_and_returns_pages() {
        let (base, server) = mock_server(
            r#"{"id":"ocr-1","object":"ocr.result","page_count":1,"pages":[{"index":0,"text":"hello","finish_reason":"stop"}],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}"#,
        )
        .await;

        let path = std::env::temp_dir().join(format!("arcc-ocr-test-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"hello").expect("write temp file");

        let out = execute(
            &test_model(base),
            &WebToolsConfig::default(),
            OCR_TOOL,
            &json!({"path": path.display().to_string()}),
        )
        .await
        .expect("ocr succeeds");
        let request = server.await.expect("server joined");
        let _ = std::fs::remove_file(&path);

        assert!(request.starts_with("POST /v1/tools/ocr "), "{request}");
        assert!(request.contains("multipart/form-data"));
        assert!(request.contains("name=\"purpose\""));
        assert!(request.contains("text_recognition"));
        assert!(request.contains("name=\"file\""));

        let parsed: Value = serde_json::from_str(&out).expect("valid JSON output");
        assert_eq!(parsed["page_count"], 1);
        assert_eq!(parsed["pages"][0]["text"], "hello");
    }

    #[tokio::test]
    async fn api_errors_surface_as_typed_errors() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let body = r#"{"error":{"message":"'count' must be between 1 and 50","type":"invalid_request_error"}}"#;
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });

        let err = execute(
            &test_model(format!("http://{addr}")),
            &WebToolsConfig::default(),
            SEARCH_TOOL,
            &json!({"query": "x"}),
        )
        .await
        .expect_err("API error");
        match err {
            WebToolError::Api { code, message } => {
                assert_eq!(code, 400);
                assert!(message.contains("between 1 and 50"), "{message}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn truncation_is_utf8_safe() {
        let (out, truncated) = truncate_chars_with_flag("你好世界", 2);
        assert_eq!(out, "你好\n...(truncated)");
        assert!(truncated);

        let (out, truncated) = truncate_chars_with_flag("abc", 10);
        assert_eq!(out, "abc");
        assert!(!truncated);
    }

    #[test]
    fn url_validation() {
        assert!(validate_url("https://example.com/a").is_ok());
        assert!(validate_url("http://example.com").is_ok());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("https://user:pass@example.com").is_err());
        assert!(validate_url("not a url").is_err());
    }

    #[test]
    fn server_tools_serialize_as_builtin() {
        let defs = server_tool_definitions();
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, SERVER_SEARCH_TOOL);
        assert_eq!(defs[0].builtin.as_deref(), Some(SERVER_SEARCH_TOOL));
        assert_eq!(defs[1].builtin.as_deref(), Some(SERVER_READ_URL_TOOL));
    }

    #[test]
    fn describe_call_uses_primary_argument() {
        assert_eq!(
            describe_call(SEARCH_TOOL, &json!({"query": "rust async"})),
            "🔎 rust async"
        );
        assert_eq!(
            describe_call(FETCH_TOOL, &json!({"url": "https://example.com"})),
            "🌐 https://example.com"
        );
    }
}
