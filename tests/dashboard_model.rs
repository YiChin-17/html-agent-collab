use collab::core::{
    Attachment, CollaborationState, DashboardAction, DashboardRuntimeState, DashboardSnapshot,
};
use collab::dashboard::{
    DASHBOARD_ACTION_CAPACITY, DASHBOARD_FEEDBACK_LIMIT, DashboardControl, DashboardReducer,
    FEEDBACK_MARKER_LIMIT, build_marker_snapshot, build_snapshot, channel,
};
use collab::feedback::{FeedbackKind, FeedbackRecord, FeedbackState};

fn attachment(id: &str, state: CollaborationState) -> Attachment {
    Attachment {
        attachment_id: id.into(),
        agent_kind: "codex".into(),
        tui_session_id: Some("must-not-leak".into()),
        pid: 42,
        attached_at_epoch_secs: 1,
        last_heartbeat_epoch_secs: 2,
        collaboration_state: state,
        active: state.is_connected(),
    }
}

fn feedback(index: u64) -> FeedbackRecord {
    FeedbackRecord {
        id: format!("fb-{index:04}-00000001"),
        kind: FeedbackKind::Textbox,
        text: format!("feedback {index}"),
        state: FeedbackState::Pending,
        orphaned: false,
        created_at: index,
        page_url: "http://127.0.0.1/private".into(),
        viewport: serde_json::json!({"width": 800}),
        elements: serde_json::json!([{"selector": "#private", "attributes": {"id": "private"}}]),
        attachments: vec!["/private/absolute/path.png".into()],
        lease: None,
        marks: serde_json::json!({"svg": "private"}),
        preview_draft: None,
        failure_reason: None,
        recovery: None,
    }
}

fn empty_snapshot(revision: u64) -> DashboardSnapshot {
    build_snapshot(
        revision,
        DashboardRuntimeState::Running,
        "test-preview-session",
        &[],
        &[],
        None,
        None,
    )
}

#[test]
fn marker_snapshot_keeps_only_the_fifty_newest_unresolved_element_comments() {
    let mut records = (1..=53)
        .map(|index| {
            let mut record = feedback(index);
            record.kind = FeedbackKind::ElementComment;
            record.elements = serde_json::json!([{"selector": format!("#item-{index}")}]);
            record
        })
        .collect::<Vec<_>>();
    let mut resolved = feedback(100);
    resolved.kind = FeedbackKind::ElementComment;
    resolved.state = FeedbackState::Resolved;
    records.push(resolved);
    records.push(feedback(101));
    records[52].state = FeedbackState::Failed;
    records[52].failure_reason = Some("verification failed".into());

    let snapshot = build_marker_snapshot(7, &records);

    assert_eq!(snapshot.revision, 7);
    assert_eq!(snapshot.items.len(), FEEDBACK_MARKER_LIMIT);
    assert_eq!(snapshot.items.first().unwrap().id, "fb-0053-00000001");
    assert_eq!(snapshot.items.last().unwrap().id, "fb-0004-00000001");
    assert_eq!(
        snapshot.items.first().unwrap().failure_reason.as_deref(),
        Some("verification failed")
    );
    assert!(
        snapshot
            .items
            .iter()
            .all(|item| item.id != "fb-0100-00000001" && item.id != "fb-0101-00000001")
    );
}

#[test]
fn bounded_feedback_view_uses_the_fifty_newest_records_in_descending_order() {
    let records = (1..=53).map(feedback).collect::<Vec<_>>();

    let snapshot = build_snapshot(
        1,
        DashboardRuntimeState::Running,
        "test-preview-session",
        &[attachment("agent-a", CollaborationState::Active)],
        &records,
        Some("agent-a"),
        None,
    );

    assert_eq!(DASHBOARD_FEEDBACK_LIMIT, 50);
    assert_eq!(snapshot.feedback_items.len(), 50);
    assert_eq!(
        snapshot.feedback_items.first().unwrap().id,
        "fb-0053-00000001"
    );
    assert_eq!(
        snapshot.feedback_items.last().unwrap().id,
        "fb-0004-00000001"
    );
    assert_eq!(snapshot.feedback_counts.pending, 53);
}

