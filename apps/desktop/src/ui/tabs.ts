import { monaco, langForPath } from "../monaco/setup";

type TabState = {
  path: string;
  model: monaco.editor.ITextModel;
  savedValue: string;
};

export type TabCallbacks = {
  onActivate: (path: string) => void;
  onClose: (path: string) => void;
  onDirtyChange?: (path: string, dirty: boolean) => void;
};

export class TabManager {
  private tabs = new Map<string, TabState>();
  private order: string[] = [];
  private active: string | null = null;
  private editor: monaco.editor.IStandaloneCodeEditor;
  private bar: HTMLElement;
  private crumbs: HTMLElement;
  private cbs: TabCallbacks;

  constructor(
    editor: monaco.editor.IStandaloneCodeEditor,
    bar: HTMLElement,
    crumbs: HTMLElement,
    cbs: TabCallbacks,
  ) {
    this.editor = editor;
    this.bar = bar;
    this.crumbs = crumbs;
    this.cbs = cbs;
  }

  get activePath(): string | null {
    return this.active;
  }

  get value(): string {
    return this.editor.getValue();
  }

  isDirty(path: string): boolean {
    const t = this.tabs.get(path);
    return t ? t.model.getValue() !== t.savedValue : false;
  }

  open(path: string, content: string, activate = true): void {
    let tab = this.tabs.get(path);
    if (!tab) {
      const model = monaco.editor.createModel(content, langForPath(path));
      tab = { path, model, savedValue: content };
      this.tabs.set(path, tab);
      this.order.push(path);
      model.onDidChangeContent(() => {
        this.renderBar();
        this.cbs.onDirtyChange?.(path, this.isDirty(path));
      });
    } else if (tab.model.getValue() === tab.savedValue) {
      // Refresh from disk only if not dirty
      tab.model.setValue(content);
      tab.savedValue = content;
    }
    if (activate) this.activate(path);
    else this.renderBar();
  }

  activate(path: string): void {
    const tab = this.tabs.get(path);
    if (!tab) return;
    this.active = path;
    this.editor.setModel(tab.model);
    this.renderBar();
    this.renderBreadcrumbs(path);
    this.cbs.onActivate(path);
    document.title = `${baseName(path)} — Maqqa`;
  }

  markSaved(path: string, content?: string): void {
    const tab = this.tabs.get(path);
    if (!tab) return;
    tab.savedValue = content ?? tab.model.getValue();
    this.renderBar();
  }

  close(path: string): void {
    const tab = this.tabs.get(path);
    if (!tab) return;
    const idx = this.order.indexOf(path);
    this.order.splice(idx, 1);
    this.tabs.delete(path);
    if (this.active === path) {
      const next = this.order[idx] ?? this.order[idx - 1] ?? null;
      this.active = null;
      if (next) this.activate(next);
      else {
        this.editor.setModel(monaco.editor.createModel("// No file open\n", "plaintext"));
        this.renderBreadcrumbs(null);
        document.title = "Maqqa";
      }
    }
    tab.model.dispose();
    this.renderBar();
    this.cbs.onClose(path);
  }

  paths(): string[] {
    return [...this.order];
  }

  private renderBar(): void {
    this.bar.innerHTML = "";
    for (const path of this.order) {
      const dirty = this.isDirty(path);
      const el = document.createElement("button");
      el.type = "button";
      el.className = `tab${this.active === path ? " active" : ""}`;
      el.title = path;
      el.innerHTML = `
        <span class="tab-label">${escapeHtml(baseName(path))}${dirty ? '<span class="tab-dirty"> •</span>' : ""}</span>
        <span class="tab-close" data-close="${escapeHtml(path)}" title="Close">×</span>
      `;
      el.addEventListener("click", (e) => {
        const t = e.target as HTMLElement;
        if (t.dataset.close || t.closest("[data-close]")) {
          e.stopPropagation();
          this.close(path);
          return;
        }
        this.activate(path);
      });
      el.addEventListener("auxclick", (e) => {
        if (e.button === 1) {
          e.preventDefault();
          this.close(path);
        }
      });
      this.bar.appendChild(el);
    }
  }

  renderBreadcrumbs(path: string | null): void {
    this.crumbs.innerHTML = "";
    if (!path) return;
    const parts = path.split("/").filter(Boolean);
    parts.forEach((seg, i) => {
      if (i > 0) {
        const sep = document.createElement("span");
        sep.className = "breadcrumb-sep";
        sep.textContent = " › ";
        this.crumbs.appendChild(sep);
      }
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "breadcrumb-seg";
      btn.textContent = seg;
      const sub = parts.slice(0, i + 1).join("/");
      btn.onclick = () => {
        if (this.tabs.has(sub)) this.activate(sub);
      };
      this.crumbs.appendChild(btn);
    });
  }
}

function baseName(path: string): string {
  const i = path.lastIndexOf("/");
  return i >= 0 ? path.slice(i + 1) : path;
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}
