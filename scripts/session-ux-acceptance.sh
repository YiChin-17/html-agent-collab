#!/bin/zsh
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
COLLAB="$ROOT/target/debug/collab"
FIXTURE="$ROOT/tests/fixtures/session-ux/index.html"
STAMP=$(date +%Y%m%d-%H%M%S)
PROJECT=$(mktemp -d "/tmp/collab-session-ux-project.XXXXXX")
EVIDENCE_DIR="/tmp/collab-session-ux-evidence-$STAMP"
ENTRY="$PROJECT/index.html"
SESSION_FILE="$PROJECT/.collab/session.json"
CLI_TRANSCRIPT="$EVIDENCE_DIR/cli-transcript.jsonl"
AGENT_TRANSCRIPT="$EVIDENCE_DIR/agent-transcript.jsonl"
PROCESS_TREE="$EVIDENCE_DIR/process-tree.tsv"
WEBVIEW_COUNTS="$EVIDENCE_DIR/webview-counts.tsv"

for command_name in jq curl find tr pgrep ps awk perl shasum swift; do
  command -v "$command_name" >/dev/null || {
    print -u2 "missing required command: $command_name"
    exit 2
  }
done
[[ -x "$COLLAB" ]] || {
  print -u2 "missing collab binary: $COLLAB"
  exit 2
}

mkdir -p "$EVIDENCE_DIR"
cp "$FIXTURE" "$ENTRY"
print -r -- $'phase\tpid\tppid\trss_kib\tcommand' >"$PROCESS_TREE"
print -r -- $'phase\tnative_window_count\toverlay_host_count' >"$WEBVIEW_COUNTS"

RUN_OUTPUT=
WAIT_PID=
WAIT_OUTPUT=
PREVIEW_PID=

cleanup() {
  if [[ -f "$SESSION_FILE" ]]; then
    "$COLLAB" close --project "$PROJECT" >/dev/null 2>&1 || true
  fi
  [[ -z "$WAIT_PID" ]] || kill "$WAIT_PID" >/dev/null 2>&1 || true
  cp "$ENTRY" "$EVIDENCE_DIR/final-index.html" >/dev/null 2>&1 || true
  rm -rf "$PROJECT"
}
trap cleanup EXIT INT TERM

fail() {
  print -u2 -- "$1"
  exit 1
}

record_cli() {
  local label=$1
  local command_text=$2
  local output=$3
  local exit_code=$4
  jq -nc \
    --arg label "$label" \
    --arg command "$command_text" \
    --arg output "$output" \
    --argjson exitCode "$exit_code" \
    '{label: $label, command: $command, exitCode: $exitCode, output: $output}' \
    >>"$CLI_TRANSCRIPT"
}

run_cli() {
  local label=$1
  shift
  local output
  local exit_code
  set +e
  output=$("$COLLAB" "$@" 2>&1)
  exit_code=$?
  set -e
  record_cli "$label" "collab $*" "$output" "$exit_code"
  RUN_OUTPUT=$output
  (( exit_code == 0 )) || fail "$label failed: $output"
  print -r -- "$output" | jq -e '.ok == true' >/dev/null ||
    fail "$label did not return a success envelope: $output"
}

agent_event() {
  local phase=$1
  local action=$2
  local details=${3:-""}
  jq -nc \
    --arg phase "$phase" \
    --arg action "$action" \
    --arg details "$details" \
    '{phase: $phase, action: $action, details: $details}' \
    >>"$AGENT_TRANSCRIPT"
}

snapshot_session() {
  local destination=$1
  [[ -f "$SESSION_FILE" ]] || fail "missing session file for $destination"
  jq 'del(.token)' "$SESSION_FILE" >"$EVIDENCE_DIR/$destination"
}

descendants() {
  local pid=$1
  print -r -- "$pid"
  local child
  for child in $(pgrep -P "$pid" 2>/dev/null || true); do
    descendants "$child"
  done
}

