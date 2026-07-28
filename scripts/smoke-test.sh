#!/bin/zsh
# 啟動 collab open 並確認前 3 秒內無 crash。
# 用途：涉及 native UI 改動後的最低限度驗證。
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
COLLAB="$ROOT/target/debug/collab"
FIXTURE="$ROOT/tests/fixtures/session-ux/index.html"
PROJECT=$(mktemp -d "/tmp/collab-smoke-test.XXXXXX")
ENTRY="$PROJECT/index.html"
WAIT_SECONDS=3

cleanup() {
  if [[ -n "${COLLAB_PID:-}" ]] && kill -0 "$COLLAB_PID" 2>/dev/null; then
    kill "$COLLAB_PID" 2>/dev/null || true
    wait "$COLLAB_PID" 2>/dev/null || true
  fi
  rm -rf "$PROJECT"
}
trap cleanup EXIT

[[ -x "$COLLAB" ]] || {
  print -u2 "missing collab binary: $COLLAB (run cargo build first)"
  exit 2
}
[[ -f "$FIXTURE" ]] || {
  print -u2 "missing fixture: $FIXTURE"
  exit 2
}

cp "$FIXTURE" "$ENTRY"

print "starting collab open (waiting ${WAIT_SECONDS}s for crash)..."
"$COLLAB" open "$ENTRY" &
COLLAB_PID=$!

sleep "$WAIT_SECONDS"

if kill -0 "$COLLAB_PID" 2>/dev/null; then
  print "smoke test passed: process $COLLAB_PID still alive after ${WAIT_SECONDS}s"
  exit 0
else
  wait "$COLLAB_PID" 2>/dev/null
  EXIT_CODE=$?
  print -u2 "smoke test FAILED: process exited with code $EXIT_CODE within ${WAIT_SECONDS}s"
  exit 1
fi
