use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenWorkspaceRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenWorkspaceResponse {
    pub path: String,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFileRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFileResponse {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteFileRequest {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteFileResponse {
    pub path: String,
    pub bytes_written: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub path: String,
    pub content: String,
    pub cursor_line: usize,
    pub cursor_column: usize,
    pub prefix: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub request_id: String,
    pub text: String,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelCompletionRequest {
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditRequest {
    /// Hint / primary path. May be empty for open-ended multi-file creates.
    pub path: String,
    pub content: String,
    pub selection: Option<String>,
    pub prompt: String,
    pub intent: EditIntent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EditIntent {
    Inline,
    Selection,
    Architecture,
    /// Create a new file from scratch (content may be empty).
    Create,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub original: String,
    pub proposed: String,
    pub diff: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditResponse {
    /// Primary file (first change) for simple UI bindings.
    pub path: String,
    pub original: String,
    pub proposed: String,
    pub diff: String,
    /// All created/edited files for this request.
    pub files: Vec<FileChange>,
    pub healed: bool,
    pub attempts: u32,
    pub diagnostics: Option<String>,
    pub error: Option<String>,
    /// Human-readable summary of what the agent did.
    #[serde(default)]
    pub summary: Option<String>,
    /// Step-by-step agent activity log.
    #[serde(default)]
    pub log: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyEditRequest {
    /// Legacy single-file fields (optional when `files` is set).
    pub path: Option<String>,
    pub content: Option<String>,
    /// Preferred: apply every file in the bundle.
    pub files: Option<Vec<ApplyFile>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyEditResponse {
    pub paths: Vec<String>,
    pub applied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub workspace: Option<String>,
    pub ghost_model: String,
    pub reason_model: String,
    pub index_chunks: usize,
    pub index_ready: bool,
    pub last_heal: Option<HealStatus>,
    pub ollama_ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealStatus {
    pub at: DateTime<Utc>,
    pub path: String,
    pub success: bool,
    pub attempts: u32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamTokenEvent {
    pub request_id: String,
    pub token: String,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunkDto {
    pub file_path: String,
    pub symbol_name: String,
    pub chunk_type: String,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCall {
    pub tool: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    pub ok: bool,
    pub output: String,
}
