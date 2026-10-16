#!/usr/bin/env bash
# Build unsigned macOS .app (+ .dmg when possible) and CLI binaries into dist/macos/.
#
# Usage:
#   ./scripts/build_macos_release.sh              # host arch only
#   ./scripts/build_macos_release.sh --universal  # lipo arm64+x86_64 (needs both rustup targets)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
# Match tauri.conf.json macOS.minimumSystemVersion
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"

UNIVERSAL=0
for arg in "$@"; do
  case "$arg" in
    --universal|-u) UNIVERSAL=1 ;;
    -h|--help)
      sed -n '1,8p' "$0"
      exit 0
      ;;
  esac
done

if [[ "$UNIVERSAL" -eq 1 ]]; then
  TRIPLE="universal-apple-darwin"
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
else
  ARCH="$(uname -m)"
  case "$ARCH" in
    arm64) TRIPLE="aarch64-apple-darwin" ;;
    x86_64) TRIPLE="x86_64-apple-darwin" ;;
    *) echo "unsupported arch: $ARCH"; exit 1 ;;
  esac
fi

OUT="$ROOT/dist/macos"
rm -rf "$OUT"
mkdir -p "$OUT"

echo "== desktop (Tauri $TRIPLE) =="
(
  cd apps/desktop
  if [[ -f package-lock.json ]]; then npm ci; else npm install; fi
  # Prefer app+dmg; fall back to app-only if hdiutil/dmg scripting fails.
  if ! npx tauri build --target "$TRIPLE" --bundles app,dmg; then
    echo "dmg bundle failed; retrying app-only…"
    npx tauri build --target "$TRIPLE" --bundles app
  fi
)

BUNDLE_DIR="$CARGO_TARGET_DIR/${TRIPLE}/release/bundle"
if [[ ! -d "$BUNDLE_DIR" ]]; then
  BUNDLE_DIR="$CARGO_TARGET_DIR/release/bundle"
fi
if [[ ! -d "$BUNDLE_DIR" ]]; then
  BUNDLE_DIR="$(find "$CARGO_TARGET_DIR" -type d -path '*/release/bundle' 2>/dev/null | head -1 || true)"
fi
echo "bundle dir: ${BUNDLE_DIR:-missing}"
test -d "$BUNDLE_DIR"

find "$BUNDLE_DIR" -name '*.dmg' -exec cp {} "$OUT/" \;
while IFS= read -r -d '' app; do
  cp -R "$app" "$OUT/"
done < <(find "$BUNDLE_DIR" -name '*.app' -print0)

echo "== CLI binaries =="
BIN_DEST="$OUT/cli"
mkdir -p "$BIN_DEST"
if [[ "$UNIVERSAL" -eq 1 ]]; then
  cargo build -p vibe_core --release --target aarch64-apple-darwin --bins
  cargo build -p vibe_core --release --target x86_64-apple-darwin --bins
  for b in vibe_scenario vibe_bench vibe_eval; do
    arm="$CARGO_TARGET_DIR/aarch64-apple-darwin/release/$b"
    intel="$CARGO_TARGET_DIR/x86_64-apple-darwin/release/$b"
    if [[ -x "$arm" && -x "$intel" ]]; then
      lipo -create -output "$BIN_DEST/$b" "$arm" "$intel"
    fi
  done
else
  cargo build -p vibe_core --release --target "$TRIPLE" --bins
  for b in vibe_scenario vibe_bench vibe_eval; do
    src="$CARGO_TARGET_DIR/${TRIPLE}/release/$b"
    [[ -x "$src" ]] || src="$CARGO_TARGET_DIR/release/$b"
    if [[ -x "$src" ]]; then
      cp "$src" "$BIN_DEST/"
    fi
  done
fi
# Artifact name for universal uses a stable label
ARTIFACT_TRIPLE="$TRIPLE"
[[ "$UNIVERSAL" -eq 1 ]] && ARTIFACT_TRIPLE="universal-apple-darwin"
cp "$ROOT/config.example.toml" "$OUT/config.example.toml"
cat > "$OUT/README.txt" <<EOF
Maqqa macOS preview ($(date -u +%Y-%m-%d))
Model Augmented Quick Qode Assistant — https://maqqa.dev
Arch: $ARTIFACT_TRIPLE

Contents
- Maqqa.app / *.dmg        — desktop UI (unsigned)
- cli/                     — vibe_scenario, vibe_bench, vibe_eval
- config.example.toml      — copy to ~/.vibe/config.toml

Requirements
- macOS 12+
- Ollama running locally (https://ollama.com) with ghost/reason/embed models

Gatekeeper (unsigned builds)
  xattr -cr "Maqqa.app"
  # or: right-click → Open the first time

Notarization / Developer ID signing is not included in this preview build.
EOF

(
  cd "$OUT"
  # Zip a single .app (glob can confuse ditto when multiple matches exist)
  APP_BUNDLE="$(ls -d *.app 2>/dev/null | head -1 || true)"
  if [[ -n "$APP_BUNDLE" && -d "$APP_BUNDLE" ]]; then
    ditto -c -k --sequesterRsrc --keepParent "$APP_BUNDLE" "Maqqa-${ARTIFACT_TRIPLE}.app.zip"
  fi
  tar -czf "maqqa-cli-${ARTIFACT_TRIPLE}.tar.gz" cli config.example.toml README.txt
)

echo
echo "Artifacts in $OUT:"
ls -lah "$OUT"
echo
echo "Done. Upload the .dmg / .app.zip and cli tarball to a GitHub Release."