#[test]
fn persisted_dashboard_view_counts_all_records_without_retaining_more_than_fifty() {
    let root = std::env::temp_dir().join(format!("collab-dashboard-view-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for index in 1..=53 {
        collab::feedback::write_record(&root, &feedback(index)).unwrap();
    }

    let view = collab::feedback::dashboard_view(&root, DASHBOARD_FEEDBACK_LIMIT).unwrap();

    assert_eq!(view.records.len(), 50);
    assert_eq!(view.records.first().unwrap().id, "fb-0053-00000001");
    assert_eq!(view.records.last().unwrap().id, "fb-0004-00000001");
    assert_eq!(view.counts.pending, 53);
}

#[test]
fn dashboard_snapshot_serialization_is_an_allowlist() {
    let snapshot = build_snapshot(
        1,
        DashboardRuntimeState::Running,
        "test-preview-session",
        &[attachment("agent-a", CollaborationState::Active)],
        &[feedback(1)],
        Some("agent-a"),
        None,
    );
    let value = serde_json::to_value(snapshot).unwrap();
    let serialized = serde_json::to_string(&value).unwrap();

    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        [
            "attachments",
            "error",
            "feedbackCounts",
            "feedbackItems",
            "previewSessionId",
            "revision",
            "runtimeState",
            "selectedAttachmentId",
        ]
    );
    let feedback_item = value["feedbackItems"][0].as_object().unwrap();
    assert_eq!(
        feedback_item.keys().cloned().collect::<Vec<_>>(),
        [
            "attachmentId",
            "failureReason",
            "id",
            "kind",
            "orphaned",
            "state",
            "text",
        ]
    );
    for forbidden in [
        "controlToken",
        "tuiSessionId",
        "pageUrl",
        "viewport",
        "elements",
        "/private/absolute",
        "marks",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "snapshot leaked {forbidden}"
        );
    }
}

#[tokio::test]
async fn state_channel_collapses_to_the_latest_revision() {
    let (publisher, mut handle, _receiver) = channel(empty_snapshot(10));
    for revision in 11..=13 {
        publisher.publish(empty_snapshot(revision));
    }

    handle.snapshots.changed().await.unwrap();
    assert_eq!(handle.snapshots.borrow_and_update().revision, 13);
}

#[tokio::test]
async fn action_channel_has_fixed_capacity_and_per_action_completion() {
    let (_publisher, handle, mut receiver) = channel(empty_snapshot(1));
    let mut completions = Vec::new();
    for _ in 0..DASHBOARD_ACTION_CAPACITY {
        completions.push(handle.try_submit(DashboardAction::Close).unwrap());
    }
    assert_eq!(
        handle
            .try_submit(DashboardAction::Close)
            .unwrap_err()
            .code(),
        "busy"
    );

    let request = receiver.recv().await.unwrap();
    request.respond(Ok(2));
    assert_eq!(completions.remove(0).await.unwrap().unwrap(), 2);
}

/// design「Observable state matrix」的一列：情境、attachments、runtime，
/// 以及 Native Paint、Connect agent、頁面 collaboration toolbar 的預期可見性。
struct StateMatrixRow {
    label: &'static str,
    attachments: Vec<Attachment>,
    runtime: DashboardRuntimeState,
    paint: bool,
    connect: bool,
    toolbar: bool,
}

