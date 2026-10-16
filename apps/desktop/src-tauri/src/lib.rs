use std::sync::Arc;
use tauri::State;
use vibe_core::Engine;
use vibe_ipc::*;

struct AppState {
    engine: Arc<Engine>,
}

#[tauri::command]
async fn open_workspace(
    state: State<'_, AppState>,
    path: String,
) -> Result<OpenWorkspaceResponse, String> {
    state
        .engine
        .open_workspace(&path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn read_file(state: State<'_, AppState>, path: String) -> Result<ReadFileResponse, String> {
    state
        .engine
        .read_file(&path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn write_file(
    state: State<'_, AppState>,
    path: String,
    content: String,
) -> Result<WriteFileResponse, String> {
    state
        .engine
        .write_file(&path, &content)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_completions(
    state: State<'_, AppState>,
    path: String,
    content: String,
    cursor_line: usize,
    cursor_column: usize,
    prefix: String,
    request_id: String,
) -> Result<CompletionResponse, String> {
    let req = CompletionRequest {
        path,
        content,
        cursor_line,
        cursor_column,
        prefix,
        request_id,
    };
    state
        .engine
        .get_completions(req)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn cancel_completion(state: State<'_, AppState>, request_id: String) {
    state.engine.cancel_completion(&request_id);
}

#[tauri::command]
async fn run_edit(
    state: State<'_, AppState>,
    path: String,
    content: String,
    selection: Option<String>,
    prompt: String,
    intent: EditIntent,
) -> Result<EditResponse, String> {
    let req = EditRequest {
        path,
        content,
        selection,
        prompt,
        intent,
    };
    state.engine.run_edit(req).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn apply_edit(
    state: State<'_, AppState>,
    path: Option<String>,
    content: Option<String>,
    files: Option<Vec<ApplyFile>>,
) -> Result<ApplyEditResponse, String> {
    state
        .engine
        .apply_edit(ApplyEditRequest {
            path,
            content,
            files,
        })
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_status(state: State<'_, AppState>) -> Result<StatusResponse, String> {
    Ok(state.engine.status().await)
}

#[tauri::command]
async fn mcp_call(
    state: State<'_, AppState>,
    tool: String,
    args: serde_json::Value,
) -> Result<McpToolResult, String> {
    state
        .engine
        .mcp_call(&tool, args)
        .await
        .map_err(|e| e.to_string())
}

/// Demo / recording boot config from env (see scripts/record_vibe_demo.sh).
#[derive(serde::Serialize)]
struct DemoConfig {
    workspace: Option<String>,
    prompts_file: Option<String>,
    from: u32,
    to: u32,
    /// ms pause after each prompt before the next (default 2500)
    pause_ms: u64,
    /// ms between typed characters in the prompt box (default 8)
    type_ms: u64,
}

#[tauri::command]
fn demo_config() -> DemoConfig {
    let parse_u32 = |k: &str, default: u32| {
        std::env::var(k)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    };
    let parse_u64 = |k: &str, default: u64| {
        std::env::var(k)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    };
    DemoConfig {
        workspace: std::env::var("VIBE_DEMO_WORKSPACE")
            .ok()
            .filter(|s| !s.is_empty()),
        prompts_file: std::env::var("VIBE_DEMO_PROMPTS_FILE")
            .ok()
            .filter(|s| !s.is_empty()),
        from: parse_u32("VIBE_DEMO_FROM", 1),
        to: parse_u32("VIBE_DEMO_TO", 3),
        pause_ms: parse_u64("VIBE_DEMO_PAUSE_MS", 2500),
        type_ms: parse_u64("VIBE_DEMO_TYPE_MS", 8),
    }
}

#[tauri::command]
fn read_host_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let engine = runtime
        .block_on(Engine::bootstrap())
        .expect("engine bootstrap");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState { engine })
        .invoke_handler(tauri::generate_handler![
            open_workspace,
            read_file,
            write_file,
            get_completions,
            cancel_completion,
            run_edit,
            apply_edit,
            get_status,
            mcp_call,
            demo_config,
            read_host_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
