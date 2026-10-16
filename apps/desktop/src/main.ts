import "./styles.css";
import { open } from "@tauri-apps/plugin-dialog";
import type { EditResponse, FileEntry } from "./types";
import * as ipc from "./ipc";
import { createMainEditor, monaco } from "./monaco/setup";
import { mountWorkbench, wireResizeHandles } from "./ui/layout";
import { renderExplorer, setExplorerActive } from "./ui/explorer";
import { TabManager } from "./ui/tabs";
import { applyStatus, setCursorStatus, setLlmBusy } from "./ui/statusBar";
import { showDiffReview, hideDiffReview } from "./ui/diffReview";
import { focusAgentPrompt, setAgentStatus, setAcceptRejectEnabled } from "./ui/agentPanel";

const app = document.querySelector("#app")!;
mountWorkbench(app);
const workbench = document.getElementById("workbench")!;
wireResizeHandles(workbench);

const editor = createMainEditor(document.getElementById("editor")!);
const tabs = new TabManager(
  editor,
  document.getElementById("tab-bar")!,
  document.getElementById("breadcrumbs")!,
  {
    onActivate: (path) => {
      currentPath = path;
      setExplorerActive(document.getElementById("file-list")!, path);
      const gf = document.getElementById("cmdk-greenfield") as HTMLInputElement;
      if (!gf.checked) {
        (document.getElementById("cmdk-path") as HTMLInputElement).value = path;
      }
      const model = editor.getModel();
      setCursorStatus(1, 1, model?.getLanguageId() ?? "plaintext");
      hideDiffReview();
    },
    onClose: () => {
      /* explorer highlight updated on next activate */
    },
  },
);

let currentPath: string | null = null;
let workspacePath: string | null = null;
let workspaceFiles: FileEntry[] = [];
let pendingEdit: EditResponse | null = null;
let completionSeq = 0;
let debounceTimer: number | undefined;
const pendingCancels = new Set<string>();
let panelTab: "output" | "diff" = "output";

function normalizeRelPath(p: string): string {
  return p.trim().replace(/^\/+/, "").replace(/\\/g, "/");
}

function guessPathFromPrompt(prompt: string): string | null {
  const m = prompt.match(
    /\b(?:create|new|write|add)\b[^.\n]*?\b([\w./-]+\.(?:py|js|ts|tsx|rs|go|java|md))\b/i,
  );
  return m ? normalizeRelPath(m[1]) : null;
}

function setPanelContent(output: string, diffText?: string): void {
  document.getElementById("panel-output")!.textContent = output;
  if (diffText !== undefined) {
    document.getElementById("panel-diff")!.textContent = diffText;
  }
  showPanelTab(panelTab);
  workbench.classList.remove("panel-collapsed");
}

function showPanelTab(which: "output" | "diff"): void {
  panelTab = which;
  document.querySelectorAll(".panel-tab").forEach((el) => {
    el.classList.toggle("active", (el as HTMLElement).dataset.panel === which);
  });
  document.getElementById("panel-output")!.hidden = which !== "output";
  document.getElementById("panel-diff")!.hidden = which !== "diff";
}

function ensureAgentOpen(): void {
  workbench.classList.remove("agent-collapsed");
  document.getElementById("act-agent")!.classList.add("active");
}

function toggleAgent(): void {
  workbench.classList.toggle("agent-collapsed");
  const open = !workbench.classList.contains("agent-collapsed");
  document.getElementById("act-agent")!.classList.toggle("active", open);
  if (open) focusAgentPrompt();
}

function toggleSidebar(): void {
  workbench.classList.toggle("sidebar-collapsed");
  const open = !workbench.classList.contains("sidebar-collapsed");
  document.getElementById("act-explorer")!.classList.toggle("active", open);
}

function togglePanel(): void {
  workbench.classList.toggle("panel-collapsed");
}

async function refreshStatus(): Promise<void> {
  try {
    const st = await ipc.getStatus();
    applyStatus(st);
    if (st.workspace) workspacePath = st.workspace;
  } catch {
    /* backend may not be ready */
  }
}