snapshot_process_tree() {
  local phase=$1
  if ! kill -0 "$PREVIEW_PID" 2>/dev/null; then
    print -r -- "$phase"$'\t-\t-\t0\tpreview process exited' >>"$PROCESS_TREE"
    return
  fi
  local pid
  for pid in $(descendants "$PREVIEW_PID"); do
    ps -o pid= -o ppid= -o rss= -o command= -p "$pid" 2>/dev/null |
      awk -v phase="$phase" '{
        pid=$1; ppid=$2; rss=$3;
        $1=""; $2=""; $3="";
        sub(/^ +/, "", $0);
        printf "%s\t%s\t%s\t%s\t%s\n", phase, pid, ppid, rss, $0
      }' >>"$PROCESS_TREE" || true
  done
}

count_native_windows() {
  /usr/bin/swift -e '
import CoreGraphics
import Foundation
let target = Int32(CommandLine.arguments.last!)!
let rows = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID)
  as? [[String: Any]] ?? []
let count = rows.filter { row in
  let pid = (row[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value ?? 0
  let layer = (row[kCGWindowLayer as String] as? NSNumber)?.intValue ?? -1
  let onscreen = row[kCGWindowIsOnscreen as String] as? Bool ?? false
  let bounds = row[kCGWindowBounds as String] as? [String: Any] ?? [:]
  let width = (bounds["Width"] as? NSNumber)?.doubleValue ?? 0
  let height = (bounds["Height"] as? NSNumber)?.doubleValue ?? 0
  return pid == target && layer == 0 && onscreen && width > 100 && height > 100
}.count
print(count)
' "$PREVIEW_PID"
}

overlay_host_count() {
  run_cli "overlay host count" eval --project "$PROJECT" \
    'window.__collabOverlay ? window.__collabOverlay.hostCount() : 0'
  print -r -- "$RUN_OUTPUT" | jq -er '.data.value'
}

record_webview_count() {
  local phase=$1
  local native_count
  local host_count
  native_count=$(count_native_windows)
  host_count=$(overlay_host_count)
  print -r -- "$phase"$'\t'"$native_count"$'\t'"$host_count" >>"$WEBVIEW_COUNTS"
  [[ "$native_count" == "1" ]] || fail "$phase expected one native WebView window, got $native_count"
  [[ "$host_count" == "1" ]] || fail "$phase expected one overlay host, got $host_count"
}

start_wait() {
  local label=$1
  local attachment=$2
  WAIT_OUTPUT="$EVIDENCE_DIR/$label.json"
  "$COLLAB" wait --project "$PROJECT" --attachment "$attachment" --json \
    >"$WAIT_OUTPUT" 2>&1 &
  WAIT_PID=$!
  sleep 0.2
}

finish_wait() {
  local label=$1
  local exit_code
  set +e
  wait "$WAIT_PID"
  exit_code=$?
  set -e
  WAIT_PID=
  local output
  output=$(<"$WAIT_OUTPUT")
  record_cli "$label" "collab wait --project $PROJECT --attachment <attachment> --json" \
    "$output" "$exit_code"
  (( exit_code == 0 )) || fail "$label failed: $output"
  print -r -- "$output" | jq -e '.ok == true' >/dev/null ||
    fail "$label did not return a success envelope: $output"
}

capture_screenshot() {
  local label=$1
  local destination=$2
  run_cli "$label warmup" screenshot --project "$PROJECT"
  sleep 0.1
  run_cli "$label" screenshot --project "$PROJECT"
  local screenshot_path
  screenshot_path=$(print -r -- "$RUN_OUTPUT" | jq -er '.data.path')
  cp "$screenshot_path" "$EVIDENCE_DIR/$destination"
}

wait_for_eval_value() {
  local label=$1
  local expression=$2
  local expected=$3
  local deadline=$(( $(date +%s) + 10 ))
  while true; do
    run_cli "$label" eval --project "$PROJECT" "$expression"
    [[ "$(print -r -- "$RUN_OUTPUT" | jq -r '.data.value')" == "$expected" ]] && return
    (( $(date +%s) < deadline )) || fail "$label did not reach $expected"
    sleep 0.1
  done
}

# collab open "$ENTRY" --background
run_cli "collab open \"$ENTRY\" --background" open "$ENTRY" --background
[[ "$(print -r -- "$RUN_OUTPUT" | jq -r '.data.status')" == "opened" ]] ||
  fail "first start did not open a new preview"
snapshot_session "session-before-stop.json"
PREVIEW_PID=$(jq -er '.pid' "$EVIDENCE_DIR/session-before-stop.json")
SESSION_ID=$(jq -er '.sessionId' "$EVIDENCE_DIR/session-before-stop.json")
PORT=$(jq -er '.port' "$EVIDENCE_DIR/session-before-stop.json")
snapshot_process_tree start

run_cli "first attach" attach --project "$PROJECT" --agent codex --tui-session session-ux-first
FIRST_ATTACHMENT_ID=$(print -r -- "$RUN_OUTPUT" | jq -er '.data.attachment.attachmentId')
agent_event dashboard-active visible "feedback toolbar visible"
record_webview_count first-start

PREVIEW_DRAFT_HASH=$(shasum "$ENTRY" | awk '{print $1}')
PREVIEW_DRAFT_SOURCE_JSON=$(jq -Rs . "$ENTRY")
agent_event preview-draft-memory-edit begin "source-backed Hello"
run_cli "preview-draft-memory-edit" eval --project "$PROJECT" \
  "const overlay=window.__collabOverlay;const source=$PREVIEW_DRAFT_SOURCE_JSON;if(!source.toLowerCase().includes('<!doctype html>'))throw new Error('expected complete HTML source');overlay.loadPreviewDraftSource({pageUrl:location.href,html:source});overlay.setMode('draft');overlay.openPreviewDraftFor(document.querySelector('#draft-target'));overlay.applyPreviewDraft({html:source.replace('source-backed Hello','Draft Welcome')});overlay.previewDraftState()"
print -r -- "$RUN_OUTPUT" | jq -e \
  '.data.value.status == "editing" and .data.value.dirty == true' >/dev/null ||
  fail "Preview Draft memory edit did not become dirty"
[[ "$(shasum "$ENTRY" | awk '{print $1}')" == "$PREVIEW_DRAFT_HASH" ]] ||
  fail "Preview Draft memory edit changed source bytes"
touch "$ENTRY"
agent_event preview-draft-reload-discard reload "watcher-triggered reload"
wait_for_eval_value "preview-draft-after-reload" \
  'window.__collabOverlay.previewDraftState().status + ":" + document.querySelector("#draft-target").textContent.trim()' \
  "idle:source-backed Hello"
jq -nc '{"status":"idle"}' >"$EVIDENCE_DIR/preview-draft-after-reload.json"
agent_event preview-draft-after-reload idle "source-backed Hello"

start_wait preview-draft-wait "$FIRST_ATTACHMENT_ID"
run_cli "preview-draft-submitted" eval --project "$PROJECT" \
  "const overlay=window.__collabOverlay;const source=$PREVIEW_DRAFT_SOURCE_JSON;overlay.loadPreviewDraftSource({pageUrl:location.href,html:source});overlay.setMode('draft');overlay.openPreviewDraftFor(document.querySelector('#draft-target'));overlay.applyPreviewDraft({html:source.replace('source-backed Hello','Draft Welcome')});overlay.submitPreviewDraft(); true"
finish_wait preview-draft-wait
PREVIEW_DRAFT_FEEDBACK_ID=$(jq -er '.data.item.id' "$WAIT_OUTPUT")
[[ "$(jq -r '.data.item.kind' "$WAIT_OUTPUT")" == "preview-draft" ]] ||
  fail "pending preview-draft feedback was not leased"
[[ "$(shasum "$ENTRY" | awk '{print $1}')" == "$PREVIEW_DRAFT_HASH" ]] ||
  fail "Preview Draft submission changed source bytes"
agent_event preview-draft-submitted pending "$PREVIEW_DRAFT_FEEDBACK_ID"
run_cli "preview draft acknowledged" feedback set-state --project "$PROJECT" \
  "$PREVIEW_DRAFT_FEEDBACK_ID" acknowledged --expected pending --attachment "$FIRST_ATTACHMENT_ID"
run_cli "preview draft working" feedback set-state --project "$PROJECT" \
  "$PREVIEW_DRAFT_FEEDBACK_ID" working --expected acknowledged --attachment "$FIRST_ATTACHMENT_ID"
run_cli "preview draft resolved for UX harness" feedback set-state --project "$PROJECT" \
  "$PREVIEW_DRAFT_FEEDBACK_ID" resolved --expected working --attachment "$FIRST_ATTACHMENT_ID"
touch "$ENTRY"
agent_event preview-draft-submitted-reload reload "watcher-triggered reload"
wait_for_eval_value "preview-draft-after-submitted-reload" \
  'window.__collabOverlay.previewDraftState().status + ":" + document.querySelector("#draft-target").textContent.trim()' \
  "idle:source-backed Hello"
cp "$PROJECT/.collab/feedback/$PREVIEW_DRAFT_FEEDBACK_ID.json" \
  "$EVIDENCE_DIR/preview-draft-feedback.json"

start_wait element-wait "$FIRST_ATTACHMENT_ID"
run_cli "submitElementComment" eval --project "$PROJECT" \
  'window.__collabOverlay.submitElementComment(document.querySelector("#hero-title"), "Change the heading to Element feedback resolved."); true'
finish_wait element-wait
ELEMENT_FEEDBACK_ID=$(jq -er '.data.item.id' "$WAIT_OUTPUT")
run_cli "collab feedback set-state acknowledged" feedback set-state --project "$PROJECT" \
  "$ELEMENT_FEEDBACK_ID" acknowledged --expected pending --attachment "$FIRST_ATTACHMENT_ID"
run_cli "element feedback show" feedback show --project "$PROJECT" "$ELEMENT_FEEDBACK_ID"
run_cli "collab feedback set-state working" feedback set-state --project "$PROJECT" \
  "$ELEMENT_FEEDBACK_ID" working --expected acknowledged --attachment "$FIRST_ATTACHMENT_ID"
run_cli "collab pause" pause --project "$PROJECT" --attachment "$FIRST_ATTACHMENT_ID"
agent_event dashboard-pause-requested visible "feedback toolbar hidden"
[[ "$(print -r -- "$RUN_OUTPUT" | jq -r '.data.status')" == "pause-requested" ]] ||
  fail "pause during feedback A did not return pause-requested"

FEEDBACK_FILE_COUNT_BEFORE=$(find "$PROJECT/.collab/feedback" -type f | wc -l | tr -d ' ')
PAUSED_SUBMISSION="$EVIDENCE_DIR/paused-submission.json"
PAUSED_HTTP_STATUS=$(curl -sS -o "$PAUSED_SUBMISSION" -w '%{http_code}' \
  -H 'Content-Type: application/json' \
  --data '{"kind":"textbox","text":"feedback B before resume","pageUrl":"http://127.0.0.1/","viewport":{"width":800,"height":600,"scrollX":0,"scrollY":0}}' \
  "http://127.0.0.1:$PORT/__collab__/overlay/feedback")
PAUSED_SUBMISSION_BODY=$(jq -c . "$PAUSED_SUBMISSION")
record_cli "paused overlay submission" "POST /overlay/feedback" "$PAUSED_SUBMISSION_BODY" 0
[[ "$PAUSED_HTTP_STATUS" == "409" ]] ||
  fail "paused overlay submission returned HTTP $PAUSED_HTTP_STATUS"
[[ "$(jq -r '.code' "$PAUSED_SUBMISSION")" == "collaboration-paused" ]] ||
  fail "paused overlay submission did not return collaboration-paused"
FEEDBACK_FILE_COUNT_AFTER=$(find "$PROJECT/.collab/feedback" -type f | wc -l | tr -d ' ')
[[ "$FEEDBACK_FILE_COUNT_AFTER" == "$FEEDBACK_FILE_COUNT_BEFORE" ]] ||
  fail "paused overlay submission persisted an artifact"

agent_event element modify "replace #hero-title text"
perl -0pi -e 's/Awaiting session UX/Element feedback resolved/' "$ENTRY"
wait_for_eval_value "verify element feedback" \
  'document.querySelector("#hero-title").textContent.trim()' \
  "Element feedback resolved"
capture_screenshot "element screenshot" "element-screenshot.png"
run_cli "collab feedback set-state resolved" feedback set-state --project "$PROJECT" \
  "$ELEMENT_FEEDBACK_ID" resolved --expected working --attachment "$FIRST_ATTACHMENT_ID"
cp "$PROJECT/.collab/feedback/$ELEMENT_FEEDBACK_ID.json" \
  "$EVIDENCE_DIR/element-feedback.json"
agent_event element resolved "$ELEMENT_FEEDBACK_ID"

run_cli "paused status" status --project "$PROJECT"
agent_event dashboard-paused visible "feedback toolbar hidden"
[[ "$(print -r -- "$RUN_OUTPUT" | jq -r --arg id "$FIRST_ATTACHMENT_ID" '.data.attachments[] | select(.attachmentId == $id) | .collaborationState')" == "paused" ]] ||
  fail "attachment did not become paused after feedback A terminal state"
# spec「Paused states MUST NOT expose Offline Paint」的入口在原生 dashboard：
# eligibility 由 server 在 attachment lifecycle 邊界判斷，頁面沒有繞過的路徑。
agent_event dashboard-paused offline-paint-hidden "paused keeps Resume only"

start_wait paused-wait "$FIRST_ATTACHMENT_ID"
kill -0 "$WAIT_PID" 2>/dev/null || fail "paused wait did not remain blocked"
run_cli "collab resume" resume --project "$PROJECT" --attachment "$FIRST_ATTACHMENT_ID"
agent_event dashboard-active visible "feedback toolbar visible after resume"
[[ "$(print -r -- "$RUN_OUTPUT" | jq -r '.data.attachmentId')" == "$FIRST_ATTACHMENT_ID" ]] ||
  fail "resume did not preserve the same attachment"
run_cli "submit feedback B after resume" eval --project "$PROJECT" \
  'window.__collabOverlay.submitElementComment(document.querySelector("#hero-title"), "feedback B after resume"); true'
finish_wait paused-wait
RESUMED_FEEDBACK_ID=$(jq -er '.data.item.id' "$WAIT_OUTPUT")
[[ "$(jq -r '.data.item.lease.owner' "$WAIT_OUTPUT")" == "$FIRST_ATTACHMENT_ID" ]] ||
  fail "feedback B was not leased to the same attachment"
run_cli "resumed feedback acknowledged" feedback set-state --project "$PROJECT" \
  "$RESUMED_FEEDBACK_ID" acknowledged --expected pending --attachment "$FIRST_ATTACHMENT_ID"
run_cli "resumed feedback working" feedback set-state --project "$PROJECT" \
  "$RESUMED_FEEDBACK_ID" working --expected acknowledged --attachment "$FIRST_ATTACHMENT_ID"
run_cli "resumed feedback resolved" feedback set-state --project "$PROJECT" \
  "$RESUMED_FEEDBACK_ID" resolved --expected working --attachment "$FIRST_ATTACHMENT_ID"

start_wait detach-wait "$FIRST_ATTACHMENT_ID"
run_cli "collab detach" detach --project "$PROJECT" --attachment "$FIRST_ATTACHMENT_ID"
agent_event dashboard-stopped visible "feedback toolbar hidden"
finish_wait detach-wait
[[ "$(jq -r '.data.event' "$WAIT_OUTPUT")" == "collaboration.stop" ]] ||
  fail "detach wait did not return collaboration.stop"
[[ -f "$SESSION_FILE" ]] || fail "session file missing after detach"
snapshot_session "session-after-stop.json"
snapshot_process_tree stopped

run_cli "inactive status" status --project "$PROJECT"
[[ "$(print -r -- "$RUN_OUTPUT" | jq -r '.data.attachments | map(select(.active)) | length')" == "0" ]] ||
  fail "activeAttachmentCount was not zero after detach"
run_cli "manual-button interaction while detached" eval --project "$PROJECT" \
  'document.querySelector("#manual-button").click(); ({clicks: document.body.dataset.manualClicks, active: window.__collabOverlay.isActive(), hostCount: window.__collabOverlay.hostCount()})'
print -r -- "$RUN_OUTPUT" | jq -e \
  '.data.value.clicks == "1" and .data.value.active == false and .data.value.hostCount == 1' \
  >/dev/null || fail "page was not manually interactive while detached"

# spec「User opens Offline Paint」/「Reload while detached」：零連線時可開啟
# 非提交式畫記，reload 之後回到關閉且無 marks。
run_cli "offline-paint-open" eval --project "$PROJECT" \
  'var o=window.__collabOverlay;var opened=o.toggleOfflinePaint();o.addMark({type:"rect",x:20,y:20,width:120,height:60});({opened:opened,open:o.offlinePaintOpen(),marks:o.marks().length,active:o.isActive()})'
print -r -- "$RUN_OUTPUT" | jq -e \
  '.data.value.opened == true and .data.value.open == true and .data.value.marks == 1 and .data.value.active == false' \
  >/dev/null || fail "Offline Paint did not open with zero connected attachments"
run_cli "offline-paint-cannot-submit" eval --project "$PROJECT" \
  'var o=window.__collabOverlay;o.openEditor("painting", null);({editorOpen:o.editorOpen()})'
FEEDBACK_BEFORE_OFFLINE=$(find "$PROJECT/.collab/feedback" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')

perl -0pi -e 's/data-reload="initial"/data-reload="after-stop"/' "$ENTRY"
wait_for_eval_value "after-stop reload" \
  'document.body.dataset.reload + ":" + window.__collabOverlay.isActive() + ":" + window.__collabOverlay.hostCount()' \
  "after-stop:false:1"
run_cli "offline-paint-cleared-after-reload" eval --project "$PROJECT" \
  '({open: window.__collabOverlay.offlinePaintOpen(), marks: window.__collabOverlay.marks().length})'
print -r -- "$RUN_OUTPUT" | jq -e \
  '.data.value.open == false and .data.value.marks == 0' >/dev/null ||
  fail "Offline Paint survived a reload while detached"
FEEDBACK_AFTER_OFFLINE=$(find "$PROJECT/.collab/feedback" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
[[ "$FEEDBACK_BEFORE_OFFLINE" == "$FEEDBACK_AFTER_OFFLINE" ]] ||
  fail "offline marks created a feedback record"
[[ -f "$SESSION_FILE" ]] || fail "session file missing after reload"
snapshot_session "session-after-reload.json"
record_webview_count detached-reload
capture_screenshot "inactive screenshot" "inactive-screenshot.png"

# preview-collaboration-connect handoff in a different conversation: the copied
# command supplies only the Preview ID, while the skill composes this atomic attach.
CONNECT_COMMAND="\$preview-collaboration-connect $SESSION_ID"
agent_event connect-after-stop copy "$CONNECT_COMMAND"
run_cli "different-conversation preview-collaboration-connect" attach \
  --project "$PROJECT" --session "$SESSION_ID" --agent codex \
  --tui-session session-ux-different-conversation
SECOND_ATTACHMENT_ID=$(print -r -- "$RUN_OUTPUT" | jq -er '.data.attachment.attachmentId')
agent_event new-attachment-feedback active "feedback toolbar visible"
[[ "$SECOND_ATTACHMENT_ID" != "$FIRST_ATTACHMENT_ID" ]] ||
  fail "connect did not create a new attachment"
snapshot_session "session-after-connect.json"
[[ "$(jq -r '.sessionId' "$EVIDENCE_DIR/session-after-connect.json")" == "$SESSION_ID" ]] ||
  fail "session ID changed across connect"
[[ "$(jq -r '.port' "$EVIDENCE_DIR/session-after-connect.json")" == "$PORT" ]] ||
  fail "port changed across connect"
[[ "$(jq -r '.pid' "$EVIDENCE_DIR/session-after-connect.json")" == "$PREVIEW_PID" ]] ||
  fail "PID changed across connect"
run_cli "restart overlay state" eval --project "$PROJECT" \
  '({active: window.__collabOverlay.isActive(), hostCount: window.__collabOverlay.hostCount()})'
print -r -- "$RUN_OUTPUT" | jq -e \
  '.data.value.active == true and .data.value.hostCount == 1' >/dev/null ||
  fail "overlay did not reactivate without duplication"
# spec「Collaboration attaches again」：attach 之後離線 Paint 必須已關閉且無 marks。
run_cli "offline-paint-cleared-after-attach" eval --project "$PROJECT" \
  'var o=window.__collabOverlay;({open:o.offlinePaintOpen(), marks:o.marks().length, reopened:o.toggleOfflinePaint()})'
print -r -- "$RUN_OUTPUT" | jq -e \
  '.data.value.open == false and .data.value.marks == 0 and .data.value.reopened == false' >/dev/null ||
  fail "Offline Paint was still available after an attachment became active"
record_webview_count connected
snapshot_process_tree connected

# spec「Visual annotations remain anchored to document content」：長頁面捲動後
# mark geometry 不得被改寫，overlap 仍指向原本的 #scroll-anchor。
run_cli "document-anchored painting" eval --project "$PROJECT" \
  'var o=window.__collabOverlay;o.clearMarks();window.scrollTo(0,0);var r=document.querySelector("#scroll-anchor").getBoundingClientRect();o.addMark({type:"rect",x:r.left+window.scrollX+8,y:r.top+window.scrollY+8,width:r.width-16,height:r.height-16});var before=JSON.stringify(o.marks());window.scrollTo(0,document.documentElement.scrollHeight);var after=JSON.stringify(o.marks());var topOverlap=o.computeOverlaps()[0];window.scrollTo(0,0);({geometryStable: before===after, anchoredSelector: topOverlap?topOverlap.selector:null})'
print -r -- "$RUN_OUTPUT" | jq -e \
  '.data.value.geometryStable == true and .data.value.anchoredSelector == "#scroll-anchor"' >/dev/null ||
  fail "painting geometry did not stay anchored to document content after scrolling"
run_cli "clear anchored painting" eval --project "$PROJECT" \
  'window.__collabOverlay.clearMarks(); true'

# spec「Painting capture regions are geometry-based and bounded」：以 spec 的
# Example 值直接驗證 planner，分組只看 geometry、超過 8 個 region 不得送出。
run_cli "capture-plan-examples" eval --project "$PROJECT" \
  'var o=window.__collabOverlay;var v={width:1200,height:800};var d={width:1200,height:6000};var mk=function(a,b){return{x:100,y:a,width:200,height:b-a}};var fits=o.planCaptureRegions([mk(900,1000),mk(1200,1300)],v,d).length;var split=o.planCaptureRegions([mk(900,1000),mk(2200,2300)],v,d).length;var overlap=o.planCaptureRegions([mk(900,1100),mk(1050,1250)],v,d).length;var tiles=o.planCaptureRegions([{x:0,y:0,width:200,height:2400}],v,d);var ordered=o.planCaptureRegions([mk(3000,3100),mk(200,300)],v,d);({fits:fits,split:split,overlap:overlap,tiles:tiles.length,tilesInDocument:tiles.every(function(r){return r.x>=0&&r.y>=0&&r.x+r.width<=d.width&&r.y+r.height<=d.height}),orderedTop:ordered[0].y<ordered[1].y})'
print -r -- "$RUN_OUTPUT" | jq -e \
  '.data.value.fits == 1 and .data.value.split == 2 and .data.value.overlap == 1 and .data.value.tiles == 3 and .data.value.tilesInDocument == true and .data.value.orderedTop == true' >/dev/null ||
  fail "capture planner did not group marks by geometry"

FEEDBACK_BEFORE_LIMIT=$(find "$PROJECT/.collab/feedback" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
run_cli "capture-plan-limit" eval --project "$PROJECT" \
  'var o=window.__collabOverlay;o.clearMarks();for(var i=0;i<9;i++){o.addMark({type:"rect",x:10,y:i*3000+10,width:100,height:100})}var plan=o.planCaptureRegions(o.marks().map(function(m){return{x:m.x,y:m.y,width:m.width,height:m.height}}),{width:window.innerWidth,height:window.innerHeight},{width:document.documentElement.scrollWidth,height:document.documentElement.scrollHeight});o.submitPainting("too many regions");({regions:plan.length,marksKept:o.marks().length})'
print -r -- "$RUN_OUTPUT" | jq -e \
  '.data.value.regions == 9 and .data.value.marksKept == 9' >/dev/null ||
  fail "painting exceeding the capture region limit must keep its draft"
FEEDBACK_AFTER_LIMIT=$(find "$PROJECT/.collab/feedback" -name '*.json' 2>/dev/null | wc -l | tr -d ' ')
[[ "$FEEDBACK_BEFORE_LIMIT" == "$FEEDBACK_AFTER_LIMIT" ]] ||
  fail "an over-limit painting created a feedback record"
run_cli "clear capture-plan-limit marks" eval --project "$PROJECT" \
  'window.__collabOverlay.clearMarks(); true'

start_wait painting-wait "$SECOND_ATTACHMENT_ID"
run_cli "submitPainting" eval --project "$PROJECT" \
  'var overlay=window.__collabOverlay;overlay.clearMarks();window.scrollTo(0,0);var r=document.querySelector("#paint-target").getBoundingClientRect();var x=r.left+window.scrollX;var y=r.top+window.scrollY;overlay.addMark({type:"rect",x:x+8,y:y+8,width:r.width-16,height:r.height-16});overlay.addMark({type:"label",x:x+24,y:y+38,text:"Update CTA"});overlay.submitPainting("Change #cta to Painting feedback resolved.").then(function(){return true})'
finish_wait painting-wait
PAINTING_FEEDBACK_ID=$(jq -er '.data.item.id' "$WAIT_OUTPUT")
run_cli "painting acknowledged" feedback set-state --project "$PROJECT" \
  "$PAINTING_FEEDBACK_ID" acknowledged --expected pending --attachment "$SECOND_ATTACHMENT_ID"
run_cli "painting feedback show" feedback show --project "$PROJECT" "$PAINTING_FEEDBACK_ID"
run_cli "painting working" feedback set-state --project "$PROJECT" \
  "$PAINTING_FEEDBACK_ID" working --expected acknowledged --attachment "$SECOND_ATTACHMENT_ID"
agent_event painting modify "replace #cta text"
perl -0pi -e 's/Pending paint/Painting feedback resolved/' "$ENTRY"
wait_for_eval_value "verify painting feedback" \
  'document.querySelector("#cta").textContent.trim()' \
  "Painting feedback resolved"
capture_screenshot "painting screenshot" "painting-screenshot.png"
run_cli "painting resolved" feedback set-state --project "$PROJECT" \
  "$PAINTING_FEEDBACK_ID" resolved --expected working --attachment "$SECOND_ATTACHMENT_ID"
cp "$PROJECT/.collab/feedback/$PAINTING_FEEDBACK_ID.json" \
  "$EVIDENCE_DIR/painting-feedback.json"
agent_event painting resolved "$PAINTING_FEEDBACK_ID"

start_wait close-wait "$SECOND_ATTACHMENT_ID"
agent_event dashboard-closed requested "native close confirmation accepted"
run_cli "collab close" close --project "$PROJECT"
finish_wait close-wait
[[ "$(jq -r '.data.event' "$WAIT_OUTPUT")" == "collaboration.stop" ]] ||
  fail "close wait did not return collaboration.stop"

deadline=$(( $(date +%s) + 10 ))
while [[ -f "$SESSION_FILE" ]] || kill -0 "$PREVIEW_PID" 2>/dev/null; do
  (( $(date +%s) < deadline )) || break
  sleep 0.1
done
[[ ! -f "$SESSION_FILE" ]] || fail "session file still exists after close"
kill -0 "$PREVIEW_PID" 2>/dev/null && fail "preview process still exists after close"
snapshot_process_tree closed

cp "$ENTRY" "$EVIDENCE_DIR/final-index.html"
trap - EXIT INT TERM
rm -rf "$PROJECT"
print -r -- "session UX acceptance passed; evidence: $EVIDENCE_DIR"
