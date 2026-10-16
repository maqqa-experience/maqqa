/** Mount VS Code–like workbench chrome into #app. */
export function mountWorkbench(root: Element): void {
  root.innerHTML = `
  <div class="workbench" id="workbench">
    <nav class="activity-bar" aria-label="Activity Bar">
      <button type="button" class="activity-btn active" id="act-explorer" title="Explorer (Cmd+Shift+E)" data-view="explorer">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 3h8v8H3V3zm10 0h8v8h-8V3zM3 13h8v8H3v-8zm10 0h8v8h-8v-8z"/></svg>
      </button>
      <button type="button" class="activity-btn" id="act-agent" title="Agent (Cmd+K)" data-view="agent">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 2a4 4 0 0 1 4 4v1h2a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2h2V6a4 4 0 0 1 4-4zm0 2a2 2 0 0 0-2 2v1h4V6a2 2 0 0 0-2-2zm-1 8v5h2v-5h-2z"/></svg>
      </button>
      <div class="activity-spacer"></div>
    </nav>

    <aside class="sidebar" id="sidebar">
      <div class="resize-v" id="resize-sidebar" title="Resize sidebar"></div>
      <div class="sidebar-header">
        <span>Explorer</span>
        <div class="sidebar-actions">
          <button type="button" class="icon-btn" id="open-ws" title="Open Folder">Open</button>
          <button type="button" class="icon-btn" id="save-file" title="Save (Cmd+S)">Save</button>
        </div>
      </div>
      <div class="ws-name" id="ws-label">No workspace</div>
      <div class="sidebar-body" id="file-list">
        <div class="empty-hint">Open a folder to browse files.</div>
      </div>
    </aside>

    <section class="editor-column">
      <div class="tab-bar" id="tab-bar"></div>
      <div class="breadcrumbs" id="breadcrumbs"></div>
      <div class="editor-stack">
        <div id="editor"></div>
        <div id="diff-host"></div>
      </div>
    </section>

    <aside class="agent-sidebar" id="agent-sidebar">
      <div class="resize-v" id="resize-agent" title="Resize agent"></div>
      <div class="agent-header">
        <span>Agent</span>
        <button type="button" class="icon-btn" id="agent-collapse" title="Hide agent">✕</button>
      </div>
      <div class="agent-body" id="cmdk">
        <div class="cmdk-path-row">
          <input id="cmdk-path" class="cmdk-path" type="text" placeholder="Path hint (optional)" />
          <button type="button" id="cmdk-new" class="btn-secondary" title="Create from scratch">New</button>
        </div>
        <label class="cmdk-check"><input type="checkbox" id="cmdk-greenfield" checked /> Create new files (ignore open file)</label>
        <label class="cmdk-check"><input type="checkbox" id="cmdk-auto-accept" checked /> Auto-accept agent result</label>
        <textarea id="cmdk-prompt" placeholder="Describe what to build or change…"></textarea>
        <div class="cmdk-actions">
          <button type="button" id="cmdk-run" class="btn-primary">Run edit</button>
          <button type="button" id="cmdk-accept" class="btn-secondary" disabled>Accept</button>
          <button type="button" id="cmdk-reject" class="btn-secondary" disabled>Reject</button>
        </div>
        <div id="cmdk-status"></div>
      </div>
    </aside>

    <section class="bottom-panel" id="bottom-panel">
      <div class="resize-h" id="resize-panel" title="Resize panel"></div>
      <div class="panel-tabs">
        <button type="button" class="panel-tab active" data-panel="output">Output</button>
        <button type="button" class="panel-tab" data-panel="diff">Diff</button>
        <span style="flex:1"></span>
        <button type="button" class="icon-btn" id="panel-collapse" title="Hide panel (Cmd+J)">−</button>
      </div>
      <div class="panel-body" id="panel-output">(agent output will appear here)</div>
      <div class="panel-body" id="panel-diff" hidden></div>
    </section>

    <footer class="status-bar" id="status-bar">
      <span id="st-ollama"><span class="dot bad"></span>ollama</span>
      <span id="st-llm">llm: idle</span>
      <span id="st-models" class="muted">models: —</span>
      <span id="st-index" class="muted">index: —</span>
      <span id="st-heal" class="muted">heal: —</span>
      <span class="grow"></span>
      <span id="st-cursor" class="muted">Ln 1, Col 1</span>
      <span id="st-lang" class="muted">plaintext</span>
    </footer>
  </div>

  <div class="quick-open" id="quick-open">
    <div class="quick-open-box">
      <input id="quick-open-input" type="text" placeholder="Go to file…" autocomplete="off" />
      <div class="quick-open-list" id="quick-open-list"></div>
    </div>
  </div>
  `;
}

export function wireResizeHandles(wb: HTMLElement): void {
  const sidebar = document.getElementById("sidebar")!;
  const agent = document.getElementById("agent-sidebar")!;
  const panel = document.getElementById("bottom-panel")!;

  function dragCol(
    handle: HTMLElement,
    onMove: (clientX: number) => void,
  ) {
    handle.addEventListener("mousedown", (e) => {
      e.preventDefault();
      const move = (ev: MouseEvent) => onMove(ev.clientX);
      const up = () => {
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    });
  }

  dragCol(document.getElementById("resize-sidebar")!, (x) => {
    const left = wb.getBoundingClientRect().left + 48;
    const w = Math.min(480, Math.max(160, x - left));
    wb.classList.remove("sidebar-collapsed");
    wb.style.setProperty("--sidebar-w", `${w}px`);
  });

  dragCol(document.getElementById("resize-agent")!, (x) => {
    const right = wb.getBoundingClientRect().right;
    const w = Math.min(520, Math.max(240, right - x));
    wb.classList.remove("agent-collapsed");
    wb.style.setProperty("--agent-w", `${w}px`);
  });

  const panelHandle = document.getElementById("resize-panel")!;
  panelHandle.addEventListener("mousedown", (e) => {
    e.preventDefault();
    const move = (ev: MouseEvent) => {
      const bottom = wb.getBoundingClientRect().bottom - 22;
      const h = Math.min(420, Math.max(80, bottom - ev.clientY));
      wb.style.setProperty("--panel-h", `${h}px`);
      wb.classList.remove("panel-collapsed");
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  });

  void sidebar;
  void agent;
  void panel;
}
