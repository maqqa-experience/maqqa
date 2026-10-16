# Project status

**Product:** [Maqqa](https://maqqa.dev) — Model Augmented Quick Qode Assistant  
**Stage:** public preview / research prototype (`0.1.x`)  
**Platform focus:** macOS Apple Silicon + local [Ollama](https://ollama.com)  
**Not yet:** a polished Cursor replacement, multi-OS installer, or cloud backend

## What works today

| Area | Notes |
|------|--------|
| Agent loop | Observe → critique → act → verify; multi-file writes |
| Shadow heal | Edits land in `.vibe/shadow` before Accept |
| Ghost / reason | Dual Ollama models via `~/.vibe/config.toml` |
| Index | Tree-sitter chunks + embeddings under `{workspace}/.vibe/` |
| Scenario | `vibe_scenario` multi-prompt builds (e.g. remote B2B jobs) |
| Bench | `vibe_bench` hand fixtures + optional LeetCodeDataset |
| Desktop | Tauri 2 + Monaco shell (open folder, Cmd+K, status) |

## Known gaps

- Desktop UX is minimal (no full IDE feature parity).
- Quality depends heavily on the reason model and prompts.
- Large default model tags in older docs may not fit every machine — use whatever you pulled.
- Linux/Windows desktop packaging is unproven; CI validates Rust core + frontend build only.
- Agent implementation is concentrated in a large `agent.rs` (refactors welcome).

## Suggested hardware

| Machine / RAM | Practical reason model |
|---------------|------------------------|
| MacBook Pro **16GB** | Qwen Coder **14B** (Q4 instruct) |
| Mac mini **24GB** | Qwen Coder **30B** |
| ~64GB+ | denser 30B / larger quants |
| ~96GB+ | 70B+ (as in original Studio target) |

Heal temperature A/B defaults on for `agent_profile = "small"` or `auto` with reason model **&lt;20B** (override with `ollama_heal_ab`).
