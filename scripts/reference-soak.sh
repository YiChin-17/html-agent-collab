#!/bin/zsh
set -euo pipefail

PROJECT_INPUT=${1:?"usage: scripts/reference-soak.sh <preview-project>"}
CYCLES=${CYCLES:-240}
INTERVAL_SECONDS=${INTERVAL_SECONDS:-60}
WARMUP_CYCLES=${WARMUP_CYCLES:-10}
MAX_RSS_GROWTH_KIB=${MAX_RSS_GROWTH_KIB:-102400}

for command_name in collab jq curl pgrep ps awk; do
  command -v "$command_name" >/dev/null || {
    print -u2 "missing required command: $command_name"
    exit 2
  }
done

PROJECT=$(cd "$PROJECT_INPUT" && pwd)
SESSION_FILE="$PROJECT/.collab/session.json"
[[ -f "$SESSION_FILE" ]] || {
  print -u2 "preview is not running for $PROJECT"
  exit 2
}

ATTACH_JSON=$(collab attach --project "$PROJECT" --agent soak-harness)
ATTACHMENT_ID=$(print -r -- "$ATTACH_JSON" | jq -er '.data.attachment.attachmentId')
ROOT_PID=$(jq -er '.pid' "$SESSION_FILE")
PORT=$(jq -er '.port' "$SESSION_FILE")
TOKEN=$(jq -er '.token' "$SESSION_FILE")
HEARTBEAT_BODY=$(jq -nc --arg attachment_id "$ATTACHMENT_ID" '{attachmentId: $attachment_id}')
EVIDENCE_DIR="$PROJECT/.collab/soak/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$EVIDENCE_DIR"
METRICS_TSV="$EVIDENCE_DIR/cycles.tsv"
print -r -- $'cycle\tepoch\trss_kib\tmetrics' >"$METRICS_TSV"

heartbeat() {
  print -r -- "header = \"Authorization: Bearer ${TOKEN}\"" |
    curl --fail --silent --config - \
      --header "Content-Type: application/json" \
      --data "$HEARTBEAT_BODY" \
      "http://127.0.0.1:${PORT}/__collab__/control/heartbeat" >/dev/null
}

lease_feedback() {
  local feedback_id=$1
  local attempt wait_json shown_json leased_id lease_owner

  for attempt in 1 2; do
    wait_json=
    if wait_json=$(collab wait --project "$PROJECT" --attachment "$ATTACHMENT_ID" --json); then
      leased_id=$(print -r -- "$wait_json" | jq -r '.data.item.id // empty')
      if [[ "$leased_id" == "$feedback_id" ]]; then
        return 0
      fi
    fi

    shown_json=$(collab feedback show --project "$PROJECT" "$feedback_id")
    lease_owner=$(print -r -- "$shown_json" | jq -r '.data.item.lease.owner // empty')
    if [[ "$lease_owner" == "$ATTACHMENT_ID" ]]; then
      return 0
    fi
    if [[ -z "$lease_owner" && $attempt -lt 2 ]]; then
      continue
    fi

    print -u2 "failed to lease feedback $feedback_id on cycle $cycle: ${wait_json:-no wait response}"
    return 1
  done
}

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
  for pid in $(descendants "$ROOT_PID"); do
    ps -o rss= -p "$pid" 2>/dev/null || true
  done | awk '{ total += $1 } END { print total + 0 }'
}

BASELINE_RSS_KIB=
BASELINE_METRICS=
FINAL_RSS_KIB=
FINAL_METRICS=

