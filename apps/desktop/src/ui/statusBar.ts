import type { StatusResponse } from "../types";

export function applyStatus(st: StatusResponse): void {
  const ollamaEl = document.getElementById("st-ollama")!;
  ollamaEl.innerHTML = `<span class="dot ${st.ollama_ok ? "ok" : "bad"}"></span>ollama`;
  document.getElementById("st-models")!.textContent =
    `ghost=${st.ghost_model} · reason=${st.reason_model}`;
  document.getElementById("st-index")!.textContent =
    `index: ${st.index_chunks} chunks${st.index_ready ? "" : " (warming)"}`;
  document.getElementById("st-heal")!.textContent = st.last_heal
    ? `heal: ${st.last_heal.success ? "ok" : "fail"} ${st.last_heal.path} (${st.last_heal.attempts})`
    : "heal: —";
  if (st.workspace) {
    const label = document.getElementById("ws-label");
    if (label) label.textContent = st.workspace;
  }
}

export function setLlmBusy(busy: boolean, kind = ""): void {
  const el = document.getElementById("st-llm");
  if (!el) return;
  if (busy) {
    el.textContent = `llm: calling ${kind}…`;
    el.classList.add("busy");
  } else {
    el.textContent = "llm: idle";
    el.classList.remove("busy");
  }
}

export function setCursorStatus(line: number, col: number, language: string): void {
  document.getElementById("st-cursor")!.textContent = `Ln ${line}, Col ${col}`;
  document.getElementById("st-lang")!.textContent = language;
}
