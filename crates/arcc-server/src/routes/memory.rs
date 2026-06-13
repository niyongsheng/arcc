//! Memory management HTTP endpoints — CRUD for persistent facts.
//!
//! These endpoints allow manual inspection and manipulation of the memory
//! store that the auto-extraction system populates after each `/chat` request.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::error;

use arcc_core::context::SharedContext;
use arcc_storage::db::models::MemoryFact;

// ── Helpers ───────────────────────────────────────────────────────

fn mem_err(e: impl std::fmt::Display) -> (StatusCode, Json<MemoryError>) {
    error!(err = %e, "memory operation failed");
    (StatusCode::INTERNAL_SERVER_ERROR, Json(MemoryError { error: e.to_string() }))
}

fn not_found(msg: &str) -> (StatusCode, Json<MemoryError>) {
    (StatusCode::NOT_FOUND, Json(MemoryError { error: msg.into() }))
}

fn bad_request(msg: &str) -> (StatusCode, Json<MemoryError>) {
    (StatusCode::BAD_REQUEST, Json(MemoryError { error: msg.into() }))
}

// ── Request / Response types ─────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateMemoryRequest {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMemoryRequest {
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct MemoryError {
    pub error: String,
}

// ── Route handlers ───────────────────────────────────────────────

/// GET /memory/{user_id} — list all facts for a user.
pub async fn list_memories(
    State(ctx): State<SharedContext>,
    Path(user_id): Path<String>,
) -> Result<Json<Vec<MemoryFact>>, (StatusCode, Json<MemoryError>)> {
    ctx.memory.list(&user_id).map(Json).map_err(mem_err)
}

/// POST /memory/{user_id} — create a new fact for a user.
pub async fn create_memory(
    State(ctx): State<SharedContext>,
    Path(user_id): Path<String>,
    Json(body): Json<CreateMemoryRequest>,
) -> Result<(StatusCode, Json<MemoryFact>), (StatusCode, Json<MemoryError>)> {
    let key = body.key.trim().to_owned();
    let value = body.value.trim().to_owned();

    if key.is_empty() || value.is_empty() {
        return Err(bad_request("key and value must not be empty"));
    }

    ctx.memory.set(&user_id, &key, &value, "manual").map_err(mem_err)?;

    // Read back the inserted fact.
    let facts = ctx.memory.list(&user_id).map_err(mem_err)?;
    let fact = facts.into_iter().find(|f| f.key == key).ok_or_else(|| {
        mem_err("fact not found after insert")
    })?;

    Ok((StatusCode::CREATED, Json(fact)))
}

/// PUT /memory/{user_id}/{key} — update an existing fact's value.
pub async fn update_memory(
    State(ctx): State<SharedContext>,
    Path((user_id, key)): Path<(String, String)>,
    Json(body): Json<UpdateMemoryRequest>,
) -> Result<Json<MemoryFact>, (StatusCode, Json<MemoryError>)> {
    let value = body.value.trim().to_owned();

    if value.is_empty() {
        return Err(bad_request("value must not be empty"));
    }

    ctx.memory.set(&user_id, &key, &value, "manual").map_err(mem_err)?;

    // Read back the updated fact.
    let facts = ctx.memory.list(&user_id).map_err(mem_err)?;
    let fact = facts.into_iter().find(|f| f.key == key).ok_or_else(|| {
        not_found("fact not found")
    })?;

    Ok(Json(fact))
}

/// DELETE /memory/{user_id}/{key} — delete a single fact.
pub async fn delete_memory(
    State(ctx): State<SharedContext>,
    Path((user_id, key)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<MemoryError>)> {
    let deleted = ctx.memory.delete(&user_id, &key).map_err(mem_err)?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(not_found("fact not found"))
    }
}