async function openWorkspace(): Promise<void> {
  const selected = await open({ directory: true, multiple: false });
  if (!selected || typeof selected !== "string") return;
  const res = await ipc.openWorkspacePath(selected);
  workspacePath = res.path;
  workspaceFiles = res.files;
  document.getElementById("ws-label")!.textContent = res.path;
  renderExplorer(document.getElementById("file-list")!, res.files, currentPath, {
    onOpenFile: (p) => void openFile(p),
  });
  startGreenfield();
  const first = res.files.find((f) => !f.is_dir);
  if (first) {
    (document.getElementById("cmdk-greenfield") as HTMLInputElement).checked = true;
    await openFile(first.path);
    (document.getElementById("cmdk-path") as HTMLInputElement).value = "";
    setAgentStatus(
      "Greenfield on — Run edit creates new files (uncheck to edit the open file)",
    );
  } else {
    setAgentStatus("Empty folder — describe what to create, then Run edit");
  }
  workbench.classList.remove("sidebar-collapsed");
  document.getElementById("act-explorer")!.classList.add("active");
  await refreshStatus();
}

async function refreshFileList(): Promise<void> {
  if (!workspacePath) return;
  const res = await ipc.openWorkspacePath(workspacePath);
  workspaceFiles = res.files;
  renderExplorer(document.getElementById("file-list")!, res.files, currentPath, {
    onOpenFile: (p) => void openFile(p),
  });
}

async function openFile(path: string): Promise<void> {
  const res = await ipc.readFile(path);
  currentPath = path;
  tabs.open(path, res.content, true);
  const greenfield = (document.getElementById("cmdk-greenfield") as HTMLInputElement).checked;
  if (!greenfield) {
    (document.getElementById("cmdk-path") as HTMLInputElement).value = path;
  }
  setExplorerActive(document.getElementById("file-list")!, path);
  hideDiffReview();
}

function startGreenfield(): void {
  const pathInput = document.getElementById("cmdk-path") as HTMLInputElement;
  const gf = document.getElementById("cmdk-greenfield") as HTMLInputElement;
  const auto = document.getElementById("cmdk-auto-accept") as HTMLInputElement;
  pathInput.value = "";
  gf.checked = true;
  auto.checked = true;
  ensureAgentOpen();
  setAgentStatus("Greenfield mode — describe what to create, then Run edit");
  focusAgentPrompt();
}

async function saveFile(): Promise<void> {
  const path = tabs.activePath;
  if (!path) return;
  const content = tabs.value;
  await ipc.writeFile(path, content);
  tabs.markSaved(path, content);
  await refreshStatus();
}

/* —— Ghost completions —— */
monaco.languages.registerInlineCompletionsProvider({ pattern: "**" }, {
  provideInlineCompletions: async (model, position, _ctx, token) => {
    if (!tabs.activePath) return { items: [] };
    const requestId = `c-${++completionSeq}`;
    for (const id of pendingCancels) {
      void ipc.cancelCompletion(id);
    }
    pendingCancels.clear();
    pendingCancels.add(requestId);

    return await new Promise((resolve) => {
      window.clearTimeout(debounceTimer);
      debounceTimer = window.setTimeout(async () => {
        if (token.isCancellationRequested) {
          await ipc.cancelCompletion(requestId);
          resolve({ items: [] });
          return;
        }
        const range = new monaco.Range(1, 1, position.lineNumber, position.column);
        const prefix = model.getValueInRange(range);
        try {
          const res = await ipc.getCompletions({
            path: tabs.activePath!,
            content: model.getValue(),
            cursorLine: position.lineNumber,
            cursorColumn: position.column,
            prefix,
            requestId,
          });
          pendingCancels.delete(requestId);
          if (res.cancelled || !res.text) {
            resolve({ items: [] });
            return;
          }
          resolve({
            items: [
              {
                insertText: res.text,
                range: new monaco.Range(
                  position.lineNumber,
                  position.column,
                  position.lineNumber,
                  position.column,
                ),
              },
            ],
          });
        } catch {
          resolve({ items: [] });
        }
      }, 80);
    });
  },
  freeInlineCompletions: () => {},
});

