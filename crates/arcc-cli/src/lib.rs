//! arcc-cli: headless execution with portable-pty + LLM integration + tool calling.

use std::io::{Read, Write};
use tracing::info;

use arcc_core::context::SharedContext;
use arcc_core::model::types::{ChatMessage, ChatRequest, StreamChunk, ToolCall};
use arcc_core::tools;
use futures::StreamExt;
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

/// Execute a prompt: `!cmd` for raw shell, otherwise route through the LLM.
pub async fn run(ctx: SharedContext, prompt: &str) -> anyhow::Result<()> {
    let prompt = prompt.trim();

    if let Some(cmd) = prompt.strip_prefix('!') {
        let cmd = cmd.trim();
        info!(%cmd, "executing raw command via pty");
        run_pty_command(cmd)?;
        return Ok(());
    }

    // --- LLM call with tool support ---
    let provider = ctx
        .providers
        .pick(prompt.len(), true)
        .ok_or_else(|| anyhow::anyhow!("no model provider available"))?
        .clone();

    let skip_permissions = ctx.dangerously_skip_permissions;
    let tool_def = tools::command_tool_definition();
    let temperature = ctx.storage.config.model.temperature;
    let max_tokens = ctx.storage.config.model.max_output_tokens;

    let system_msg = arcc_core::model::prompts::templates::cli().to_chat_message();

    let mut messages = vec![
        system_msg,
        ChatMessage {
            role: "user".into(),
            content: prompt.to_owned(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        },
    ];

    info!(model = %provider.model_name(), "CLI tool-calling stream");

    // Tool-calling loop: Phase 1 with tools, Phase N with results.
    let mut phase = 1;
    loop {
        let has_tools = phase == 1;
        let req = ChatRequest {
            model: provider.model_name().to_owned(),
            messages: messages.clone(),
            tools: if has_tools {
                Some(vec![tool_def.clone()])
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

        match provider.chat_stream(req).await {
            Ok(stream) => {
                let mut stream = Box::pin(stream);
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(StreamChunk::Content(text)) => {
                            print!("{text}");
                            std::io::stdout().flush()?;
                        }
                        Ok(StreamChunk::Reasoning(text)) => {
                            reasoning_buf.push_str(&text);
                            eprint!("\x1b[2m{text}\x1b[0m");
                        }
                        Ok(StreamChunk::ToolCallStart(tc)) => {
                            tool_calls.push(tc);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("\n[error] {e}");
                            return Err(e.into());
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("\n[error] {e}");
                return Err(e.into());
            }
        }

        if tool_calls.is_empty() {
            println!();
            return Ok(());
        }

        // Execute tool calls and build follow-up messages.
        for tc in &tool_calls {
            let command = tc.arguments["command"]
                .as_str()
                .unwrap_or("")
                .to_owned();

            eprintln!("\n⚡ {command}");

            let al = ctx.allowlist.read().await;
            let executed = tools::execute_command(&command, &al, skip_permissions).await;
            drop(al);

            messages.push(ChatMessage {
                role: "assistant".into(),
                content: String::new(),
                tool_calls: Some(vec![tc.clone()]),
                tool_call_id: None,
                reasoning_content: if reasoning_buf.is_empty() { None } else { Some(reasoning_buf.clone()) },
            });

            match executed {
                Ok(output) => {
                    let content = if output.stderr.is_empty() {
                        output.stdout
                    } else {
                        format!(
                            "exit_code: {:?}\nstdout:\n{}\nstderr:\n{}",
                            output.exit_code, output.stdout, output.stderr
                        )
                    };
                    messages.push(ChatMessage {
                        role: "tool".into(),
                        content,
                        tool_calls: None,
                        tool_call_id: Some(tc.id.clone()),
                        reasoning_content: None,
                    });
                    eprintln!("exit={:?}", output.exit_code);
                }
                Err(e) => {
                    messages.push(ChatMessage {
                        role: "tool".into(),
                        content: format!("error: {e}"),
                        tool_calls: None,
                        tool_call_id: Some(tc.id.clone()),
                        reasoning_content: None,
                    });
                    eprintln!("error: {e}");
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
