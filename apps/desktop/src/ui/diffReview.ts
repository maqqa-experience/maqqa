import { monaco, langForPath } from "../monaco/setup";

let diffEditor: monaco.editor.IStandaloneDiffEditor | null = null;
let originalModel: monaco.editor.ITextModel | null = null;
let modifiedModel: monaco.editor.ITextModel | null = null;

export function showDiffReview(
  path: string,
  original: string,
  proposed: string,
): void {
  const host = document.getElementById("diff-host")!;
  const editorEl = document.getElementById("editor")!;
  host.classList.add("visible");
  editorEl.classList.add("hidden-for-diff");

  if (!diffEditor) {
    diffEditor = monaco.editor.createDiffEditor(host, {
      automaticLayout: true,
      theme: "vs-dark",
      readOnly: true,
      renderSideBySide: true,
      fontFamily: "SF Mono, Menlo, Monaco, Consolas, monospace",
      fontSize: 13,
      minimap: { enabled: false },
    });
  }

  originalModel?.dispose();
  modifiedModel?.dispose();
  const lang = langForPath(path);
  originalModel = monaco.editor.createModel(original || "", lang);
  modifiedModel = monaco.editor.createModel(proposed || "", lang);
  diffEditor.setModel({ original: originalModel, modified: modifiedModel });
}

export function hideDiffReview(): void {
  const host = document.getElementById("diff-host")!;
  const editorEl = document.getElementById("editor")!;
  host.classList.remove("visible");
  editorEl.classList.remove("hidden-for-diff");
}

export function disposeDiffReview(): void {
  hideDiffReview();
  diffEditor?.dispose();
  diffEditor = null;
  originalModel?.dispose();
  modifiedModel?.dispose();
  originalModel = null;
  modifiedModel = null;
}
