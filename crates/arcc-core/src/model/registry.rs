use std::collections::HashMap;
use std::sync::Arc;

use super::provider::ModelProvider;

/// Registry of model providers, keyed by display name.
///
/// Supports dual-model dispatch: "pro" for complex reasoning,
/// "flash" for fast chat and context compression.
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn ModelProvider>>,
    pro_model: String,
    flash_model: String,
}

impl ProviderRegistry {
    pub fn new(pro_model: &str, flash_model: &str) -> Self {
        Self {
            providers: HashMap::new(),
            pro_model: pro_model.to_owned(),
            flash_model: flash_model.to_owned(),
        }
    }

    /// Register a provider under the given name.
    pub fn register(&mut self, name: &str, provider: Arc<dyn ModelProvider>) {
        self.providers.insert(name.to_owned(), provider);
    }

    /// Resolve a provider by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn ModelProvider>> {
        self.providers.get(name)
    }

    /// Get the registered pro (complex reasoning) provider.
    pub fn pro(&self) -> Option<&Arc<dyn ModelProvider>> {
        self.providers.get(&self.pro_model)
    }

    /// Get the registered flash (fast chat) provider.
    pub fn flash(&self) -> Option<&Arc<dyn ModelProvider>> {
        self.providers.get(&self.flash_model)
    }

    /// Pick pro or flash based on task complexity heuristic.
    ///
    /// When tools are needed, always use flash (`deepseek-chat`) since it
    /// has the most reliable streaming tool-calling support.
    pub fn pick(&self, prompt_len: usize, has_tools: bool) -> Option<&Arc<dyn ModelProvider>> {
        if has_tools {
            self.flash()
        } else if prompt_len > 256 {
            self.pro()
        } else {
            self.flash()
        }
    }

    /// Number of registered providers.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// True if no providers registered.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}
