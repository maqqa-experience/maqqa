# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.2] - 2026-10-16

### Added

- GitHub Actions `Release Linux / Windows`: AppImage/`.deb` and MSI/NSIS on `v*.*.*` tags (draft).
- README: why Maqqa exists; hardware recs; Linux/Windows build docs; disable ghost with empty `ghost_model`.

### Changed

- Empty `ghost_model` disables inline completions (avoids sluggish 14B+ ghost on tight RAM).
- Package version aligned to `0.0.2`.

## [0.0.1] - 2026-10-16

### Added

- Initial public preview: Tauri + Monaco desktop, Rust `vibe_core`, Ollama dual-model path,
  Tree-sitter index, shadow heal, scenario/bench CLIs, docs, Maqqa branding + icons.
- macOS packaging (`build_macos_release.sh`) + `Release macOS` Actions on tags.
- Checkpoint-heal temperature A/B, `ollama_num_ctx`, agent profile auto/small/default.
