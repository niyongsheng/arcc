//! ACP session state — wraps a core `Session` (history + SQLite
//! persistence) plus ACP-specific bookkeeping: working directory,
//! cancellation token, running flag and model preference.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use arcc_core::context::SharedContext;
use arcc_core::session::Session;

/// Per-session state for one ACP session.
pub struct AcpSessionHandle {
    /// Core conversation session (message history + persistence).
    pub session: Arc<RwLock<Session>>,
    /// Working directory from `session/new` (defaults to process cwd).
    pub cwd: PathBuf,
    /// Cancellation token for the *currently running* prompt, if any.
    /// Rebuilt at every `session/prompt` start; `session/cancel` (and
    /// `session/close`) cancel it, which tears down the stream and any
    /// in-flight tool process.
    pub cancel: RwLock<CancellationToken>,
    /// True while a prompt is executing — guards concurrent `session/prompt`
    /// calls (rejected with -32004). Swap-once makes the check atomic.
    pub running: AtomicBool,
    /// Model preference set via `session/set_config_option`:
    /// `Some("pro")` / `Some("flash")` / `None` (default = flash).
    pub model_pref: RwLock<Option<String>>,
}

impl AcpSessionHandle {
    fn new(session: Arc<RwLock<Session>>, cwd: PathBuf) -> Self {
        Self {
            session,
            cwd,
            cancel: RwLock::new(CancellationToken::new()),
            running: AtomicBool::new(false),
            model_pref: RwLock::new(None),
        }
    }

    /// The ACP session id (== core session id).
    pub async fn id(&self) -> String {
        self.session.read().await.id.clone()
    }

    /// Cancel the in-flight prompt, if any. The next `session/prompt`
    /// starts with a fresh token, so cancels never leak across turns.
    pub async fn cancel(&self) {
        self.cancel.read().await.cancel();
    }

    /// Replace the cancellation token and return it — called at the start
    /// of every prompt run so this run observes its own (uncancelled) token.
    pub async fn fresh_cancel_token(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self.cancel.write().await = token.clone();
        token
    }
}

/// Registry of live ACP sessions, keyed by session id.
#[derive(Default)]
pub struct AcpSessionRegistry {
    sessions: RwLock<HashMap<String, Arc<AcpSessionHandle>>>,
}

impl AcpSessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new ACP session (backed by a core `Session`).
    pub async fn create(&self, ctx: &SharedContext, cwd: PathBuf) -> Arc<AcpSessionHandle> {
        let core_session = ctx.sessions.create("acp", "acp").await;
        let id = core_session.read().await.id.clone();
        let handle = Arc::new(AcpSessionHandle::new(core_session, cwd));
        self.sessions
            .write()
            .await
            .insert(id.clone(), Arc::clone(&handle));
        tracing::info!(%id, cwd = %handle.cwd.display(), "acp session created");
        handle
    }

    pub async fn get(&self, id: &str) -> Option<Arc<AcpSessionHandle>> {
        self.sessions.read().await.get(id).cloned()
    }

    /// Remove a session: cancel any in-flight prompt, drop the ACP handle
    /// and the underlying core session.
    pub async fn remove(&self, ctx: &SharedContext, id: &str) -> Option<Arc<AcpSessionHandle>> {
        let handle = self.sessions.write().await.remove(id);
        if let Some(h) = &handle {
            h.cancel().await;
            ctx.sessions.remove(id).await;
        }
        handle
    }
}