editor.onDidChangeCursorPosition((e) => {
  const model = editor.getModel();
  setCursorStatus(
    e.position.lineNumber,
    e.position.column,
    model?.getLanguageId() ?? "plaintext",
  );
});

/* —— Agent run —— */
async function runCmdK(): Promise<void> {
  ensureAgentOpen();
  workbench.classList.remove("panel-collapsed");
  const status = document.getElementById("cmdk-status")!;
  const pathInput = document.getElementById("cmdk-path") as HTMLInputElement;

  if (!workspacePath) {
    status.textContent = "Open a folder first";
    return;
  }

  const prompt = (document.getElementById("cmdk-prompt") as HTMLTextAreaElement).value.trim();
  if (!prompt) {
    status.textContent = "Enter a prompt describing the change or new file";
    return;
  }

  const greenfield = (document.getElementById("cmdk-greenfield") as HTMLInputElement).checked;
  const pathHint =
    normalizeRelPath(pathInput.value) || (greenfield ? "" : guessPathFromPrompt(prompt) || "");

  const editingOpenFile =
    !greenfield && !!pathHint && !!tabs.activePath && tabs.activePath === pathHint;
  let creating = greenfield || !editingOpenFile;
  let buffer = "";
  let selection: string | null = null;

  if (editingOpenFile) {
    creating = false;
    buffer = tabs.value;
    const sel = editor.getSelection();
    selection = sel && !sel.isEmpty() ? editor.getModel()!.getValueInRange(sel) : null;
    try {
      await ipc.writeFile(pathHint, buffer);
      tabs.markSaved(pathHint, buffer);
    } catch (e) {
      status.textContent = `Save before edit failed: ${String(e)}`;
      return;
    }
  } else {
    creating = true;
    buffer = "";
  }

  status.textContent = creating
    ? "Agent running: observe → critique → act until intent met…"
    : "Agent/LLM running…";
  setLlmBusy(true, creating ? "agent-create" : "agent-edit");
  setPanelContent(
    "(agentic loop — multiple Ollama calls; watch status: llm: calling…)",
    "",
  );
  showPanelTab("output");
  setAcceptRejectEnabled(false, false);
  (document.getElementById("cmdk-run") as HTMLButtonElement).disabled = true;
  hideDiffReview();

  const intent = creating
    ? "create"
    : /multi[- ]?file|architect|refactor (the )?project|across (the )?codebase/i.test(prompt)
      ? "architecture"
      : "selection";

  try {
    const res = await ipc.runEdit({
      path: creating ? pathHint || "" : pathHint,
      content: buffer,
      selection,
      prompt,
      intent,
    });
    pendingEdit = res;
    const fileList = (res.files?.length ? res.files : null)
      ?.map((f) => `• ${f.path}`)
      .join("\n");
    const activityLog = res.log?.length
      ? `AGENT LOG\n${res.log.map((l) => `  ${l}`).join("\n")}`
      : null;
    const body = [
      res.summary ? `WHAT THE AGENT DID\n${res.summary}` : null,
      activityLog,
      fileList ? `FILES (${res.files.length}):\n${fileList}` : null,
      res.error ? `ERROR: ${res.error}` : null,
      res.diagnostics ? `Diagnostics:\n${res.diagnostics}` : null,
    ]
      .filter(Boolean)
      .join("\n\n");
    const diffText = res.diff || res.proposed || "(empty model response)";
    setPanelContent(body || "(no summary)", diffText);
    if (res.summary) console.info("[vibe] agent summary:\n", res.summary);

    const hasFiles =
      (res.files?.some((f) => !!f.proposed) ?? false) || !!res.proposed;
    setAcceptRejectEnabled(hasFiles, true);
    const n =
      res.files?.filter((f) => !!f.proposed).length || (res.proposed ? 1 : 0);

    // Show Monaco diff for primary file under review
    if (hasFiles) {
      const primary =
        res.files?.find((f) => !!f.proposed) ??
        (res.proposed
          ? { path: res.path, original: res.original, proposed: res.proposed, diff: res.diff }
          : null);
      if (primary) {
        showDiffReview(primary.path, primary.original || "", primary.proposed);
        showPanelTab("diff");
      }
    }

    const autoAccept =
      (document.getElementById("cmdk-auto-accept") as HTMLInputElement).checked &&
      (creating || intent === "architecture" || intent === "create");

    if (hasFiles && autoAccept) {
      status.textContent = `Auto-accepting ${n} file(s)…`;
      await acceptEdit();
      await refreshStatus();
      return;
    }

    status.textContent = res.error
      ? `Agent finished with issues after ${res.attempts} turn(s) — ${n} file(s); Accept enabled`
      : res.healed
        ? `Verified ${n} file(s) in ${res.attempts} turn(s) — Accept to write`
        : n
          ? `Draft ready — Accept writes ${n} file(s)`
          : "Empty response from model";
  } catch (e) {
    pendingEdit = null;
    status.textContent = `run_edit failed: ${String(e)}`;
    setPanelContent(String(e), String(e));
  } finally {
    setLlmBusy(false);
    (document.getElementById("cmdk-run") as HTMLButtonElement).disabled = false;
  }
  await refreshStatus();
}

