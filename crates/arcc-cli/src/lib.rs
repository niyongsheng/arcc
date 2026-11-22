//! arcc-cli: headless execution with portable-pty + LLM integration + tool calling.
//!
//! Supports two output modes:
//! - **Streaming** (default): LLM tokens printed as they arrive (`print!` / `eprint!`).
//! - **JSON** (`--json`): collects full result, outputs a single JSON object at the end.

use serde::Serialize;
use std::io::{Read, Write};
use std::time::Instant;
use tracing::info;

use arcc_core::context::SharedContext;
use arcc_core::model::types::{ChatMessage, ChatRequest, StreamChunk, ToolCall};
use arcc_core::tools;
use futures::StreamExt;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

/// Outcome of a single tool-call execution.
#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub command: String,
    pub status: String, // "ok" | "blocked" | "error"
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

/// Final JSON output when `--json` is active.
#[derive(Debug, Serialize)]
pub struct CliResult {
    pub response: String,
    pub tool_calls: Vec<ToolResult>,
    /// "ok" (all tools succeeded), "partial" (some blocked/errored), "error" (LLM failed).
    pub status: String,
}

/// Execute a prompt: `!cmd` for raw shell, otherwise route through the LLM.
///
/// When `json_mode` is `true`, all output is buffered and emitted as a single
/// JSON object on stdout — no streaming fragments, no stderr tool-echo.
pub async fn run(ctx: SharedContext, prompt: &str, json_mode: bool) -> anyhow::Result<()> {
    let prompt = prompt.trim();

    if let Some(cmd) = prompt.strip_prefix('!') {
        let cmd = cmd.trim();
        info!(%cmd, "executing raw command via pty");

        if json_mode {
            // JSON mode: capture PTY output instead of printing it.
            let output = capture_pty_output(cmd)?;
            let result = CliResult {
                response: String::new(),
                tool_calls: vec![ToolResult {
                    command: cmd.to_owned(),
                    status: "ok".into(),
                    stdout: output,
                    stderr: String::new(),
                    exit_code: None,
                    error: None,
                }],
                status: "ok".into(),
            };
            println!("{}", serde_json::to_string(&result)?);
        } else {
            run_pty_command(cmd)?;
        }
        return Ok(());
    }

    // --- LLM call with tool support ---
    let mut provider = ctx
        .providers
        .pick(prompt, true)
        .ok_or_else(|| anyhow::anyhow!("no model provider available"))?
        .clone();

    // Server-side web tools only work with the flash model.
    let server_tools = ctx.storage.config.web.server_tools;
    if server_tools && let Some(flash) = ctx.providers.flash() {
        provider = flash.clone();
    }

    let skip_permissions = ctx.dangerously_skip_permissions;
    let mut tool_defs = vec![tools::command_tool_definition()];
    if ctx.storage.config.web.enabled {
        if !server_tools {
            tool_defs.push(arcc_core::web::search_tool_definition());
            tool_defs.push(arcc_core::web::fetch_tool_definition());
        }
        tool_defs.push(arcc_core::web::ocr_tool_definition());
    }
    if server_tools {
        tool_defs.extend(arcc_core::web::server_tool_definitions());
    }
    let temperature = ctx.storage.config.model.temperature;
    let max_tokens = ctx.storage.config.model.max_output_tokens;

    let system_msg = {
        let mut msg = arcc_core::model::prompts::templates::cli().to_chat_message();
        if let Some(ref text) = *ctx.project_instructions.read().await {
            msg.content.push_str("\n\n## Project Instructions\n\n");
            msg.content.push_str(text);
        }
        msg
    };

    let mut messages = vec![system_msg, ChatMessage::user(prompt.to_owned())];

    info!(model = %provider.model_name(), "CLI tool-calling stream");

    // JSON mode buffers instead of printing.
    let mut json_response = String::new();
    let mut json_tool_calls: Vec<ToolResult> = Vec::new();
    let mut json_status = "ok".to_owned();

    // Tool-calling loop: Phase 1 with tools, Phase N with results.
    let mut phase = 1;
    loop {
        let has_tools = phase == 1;
        let req = ChatRequest {
            model: provider.model_name().to_owned(),
            messages: messages.clone(),
            tools: if has_tools {
                Some(tool_defs.clone())
            } else {
                None
            },
            tool_choice: if has_tools {
                Some(serde_json::json!("auto"))
            } else {
                None
            },
            temperature: Some(temperature),
            max_tokens: Some(max_tokens),
            stream: true,
            thinking_mode: None,
            reasoning_effort: None,
        };

        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut reasoning_buf = String::new();
        let mut citations: Vec<arcc_core::model::types::Citation> = Vec::new();

        match provider.chat_stream(req).await {
            Ok(stream) => {
                let mut stream = Box::pin(stream);
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(StreamChunk::Content(text)) => {
                            if json_mode {
                                json_response.push_str(&text);
                            } else {
                                print!("{text}");
                                std::io::stdout().flush()?;
                            }
                        }
                        Ok(StreamChunk::Reasoning(text)) => {
                            reasoning_buf.push_str(&text);
                            if !json_mode {
                                eprint!("\x1b[2m{text}\x1b[0m");
                            }
                        }
                        Ok(StreamChunk::ToolCallStart(tc)) => {
                            tool_calls.push(tc);
                        }
                        Ok(StreamChunk::ServerTool { tool, detail }) => {
                            if !json_mode {
                                let text = if detail.is_empty() {
                                    format!("🔎 {tool}")
                                } else {
                                    format!("🔎 {tool}: {detail}")
                                };
                                eprintln!("\n{text}");
                            }
                        }
                        Ok(StreamChunk::Citations(list)) => citations.extend(list),
                        Ok(_) => {}
                        Err(e) => {
                            if json_mode {
                                json_status = "error".into();
                                json_tool_calls.push(ToolResult {
                                    command: String::new(),
                                    status: "error".into(),
                                    stdout: String::new(),
                                    stderr: String::new(),
                                    exit_code: None,
                                    error: Some(e.to_string()),
                                });
                                let result = CliResult {
                                    response: json_response,
                                    tool_calls: json_tool_calls,
                                    status: json_status,
                                };
                                println!("{}", serde_json::to_string(&result)?);
                            } else {
                                eprintln!("\n[error] {e}");
                            }
                            return Err(e.into());
                        }
                    }
                }
            }
            Err(e) => {
                if json_mode {
                    json_status = "error".into();
                    let result = CliResult {
                        response: json_response,
                        tool_calls: json_tool_calls,
                        status: json_status,
                    };
                    println!("{}", serde_json::to_string(&result)?);
                } else {
                    eprintln!("\n[error] {e}");
                }
                return Err(e.into());
            }
        }

        // Server-side web answers carry citations; render them as sources.
        if !citations.is_empty() {
            let block = arcc_core::model::types::citations_markdown(&citations);
            if !block.is_empty() {
                if json_mode {
                    json_response.push_str(&block);
                } else {
                    print!("{block}");
                }
            }
        }

        if tool_calls.is_empty() {
            if json_mode {
                let result = CliResult {
                    response: json_response,
                    tool_calls: json_tool_calls,
                    status: json_status,
                };
                println!("{}", serde_json::to_string(&result)?);
            } else {
                println!();
            }
            return Ok(());
        }

        // Execute tool calls and build follow-up messages.
        for tc in &tool_calls {
            if matches!(
                tc.name.as_str(),
                arcc_core::web::SEARCH_TOOL | arcc_core::web::FETCH_TOOL | arcc_core::web::OCR_TOOL
            ) {
                let label = arcc_core::web::describe_call(&tc.name, &tc.arguments);
                if !json_mode {
                    eprintln!("\n⚡ {label}");
                }

                messages.push(ChatMessage {
                    role: "assistant".into(),
                    content: String::new(),
                    images: Vec::new(),
                    tool_calls: Some(vec![tc.clone()]),
                    tool_call_id: None,
                    reasoning_content: if reasoning_buf.is_empty() {
                        None
                    } else {
                        Some(reasoning_buf.clone())
                    },
                });

                let started = Instant::now();
                let result = arcc_core::web::execute(
                    &ctx.storage.config.model,
                    &ctx.storage.config.web,
                    &tc.name,
                    &tc.arguments,
                )
                .await;
                let elapsed_ms = started.elapsed().as_millis() as u64;
                arcc_core::web::audit(&ctx, "cli", &tc.name, &tc.arguments, &result, elapsed_ms);

                match result {
                    Ok(content) => {
                        messages.push(ChatMessage::tool_result(tc.id.clone(), content.clone()));
                        if json_mode {
                            json_tool_calls.push(ToolResult {
                                command: label,
                                status: "ok".into(),
                                stdout: content,
                                stderr: String::new(),
                                exit_code: None,
                                error: None,
                            });
                        } else {
                            eprintln!("✓ done");
                        }
                    }
                    Err(e) => {
                        let err = e.to_string();
                        messages.push(ChatMessage::tool_result(
                            tc.id.clone(),
                            format!("error: {err}"),
                        ));
                        if json_mode {
                            json_status = "partial".into();
                            json_tool_calls.push(ToolResult {
                                command: label,
                                status: "error".into(),
                                stdout: String::new(),
                                stderr: String::new(),
                                exit_code: None,
                                error: Some(err),
                            });
                        } else {
                            eprintln!("error: {e}");
                        }
                    }
                }
                continue;
            }

            let command = tc.arguments["command"].as_str().unwrap_or("").to_owned();

            if !json_mode {
                eprintln!("\n⚡ {command}");
            }

            let al = ctx.allowlist.read().await;
            let executed = tools::execute_command(&command, &al, skip_permissions).await;
            drop(al);

            messages.push(ChatMessage {
                role: "assistant".into(),
                content: String::new(),
                images: Vec::new(),
                tool_calls: Some(vec![tc.clone()]),
                tool_call_id: None,
                reasoning_content: if reasoning_buf.is_empty() {
                    None
                } else {
                    Some(reasoning_buf.clone())
                },
            });

            match executed {
                Ok(output) => {
                    let content = output.to_content();
                    messages.push(ChatMessage::tool_result(tc.id.clone(), content));
                    if json_mode {
                        json_tool_calls.push(ToolResult {
                            command: command.clone(),
                            status: "ok".into(),
                            stdout: output.stdout,
                            stderr: output.stderr,
                            exit_code: output.exit_code,
                            error: None,
                        });
                    } else {
                        eprintln!("exit={:?}", output.exit_code);
                    }
                }
                Err(e) => {
                    let err_str = e.to_string();
                    let status = if err_str.contains("confirmation") || err_str.contains("blocked")
                    {
                        "blocked"
                    } else {
                        "error"
                    };
                    if json_mode {
                        json_tool_calls.push(ToolResult {
                            command: command.clone(),
                            status: status.into(),
                            stdout: String::new(),
                            stderr: String::new(),
                            exit_code: None,
                            error: Some(err_str.clone()),
                        });
                        if status == "blocked" {
                            json_status = "partial".into();
                        }
                    } else {
                        eprintln!("error: {e}");
                    }
                    messages.push(ChatMessage::tool_result(
                        tc.id.clone(),
                        format!("error: {e}"),
                    ));
                }
            }
        }

        phase = 2; // Follow-up without tools.
    }
}

/// Run a shell command through a portable-pty, preserving ANSI output.
fn run_pty_command(cmd: &str) -> anyhow::Result<()> {
    let pty_system = NativePtySystem::default();
    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut cmd_builder = CommandBuilder::new(&shell);
    cmd_builder.arg("-c");
    cmd_builder.arg(cmd);

    let mut proc = pair.slave.spawn_command(cmd_builder)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut output = Vec::new();
    reader.read_to_end(&mut output)?;

    std::io::stdout().write_all(&output)?;
    proc.wait()?;
    Ok(())
}

/// Like `run_pty_command` but returns output as a `String` instead of printing.
fn capture_pty_output(cmd: &str) -> anyhow::Result<String> {
    let pty_system = NativePtySystem::default();
    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut cmd_builder = CommandBuilder::new(&shell);
    cmd_builder.arg("-c");
    cmd_builder.arg(cmd);

    let mut proc = pair.slave.spawn_command(cmd_builder)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut output = Vec::new();
    reader.read_to_end(&mut output)?;
    proc.wait()?;

    Ok(String::from_utf8_lossy(&output).to_string())
}
