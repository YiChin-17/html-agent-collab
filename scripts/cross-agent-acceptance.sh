#!/bin/zsh
set -euo pipefail

AGENT=${1:?"usage: scripts/cross-agent-acceptance.sh <claude-code|codex>"}
[[ "$AGENT" == "claude-code" || "$AGENT" == "codex" ]] || {
  print -u2 "agent must be claude-code or codex"
  exit 2
}

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PROJECT="$ROOT/tests/fixtures/acceptance"
COLLAB="$ROOT/target/debug/collab"
STAMP=$(date +%Y%m%d-%H%M%S)
EVIDENCE_DIR="$PROJECT/.acceptance/$AGENT-$STAMP"
TRANSCRIPT="$EVIDENCE_DIR/transcript.jsonl"
TIMING="$EVIDENCE_DIR/timing.json"
PROCESS_TREE="$EVIDENCE_DIR/process-tree.tsv"
ARTIFACT_DIR="$EVIDENCE_DIR/feedback-artifacts"
PROMPT_FILE="$EVIDENCE_DIR/prompts.txt"

for command_name in jq curl shasum pgrep ps awk perl; do
  command -v "$command_name" >/dev/null || {
    print -u2 "missing required command: $command_name"
    exit 2
  }
done
[[ -x "$COLLAB" ]] || {
  print -u2 "missing collab binary: $COLLAB"
  exit 2
}

mkdir -p "$EVIDENCE_DIR" "$ARTIFACT_DIR"
rm -rf "$PROJECT/.collab"
cp "$PROJECT/index.base.html" "$PROJECT/index.html"
export PATH="$ROOT/target/debug:$PATH"

PREVIEW_PID=
AGENT_PID=
SAMPLER_PID=

