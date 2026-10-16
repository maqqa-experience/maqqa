# Demos

Generated screen recordings and preview stills from `scripts/record_vibe_demo.sh`.

Media files (`*.mp4`, `*.png`) are gitignored. Regenerate locally:

```bash
./scripts/record_vibe_demo.sh --from 1 --to 3 --auto-exit
# or watch without recording:
./scripts/record_vibe_demo.sh --no-record --from 1 --to 5
```

Writes `maqqa_demo_interactive.mp4` plus `preview_{start,mid,end}.png`.

Requires the desktop app, Ollama (`qwen3:4b-instruct` + `nomic-embed-text` by default), and Screen Recording permission on macOS.
