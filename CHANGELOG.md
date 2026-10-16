# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- macOS binary packaging: `scripts/build_macos_release.sh` (`.app` / `.dmg` + CLI tarball) and GitHub Actions `Release macOS` on `v*.*.*` tags (unsigned draft).
- Checkpoint-heal **temperature A/B**: run base vs cooler heal temp from the same snapshot and keep the winner before a step is DONE.
- Config: `ollama_temperature`, `ollama_heal_temperature`, `ollama_heal_ab` (optional override).
- Auto heal A/B: follows `agent_profile` (`small` → on, `default`/`large` → off, `auto` → reason model &lt;20B by tag).
- Default cool heal temp `0.05` when A/B is on and heal temp is unset.
- `ollama_num_ctx` to cap Ollama KV / context window.
- Multi-prompt `vibe_scenario` runner and remote B2B jobs fixture scenario.
- Coding bench (`vibe_bench`) with hand fixtures + optional LeetCodeDataset import.

### Changed

- Rebranded product to **Maqqa** (Model Augmented Quick Qode Assistant); app bundle `Maqqa.app`, identifier `dev.maqqa.app`.
- Replaced solid-green placeholder icons with a Maqqa “M” mark (Dock / `.icns` / `.ico`).
- Diagnose-apply auto-clobber removed from the agent heal path (safer apply policy).

## [0.1.0] - 2026-10-16

### Added

- Initial public preview: Tauri + Monaco desktop shell, Rust `vibe_core` engine,
  Ollama dual-model ghost/reason path, Tree-sitter index, shadow-workspace heal,
  MCP-style local tools, docs and example config.