cleanup() {
  if [[ -f "$PROJECT/.collab/session.json" ]]; then
    "$COLLAB" close --project "$PROJECT" >/dev/null 2>&1 || true
  fi
  [[ -z "$SAMPLER_PID" ]] || kill "$SAMPLER_PID" >/dev/null 2>&1 || true
  [[ -z "$AGENT_PID" ]] || kill "$AGENT_PID" >/dev/null 2>&1 || true
  [[ -z "$PREVIEW_PID" ]] || kill "$PREVIEW_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

"$COLLAB" open "$PROJECT" >"$EVIDENCE_DIR/preview.log" 2>&1 &
PREVIEW_PID=$!

deadline=$(( $(date +%s) + 30 ))
while [[ ! -f "$PROJECT/.collab/session.json" ]]; do
  (( $(date +%s) < deadline )) || {
    print -u2 "preview did not start"
    exit 1
  }
  sleep 0.1
done

PORT=$(jq -er '.port' "$PROJECT/.collab/session.json")

descendants() {
  local pid=$1
  print -r -- "$pid"
  local child
  for child in $(pgrep -P "$pid" 2>/dev/null || true); do
    descendants "$child"
  done
}

process_tree_rss_kib() {
  local pid
  for pid in $(descendants "$PREVIEW_PID"); do
    ps -o rss= -p "$pid" 2>/dev/null || true
  done | awk '{ total += $1 } END { print total + 0 }'
}

print -r -- $'epoch\tpreview_rss_kib\tattachment_count\tfeedback_memory_items' >"$PROCESS_TREE"
(
  while kill -0 "$PREVIEW_PID" 2>/dev/null; do
    metrics=$(curl --silent "http://127.0.0.1:${PORT}/__collab__/metrics" || print '{}')
    print -r -- "$(date +%s)"$'\t'"$(process_tree_rss_kib)"$'\t'"$(print -r -- "$metrics" | jq -r '.attachmentCount // -1')"$'\t'"$(print -r -- "$metrics" | jq -r '.feedbackMemoryItems // -1')" >>"$PROCESS_TREE"
    sleep 1
  done
) &
SAMPLER_PID=$!

launch_start_agent() {
  local phase=$1
  local prompt="請明確載入並遵循專案中的 preview-collaboration-start skill，以 $PROJECT/index.html 開始或沿用 preview collaboration。這是 $phase 階段。只可修改 $PROJECT/index.html，不可修改其他檔案。必須依 skill 使用 collab open --background、attach，並持續執行 wait、acknowledge、show、working、修改、collab eval、collab screenshot、resolved 或 failed、再次 wait；收到 collaboration.stop 才結束。你的 agent kind 是 $AGENT。"
  print -r -- "[$phase] $prompt" >>"$PROMPT_FILE"
  jq -nc --arg event "harness.start" --arg phase "$phase" \
    '{event: $event, phase: $phase}' >>"$TRANSCRIPT"

  if [[ "$AGENT" == "claude-code" ]]; then
    claude -p \
      --model sonnet \
      --dangerously-skip-permissions \
      --no-session-persistence \
      --output-format stream-json \
      --verbose \
      "$prompt" >>"$TRANSCRIPT" 2>&1 &
  else
    codex exec \
      --ephemeral \
      --dangerously-bypass-approvals-and-sandbox \
      --json \
      -C "$ROOT" \
      "$prompt" >>"$TRANSCRIPT" 2>&1 &
  fi
  AGENT_PID=$!
}

wait_for_active_attachment() {
  local deadline=$(( $(date +%s) + 120 ))
  while true; do
    active_attachment_count=$(curl --fail --silent "http://127.0.0.1:${PORT}/__collab__/metrics" | jq -er '.activeAttachmentCount')
    (( active_attachment_count >= 1 )) && return
    kill -0 "$AGENT_PID" 2>/dev/null || {
      print -u2 "$AGENT exited before attaching"
      return 1
    }
    (( $(date +%s) < deadline )) || {
      print -u2 "$AGENT did not attach"
      return 1
    }
    sleep 0.25
  done
}

launch_start_agent first-start
wait_for_active_attachment
"$COLLAB" status --project "$PROJECT" \
  | jq -e '.data.attachments | map(select(.active)) | last' \
  >"$EVIDENCE_DIR/first-attachment.json"
FIRST_ATTACHMENT_ID=$(jq -er '.attachmentId' "$EVIDENCE_DIR/first-attachment.json")

now_ms() {
  perl -MTime::HiRes=time -e 'printf "%.0f\n", time * 1000'
}

wait_for_new_feedback() {
  local before_count=$1
  local deadline=$(( $(date +%s) + 30 ))
  while true; do
    local files=("$PROJECT"/.collab/feedback/*.json(N))
    if (( ${#files[@]} > before_count )); then
      ls -t "$PROJECT"/.collab/feedback/*.json | head -1
      return
    fi
    (( $(date +%s) < deadline )) || return 1
    sleep 0.1
  done
}

wait_for_state() {
  local record=$1
  local expected=$2
  local deadline=$(( $(date +%s) + 240 ))
  while true; do
    [[ "$(jq -r '.state' "$record")" == "$expected" ]] && return
    kill -0 "$AGENT_PID" 2>/dev/null || {
      print -u2 "$AGENT exited before feedback reached $expected"
      return 1
    }
    (( $(date +%s) < deadline )) || return 1
    sleep 0.2
  done
}

wait_until_agent_waits_again() {
  local deadline=$(( $(date +%s) + 60 ))
  while true; do
    local resolved_line=""
    local wait_line=""
    resolved_line=$(grep -n "resolved --expected working" "$TRANSCRIPT" | tail -1 | cut -d: -f1 || true)
    wait_line=$(grep -n "collab wait --project" "$TRANSCRIPT" | tail -1 | cut -d: -f1 || true)
    if [[ -n "$resolved_line" && -n "$wait_line" ]] && (( wait_line > resolved_line )); then
      return
    fi
    kill -0 "$AGENT_PID" 2>/dev/null || return 1
    (( $(date +%s) < deadline )) || return 1
    sleep 0.2
  done
}

measure_reload() {
  local original_checksum=$1
  local expression=$2
  local expected=$3
  local deadline=$(( $(date +%s) + 240 ))
  local changed_ms=
  while true; do
    if [[ "$(shasum "$PROJECT/index.html" | awk '{print $1}')" != "$original_checksum" ]]; then
      changed_ms=$(now_ms)
      break
    fi
    kill -0 "$AGENT_PID" 2>/dev/null || return 1
    (( $(date +%s) < deadline )) || return 1
    sleep 0.05
  done
  deadline=$(( $(date +%s) + 5 ))
  while true; do
    value=$("$COLLAB" eval --project "$PROJECT" "$expression" | jq -r '.data.value // empty')
    if [[ "$value" == "$expected" ]]; then
      print -r -- $(( $(now_ms) - changed_ms ))
      return
    fi
    (( $(date +%s) < deadline )) || return 1
    sleep 0.05
  done
}

copy_feedback_artifacts() {
  local record=$1
  local feedback_id
  feedback_id=$(jq -er '.id' "$record")
  cp "$record" "$ARTIFACT_DIR/$feedback_id.json"
  jq -r '.attachments[]?' "$record" | while IFS= read -r attachment; do
    [[ -f "$attachment" ]] && cp "$attachment" "$ARTIFACT_DIR/"
  done
}

feedback_files=("$PROJECT"/.collab/feedback/*.json(N))
feedback_count=${#feedback_files[@]}
first_checksum=$(shasum "$PROJECT/index.html" | awk '{print $1}')
first_text="Agent Verified: $AGENT"
"$COLLAB" eval --project "$PROJECT" \
  "window.__collabOverlay.submitElementComment(document.querySelector('#hero-title'),'Change #hero-title text to \"$first_text\" and set data-agent=\"$AGENT\".'); true" \
  >"$EVIDENCE_DIR/submit-element.json"
first_record=$(wait_for_new_feedback "$feedback_count")
first_reload_ms=$(measure_reload "$first_checksum" "document.querySelector('#hero-title').textContent.trim()" "$first_text")
wait_for_state "$first_record" resolved
copy_feedback_artifacts "$first_record"
"$COLLAB" screenshot --project "$PROJECT" >"$EVIDENCE_DIR/element-screenshot.json"

wait_until_agent_waits_again
feedback_files=("$PROJECT"/.collab/feedback/*.json(N))
feedback_count=${#feedback_files[@]}
draft_checksum=$(shasum "$PROJECT/index.html" | awk '{print $1}')
print -r -- "$draft_checksum" >"$EVIDENCE_DIR/preview-draft-before.sha256"
draft_text="Preview Draft: $AGENT"
draft_source_json=$(jq -Rs . "$PROJECT/index.html")
draft_text_json=$(jq -Rn --arg value "$draft_text" '$value')
"$COLLAB" eval --project "$PROJECT" \
  "const overlay=window.__collabOverlay;const source=$draft_source_json;const draftText=$draft_text_json;const target=document.querySelector('#draft-target');if(!source.toLowerCase().includes('<!doctype html>'))throw new Error('expected complete HTML source');if(target.textContent.trim()!=='source-backed Hello')throw new Error('expected source-backed Hello');overlay.loadPreviewDraftSource({pageUrl:location.href,html:source});overlay.setMode('draft');overlay.openPreviewDraftFor(target);overlay.applyPreviewDraft({html:source.replace('source-backed Hello',draftText)});overlay.previewDraftState()" \
  >"$EVIDENCE_DIR/preview-draft-memory-edit.json"
shasum "$PROJECT/index.html" | awk '{print $1}' \
  >"$EVIDENCE_DIR/preview-draft-after-memory-edit.sha256"
cmp "$EVIDENCE_DIR/preview-draft-before.sha256" \
  "$EVIDENCE_DIR/preview-draft-after-memory-edit.sha256"
"$COLLAB" eval --project "$PROJECT" \
  "window.__collabOverlay.submitPreviewDraft(); true" \
  >"$EVIDENCE_DIR/preview-draft-submit.json"
shasum "$PROJECT/index.html" | awk '{print $1}' \
  >"$EVIDENCE_DIR/preview-draft-after-submit.sha256"
cmp "$EVIDENCE_DIR/preview-draft-before.sha256" \
  "$EVIDENCE_DIR/preview-draft-after-submit.sha256"
draft_record=$(wait_for_new_feedback "$feedback_count")
draft_reload_ms=$(measure_reload "$draft_checksum" \
  "document.querySelector('#draft-target').textContent.trim()" "$draft_text")
wait_for_state "$draft_record" resolved
cp "$draft_record" "$EVIDENCE_DIR/preview-draft-feedback.json"
copy_feedback_artifacts "$draft_record"
"$COLLAB" screenshot --project "$PROJECT" >"$EVIDENCE_DIR/preview-draft-screenshot.json"

wait_until_agent_waits_again
cp "$PROJECT/.collab/session.json" "$EVIDENCE_DIR/session-before-stop.json"
jq -nc --arg event "harness.sigint" --arg attachmentId "$FIRST_ATTACHMENT_ID" \
  '{event: $event, attachmentId: $attachmentId}' >>"$TRANSCRIPT"
kill -INT "$AGENT_PID"
wait "$AGENT_PID" || true
AGENT_PID=

"$COLLAB" detach --project "$PROJECT" --attachment "$FIRST_ATTACHMENT_ID" \
  >"$EVIDENCE_DIR/stop-detach.json"
jq -nc --arg event "preview-collaboration-stop" --arg attachmentId "$FIRST_ATTACHMENT_ID" \
  '{event: $event, command: "collab detach", attachmentId: $attachmentId}' >>"$TRANSCRIPT"

launch_start_agent second-start
wait_for_active_attachment
"$COLLAB" status --project "$PROJECT" \
  | jq -e '.data.attachments | map(select(.active)) | last' \
  >"$EVIDENCE_DIR/second-attachment.json"
SECOND_ATTACHMENT_ID=$(jq -er '.attachmentId' "$EVIDENCE_DIR/second-attachment.json")
[[ "$SECOND_ATTACHMENT_ID" != "$FIRST_ATTACHMENT_ID" ]] || {
  print -u2 "restart reused the detached attachment instead of creating a new one"
  exit 1
}
cp "$PROJECT/.collab/session.json" "$EVIDENCE_DIR/session-after-restart.json"
jq -e -s \
  '.[0] as $before | .[1] as $after |
   $before.sessionId == $after.sessionId and
   $before.port == $after.port and
   $before.pid == $after.pid and
   $before.entryFile == $after.entryFile' \
  "$EVIDENCE_DIR/session-before-stop.json" \
  "$EVIDENCE_DIR/session-after-restart.json" >/dev/null

feedback_files=("$PROJECT"/.collab/feedback/*.json(N))
feedback_count=${#feedback_files[@]}
second_checksum=$(shasum "$PROJECT/index.html" | awk '{print $1}')
second_text="Paint Verified: $AGENT"
"$COLLAB" eval --project "$PROJECT" \
  "window.__collabOverlay.clearMarks();var r=document.querySelector('#paint-target').getBoundingClientRect();window.__collabOverlay.addMark({type:'rect',x:r.x+8,y:r.y+8,width:r.width-16,height:r.height-16});window.__collabOverlay.addMark({type:'label',x:r.x+20,y:r.y+34,text:'Update CTA'});window.__collabOverlay.submitPainting('Change #cta text to \"$second_text\" and set data-agent=\"$AGENT\".'); true" \
  >"$EVIDENCE_DIR/submit-painting.json"
second_record=$(wait_for_new_feedback "$feedback_count")
second_reload_ms=$(measure_reload "$second_checksum" "document.querySelector('#cta').textContent.trim()" "$second_text")
wait_for_state "$second_record" resolved
copy_feedback_artifacts "$second_record"
"$COLLAB" screenshot --project "$PROJECT" >"$EVIDENCE_DIR/painting-screenshot.json"

wait_until_agent_waits_again
"$COLLAB" close --project "$PROJECT" >"$EVIDENCE_DIR/close.json"
jq -nc --arg event "preview-collaboration-close" \
  '{event: $event, command: "collab close"}' >>"$TRANSCRIPT"

deadline=$(( $(date +%s) + 120 ))
while kill -0 "$AGENT_PID" 2>/dev/null; do
  (( $(date +%s) < deadline )) || {
    print -u2 "$AGENT did not exit after preview close"
    exit 1
  }
  sleep 0.25
done
wait "$AGENT_PID"
AGENT_PID=

(( first_reload_ms <= 1000 )) || {
  print -u2 "element reload ${first_reload_ms}ms exceeds 1000ms"
  exit 1
}
(( second_reload_ms <= 1000 )) || {
  print -u2 "painting reload ${second_reload_ms}ms exceeds 1000ms"
  exit 1
}

jq -n \
  --arg agent "$AGENT" \
  --argjson elementReloadElapsedMs "$first_reload_ms" \
  --argjson previewDraftReloadElapsedMs "$draft_reload_ms" \
  --argjson paintingReloadElapsedMs "$second_reload_ms" \
  '{
    agent: $agent,
    reloadElapsedMs: {
      elementComment: $elementReloadElapsedMs,
      previewDraft: $previewDraftReloadElapsedMs,
      paintingTextbox: $paintingReloadElapsedMs
    }
  }' >"$TIMING"

print -r -- "cross-agent acceptance passed; evidence: $EVIDENCE_DIR"
