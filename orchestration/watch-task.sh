#!/usr/bin/env bash
# Run one task through OpenCode with a stall/death watchdog.
#
#   orchestration/watch-task.sh T21
#
# Differs from run-task.sh: keeps watching for the whole run, not just startup.
# Every CHECK_SECS it asks two questions — is the process alive, and has the
# transcript grown? A dead process without a final report, or a transcript that
# has not grown for STALL_SECS, is treated as a dead session and retried from
# scratch. Status is appended to orchestration/runs/<task>.status so an
# orchestrator can poll one file instead of parsing the transcript.
set -u
NAME="${1:?usage: watch-task.sh <task-id>}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROMPT="${2:-$ROOT/orchestration/prompts/$NAME.md}"
CHECK_SECS="${CHECK_SECS:-300}"     # watcher wakes this often
STALL_SECS="${STALL_SECS:-900}"     # no transcript growth this long = dead
MAX_ATTEMPTS="${MAX_ATTEMPTS:-3}"
[ -f "$PROMPT" ] || { echo "no prompt file: $PROMPT"; exit 1; }
mkdir -p "$ROOT/orchestration/runs"
OUT="$ROOT/orchestration/runs/$NAME.jsonl"
STATUS="$ROOT/orchestration/runs/$NAME.status"
command -v cargo >/dev/null || export PATH="$HOME/.cargo/bin:$PATH"
cd "$ROOT" || exit 1

say() { echo "[$(date '+%H:%M:%S')] $*" | tee -a "$STATUS"; }
# Model events only. `--print-logs` mixes plain-text server logs into the same
# stream, and those keep flowing even when the model is wedged, so byte growth
# is not evidence of progress.
events_of() { grep -c '"type":"tool_use"\|"type":"text"\|"type":"step_start"' "$1" 2>/dev/null || echo 0; }

: > "$STATUS"
for attempt in $(seq 1 "$MAX_ATTEMPTS"); do
  : > "$OUT"
  say "attempt $attempt/$MAX_ATTEMPTS: starting opencode for $NAME"
  opencode run --standalone --print-logs --log-level warn --auto \
    --title "rivet $NAME" --format json "$(cat "$PROMPT")" > "$OUT" 2>&1 &
  PID=$!
  last_size=0
  quiet_for=0
  verdict=""
  while :; do
    sleep "$CHECK_SECS" &
    wait $!
    now_size="$(events_of "$OUT")"
    if kill -0 "$PID" 2>/dev/null; then alive=1; else alive=0; fi
    if [ "$now_size" -gt "$last_size" ]; then
      quiet_for=0
      say "alive=$alive events=$now_size (growing)"
    else
      quiet_for=$(( quiet_for + CHECK_SECS ))
      say "alive=$alive events=$now_size (no growth for ${quiet_for}s)"
    fi
    last_size="$now_size"

    if [ "$alive" = 0 ]; then
      wait "$PID"; rc=$?
      # A real finish writes a final assistant message; anything else is a death.
      if grep -q '"type":"text"' "$OUT" 2>/dev/null && [ "$now_size" -gt 5 ]; then
        verdict="done"; say "opencode exited rc=$rc with a transcript; treating as finished"
      else
        verdict="dead"; say "opencode exited rc=$rc with no usable transcript"
      fi
      break
    fi
    if [ "$quiet_for" -ge "$STALL_SECS" ]; then
      say "stalled ${quiet_for}s with no output; killing session"
      pkill -P "$PID" 2>/dev/null; kill "$PID" 2>/dev/null; sleep 3; kill -9 "$PID" 2>/dev/null
      wait "$PID" 2>/dev/null
      verdict="stalled"
      break
    fi
  done

  if [ "$verdict" = "done" ]; then
    say "RUN COMPLETE for $NAME"
    "$ROOT/orchestration/report.py" "$OUT" | tee -a "$STATUS"
    say "STATUS=ok"
    exit 0
  fi
  say "retrying after verdict=$verdict"
done
say "STATUS=gaveup"
exit 3
