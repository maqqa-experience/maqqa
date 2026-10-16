#!/usr/bin/env bash
# Run all 100 remote_b2b_jobs prompts via vibe_scenario (CLI, durable).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WS="${1:-/tmp/vibe_jobs_full}"
FROM="${VIBE_SCENARIO_FROM:-1}"
TO="${VIBE_SCENARIO_TO:-100}"
JSON="${VIBE_SCENARIO_JSON:-/tmp/jobs_scenario_full.json}"
LOG="${VIBE_SCENARIO_LOG:-/tmp/vibe_scenario_full.log}"

unset CARGO_TARGET_DIR
export CARGO_TARGET_DIR="${ROOT}/target"
# Use the user's normal ~/.vibe/config.toml (remote coder model) unless overridden
# export VIBE_CONFIG=... for a fast local demo model

source "$HOME/.cargo/env" 2>/dev/null || true
mkdir -p "$WS"
# Only wipe when starting from prompt 1 (resuming must keep prior agent work).
if [[ "$FROM" -eq 1 ]]; then
  echo "Clearing workspace $WS (from=1)"
  find "$WS" -mindepth 1 -maxdepth 1 -exec rm -rf {} + 2>/dev/null || true
else
  echo "Keeping workspace $WS (resume from=$FROM)"
fi

echo "scenario=remote_b2b_jobs from=$FROM to=$TO"
echo "workspace=$WS"
echo "json=$JSON"
echo "log=$LOG"
cd "$ROOT"
cargo build -p vibe_core --bin vibe_scenario -q
set -o pipefail
# Continue past checkpoint failures so a single bad smoke check does not abort 1–100.
cargo run -p vibe_core --bin vibe_scenario --quiet -- \
  --workspace "$WS" --from "$FROM" --to "$TO" --json "$JSON" \
  --continue-on-checkpoint-fail \
  2>&1 | tee "$LOG"
echo
echo "=== done ==="
grep -E '^SCORE:|^=== prompt' "$LOG" | tail -30 || true
ls -lh "$JSON" "$WS" | head -20