// design「Observable state matrix」：Native Paint、Connect agent 與頁面
// collaboration toolbar 的可見性逐列固定，避免離線標記與待處理 feedback 混淆。
#[test]
fn observable_state_matrix_gates_offline_paint_and_collaboration_toolbar() {
    let cases = [
        StateMatrixRow {
            label: "running with zero connected",
            attachments: vec![],
            runtime: DashboardRuntimeState::Running,
            paint: true,
            connect: true,
            toolbar: false,
        },
        StateMatrixRow {
            label: "active attachment",
            attachments: vec![attachment("agent-a", CollaborationState::Active)],
            runtime: DashboardRuntimeState::Running,
            paint: false,
            connect: false,
            toolbar: true,
        },
        StateMatrixRow {
            label: "pause-requested attachment",
            attachments: vec![attachment("agent-a", CollaborationState::PauseRequested)],
            runtime: DashboardRuntimeState::Running,
            paint: false,
            connect: false,
            toolbar: false,
        },
        StateMatrixRow {
            label: "paused attachment",
            attachments: vec![attachment("agent-a", CollaborationState::Paused)],
            runtime: DashboardRuntimeState::Running,
            paint: false,
            connect: false,
            toolbar: false,
        },
        StateMatrixRow {
            label: "one paused plus another active",
            attachments: vec![
                attachment("agent-a", CollaborationState::Paused),
                attachment("agent-b", CollaborationState::Active),
            ],
            runtime: DashboardRuntimeState::Running,
            paint: false,
            connect: false,
            toolbar: true,
        },
        StateMatrixRow {
            label: "only inactive attachments",
            attachments: vec![attachment("agent-a", CollaborationState::Inactive)],
            runtime: DashboardRuntimeState::Running,
            paint: true,
            connect: true,
            toolbar: false,
        },
        StateMatrixRow {
            label: "preview closed",
            attachments: vec![],
            runtime: DashboardRuntimeState::Closed,
            paint: false,
            connect: false,
            toolbar: false,
        },
    ];

    for case in cases {
        let reducer = DashboardReducer::new(build_snapshot(
            1,
            case.runtime,
            "test-preview-session",
            &case.attachments,
            &[],
            None,
            None,
        ));
        let label = case.label;

        assert_eq!(
            reducer.offline_paint_available(),
            case.paint,
            "{label}: native Paint visibility"
        );
        assert_eq!(
            reducer.connect_agent_available(),
            case.connect,
            "{label}: Connect agent visibility"
        );
        assert_eq!(
            reducer.feedback_tools_visible(),
            case.toolbar,
            "{label}: page collaboration toolbar visibility"
        );
    }
}

// spec「Selected attachment is paused」：Paused 只留 Resume，不得出現 Offline Paint。
#[test]
fn paused_attachment_keeps_resume_without_offering_offline_paint() {
    let reducer = DashboardReducer::new(build_snapshot(
        1,
        DashboardRuntimeState::Running,
        "test-preview-session",
        &[attachment("agent-a", CollaborationState::Paused)],
        &[],
        Some("agent-a"),
        None,
    ));

    assert_eq!(reducer.selected_state_text(), "Paused");
    assert!(reducer.action_for(DashboardControl::Resume).is_ok());
    assert!(!reducer.offline_paint_available());
    assert!(reducer.action_for(DashboardControl::OfflinePaint).is_err());
}

// design「原生 Offline Paint 經既有 dashboard action 與 WebviewCommand 邊界」：
// Paint 只在 zero connected 時映射到不帶 attachment ID 的 action。
#[test]
fn offline_paint_control_maps_to_an_attachment_free_action() {
    let reducer = DashboardReducer::new(build_snapshot(
        1,
        DashboardRuntimeState::Running,
        "test-preview-session",
        &[],
        &[],
        None,
        None,
    ));

    assert_eq!(
        reducer.action_for(DashboardControl::OfflinePaint).unwrap(),
        DashboardAction::ToggleOfflinePaint
    );
}

#[test]
fn reducer_preserves_last_known_good_feedback_on_storage_error() {
    let good = build_snapshot(
        7,
        DashboardRuntimeState::Running,
        "test-preview-session",
        &[],
        &[feedback(1)],
        None,
        None,
    );
    let mut reducer = DashboardReducer::new(good);

    reducer.apply_snapshot(build_snapshot(
        8,
        DashboardRuntimeState::Running,
        "test-preview-session",
        &[],
        &[],
        None,
        Some("feedback storage unavailable".into()),
    ));

    assert_eq!(reducer.snapshot().revision, 8);
    assert_eq!(reducer.snapshot().feedback_items.len(), 1);
    assert_eq!(
        reducer.snapshot().error.as_deref(),
        Some("feedback storage unavailable")
    );
}
