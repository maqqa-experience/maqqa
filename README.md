# Maqqa

[![CI](https://github.com/maqqa-experience/maqqa/actions/workflows/ci.yml/badge.svg)](https://github.com/maqqa-experience/maqqa/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![maqqa.dev](https://img.shields.io/badge/maqqa.dev-brand-0a0a0a)](https://maqqa.dev)

<p align="center">
  <img src="docs/assets/maqqa-og.png" alt="Maqqa — Model Augmented Quick Qode Assistant" width="720" />
</p>

<p align="center">
  <img src="docs/assets/maqqa-screenshot.png" alt="Maqqa editor with agent panel" width="720" />
</p>

**Model Augmented Quick Qode Assistant** — local-first AI code editor: Tauri + Monaco UI, Rust agent engine, [Ollama](https://ollama.com) inference, Tree-sitter indexing, shadow-workspace self-heal.

> **Status:** experimental `0.0.x` preview. Research / local-agent playground (macOS primary; Linux/Windows builds via Releases). **Not** a drop-in Cursor or Bolt replacement. See [`docs/STATUS.md`](docs/STATUS.md).

## What Maqqa is (and isn’t)

**Maqqa is** a **local agent harness** for Ollama coder models (especially Qwen Coder **14B** on 16GB and **30B** on ~24GB). Goal: modest **vibe / agent coding** from an empty or small workspace — multi-file edits that get **checked before Accept** — without the usual local-agent failure modes (tool-call spam, cloud-sized prompts, no verify loop, code that doesn’t compile).

**Maqqa is not** offline Lovable / Bolt / v0. Those are cloud sandboxes + frontier models. A local 14B will not match “one prompt → full production app with live preview.” Same ceiling as every other local 14B stack for that category.

**Also not** “Cline or Cursor pointed at `localhost:11434`.” Those UIs ship prompts and tool schemas tuned for Claude/GPT. Dumping that on a 14B usually fails. Maqqa is built **around** local models instead of treating Ollama as a drop-in backend.

| Bar | Expect |
|-----|--------|
| Prompt → deployable full-stack app like Bolt | No — use cloud vibe tools |
| Local 14B agent that is less broken than cloud-harness-on-Ollama | Yes — that’s the point |
| 16GB laptop | Yes — **14B reason only**, `ghost_model = ""`, capped `ollama_num_ctx` |
| 24GB+ | Comfortable **30B** reason (+ optional ghost / embed) |

## Why this exists

Most agent harnesses assume frontier cloud models. Point the same loops at a **local** coder (even on strong Apple Silicon) and quality drops: brittle multi-file edits, weak self-correction, rigid JSON tool calls the model can’t follow, and context that balloons to 30k+ tokens until a laptop crawls or OOMs.

Maqqa closes that gap with harness choices local models need:

- **Shadow workspace** — edits land in `.vibe/shadow` first; syntax / checkpoint repair (optional **temperature A/B** on &lt;20B) before you Accept  
- **Context caps** — `ollama_num_ctx` (e.g. 12–14k) so modest agent turns stay in an 8–14k band instead of death-spiraling  
- **Ghost optional** — inline completions are a separate small model; **disable on tight RAM** (`ghost_model = ""`) so you don’t dual-load 7B+14B  
- **Scenario & bench CLIs** — measure local agents, don’t only demo them once on a 70B API  

## Features

- **Ghost completions** (optional) — small/fast Ollama model for inline suggestions; leave empty to disable  
- **Cmd+K / agent edits** — reason model writes through a **shadow workspace**; you Accept  
- **Self-heal** — syntax + checkpoint repair loops (optional **temperature A/B** on smaller models)  
- **Workspace index** — Tree-sitter chunks → embeddings → retrieval  
- **Scenario & bench CLIs** — multi-prompt product builds and coding benchmarks  

## Architecture

```
apps/desktop          Tauri 2 + Monaco
crates/vibe_core      agent, indexer, shadow heal, ollama, router, MCP tools
crates/vibe_ipc       shared IPC types
{workspace}/.vibe/    per-workspace runtime (shadow, index, logs) — gitignored
~/.vibe/config.toml   global model / sampling settings
```

### Dual models

| Tier | Role | Example tags |
|------|------|----------------|
| Ghost | Inline completions | `qwen2.5-coder:7b-instruct-q4_K_M` / `q8_0` |
| Reason | Cmd+K / agent / heal | `Qwen2.5-Coder:14b-instruct-q4_K_M`, `qwen3-coder:30b`, … |
| Embed | RAG | `nomic-embed-text` |

### Recommended hardware

| Machine | Reason model (Ollama) | Ghost |
|---------|------------------------|--------|
| MacBook Pro **16GB** | Qwen Coder **14B** (e.g. `Qwen2.5-Coder:14b-instruct-q4_K_M`) | **Off** — `ghost_model = ""` |
| Mac mini **24GB** | Qwen Coder **30B** (e.g. `qwen3-coder:30b`) | Optional 7B-class |

`24GB` is for **30B comfort**, not a hard floor — **16GB + 14B + ghost off** is a supported path. Cap context with `ollama_num_ctx = 12000` (or similar). Do **not** point ghost at the 14B+ reason model (typing becomes sluggish). On 14B, heal A/B defaults on via `agent_profile = "auto"`; force with `ollama_heal_ab = true`, or turn off with `false` if you want fewer regen passes.

Example lean laptop config:

```toml
ghost_model = ""
reason_model = "Qwen2.5-Coder:14b-instruct-q4_K_M"
ollama_num_ctx = 12000
agent_profile = "auto"
# ollama_heal_ab = false   # optional: fewer heal retries
# agent_max_turns = 4      # optional: shorter loops
```

Latency targets (aspirational when ghost is enabled): first-token p95 **&lt;100ms**; UI paint after token **&lt;30ms**.

## Quick start

```bash
# 1) Ollama
brew install ollama   # or https://ollama.com
ollama serve

ollama pull Qwen2.5-Coder:14b-instruct-q4_K_M   # or 30b if you have ~24GB+
ollama pull nomic-embed-text
# optional ghost (skip on 16GB):
# ollama pull qwen2.5-coder:7b-instruct-q4_K_M

# 2) Config
cp config.example.toml ~/.vibe/config.toml
# edit reason_model; on 16GB set ghost_model = ""

# 3) Smoke the engine
cargo test -p vibe_core --lib
cargo run -p vibe_core --bin vibe_eval -- --smoke-only

# 4) Desktop
cd apps/desktop && npm install && npm run tauri dev
```

More detail: [`docs/FIRST_RUN.md`](docs/FIRST_RUN.md).

### Optional Ollama residency (Mac Studio / large RAM)

```bash
export OLLAMA_NUM_PARALLEL=2
export OLLAMA_MAX_LOADED_MODELS=2
```

## Develop

```bash
cargo check -p vibe_core
cargo test -p vibe_core --lib
cargo test -p vibe_ipc
cargo clippy -p vibe_core -p vibe_ipc --all-targets -- -D warnings

# Dual-model smoke (requires Ollama)
cargo run -p vibe_core --bin vibe_eval -- --smoke-only

# Shadow heal eval on fixtures/rust_broken
cargo run -p vibe_core --bin vibe_eval

# Multi-prompt scenario (~100 prompts → remote B2B job aggregator)
cargo run -p vibe_core --bin vibe_scenario -- --list
cargo run -p vibe_core --bin vibe_scenario -- --from 1 --to 10
# full run (long): --from 1 --to 100 --json /tmp/jobs_scenario.json

# Desktop
cd apps/desktop && npm install && npm run tauri dev

# Interactive demo (prefer Terminal.app + Screen Recording for capture)
./scripts/record_vibe_demo.sh --from 1 --to 3
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

### Tauri commands

| Command | Purpose |
|---------|---------|
| `open_workspace` | Index folder, list files |
| `read_file` / `write_file` | Buffer I/O |
| `get_completions` / `cancel_completion` | Ghost text |
| `run_edit` | Cmd+K → reason model → shadow heal |
| `apply_edit` | Accept healed patch into live tree |
| `get_status` | Models, index size, last heal |
| `mcp_call` | Local tools: read_file, list_dir, git_*, run_terminal |

## Coding ability bench

Hand fixtures: `fixtures/coding/`. Optional large corpus from
[newfacade/LeetCodeDataset](https://github.com/newfacade/LeetCodeDataset) (MIT) via `scripts/import_leetcode.py`.

```bash
python3 scripts/import_leetcode.py --clone   # one-time
cargo run -p vibe_core --bin vibe_bench -- --source hand
cargo run -p vibe_core --bin vibe_bench -- --source lcd --split test --difficulty Easy --limit 20
```

## Agentic loop + shadow heal

Create / multi-file requests run an **agent loop** (`agent_max_turns` in config):

1. **Observe** workspace + agent-written files  
2. **Critique** against prompt intent (`DONE` / `CONTINUE`)  
3. **Act** — write/update files  
4. **Verify** / repair  
5. Repeat until done or turn budget exhausted  

Checkpoint heals can **A/B** base vs cooler sampling temperature and keep the better workspace (defaults for `agent_profile = "small"` or `auto` with reason model &lt;20B). Override with `ollama_heal_ab`.

## Config highlights

Copy [`config.example.toml`](config.example.toml) → `~/.vibe/config.toml`.

| Key | Meaning |
|-----|---------|
| `reason_model` / `ghost_model` | Ollama tags; empty `ghost_model` disables inline completions |
| `ollama_num_ctx` | Cap generate context / KV |
| `ollama_temperature` | Default sampling |
| `ollama_heal_temperature` | Cooler (or hotter) heal temp |
| `ollama_heal_ab` | `true`/`false` force; omit = follow profile / size |
| `agent_profile` | `auto` / `small` / `default` |

## Index

Tree-sitter chunks → Ollama embeddings → cosine ANN under `{workspace}/.vibe/index.db/`.

## Security

Agent runs can edit files and execute verify commands in the workspace. See [`SECURITY.md`](SECURITY.md).

## Building

Prerequisites on all platforms: **Rust stable**, **Node 20+**, and a local [Ollama](https://ollama.com) for inference (not bundled). Dev:

```bash
cd apps/desktop && npm install && npm run tauri dev
```

CLI tools (any OS):

```bash
cargo build -p vibe_core --release --bins
# → target/release/vibe_scenario, vibe_bench, vibe_eval
```

Tagging `v*.*.*` runs GitHub Actions that attach **macOS** (Apple Silicon), **Linux** (`.AppImage` / `.deb`), and **Windows** (`.msi` / NSIS) artifacts to a **draft** release. You can also run the workflows manually from the Actions tab. Local recipes below are useful for debugging.

### macOS

```bash
./scripts/build_macos_release.sh              # host arch (arm64 or Intel)
./scripts/build_macos_release.sh --universal  # arm64 + x86_64 (needs both rustup targets)
# → dist/macos/  (.app, .dmg, cli tarball)
```

Universal builds use Tauri’s `universal-apple-darwin` target (`lipo`). Unsigned preview: right-click → Open, or `xattr -cr "Maqqa.app"`.

### Linux

Install Tauri/webkit system deps (Debian/Ubuntu example):

```bash
sudo apt update
sudo apt install -y \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev \
  patchelf libssl-dev libgtk-3-dev libsoup-3.0-dev
```

See [Tauri Linux prerequisites](https://v2.tauri.app/start/prerequisites/#linux) for Fedora/Arch packages.

```bash
cd apps/desktop
npm ci
# AppImage + .deb (omit bundles you do not need)
npx tauri build --bundles appimage,deb
# artifacts under:
#   src-tauri/target/release/bundle/appimage/
#   src-tauri/target/release/bundle/deb/
```

Also build the CLIs with `cargo build -p vibe_core --release --bins` from the repo root. Config path is still `~/.vibe/config.toml`.

### Windows

1. Install [Build Tools for Visual Studio](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (C++ workload) and the [WebView2 runtime](https://developer.microsoft.com/microsoft-edge/webview2/) (usually present on Win10/11).
2. Install Rust (`rustup`) and Node 20+.

In **PowerShell** or **Developer Command Prompt**:

```powershell
cd apps\desktop
npm ci
npx tauri build --bundles msi,nsis
# artifacts under:
#   src-tauri\target\release\bundle\msi\
#   src-tauri\target\release\bundle\nsis\
```

CLIs:

```powershell
cargo build -p vibe_core --release --bins
```

User config: `%USERPROFILE%\.vibe\config.toml` (same keys as `config.example.toml`).

## License

[MIT](LICENSE)
