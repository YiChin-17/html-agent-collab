//! Task 6.1：bounded collections、resource metrics 與四小時 soak harness contract。

use collab::{feedback, server, watcher, webview};

#[test]
fn resource_capacities_are_explicit_and_bounded() {
    const { assert!(server::ATTACHMENT_CAPACITY > 0) };
    const { assert!(server::ATTACHMENT_CAPACITY <= 64) };
    assert_eq!(server::CONSOLE_BUFFER_CAPACITY, 0);
    assert_eq!(server::RESPONSE_BUFFER_CAPACITY, 1);
    const { assert!(server::CONTROL_BODY_LIMIT_BYTES <= 2 * 1024 * 1024) };
    const { assert!(watcher::EVENT_QUEUE_CAPACITY <= 1024) };
    const { assert!(feedback::FEEDBACK_VIEW_LIMIT <= 512) };
    const { assert!(webview::COMMAND_QUEUE_CAPACITY <= 64) };
}

#[test]
fn preview_draft_feedback_limits_are_explicit_and_bounded() {
    let source = std::fs::read_to_string("src/feedback.rs").unwrap();

    assert!(source.contains("PREVIEW_DRAFT_DOCUMENT_LIMIT_BYTES"));
    assert!(source.contains("262_144"));
    assert!(source.contains("PreviewDraft"));
}

#[test]
fn default_feedback_lease_allows_a_real_agent_edit_cycle() {
    assert!(
        feedback::DEFAULT_LEASE_DURATION >= std::time::Duration::from_secs(300),
        "the default lease must cover inspect, edit, reload, eval, and native screenshot"
    );
}

#[test]
fn feedback_view_never_retains_more_than_its_limit() {
    let root =
        std::env::temp_dir().join(format!("collab-resource-feedback-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for index in 0..(feedback::FEEDBACK_VIEW_LIMIT + 5) {
        let incoming = feedback::validate(serde_json::json!({
            "kind": "textbox",
            "text": format!("item {index}"),
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        }))
        .unwrap();
        let mut record = feedback::prepare(incoming);
        record.id = format!("fb-{index:04}-00000001");
        record.created_at = index as u64;
        feedback::write_record(&root, &record).unwrap();
    }

    let records = feedback::list_records(&root).unwrap();
    assert_eq!(records.len(), feedback::FEEDBACK_VIEW_LIMIT);
    assert_eq!(records.first().unwrap().id, "fb-0000-00000001");
}

#[test]
fn active_feedback_view_is_not_hidden_by_older_terminal_records() {
    let root = std::env::temp_dir().join(format!(
        "collab-resource-active-feedback-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    for index in 0..feedback::FEEDBACK_VIEW_LIMIT {
        let incoming = feedback::validate(serde_json::json!({
            "kind": "textbox",
            "text": format!("resolved {index}"),
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        }))
        .unwrap();
        let mut record = feedback::prepare(incoming);
        record.id = format!("fb-{index:04}-00000002");
        record.created_at = index as u64;
        record.state = feedback::FeedbackState::Resolved;
        feedback::write_record(&root, &record).unwrap();
    }
    let incoming = feedback::validate(serde_json::json!({
        "kind": "textbox",
        "text": "still pending",
        "pageUrl": "http://127.0.0.1/",
        "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
    }))
    .unwrap();
    let mut pending = feedback::prepare(incoming);
    pending.id = "fb-9999-00000003".into();
    pending.created_at = feedback::FEEDBACK_VIEW_LIMIT as u64 + 1;
    feedback::write_record(&root, &pending).unwrap();

    let active = feedback::list_active_records(&root).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, "fb-9999-00000003");
}

#[test]
fn app_uses_the_bounded_watcher_event_capacity() {
    let app = std::fs::read_to_string("src/app.rs").unwrap();
    assert!(app.contains("watcher::EVENT_QUEUE_CAPACITY"));
    assert!(!app.contains("mpsc::channel(1024)"));
}

#[test]
fn soak_harness_defaults_to_four_hours_and_checks_required_operations() {
    let harness = std::fs::read_to_string("scripts/reference-soak.sh").unwrap();
    for contract in [
        "CYCLES=${CYCLES:-240}",
        "INTERVAL_SECONDS=${INTERVAL_SECONDS:-60}",
        "WARMUP_CYCLES=${WARMUP_CYCLES:-10}",
        "MAX_RSS_GROWTH_KIB=${MAX_RSS_GROWTH_KIB:-102400}",
        "collab eval",
        "collab screenshot",
        "collab wait",
        "collab feedback set-state",
        "/__collab__/control/heartbeat",
        "/__collab__/overlay/feedback",
        "/__collab__/metrics",
        "loadPreviewDraftSource",
        "openPreviewDraftFor",
        "applyPreviewDraft",
        "undoPreviewDraft",
        "redoPreviewDraft",
        "resetPreviewDraft",
        "previewDraftState",
        "undoDepth <= 50",
        "redoDepth <= 50",
        "utf8bytelength) <= 262144",
        ".reset.status == \"editing\"",
        ".hostCount == 1",
    ] {
        assert!(
            harness.contains(contract),
            "missing soak contract: {contract}"
        );
    }
    assert!(harness.contains("\"${cycle}\"$'\\t'"));
    assert!(!harness.contains("\"${cycle}\\t"));
}
