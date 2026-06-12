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

    /// Pick pro or flash based on task complexity.
    ///
    /// Uses a hybrid heuristic:
    /// - Prompt contains complexity keywords (分析, debug, optimize, why, 根因, …) → Pro
    /// - Long prompt (>256 chars) → Pro
    /// - Everything else → Flash
    ///
    /// The caller can always override by calling `.pro()` or `.flash()` directly.
    pub fn pick(&self, prompt: &str, _has_tools: bool) -> Option<&Arc<dyn ModelProvider>> {
        /// Keywords suggesting the user needs deep reasoning.
        const COMPLEXITY_KEYWORDS: &[&str] = &[
            // Chinese
            "分析", "为什么", "根因", "调试", "优化", "设计", "架构",
            "比较", "解释", "如何实现", "对比", "总结", "评估", "预测",
            "推理", "规划", "方案", "原理", "机制", "区别",
            // English
            "analyze", "why", "root cause", "debug", "optimize",
            "design", "architecture", "refactor", "explain",
            "compare", "performance", "security",
        ];

        let lower = prompt.to_lowercase();
        let has_complex_keyword = COMPLEXITY_KEYWORDS
            .iter()
            .any(|k| lower.contains(k));

        if prompt.len() > 256 || (prompt.len() > 30 && has_complex_keyword) {
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
