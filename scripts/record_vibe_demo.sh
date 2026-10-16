#!/usr/bin/env bash
# Launch Maqqa with scenario autoplay and capture a screen recording.
#
# Usage:
#   ./scripts/record_vibe_demo.sh --from 1 --to 3     # short demo + mp4
#   ./scripts/record_vibe_demo.sh --from 1 --to 100   # full scenario (very long)
#   ./scripts/record_vibe_demo.sh --no-record --from 1 --to 100
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FROM=1
TO=3
RECORD=1
AUTO_EXIT=0
OUT_DIR="${ROOT}/demos"
WS="${VIBE_DEMO_WORKSPACE:-/tmp/vibe_demo_ws}"
PROMPTS="${ROOT}/fixtures/scenarios/remote_b2b_jobs/prompts.jsonl"
MAX_SECS=600

while [[ $# -gt 0 ]]; do
  case "$1" in
    --from) FROM="$2"; shift 2 ;;
    --to) TO="$2"; shift 2 ;;
    --workspace) WS="$2"; shift 2 ;;
    --no-record) RECORD=0; shift ;;
    --auto-exit) AUTO_EXIT=1; shift ;;
    --out-dir) OUT_DIR="$2"; shift 2 ;;
    --max-secs) MAX_SECS="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,12p' "$0"
      exit 0
      ;;
    *) echo "Unknown arg: $1" >&2; exit 1 ;;
  esac
done

mkdir -p "$OUT_DIR" "$WS"
find "$WS" -mindepth 1 -maxdepth 1 ! -name '.git' -exec rm -rf {} + 2>/dev/null || true
rm -rf "${WS}/.vibe" 2>/dev/null || true

STAMP="$(date +%Y%m%d_%H%M%S)"
MP4="${OUT_DIR}/vibe_demo_${STAMP}.mp4"
REC_PID=""
TAURI_PID=""
VITE_PID=""