for (( cycle = 1; cycle <= CYCLES; cycle++ )); do
  cycle_started=$(date +%s)

  heartbeat
  touch "$PROJECT/index.html"
  sleep 1
  collab eval --project "$PROJECT" "document.documentElement.dataset.soakCycle='${cycle}'; true" >/dev/null
  collab screenshot --project "$PROJECT" >/dev/null
  DRAFT_SOURCE_JSON=$(jq -Rs . "$PROJECT/index.html")
  DRAFT_JSON=$(collab eval --project "$PROJECT" "(()=>{const o=window.__collabOverlay;const source=$DRAFT_SOURCE_JSON;const target=document.querySelector('#counter');o.loadPreviewDraftSource({pageUrl:location.href,html:source});o.setMode('draft');o.openPreviewDraftFor(target);for(let edit=1;edit<=51;edit++){o.applyPreviewDraft({html:source.replace('<main id=\"counter\">','<main id=\"counter\" data-draft=\"'+edit+'\">').replace('Reference soak fixture','Preview Draft cycle '+edit)});}const bounded=o.previewDraftState();o.undoPreviewDraft();const undone=o.previewDraftState();o.redoPreviewDraft();const redone=o.previewDraftState();o.resetPreviewDraft();o.setMode(null);return {bounded:bounded,undone:undone,redone:redone,reset:o.previewDraftState(),hostCount:o.hostCount()};})()")
  print -r -- "$DRAFT_JSON" | jq -e '
    .data.value.bounded.undoDepth <= 50 and
    .data.value.bounded.redoDepth <= 50 and
    .data.value.undone.redoDepth == 1 and
    .data.value.redone.redoDepth == 0 and
    .data.value.reset.status == "editing" and
    (.data.value.bounded.currentHtml | startswith("<!doctype html>")) and
    (.data.value.bounded.currentHtml | utf8bytelength) <= 262144 and
    .data.value.hostCount == 1
  ' >/dev/null
  FEEDBACK_BODY=$(jq -nc \
    --arg text "soak cycle ${cycle}" \
    --arg page_url "http://127.0.0.1:${PORT}/index.html" \
    '{
      kind: "textbox",
      text: $text,
      pageUrl: $page_url,
      viewport: {width: 1280, height: 800, scrollX: 0, scrollY: 0}
    }')
  FEEDBACK_RESPONSE=$(curl --fail --silent \
    --header "Content-Type: application/json" \
    --data "$FEEDBACK_BODY" \
    "http://127.0.0.1:${PORT}/__collab__/overlay/feedback")
  FEEDBACK_ID=$(print -r -- "$FEEDBACK_RESPONSE" | jq -er 'select(.state == "pending") | .id')

  lease_feedback "$FEEDBACK_ID"
  collab feedback set-state --project "$PROJECT" "$FEEDBACK_ID" acknowledged --expected pending --attachment "$ATTACHMENT_ID" >/dev/null
  collab feedback set-state --project "$PROJECT" "$FEEDBACK_ID" working --expected acknowledged --attachment "$ATTACHMENT_ID" >/dev/null
  collab feedback set-state --project "$PROJECT" "$FEEDBACK_ID" resolved --expected working --attachment "$ATTACHMENT_ID" >/dev/null

  METRICS=$(curl --fail --silent "http://127.0.0.1:${PORT}/__collab__/metrics")
  print -r -- "$METRICS" | jq -e '
    .attachmentCount <= .attachmentCapacity and
    .consoleItems <= .consoleCapacity and
    .webviewCommandQueued <= .webviewCommandCapacity and
    .feedbackMemoryItems <= .feedbackViewLimit
  ' >/dev/null
  RSS_KIB=$(process_tree_rss_kib)
  print -r -- "${cycle}"$'\t'"$(date +%s)"$'\t'"${RSS_KIB}"$'\t'"$(print -r -- "$METRICS" | jq -c .)" >>"$METRICS_TSV"

  if (( cycle == WARMUP_CYCLES )); then
    BASELINE_RSS_KIB=$RSS_KIB
    BASELINE_METRICS=$METRICS
    print -r -- "$METRICS" >"$EVIDENCE_DIR/baseline-metrics.json"
  fi
  FINAL_RSS_KIB=$RSS_KIB
  FINAL_METRICS=$METRICS

  cycle_elapsed=$(( $(date +%s) - cycle_started ))
  remaining=$(( INTERVAL_SECONDS - cycle_elapsed ))
  if (( cycle < CYCLES && remaining > 0 )); then
    sleep "$remaining"
  fi
done

[[ -n "$BASELINE_RSS_KIB" ]] || {
  print -u2 "WARMUP_CYCLES must be less than or equal to CYCLES"
  exit 2
}

RSS_GROWTH_KIB=$(( FINAL_RSS_KIB - BASELINE_RSS_KIB ))
print -r -- "$FINAL_METRICS" >"$EVIDENCE_DIR/final-metrics.json"
jq -n \
  --argjson cycles "$CYCLES" \
  --argjson intervalSeconds "$INTERVAL_SECONDS" \
  --argjson baselineRssKiB "$BASELINE_RSS_KIB" \
  --argjson finalRssKiB "$FINAL_RSS_KIB" \
  --argjson rssGrowthKiB "$RSS_GROWTH_KIB" \
  --argjson maxRssGrowthKiB "$MAX_RSS_GROWTH_KIB" \
  '{
    cycles: $cycles,
    intervalSeconds: $intervalSeconds,
    baselineRssKiB: $baselineRssKiB,
    finalRssKiB: $finalRssKiB,
    rssGrowthKiB: $rssGrowthKiB,
    maxRssGrowthKiB: $maxRssGrowthKiB
  }' >"$EVIDENCE_DIR/summary.json"

print -r -- "$FINAL_METRICS" | jq -e \
  --argjson baselineFeedback "$(print -r -- "$BASELINE_METRICS" | jq '.feedbackMemoryItems')" \
  --argjson baselineConsole "$(print -r -- "$BASELINE_METRICS" | jq '.consoleItems')" '
    .feedbackMemoryItems <= $baselineFeedback and
    .consoleItems <= $baselineConsole
  ' >/dev/null

if (( RSS_GROWTH_KIB > MAX_RSS_GROWTH_KIB )); then
  print -u2 "RSS growth ${RSS_GROWTH_KIB} KiB exceeds ${MAX_RSS_GROWTH_KIB} KiB"
  exit 1
fi

print -r -- "soak passed; evidence: $EVIDENCE_DIR"
