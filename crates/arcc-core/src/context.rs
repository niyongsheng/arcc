use std::sync::Arc;
use tokio::sync::RwLock;

use crate::feishu_client::FeishuClient;
use crate::memory::MemoryManager;
use crate::model::registry::ProviderRegistry;
use crate::mcp::scheduler::McpScheduler;
use crate::safety::allowlist::Allowlist;
use crate::session::SessionManager;

/// Shared application context passed to all run modes.
pub struct AppContext {
    pub providers: ProviderRegistry,
    pub sessions: SessionManager,
    pub storage: arcc_storage::ArccStorage,
    pub memory: MemoryManager,
    pub feishu_client: Option<FeishuClient>,
    pub mcp: McpScheduler,
    pub dangerously_skip_permissions: bool,
    /// Runtime command allowlist, modifiable via interactive prompts.
    pub allowlist: RwLock<Allowlist>,
    /// Project-level instructions loaded from ARCC.md in repo root.
    pub project_instructions: RwLock<Option<String>>,
}

impl AppContext {
    pub fn new(
        providers: ProviderRegistry,
        storage: arcc_storage::ArccStorage,
        dangerously_skip_permissions: bool,
    ) -> Self {
        let context_max = storage.config.model.context_max_tokens;
        let allowlist = Allowlist::new(
            storage.config.safety.require_human_confirm.clone(),
        );
        // Share the DB connection so Session / SessionManager persist to SQLite.
        let db = storage.db.clone();
        let flash = providers
            .flash()
            .expect("flash provider required for memory system")
            .clone();
        let memory = MemoryManager::new(db.clone(), flash);

        // Initialise Feishu API client if enabled in config.
        let feishu_client = if storage.config.feishu.enabled {
            Some(FeishuClient::new(
                storage.config.feishu.app_id.clone(),
                storage.config.feishu.app_secret.clone(),
            ))
        } else {
            None
        };

        Self {
            providers,
            sessions: SessionManager::with_db(context_max, db),
            storage,
            memory,
            feishu_client,
            mcp: McpScheduler::new(),
            dangerously_skip_permissions,
            project_instructions: tokio::sync::RwLock::new(None),
            allowlist: RwLock::new(allowlist),
        }
    }
}

pub type SharedContext = Arc<AppContext>;