cleanup() {
  if [[ -n "${REC_PID}" ]] && kill -0 "$REC_PID" 2>/dev/null; then
    kill -INT "$REC_PID" 2>/dev/null || true
    wait "$REC_PID" 2>/dev/null || true
  fi
  if [[ -n "${TAURI_PID}" ]] && kill -0 "$TAURI_PID" 2>/dev/null; then
    kill "$TAURI_PID" 2>/dev/null || true
    wait "$TAURI_PID" 2>/dev/null || true
  fi
  if [[ -n "${VITE_PID}" ]] && kill -0 "$VITE_PID" 2>/dev/null; then
    kill "$VITE_PID" 2>/dev/null || true
    wait "$VITE_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

export VIBE_DEMO_WORKSPACE="$WS"
export VIBE_DEMO_PROMPTS_FILE="$PROMPTS"
export VIBE_DEMO_FROM="$FROM"
export VIBE_DEMO_TO="$TO"
export VIBE_DEMO_PAUSE_MS="${VIBE_DEMO_PAUSE_MS:-2000}"
export VIBE_DEMO_TYPE_MS="${VIBE_DEMO_TYPE_MS:-6}"
export VIBE_CONFIG="${VIBE_CONFIG:-${ROOT}/fixtures/scenarios/remote_b2b_jobs/demo_config.toml}"
unset CARGO_TARGET_DIR
export CARGO_TARGET_DIR="${ROOT}/target"

echo "workspace=$WS"
echo "prompts=$PROMPTS ($FROM-$TO)"
echo "config=$VIBE_CONFIG"
echo "out=$MP4"

# Free port if a leftover vite is holding it
if lsof -ti :1420 >/dev/null 2>&1; then
  echo "Port 1420 busy — stopping leftover process"
  lsof -ti :1420 | xargs kill -9 2>/dev/null || true
  sleep 1
fi

# Bring Maqqa to front and return "x,y,w,h" for its main window (empty on failure).
maqqa_window_rect() {
  local pid="$1"
  osascript 2>/dev/null <<OSA || true
tell application "System Events"
  set frontmost of (first process whose unix id is ${pid}) to true
  tell (first process whose unix id is ${pid})
    if (count of windows) is 0 then return ""
    tell window 1
      set {x, y} to position
      set {w, h} to size
      return (x as text) & "," & (y as text) & "," & (w as text) & "," & (h as text)
    end tell
  end tell
end tell
OSA
}

FRAMES_DIR="/tmp/vibe_demo_frames_$$"
REC_PID=""

cd "${ROOT}/apps/desktop"
echo "Starting Vite…"
npm run dev >/tmp/vibe_vite.log 2>&1 &
VITE_PID=$!
for _ in $(seq 1 40); do
  curl -sf http://localhost:1420/ >/dev/null 2>&1 && break
  sleep 0.25
done

source "$HOME/.cargo/env" 2>/dev/null || true
echo "Building vibe_editor…"
(cd "$ROOT" && cargo build -p vibe_editor -q)
echo "Launching Maqqa…"
"$ROOT/target/debug/vibe_editor" >/tmp/vibe_editor_demo.log 2>&1 &
TAURI_PID=$!
sleep 2
if ! kill -0 "$TAURI_PID" 2>/dev/null; then
  echo "Editor failed to start — see /tmp/vibe_editor_demo.log" >&2
  cat /tmp/vibe_editor_demo.log >&2 || true
  exit 1
fi
echo "editor pid=$TAURI_PID (look for the Maqqa window)"

# Wait briefly for the window, then start focused capture
for _ in $(seq 1 20); do
  RECT="$(maqqa_window_rect "$TAURI_PID")"
  [[ -n "$RECT" ]] && break
  sleep 0.25
done
echo "window rect=${RECT:-unknown}"

if [[ "$RECORD" -eq 1 ]]; then
  # Timed region screenshots of the Maqqa window (Screen Recording permission required).
  echo "Starting screenshot recorder → $FRAMES_DIR"
  mkdir -p "$FRAMES_DIR"
  rm -f "$FRAMES_DIR"/frame_*.png
  (
    i=1
    end=$((SECONDS + MAX_SECS))
    while (( SECONDS < end )); do
      printf -v f "%s/frame_%04d.png" "$FRAMES_DIR" "$i"
      # Re-activate + refresh bounds so Cursor/Finder don't steal the frame
      r="$(maqqa_window_rect "$TAURI_PID")"
      if [[ -n "$r" ]]; then
        screencapture -x -R "$r" "$f" 2>/dev/null || true
      else
        screencapture -x -C "$f" 2>/dev/null || true
      fi
      if [[ ! -s "$f" ]]; then
        echo "screenshot failed at frame $i — grant Screen Recording to Terminal/Cursor" >&2
        break
      fi
      i=$((i + 1))
      sleep 2
    done
  ) >/tmp/vibe_screencap_loop.log 2>&1 &
  REC_PID=$!
  echo "recording pid=$REC_PID (Maqqa window every 2s)"
fi

echo "Waiting for demo (max ${MAX_SECS}s)…"
DEADLINE=$((SECONDS + MAX_SECS))
SCAFFOLD_SEEN=0
while (( SECONDS < DEADLINE )); do
  if [[ -f "${WS}/jobs_aggregator/models.py" ]] || [[ -f "${WS}/pyproject.toml" ]] || [[ -f "${WS}/README.md" ]]; then
    if [[ "$SCAFFOLD_SEEN" -eq 0 ]]; then
      SCAFFOLD_SEEN=1
      LEFT=$(( (TO - FROM) * 55 + 40 ))
      (( LEFT > 45 )) || LEFT=45
      echo "Scaffold detected — allowing ~${LEFT}s more for remaining prompts…"
      sleep "$LEFT"
      break
    fi
  fi
  if ! kill -0 "$TAURI_PID" 2>/dev/null; then
    echo "Editor exited early — see /tmp/vibe_editor_demo.log" >&2
    break
  fi
  sleep 3
done

if [[ "$RECORD" -eq 1 && -n "$REC_PID" ]]; then
  echo "Stopping recording…"
  kill -INT "$REC_PID" 2>/dev/null || true
  wait "$REC_PID" 2>/dev/null || true
  REC_PID=""
  N=$(find "$FRAMES_DIR" -name 'frame_*.png' 2>/dev/null | wc -l | tr -d ' ')
  if [[ "${N:-0}" -ge 3 ]]; then
    echo "Encoding $N frames → $MP4 …"
    ffmpeg -y -framerate 2 -i "$FRAMES_DIR/frame_%04d.png" \
      -c:v libx264 -pix_fmt yuv420p -preset fast -crf 23 \
      -movflags +faststart "$MP4" </dev/null >/tmp/vibe_ffmpeg_encode.log 2>&1
    if ffprobe -v error -show_entries format=duration -of default=nw=1:nk=1 "$MP4" >/dev/null 2>&1; then
      echo "Video: $MP4"
      ls -lh "$MP4"
      ffprobe -v error -show_entries format=duration,size -of default=nw=1 "$MP4" || true
      # Stable names for README / demos folder
      cp -f "$MP4" "${OUT_DIR}/maqqa_demo_interactive.mp4"
      ln -sfn "maqqa_demo_interactive.mp4" "${OUT_DIR}/vibe_demo_interactive.mp4" 2>/dev/null \
        || cp -f "$MP4" "${OUT_DIR}/vibe_demo_interactive.mp4"
      # Start / mid / end stills from the capture set (macOS bash 3–safe)
      START=$(ls "$FRAMES_DIR"/frame_*.png | sort | head -1)
      MID=$(ls "$FRAMES_DIR"/frame_*.png | sort | sed -n "$(( (N + 1) / 2 ))p")
      END=$(ls "$FRAMES_DIR"/frame_*.png | sort | tail -1)
      cp -f "$START" "${OUT_DIR}/preview_start.png"
      cp -f "$MID" "${OUT_DIR}/preview_mid.png"
      cp -f "$END" "${OUT_DIR}/preview_end.png"
      echo "Stills: preview_{start,mid,end}.png"
    else
      echo "WARNING: encode failed — see /tmp/vibe_ffmpeg_encode.log" >&2
    fi
  else
    echo "WARNING: too few screenshots ($N) — grant Screen Recording to Terminal." >&2
  fi
  rm -rf "$FRAMES_DIR"
fi

echo "Demo workspace: $WS"
if [[ "$AUTO_EXIT" -eq 1 ]]; then
  echo "Auto-exit: closing editor (pid $TAURI_PID)"
  if [[ -n "${TAURI_PID}" ]] && kill -0 "$TAURI_PID" 2>/dev/null; then
    kill "$TAURI_PID" 2>/dev/null || true
    wait "$TAURI_PID" 2>/dev/null || true
  fi
  TAURI_PID=""
  exit 0
fi
echo "Editor still running (pid $TAURI_PID) — close the window when done, or Ctrl+C."
# Don't kill editor on exit from here — clear trap kill of editor by unsetting before wait
wait "$TAURI_PID" || true
TAURI_PID=""
