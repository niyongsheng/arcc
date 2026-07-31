//! Outbound permission requests.
//!
//! `session/request_permission` is an agent→client *method call*: the
//! prompt loop sends it with a generated request id and blocks on the
//! response. The stdin read loop routes any incoming message whose id
//! matches a registered request back through its oneshot here.

use std::collections::HashMap;

use serde_json::Value;
use tokio::sync::{oneshot, RwLock};

/// Registry of in-flight permission requests, keyed by outbound request id.
#[derive(Default)]
pub struct PermRegistry {
    pending: RwLock<HashMap<String, oneshot::Sender<Value>>>,
}

impl PermRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the response channel for a request about to be sent.
    /// The caller must send the `session/request_permission` message with
    /// `id = request_id` immediately after.
    pub async fn register(&self, request_id: String, tx: oneshot::Sender<Value>) {
        self.pending.write().await.insert(request_id, tx);
    }

    /// Route an incoming client response to its pending request.
    ///
    /// Returns `true` if `id` matched a registered permission request
    /// (the response was consumed); `false` for unknown/foreign ids,
    /// which the caller can ignore or log.
    pub async fn resolve(&self, id: &Value, result: Option<Value>) -> bool {
        let request_id = match id.as_str() {
            Some(s) => s,
            None => return false,
        };
        let tx = self.pending.write().await.remove(request_id);
        match tx {
            Some(tx) => {
                let outcome = result.unwrap_or(Value::Null);
                let _ = tx.send(outcome);
                true
            }
            None => false,
        }
    }

    /// Fail every pending request (e.g. stdin closed). Prompt loops wake
    /// with `Value::Null`, which the permission parser treats as "reject".
    pub async fn cancel_all(&self) {
        let mut map = self.pending.write().await;
        for (_, tx) in map.drain() {
            let _ = tx.send(Value::Null);
        }
    }
}
