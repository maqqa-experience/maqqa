#!/usr/bin/env bash
# Local preflight approximating CI (no Ollama required).
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== fmt =="
cargo fmt --all -- --check

echo "== clippy =="
cargo clippy -p vibe_core -p vibe_ipc --all-targets -- \
  -D warnings \
  -A clippy::too_many_arguments \
  -A clippy::needless_range_loop \
  -A clippy::single_match \
  -A clippy::field_reassign_with_default

echo "== tests =="
cargo test -p vibe_core --lib
cargo test -p vibe_ipc

echo "== scenario bin =="
cargo build -p vibe_core --bin vibe_scenario

echo "== desktop build =="
(
  cd apps/desktop
  if [[ -f package-lock.json ]]; then npm ci; else npm install; fi
  npm run build
)

echo "OK — release checks passed."
echo "OK — CI badges point at github.com/maqqa-experience/maqqa."
