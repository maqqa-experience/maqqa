import { invoke } from "@tauri-apps/api/core";
import type { EditResponse, FileEntry, StatusResponse } from "./types";

export async function getStatus(): Promise<StatusResponse> {
  return invoke<StatusResponse>("get_status");
}

export async function openWorkspacePath(
  path: string,
): Promise<{ path: string; files: FileEntry[] }> {
  return invoke("open_workspace", { path });
}

export async function readFile(path: string): Promise<{ path: string; content: string }> {
  return invoke("read_file", { path });
}

export async function writeFile(path: string, content: string): Promise<void> {
  await invoke("write_file", { path, content });
}

export async function runEdit(args: {
  path: string;
  content: string;
  selection: string | null;
  prompt: string;
  intent: string;
}): Promise<EditResponse> {
  return invoke<EditResponse>("run_edit", args);
}

export async function applyEdit(files: { path: string; content: string }[]): Promise<{
  paths: string[];
  applied: boolean;
}> {
  return invoke("apply_edit", { files, path: null, content: null });
}

export async function getCompletions(args: {
  path: string;
  content: string;
  cursorLine: number;
  cursorColumn: number;
  prefix: string;
  requestId: string;
}): Promise<{ text: string; cancelled: boolean; request_id: string }> {
  return invoke("get_completions", args);
}

export async function cancelCompletion(requestId: string): Promise<void> {
  await invoke("cancel_completion", { requestId });
}

export type DemoConfig = {
  workspace: string | null;
  prompts_file: string | null;
  from: number;
  to: number;
  pause_ms: number;
  type_ms: number;
};

export async function demoConfig(): Promise<DemoConfig> {
  return invoke<DemoConfig>("demo_config");
}

export async function readHostFile(path: string): Promise<string> {
  return invoke<string>("read_host_file", { path });
}