async function acceptEdit(): Promise<void> {
  if (!pendingEdit) return;
  const bundle =
    pendingEdit.files?.length > 0
      ? pendingEdit.files
          .filter((f) => !!f.proposed)
          .map((f) => ({ path: f.path, content: f.proposed }))
      : pendingEdit.proposed
        ? [{ path: pendingEdit.path, content: pendingEdit.proposed }]
        : [];
  if (!bundle.length) {
    setAgentStatus("Nothing to accept — agent returned no file contents");
    setAcceptRejectEnabled(false, true);
    return;
  }

  try {
    const applied = await ipc.applyEdit(bundle);
    const primary = applied.paths[0] || pendingEdit.path;
    const summary = pendingEdit.summary ?? null;
    const priorLog = pendingEdit.log ?? [];

    hideDiffReview();
    for (const f of bundle) {
      tabs.open(f.path, f.content, f.path === primary);
      tabs.markSaved(f.path, f.content);
    }
    currentPath = primary;
    (document.getElementById("cmdk-path") as HTMLInputElement).value = primary;
    (document.getElementById("cmdk-greenfield") as HTMLInputElement).checked = false;

    pendingEdit = null;
    setAcceptRejectEnabled(false, false);

    const report = [
      summary ? `WHAT THE AGENT DID\n${summary}` : null,
      priorLog.length ? `AGENT LOG\n${priorLog.map((l) => `  ${l}`).join("\n")}` : null,
      `Wrote ${applied.paths.length} file(s):`,
      ...applied.paths.map((p) => `✓ ${p}`),
    ]
      .filter(Boolean)
      .join("\n\n");
    setAgentStatus(
      summary
        ? `Done — ${applied.paths.length} file(s) written`
        : `Wrote ${applied.paths.length} file(s)`,
    );
    setPanelContent(report, "");
    showPanelTab("output");
    await refreshFileList();
    setExplorerActive(document.getElementById("file-list")!, primary);
  } catch (e) {
    setAgentStatus(`Accept failed: ${String(e)}`);
    setPanelContent(String(e));
    setAcceptRejectEnabled(true, true);
  }
}

function rejectEdit(): void {
  pendingEdit = null;
  hideDiffReview();
  setAcceptRejectEnabled(false, false);
  setPanelContent("(rejected)", "");
  setAgentStatus("Rejected");
}

/* —— Quick open (Cmd+P) —— */
function openQuickOpen(): void {
  const overlay = document.getElementById("quick-open")!;
  const input = document.getElementById("quick-open-input") as HTMLInputElement;
  overlay.classList.add("open");
  input.value = "";
  renderQuickOpen("");
  input.focus();
}

function closeQuickOpen(): void {
  document.getElementById("quick-open")!.classList.remove("open");
}

