# First run

## 1. Install Ollama

```bash
brew install ollama   # or download from https://ollama.com
ollama serve
```

Pull at least:

```bash
ollama pull nomic-embed-text
ollama pull qwen2.5-coder:7b-instruct-q4_K_M          # ghost
ollama pull Qwen2.5-Coder:14b-instruct-q4_K_M         # reason (example)
```

Use any instruct/coder tags that fit your RAM — then point config at those exact names from `ollama list`.

## 2. Config

```bash
mkdir -p ~/.vibe
cp config.example.toml ~/.vibe/config.toml
```

Edit `ghost_model`, `reason_model`, and `reason_model_fallback` to match pulled tags.
Optional: set `ollama_url` if Ollama is not on `127.0.0.1:11434`.

## 3. Engine smoke

```bash
cargo test -p vibe_core --lib
cargo run -p vibe_core --bin vibe_eval -- --smoke-only
```

## 4. Desktop

```bash
cd apps/desktop
npm install
npm run tauri dev
```

1. Open a project folder  
2. Wait for status `index: N chunks`  
3. Try ghost completions while typing  
4. Use **Cmd+K** for a structural edit; **Accept** only after the shadow heal looks right  

## 5. Optional scenario slice

```bash
cargo run -p vibe_core --release --bin vibe_scenario -- --from 1 --to 10
```

Logs: `{workspace}/.vibe/logs/` (local only, gitignored).

## Troubleshooting

| Symptom | Try |
|---------|-----|
| `ollama not reachable` | `ollama serve`; check `ollama_url` |
| Wrong / missing model | `ollama list` and fix tags in `~/.vibe/config.toml` |
| Huge VRAM / slow load | Lower `ollama_num_ctx`; use a smaller quant |
| Heal thrash on small models | Leave `agent_profile = "auto"` (A/B on for &lt;20B) or set `ollama_heal_ab = true` |
