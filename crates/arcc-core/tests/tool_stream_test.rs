use arcc_core::model::deepseek::DeepSeekProvider;
use arcc_core::model::types::{ChatMessage, ChatRequest};
use arcc_core::model::provider::ModelProvider;
use arcc_core::tools;
use futures::StreamExt;

#[tokio::test]
async fn test_tool_call_stream() {
    let api_key = std::env::var("DEEPSEEK_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        eprintln!("SKIP: no API key");
        return;
    }
    let provider = DeepSeekProvider::new("https://api.deepseek.com", &api_key, "deepseek-chat");
    
    let req = ChatRequest {
        model: "deepseek-chat".into(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: "You are ARCC. Use execute_command tool. Always use it for system queries.".into(),
                tool_calls: None, tool_call_id: None, reasoning_content: None,
            },
            ChatMessage {
                role: "user".into(),
                content: "当前网络状态".into(),
                tool_calls: None, tool_call_id: None, reasoning_content: None,
            },
        ],
        tools: Some(vec![tools::command_tool_definition()]),
        tool_choice: Some(serde_json::json!("auto")),
        temperature: Some(0.7),
        max_tokens: Some(4096),
        stream: true,
        thinking_mode: None,
        reasoning_effort: None,
    };
    
    let mut stream = provider.chat_stream(req).await.expect("chat_stream");
    let mut tool_calls = vec![];
    
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(arcc_core::model::types::StreamChunk::Content(t)) => {
                eprint!("[C]{t}");
            }
            Ok(arcc_core::model::types::StreamChunk::ToolCallStart(tc)) => {
                eprintln!("\n[TOOL] {} args={}", tc.name, tc.arguments);
                tool_calls.push(tc);
            }
            Ok(arcc_core::model::types::StreamChunk::Reasoning(t)) => {
                eprintln!("[R]{t}");
            }
            Ok(arcc_core::model::types::StreamChunk::Finish(u)) => {
                eprintln!("\n[FINISH] {}in {}out", u.prompt_tokens, u.completion_tokens);
            }
            Ok(other) => eprintln!("[OTHER] {:?}", other),
            Err(e) => eprintln!("\n[ERR] {e}"),
        }
    }
    
    assert!(!tool_calls.is_empty(), "Expected at least one tool call!");
    eprintln!("\n*** GOT {} TOOL CALL(S) ***", tool_calls.len());
}