function renderQuickOpen(filter: string): void {
  const list = document.getElementById("quick-open-list")!;
  list.innerHTML = "";
  const q = filter.toLowerCase();
  const matches = workspaceFiles
    .filter((f) => !f.is_dir && (!q || f.path.toLowerCase().includes(q)))
    .slice(0, 40);
  matches.forEach((f, i) => {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = `quick-open-item${i === 0 ? " active" : ""}`;
    btn.textContent = f.path;
    btn.onclick = () => {
      closeQuickOpen();
      void openFile(f.path);
    };
    list.appendChild(btn);
  });
}

/* —— Events —— */
document.getElementById("open-ws")!.addEventListener("click", () => void openWorkspace());
document.getElementById("save-file")!.addEventListener("click", () => void saveFile());
document.getElementById("cmdk-new")!.addEventListener("click", (e) => {
  e.preventDefault();
  startGreenfield();
});
document.getElementById("cmdk-greenfield")!.addEventListener("change", () => {
  const gf = document.getElementById("cmdk-greenfield") as HTMLInputElement;
  const auto = document.getElementById("cmdk-auto-accept") as HTMLInputElement;
  if (gf.checked) {
    (document.getElementById("cmdk-path") as HTMLInputElement).value = "";
    auto.checked = true;
    setAgentStatus("Greenfield on — new files only (auto-accept on)");
  } else if (tabs.activePath) {
    (document.getElementById("cmdk-path") as HTMLInputElement).value = tabs.activePath;
    setAgentStatus(`Editing ${tabs.activePath}`);
  }
});
document.getElementById("cmdk-run")!.addEventListener("click", (e) => {
  e.preventDefault();
  void runCmdK();
});
document.getElementById("cmdk-accept")!.addEventListener("click", () => void acceptEdit());
document.getElementById("cmdk-reject")!.addEventListener("click", rejectEdit);

document.getElementById("act-explorer")!.addEventListener("click", () => {
  if (workbench.classList.contains("sidebar-collapsed")) {
    workbench.classList.remove("sidebar-collapsed");
    document.getElementById("act-explorer")!.classList.add("active");
  } else {
    toggleSidebar();
  }
});
document.getElementById("act-agent")!.addEventListener("click", () => toggleAgent());
document.getElementById("agent-collapse")!.addEventListener("click", () => {
  workbench.classList.add("agent-collapsed");
  document.getElementById("act-agent")!.classList.remove("active");
});
document.getElementById("panel-collapse")!.addEventListener("click", () => togglePanel());

document.querySelectorAll(".panel-tab").forEach((el) => {
  el.addEventListener("click", () => {
    const which = (el as HTMLElement).dataset.panel as "output" | "diff";
    showPanelTab(which);
  });
});

document.getElementById("quick-open-input")!.addEventListener("input", (e) => {
  renderQuickOpen((e.target as HTMLInputElement).value);
});
document.getElementById("quick-open-input")!.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    e.preventDefault();
    closeQuickOpen();
  }
  if (e.key === "Enter") {
    e.preventDefault();
    const first = document.querySelector(".quick-open-item") as HTMLButtonElement | null;
    first?.click();
  }
});
document.getElementById("quick-open")!.addEventListener("click", (e) => {
  if (e.target === document.getElementById("quick-open")) closeQuickOpen();
});

window.addEventListener("keydown", (e) => {
  const meta = e.metaKey || e.ctrlKey;
  if (meta && e.key.toLowerCase() === "k") {
    e.preventDefault();
    ensureAgentOpen();
    focusAgentPrompt();
  }
  if (meta && e.key.toLowerCase() === "s") {
    e.preventDefault();
    void saveFile();
  }
  if (meta && e.key.toLowerCase() === "b") {
    e.preventDefault();
    toggleSidebar();
  }
  if (meta && e.key.toLowerCase() === "j") {
    e.preventDefault();
    togglePanel();
  }
  if (meta && e.key.toLowerCase() === "p") {
    e.preventDefault();
    openQuickOpen();
  }
  if (e.key === "Escape") {
    if (document.getElementById("quick-open")!.classList.contains("open")) {
      closeQuickOpen();
      return;
    }
    (document.activeElement as HTMLElement)?.blur?.();
  }
});

