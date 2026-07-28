use collab::core::{Attachment, CollaborationState, DashboardAction, DashboardRuntimeState};
use collab::dashboard::{DashboardControl, DashboardReducer, build_snapshot};

fn attachment(id: &str, state: CollaborationState) -> Attachment {
    Attachment {
        attachment_id: id.into(),
        agent_kind: "codex".into(),
        tui_session_id: None,
        pid: 1,
        attached_at_epoch_secs: 1,
        last_heartbeat_epoch_secs: 1,
        collaboration_state: state,
        active: state.is_connected(),
    }
}

fn snapshot(
    revision: u64,
    attachments: &[Attachment],
    selected: Option<&str>,
) -> collab::core::DashboardSnapshot {
    build_snapshot(
        revision,
        DashboardRuntimeState::Running,
        "test-preview-session",
        attachments,
        &[],
        selected,
        None,
    )
}

#[test]
fn multiple_attachments_require_explicit_selection_and_stale_selection_is_cleared() {
    let attachments = [
        attachment("agent-a", CollaborationState::Active),
        attachment("agent-b", CollaborationState::Paused),
    ];
    let mut reducer = DashboardReducer::new(snapshot(1, &attachments, None));

    assert_eq!(
        reducer
            .action_for(DashboardControl::Pause)
            .unwrap_err()
            .code(),
        "selection-required"
    );
    reducer.select(Some("agent-a"));
    assert_eq!(
        reducer.action_for(DashboardControl::Pause).unwrap(),
        DashboardAction::Pause {
            attachment_id: "agent-a".into()
        }
    );

    reducer.apply_snapshot(snapshot(
        2,
        &[attachment("agent-b", CollaborationState::Paused)],
        Some("agent-a"),
    ));
    assert_eq!(reducer.snapshot().selected_attachment_id, None);
    assert_eq!(
        reducer
            .action_for(DashboardControl::Resume)
            .unwrap_err()
            .code(),
        "selection-required"
    );
}

#[test]
fn valid_local_selection_survives_server_snapshots_that_only_supply_a_hint() {
    let attachments = [
        attachment("agent-a", CollaborationState::Active),
        attachment("agent-b", CollaborationState::Paused),
    ];
    let mut reducer = DashboardReducer::new(snapshot(1, &attachments, None));
    reducer.select(Some("agent-b"));

    reducer.apply_snapshot(snapshot(2, &attachments, None));

    assert_eq!(
        reducer.snapshot().selected_attachment_id.as_deref(),
        Some("agent-b")
    );
}

#[test]
fn pending_action_commits_only_after_success_and_a_newer_matching_snapshot() {
    let mut reducer = DashboardReducer::new(snapshot(
        4,
        &[attachment("agent-a", CollaborationState::Active)],
        Some("agent-a"),
    ));
    let action = reducer.action_for(DashboardControl::Pause).unwrap();

    reducer.begin_action(action.clone()).unwrap();
    assert_eq!(reducer.pending_action(), Some(&action));
    reducer.complete_action(Ok(5));
    assert_eq!(reducer.pending_action(), Some(&action));

    reducer.apply_snapshot(snapshot(
        5,
        &[attachment("agent-a", CollaborationState::PauseRequested)],
        Some("agent-a"),
    ));
    assert_eq!(reducer.pending_action(), None);
}

#[test]
fn failed_action_restores_controls_and_preserves_last_known_good_state() {
    let initial = snapshot(
        9,
        &[attachment("agent-a", CollaborationState::Active)],
        Some("agent-a"),
    );
    let mut reducer = DashboardReducer::new(initial.clone());
    let action = reducer.action_for(DashboardControl::Pause).unwrap();

    reducer.begin_action(action).unwrap();
    reducer.complete_action(Err(collab::core::DashboardActionError::Busy));

    assert_eq!(reducer.pending_action(), None);
    assert_eq!(reducer.snapshot(), &initial);
    assert_eq!(reducer.banner(), Some("Dashboard action queue is busy"));
}

#[test]
fn close_is_only_submitted_after_native_confirmation() {
    let dashboard = std::fs::read_to_string("src/dashboard.rs").unwrap();

    assert!(dashboard.contains("NSAlert"));
    assert!(dashboard.contains("beginSheetModalForWindow"));
    assert!(dashboard.contains("Close preview"));
    assert!(dashboard.contains("confirmed"));
    assert!(!dashboard.contains("window.confirm("));
}

#[test]
fn preview_page_has_no_privileged_dashboard_action_bridge() {
    let overlay = std::fs::read_to_string("web/overlay.js").unwrap();

    for forbidden in [
        "DashboardAction",
        "controlToken",
        "/control/pause",
        "/control/resume",
        "/control/detach",
        "/control/close",
    ] {
        assert!(
            !overlay.contains(forbidden),
            "page exposes privileged action: {forbidden}"
        );
    }
}
