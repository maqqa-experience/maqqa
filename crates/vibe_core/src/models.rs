//! Canonical Ollama model tags for the 96GB Mac Studio dual-model setup.

/// Tier 1 — ghost-text / inline completions (~8GB at Q8).
pub const GHOST_MODEL_DEFAULT: &str = "qwen2.5-coder:7b-instruct-q8_0";

/// Tier 2/3 primary — multi-file refactor + shadow heal (~50–65GB).
pub const REASON_MODEL_PRIMARY: &str = "deepseek-coder-v2:236b-instruct-q2_K";

/// Tier 2/3 fallback when 236B is unavailable.
pub const REASON_MODEL_FALLBACK: &str = "llama3.1:70b-instruct-q4_K_M";

/// Local embedding model via Ollama.
pub const EMBED_MODEL_DEFAULT: &str = "nomic-embed-text";

/// Ghost-text first-token latency targets (documented for Mac Studio).
pub const GHOST_P95_FIRST_TOKEN_MS: u64 = 100;
pub const GHOST_UI_PAINT_AFTER_TOKEN_MS: u64 = 30;