void refreshStatus();
setInterval(() => void refreshStatus(), 5000);

/* —— Demo / video autoplay (VIBE_DEMO_* env) —— */
function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

async function typeIntoPrompt(text: string, typeMs: number): Promise<void> {
  const el = document.getElementById("cmdk-prompt") as HTMLTextAreaElement;
  el.value = "";
  el.focus();
  for (let i = 0; i < text.length; i++) {
    el.value = text.slice(0, i + 1);
    el.dispatchEvent(new Event("input", { bubbles: true }));
    if (typeMs > 0 && i % 2 === 0) await sleep(typeMs);
  }
  el.value = text;
}

async function openWorkspaceAt(path: string): Promise<void> {
  const res = await ipc.openWorkspacePath(path);
  workspacePath = res.path;
  workspaceFiles = res.files;
  document.getElementById("ws-label")!.textContent = res.path;
  renderExplorer(document.getElementById("file-list")!, res.files, currentPath, {
    onOpenFile: (p) => void openFile(p),
  });
  startGreenfield();
  const first = res.files.find((f) => !f.is_dir);
  if (first) {
    (document.getElementById("cmdk-greenfield") as HTMLInputElement).checked = true;
    await openFile(first.path);
    (document.getElementById("cmdk-path") as HTMLInputElement).value = "";
  }
  workbench.classList.remove("sidebar-collapsed");
  document.getElementById("act-explorer")!.classList.add("active");
  ensureAgentOpen();
  await refreshStatus();
}

async function runDemoIfConfigured(): Promise<void> {
  let cfg: ipc.DemoConfig;
  try {
    cfg = await ipc.demoConfig();
  } catch {
    return;
  }
  if (!cfg.workspace || !cfg.prompts_file) return;

  setAgentStatus(`Demo: opening ${cfg.workspace}…`);
  try {
    await openWorkspaceAt(cfg.workspace);
  } catch (e) {
    setAgentStatus(`Demo workspace failed: ${String(e)}`);
    return;
  }

  let raw: string;
  try {
    raw = await ipc.readHostFile(cfg.prompts_file);
  } catch (e) {
    setAgentStatus(`Demo prompts failed: ${String(e)}`);
    return;
  }

  type PromptRow = { id: number; prompt: string; phase?: string };
  const rows: PromptRow[] = [];
  for (const line of raw.split("\n")) {
    const t = line.trim();
    if (!t) continue;
    try {
      const j = JSON.parse(t) as PromptRow;
      if (typeof j.id === "number" && typeof j.prompt === "string") rows.push(j);
    } catch {
      /* skip */
    }
  }
  const slice = rows.filter((r) => r.id >= cfg.from && r.id <= cfg.to);
  if (!slice.length) {
    setAgentStatus(`Demo: no prompts in range ${cfg.from}–${cfg.to}`);
    return;
  }

  (document.getElementById("cmdk-auto-accept") as HTMLInputElement).checked = true;
  ensureAgentOpen();

  for (let i = 0; i < slice.length; i++) {
    const row = slice[i];
    // Stay in create/multi-file mode for every scenario step. acceptEdit()
    // otherwise clears greenfield and subsequent prompts thrash-edit README.md.
    (document.getElementById("cmdk-greenfield") as HTMLInputElement).checked = true;
    (document.getElementById("cmdk-path") as HTMLInputElement).value = "";
    (document.getElementById("cmdk-auto-accept") as HTMLInputElement).checked = true;
    setAgentStatus(
      `Demo prompt ${row.id} (${i + 1}/${slice.length}${row.phase ? ` · ${row.phase}` : ""})…`,
    );
    await typeIntoPrompt(row.prompt, cfg.type_ms);
    await sleep(600);
    await runCmdK();
    if (i + 1 < slice.length) await sleep(cfg.pause_ms);
  }
  setAgentStatus(
    `Demo finished — prompts ${cfg.from}–${cfg.to} (${slice.length} steps). You can keep editing.`,
  );
}

void runDemoIfConfigured();
