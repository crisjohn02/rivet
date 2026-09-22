#!/usr/bin/env bash
# Run one implementation task through OpenCode in the main checkout.
#
#   orchestration/run-task.sh T20
#
# Writes the transcript to orchestration/runs/<task>.jsonl and prints the
# implementor's final report. A startup watchdog kills and retries an OpenCode
# session that produces no model events within STARTUP_SECS (it sometimes hangs
# before sending the prompt); up to three attempts.
set -u
NAME="${1:?usage: run-task.sh <task-id>}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROMPT="${2:-$ROOT/orchestration/prompts/$NAME.md}"
STARTUP_SECS="${STARTUP_SECS:-150}"
[ -f "$PROMPT" ] || { echo "no prompt file: $PROMPT"; exit 1; }
mkdir -p "$ROOT/orchestration/runs"
OUT="$ROOT/orchestration/runs/$NAME.jsonl"
command -v cargo >/dev/null || export PATH="$HOME/.cargo/bin:$PATH"
cd "$ROOT" || exit 1
for attempt in 1 2 3; do
  : > "$OUT"
  opencode run --standalone --print-logs --log-level warn --auto \
    --title "rivet $NAME" --format json "$(cat "$PROMPT")" > "$OUT" 2>&1 &
  PID=$!
  started=0
  for ((i=0; i<STARTUP_SECS/5; i++)); do
    sleep 5
    if grep -q '"type":"tool"\|"type":"text"' "$OUT" 2>/dev/null; then started=1; break; fi
    kill -0 "$PID" 2>/dev/null || break
  done
  if [ "$started" = 1 ]; then wait "$PID"; echo "opencode exit=$? (attempt $attempt)"; break; fi
  if kill -0 "$PID" 2>/dev/null; then
    echo "attempt $attempt: no model events after ${STARTUP_SECS}s; killing stalled session"
    pkill -P "$PID" 2>/dev/null; kill "$PID" 2>/dev/null; sleep 2; kill -9 "$PID" 2>/dev/null
  else
    wait "$PID"; echo "attempt $attempt: exited early ($?) with no events"; head -c 800 "$OUT"; echo
  fi
  [ "$attempt" = 3 ] && { echo "GAVE UP after 3 attempts"; exit 3; }
done
exec "$ROOT/orchestration/report.py" "$OUT"
