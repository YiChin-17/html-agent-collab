use std::fs;

use collab::core::{Attachment, CollaborationState, DashboardRuntimeState};
use collab::dashboard::{DashboardReducer, build_snapshot};

fn attachment(state: CollaborationState) -> Attachment {
    Attachment {
        attachment_id: "att-private".into(),
        agent_kind: "codex".into(),
        tui_session_id: Some("tui-private".into()),
        pid: 4242,
        attached_at_epoch_secs: 1,
        last_heartbeat_epoch_secs: 1,
        collaboration_state: state,
        active: state.is_connected(),
    }
}

#[test]
fn preview_installs_persistent_native_dashboard_without_a_second_webview() {
    let app = fs::read_to_string("src/app.rs").unwrap();
    let dashboard = fs::read_to_string("src/dashboard.rs").unwrap();

    assert_eq!(app.matches("WebviewWindowBuilder::new").count(), 1);
    assert!(app.contains("dashboard::install"));
    assert!(dashboard.contains("NSTitlebarAccessoryViewController"));
    assert!(dashboard.contains("No agent connected"));
    assert!(dashboard.contains("Close preview"));
    assert!(!app.contains("privileged_page_controls"));
}

#[test]
fn native_dashboard_dependencies_are_explicit_and_platform_scoped() {
    let manifest = fs::read_to_string("Cargo.toml").unwrap();

    for feature in [
        "NSTitlebarAccessoryViewController",
        "NSStackView",
        "NSButton",
        "NSTextField",
        "NSPopover",
    ] {
        assert!(
            manifest.contains(feature),
            "missing AppKit feature {feature}"
        );
    }
}

#[test]
fn dashboard_startup_failure_has_no_page_privileged_fallback() {
    let app = fs::read_to_string("src/app.rs").unwrap();
    let overlay = fs::read_to_string("web/overlay.js").unwrap();

    assert!(app.contains("dashboard::install("));
    assert!(!overlay.contains("Pause collaboration"));
    assert!(!overlay.contains("Resume collaboration"));
    assert!(!overlay.contains("Stop collaboration"));
    assert!(!overlay.contains("Close preview"));
}

#[test]
fn dashboard_lifecycle_state_matrix_is_explicit() {
    let dashboard = fs::read_to_string("src/dashboard.rs").unwrap();

    for contract in [
        "CollaborationState::Active",
        "CollaborationState::PauseRequested",
        "CollaborationState::Paused",
        "CollaborationState::Inactive",
        "Pausing after current feedback",
        "No agent connected",
        "feedback_tools_visible",
    ] {
        assert!(
            dashboard.contains(contract),
            "missing state matrix contract: {contract}"
        );
    }
}

#[test]
fn dashboard_and_feedback_toolbar_have_separate_lifecycles() {
    let dashboard = fs::read_to_string("src/dashboard.rs").unwrap();
    let overlay = fs::read_to_string("web/overlay.js").unwrap();

    assert!(dashboard.contains("runtime_state"));
    assert!(dashboard.contains("dashboard_visible"));
    assert!(overlay.contains("function setActive(active)"));
    assert!(overlay.contains("closeEditor();"));
    assert!(overlay.contains("clearMarks();"));
}

#[test]
fn dashboard_exposes_a_non_secret_preview_id_handoff() {
    let dashboard = fs::read_to_string("src/dashboard.rs").unwrap();
    let snapshot = build_snapshot(
        1,
        DashboardRuntimeState::Running,
        "0123456789abcdef",
        &[],
        &[],
        None,
        None,
    );
    let reducer = DashboardReducer::new(snapshot.clone());
    let serialized = serde_json::to_string(&snapshot).unwrap();

    assert!(reducer.connect_agent_available());
    assert_eq!(snapshot.preview_session_id, "0123456789abcdef");
    assert_eq!(
        reducer.connect_command().as_deref(),
        Some("$preview-collaboration-connect 0123456789abcdef")
    );
    for contract in [
        "Preview ID",
        "Connect agent",
        "Copy command",
        "same project workspace",
        "NSPasteboard",
        "NSPopover",
    ] {
        assert!(
            dashboard.contains(contract),
            "dashboard is missing handoff contract: {contract}"
        );
    }
    for forbidden in [
        "/private",
        "4242",
        "tui-private",
        "att-private",
        "controlToken",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "snapshot leaked {forbidden}"
        );
        assert!(
            !reducer.connect_command().unwrap().contains(forbidden),
            "copy command leaked {forbidden}"
        );
    }
    assert!(!dashboard.contains("WebviewWindowBuilder::new"));
}

#[test]
fn preview_collaboration_terminology_is_unambiguous() {
    let paused = [attachment(CollaborationState::Paused)];
    let reducer = DashboardReducer::new(build_snapshot(
        1,
        DashboardRuntimeState::Running,
        "0123456789abcdef",
        &paused,
        &[],
        Some("att-private"),
        None,
    ));

    assert!(!reducer.connect_agent_available());
    assert_eq!(reducer.selected_state_text(), "Paused");
    assert_eq!(reducer.connect_command(), None);
}

