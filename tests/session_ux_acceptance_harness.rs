//! Task 6.1：single-entry session UX macOS acceptance harness contract。

#[test]
fn session_ux_harness_covers_the_complete_connect_handoff_lifecycle() {
    let harness = std::fs::read_to_string("scripts/session-ux-acceptance.sh")
        .expect("session UX acceptance harness should exist");

    for contract in [
        "collab open \"$ENTRY\" --background",
        "submitElementComment",
        "collab feedback set-state",
        "collab pause",
        "pause-requested",
        "collaboration-paused",
        "collab resume",
        "same attachment",
        "collab detach",
        "manual-button",
        "after-stop",
        "session-before-stop.json",
        "preview-collaboration-connect",
        "different-conversation",
        "session-after-connect.json",
        "activeAttachmentCount",
        "hostCount",
        "submitPainting",
        "collab close",
        "session ID changed across connect",
        "port changed across connect",
        "PID changed across connect",
        "session file missing after detach",
        "session file missing after reload",
        "session file still exists after close",
    ] {
        assert!(
            harness.contains(contract),
            "missing session UX acceptance contract: {contract}"
        );
    }
}

#[test]
fn session_ux_harness_preserves_required_evidence() {
    let harness = std::fs::read_to_string("scripts/session-ux-acceptance.sh")
        .expect("session UX acceptance harness should exist");

    for evidence in [
        "cli-transcript.jsonl",
        "agent-transcript.jsonl",
        "process-tree.tsv",
        "webview-counts.tsv",
        "session-before-stop.json",
        "session-after-stop.json",
        "session-after-reload.json",
        "session-after-connect.json",
        "element-feedback.json",
        "painting-feedback.json",
        "element-screenshot.png",
        "inactive-screenshot.png",
        "painting-screenshot.png",
        "CGWindowListCopyWindowInfo",
    ] {
        assert!(
            harness.contains(evidence),
            "missing acceptance evidence: {evidence}"
        );
    }
}

#[test]
fn session_ux_harness_records_dashboard_and_toolbar_state_transitions() {
    let harness = std::fs::read_to_string("scripts/session-ux-acceptance.sh")
        .expect("session UX acceptance harness should exist");

    for state in [
        "dashboard-active",
        "dashboard-pause-requested",
        "dashboard-paused",
        "dashboard-stopped",
        "dashboard-closed",
        "connect-after-stop",
        "new-attachment-feedback",
    ] {
        assert!(
            harness.contains(state),
            "missing dashboard transcript state: {state}"
        );
    }
}

#[test]
fn session_ux_harness_covers_preview_draft_reload_discard() {
    let harness = std::fs::read_to_string("scripts/session-ux-acceptance.sh")
        .expect("session UX acceptance harness should exist");

    for contract in [
        "preview-draft-memory-edit",
        "preview-draft-submitted",
        "preview-draft-after-reload",
        "loadPreviewDraftSource",
        "previewDraftState",
        "<!doctype html>",
        "\"status\":\"idle\"",
        "source-backed Hello",
        "pending preview-draft feedback",
    ] {
        assert!(
            harness.contains(contract),
            "missing Preview Draft reload contract: {contract}"
        );
    }
}
