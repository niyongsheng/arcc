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
    ctx.memory.list(&user_id).map(Json).map_err(|e| {
        error!(err = %e, "failed to list memories");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MemoryError {
                error: e.to_string(),
            }),
        )
    })
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
        return Err((
            StatusCode::BAD_REQUEST,
            Json(MemoryError {
                error: "key and value must not be empty".into(),
            }),
        ));
    }

    ctx.memory.set(&user_id, &key, &value, "manual").map_err(|e| {
        error!(err = %e, "failed to create memory");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MemoryError {
                error: e.to_string(),
            }),
        )
    })?;

    // Read back the inserted fact.
    let facts = ctx.memory.list(&user_id).map_err(|e| {
        error!(err = %e, "failed to read back memory after insert");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MemoryError {
                error: e.to_string(),
            }),
        )
    })?;
    let fact = facts.into_iter().find(|f| f.key == key).ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MemoryError {
                error: "fact not found after insert".into(),
            }),
        )
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
        return Err((
            StatusCode::BAD_REQUEST,
            Json(MemoryError {
                error: "value must not be empty".into(),
            }),
        ));
    }

    ctx.memory.set(&user_id, &key, &value, "manual").map_err(|e| {
        error!(err = %e, "failed to update memory");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MemoryError {
                error: e.to_string(),
            }),
        )
    })?;

    // Read back the updated fact.
    let facts = ctx.memory.list(&user_id).map_err(|e| {
        error!(err = %e, "failed to read back memory after update");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MemoryError {
                error: e.to_string(),
            }),
        )
    })?;
    let fact = facts.into_iter().find(|f| f.key == key).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(MemoryError {
                error: "fact not found".into(),
            }),
        )
    })?;

    Ok(Json(fact))
}

/// DELETE /memory/{user_id}/{key} — delete a single fact.
pub async fn delete_memory(
    State(ctx): State<SharedContext>,
    Path((user_id, key)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<MemoryError>)> {
    let deleted = ctx.memory.delete(&user_id, &key).map_err(|e| {
        error!(err = %e, "failed to delete memory");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MemoryError {
                error: e.to_string(),
            }),
        )
    })?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(MemoryError {
                error: "fact not found".into(),
            }),
        ))
    }
}
