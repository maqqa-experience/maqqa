export type FileEntry = { path: string; is_dir: boolean };

export type StatusResponse = {
  workspace: string | null;
  ghost_model: string;
  reason_model: string;
  index_chunks: number;
  index_ready: boolean;
  last_heal: {
    success: boolean;
    message: string;
    attempts: number;
    path: string;
  } | null;
  ollama_ok: boolean;
};

export type FileChange = {
  path: string;
  original: string;
  proposed: string;
  diff: string;
};

export type EditResponse = {
  path: string;
  original: string;
  proposed: string;
  diff: string;
  files: FileChange[];
  healed: boolean;
  attempts: number;
  diagnostics: string | null;
  error: string | null;
  summary?: string | null;
  log?: string[];
};

export type TreeNode = {
  name: string;
  path: string;
  isDir: boolean;
  children: TreeNode[];
};