#[test]
fn clipboard_failure_is_non_blocking_and_keeps_the_command_visible() {
    let mut reducer = DashboardReducer::new(build_snapshot(
        1,
        DashboardRuntimeState::Running,
        "0123456789abcdef",
        &[],
        &[],
        None,
        None,
    ));

    reducer.record_connect_copy_result(Err("pasteboard rejected write".into()));

    assert_eq!(reducer.banner(), Some("pasteboard rejected write"));

    reducer.clear_banner();

    assert_eq!(reducer.banner(), None);
    assert_eq!(
        reducer.connect_command().as_deref(),
        Some("$preview-collaboration-connect 0123456789abcdef")
    );
}

#[test]
fn successful_attach_closes_the_connect_handoff() {
    let mut reducer = DashboardReducer::new(build_snapshot(
        1,
        DashboardRuntimeState::Running,
        "0123456789abcdef",
        &[],
        &[],
        None,
        None,
    ));
    reducer.toggle_connect_handoff();
    assert!(reducer.connect_handoff_open());

    let active = [attachment(CollaborationState::Active)];
    reducer.apply_snapshot(build_snapshot(
        2,
        DashboardRuntimeState::Running,
        "0123456789abcdef",
        &active,
        &[],
        Some("att-private"),
        None,
    ));

    assert!(!reducer.connect_handoff_open());
    assert!(reducer.feedback_tools_visible());
}

// spec「Persistent native collaboration dashboard」：zero connected 時 Paint 是
// Offline Paint 的唯一入口，且沿用既有 native 物件生命週期與 selector 型別。
#[test]
fn native_offline_paint_button_is_the_only_zero_connected_entry() {
    let dashboard = fs::read_to_string("src/dashboard.rs").unwrap();

    for contract in [
        "offline_paint: usize",
        "offline_paint: Retained::as_ptr(&offline_paint) as usize",
        "Some(sel!(offlinePaint:))",
        "DashboardControl::OfflinePaint",
        "DashboardAction::ToggleOfflinePaint",
        r#"NSString::from_str("Paint")"#,
        r#"NSString::from_str("Offline Paint")"#,
        "offline_paint.setHidden(!offline_paint_available)",
        "offline_paint.setEnabled(offline_paint_available && !pending)",
    ] {
        assert!(
            dashboard.contains(contract),
            "missing native Offline Paint contract: {contract}"
        );
    }
    assert!(
        !dashboard.contains("WebviewWindowBuilder::new"),
        "Offline Paint must not introduce a second WebView"
    );
}

// spec「Selected attachment is active」/「…pause-requested」/「…paused」：
// Offline Paint 的可見性只由 reducer 的 zero-connected 判斷驅動。
#[test]
fn offline_paint_visibility_follows_the_reducer_state_matrix() {
    let disconnected = DashboardReducer::new(build_snapshot(
        1,
        DashboardRuntimeState::Running,
        "0123456789abcdef",
        &[],
        &[],
        None,
        None,
    ));
    assert!(disconnected.offline_paint_available());

    for state in [
        CollaborationState::Active,
        CollaborationState::PauseRequested,
        CollaborationState::Paused,
    ] {
        let connected = DashboardReducer::new(build_snapshot(
            1,
            DashboardRuntimeState::Running,
            "0123456789abcdef",
            &[attachment(state)],
            &[],
            Some("att-private"),
            None,
        ));
        assert!(
            !connected.offline_paint_available(),
            "Offline Paint must stay hidden for {state:?}"
        );
    }
}

#[test]
fn toolbar_does_not_display_inline_feedback_text() {
    let dashboard = fs::read_to_string("src/dashboard.rs").unwrap();

    assert!(
        dashboard.contains("Info"),
        "toolbar must have an Info button for feedback popover"
    );
    assert!(
        dashboard.contains("NSPopover"),
        "feedback info must use NSPopover"
    );
    assert!(
        dashboard.contains("info_popover"),
        "must track info popover pointer"
    );
    assert!(
        dashboard.contains("info_counts_label"),
        "popover must show feedback counts"
    );
    assert!(
        dashboard.contains("info_items_stack"),
        "popover must list feedback items"
    );
}

#[test]
fn connect_agent_uses_popover_instead_of_inline_display() {
    let dashboard = fs::read_to_string("src/dashboard.rs").unwrap();

    assert!(
        dashboard.contains("connect_popover"),
        "connect agent handoff must use NSPopover"
    );
    assert!(
        !dashboard.contains("preview_id.setHidden"),
        "preview_id should not be toggled inline on toolbar"
    );
    assert!(
        !dashboard.contains("command.setHidden"),
        "command should not be toggled inline on toolbar"
    );
    assert!(
        !dashboard.contains("copy.setHidden"),
        "copy should not be toggled inline on toolbar"
    );
    assert!(
        !dashboard.contains("workspace_instruction.setHidden"),
        "workspace_instruction should not be toggled inline on toolbar"
    );
}
