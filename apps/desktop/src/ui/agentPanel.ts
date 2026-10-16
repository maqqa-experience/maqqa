/** Helpers for the right-hand Agent sidebar (former Cmd+K drawer). */

export function focusAgentPrompt(): void {
  const el = document.getElementById("cmdk-prompt") as HTMLTextAreaElement | null;
  el?.focus();
}

export function setAgentStatus(text: string): void {
  const el = document.getElementById("cmdk-status");
  if (el) el.textContent = text;
}

export function setAcceptRejectEnabled(accept: boolean, reject: boolean): void {
  const a = document.getElementById("cmdk-accept") as HTMLButtonElement | null;
  const r = document.getElementById("cmdk-reject") as HTMLButtonElement | null;
  if (a) a.disabled = !accept;
  if (r) r.disabled = !reject;
}
