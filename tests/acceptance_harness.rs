//! Task 4.1：跨 agent scripted transcript 的 start、stop、restart、close contract。

#[test]
fn cross_agent_harness_preserves_required_acceptance_evidence() {
    let harness = std::fs::read_to_string("scripts/cross-agent-acceptance.sh")
        .expect("cross-agent acceptance harness should exist");

    for contract in [
        "claude-code",
        "codex",
        "preview-collaboration-start",
        "preview-collaboration-stop",
        "preview-collaboration-close",
        "submitElementComment",
        "submitPainting",
        "collab screenshot",
        "kill -INT \"$AGENT_PID\"",
        "\"$COLLAB\" detach",
        "\"$COLLAB\" close",
        "first-attachment.json",
        "second-attachment.json",
        "session-before-stop.json",
        "session-after-restart.json",
        "transcript.jsonl",
        "timing.json",
        "process-tree.tsv",
        "feedback-artifacts",
        "reloadElapsedMs",
    ] {
        assert!(
            harness.contains(contract),
            "missing acceptance contract: {contract}"
        );
    }
    assert!(harness.contains("feedback_files=("));
    assert!(harness.contains("${#feedback_files[@]}"));
    assert!(!harness.contains("${#${(N)PROJECT"));
    assert!(harness.contains("wait_until_agent_waits_again"));
    assert!(harness.contains("wait_line > resolved_line"));
    assert!(!harness.contains("\"$COLLAB\" stop"));
}

#[test]
fn acceptance_fixture_exposes_stable_element_targets() {
    let fixture = std::fs::read_to_string("tests/fixtures/acceptance/index.base.html")
        .expect("acceptance fixture should exist");

    for target in ["id=\"hero-title\"", "id=\"cta\"", "id=\"paint-target\""] {
        assert!(fixture.contains(target), "missing fixture target: {target}");
    }
}

#[test]
fn cross_agent_harness_proves_preview_draft_never_writes_before_agent_handoff() {
    let harness = std::fs::read_to_string("scripts/cross-agent-acceptance.sh")
        .expect("cross-agent acceptance harness should exist");

    for contract in [
        "preview-draft-before.sha256",
        "preview-draft-after-memory-edit.sha256",
        "preview-draft-after-submit.sha256",
        "loadPreviewDraftSource",
        "applyPreviewDraft",
        "submitPreviewDraft",
        "previewDraftState",
        "<!doctype html>",
        "source-backed Hello",
        "preview-draft-feedback.json",
    ] {
        assert!(
            harness.contains(contract),
            "missing Preview Draft acceptance contract: {contract}"
        );
    }
}
