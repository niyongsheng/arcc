use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub mode: String,
    pub created_at: String,
    pub last_active_at: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputHistoryEntry {
    pub id: i64,
    pub session_id: String,
    pub prompt: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Option<i64>,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub token_count: Option<i64>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub id: Option<i64>,
    pub session_id: String,
    pub range_start_msg_id: i64,
    pub range_end_msg_id: i64,
    pub summary_text: String,
    pub compressed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub id: Option<i64>,
    pub date: Option<String>,
    pub session_id: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    pub id: Option<i64>,
    pub user_id: String,
    pub key: String,
    pub value: String,
    pub source: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub chat_id: String,
    pub chat_type: String,
    pub open_id: String,
    pub reply_id: String,
    pub reply_id_type: String,
    pub cron: Option<String>,
    pub task_description: String,
    pub status: String,
    pub next_run_at: String,
    pub last_run_at: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}
