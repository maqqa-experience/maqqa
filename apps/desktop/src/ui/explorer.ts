import type { FileEntry, TreeNode } from "../types";

export function buildTree(files: FileEntry[]): TreeNode[] {
  const root: TreeNode[] = [];
  const dirMap = new Map<string, TreeNode>();

  const ensureDir = (segments: string[]): TreeNode => {
    let path = "";
    let children = root;
    let node: TreeNode | undefined;
    for (const seg of segments) {
      path = path ? `${path}/${seg}` : seg;
      let found = dirMap.get(path);
      if (!found) {
        found = { name: seg, path, isDir: true, children: [] };
        dirMap.set(path, found);
        children.push(found);
        children.sort(compareNodes);
      }
      node = found;
      children = found.children;
    }
    return node!;
  };

  for (const f of files) {
    if (f.is_dir) continue;
    const parts = f.path.split("/").filter(Boolean);
    if (parts.length === 0) continue;
    const fileName = parts[parts.length - 1]!;
    const parentSegs = parts.slice(0, -1);
    const list =
      parentSegs.length === 0 ? root : ensureDir(parentSegs).children;
    list.push({ name: fileName, path: f.path, isDir: false, children: [] });
    list.sort(compareNodes);
  }
  return root;
}

function compareNodes(a: TreeNode, b: TreeNode): number {
  if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
  return a.name.localeCompare(b.name);
}

export type ExplorerHandlers = {
  onOpenFile: (path: string) => void;
  onRevealDir?: (path: string) => void;
};

const collapsed = new Set<string>();

export function renderExplorer(
  container: HTMLElement,
  files: FileEntry[],
  activePath: string | null,
  handlers: ExplorerHandlers,
): void {
  container.innerHTML = "";
  if (!files.length) {
    container.innerHTML = `<div class="empty-hint">No files in workspace.</div>`;
    return;
  }
  const tree = buildTree(files);
  const frag = document.createDocumentFragment();
  for (const node of tree) {
    frag.appendChild(renderNode(node, 0, activePath, handlers));
  }
  container.appendChild(frag);
}

function renderNode(
  node: TreeNode,
  depth: number,
  activePath: string | null,
  handlers: ExplorerHandlers,
): HTMLElement {
  const pad = `${8 + depth * 12}px`;

  if (node.isDir) {
    const wrap = document.createElement("div");
    const open = !collapsed.has(node.path);
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "tree-dir";
    btn.style.paddingLeft = pad;
    btn.innerHTML = `<span class="tree-twistie">${open ? "▼" : "▶"}</span><span>${escapeHtml(node.name)}</span>`;
    const kids = document.createElement("div");
    kids.className = `tree-children${open ? " open" : ""}`;
    for (const c of node.children) {
      kids.appendChild(renderNode(c, depth + 1, activePath, handlers));
    }
    btn.onclick = () => {
      if (collapsed.has(node.path)) collapsed.delete(node.path);
      else collapsed.add(node.path);
      const nowOpen = !collapsed.has(node.path);
      kids.classList.toggle("open", nowOpen);
      btn.querySelector(".tree-twistie")!.textContent = nowOpen ? "▼" : "▶";
      handlers.onRevealDir?.(node.path);
    };
    wrap.appendChild(btn);
    wrap.appendChild(kids);
    return wrap;
  }

  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = `tree-file${activePath === node.path ? " active" : ""}`;
  btn.dataset.path = node.path;
  btn.style.paddingLeft = pad;
  btn.innerHTML = `<span class="tree-twistie"></span><span>${escapeHtml(node.name)}</span>`;
  btn.onclick = () => handlers.onOpenFile(node.path);
  return btn;
}

export function setExplorerActive(container: HTMLElement, path: string | null): void {
  container.querySelectorAll(".tree-file").forEach((el) => {
    el.classList.toggle("active", (el as HTMLElement).dataset.path === path);
  });
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}
