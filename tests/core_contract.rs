//! Task 5.2：transport-neutral typed core contract 與 MCP-optional boundary。

use collab::core::{
    Attachment, CloseResult, CollaborationState, CoreAdapter, CoreRequest, CoreResult,
    DetachRequest, DetachResult, EvalRequest, EvalResult, FeedbackStateRequest, OpError,
};

struct FakeAdapter;

impl CoreAdapter for FakeAdapter {
    type Error = &'static str;

    fn execute(&self, request: CoreRequest) -> Result<CoreResult, Self::Error> {
        match request {
            CoreRequest::Eval(EvalRequest { expression }) => Ok(CoreResult::Eval(EvalResult {
                value: serde_json::json!({ "expression": expression, "transport": "fake" }),
            })),
            _ => Err("unsupported test operation"),
        }
    }
}

#[test]
fn future_adapter_receives_typed_request_and_returns_cli_data_shape() {
    let result = FakeAdapter
        .execute(CoreRequest::Eval(EvalRequest {
            expression: "document.title".into(),
        }))
        .unwrap();
    assert_eq!(
        result.into_data(),
        serde_json::json!({
            "value": {
                "expression": "document.title",
                "transport": "fake"
            }
        })
    );
}

#[test]
fn feedback_compare_request_has_a_stable_serialized_shape() {
    let request = CoreRequest::FeedbackSetState(FeedbackStateRequest {
        feedback_id: "fb-1-00000001".into(),
        attachment_id: "att-1".into(),
        expected_state: "working".into(),
        state: "failed".into(),
        reason: Some("snapshot mismatch".into()),
    });
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        serde_json::json!({
            "operation": "feedback-set-state",
            "request": {
                "feedbackId": "fb-1-00000001",
                "attachmentId": "att-1",
                "expectedState": "working",
                "state": "failed",
                "reason": "snapshot mismatch"
            }
        })
    );
}

#[test]
fn dependency_manifest_has_no_required_mcp_runtime() {
    let manifests = format!(
        "{}\n{}",
        std::fs::read_to_string("Cargo.toml").unwrap(),
        std::fs::read_to_string("Cargo.lock").unwrap()
    )
    .to_ascii_lowercase();
    for dependency in ["rmcp", "model-context-protocol", "mcp-server"] {
        assert!(
            !manifests.contains(dependency),
            "unexpected required MCP dependency: {dependency}"
        );
    }
}

#[test]
fn control_client_is_the_cli_core_adapter() {
    fn assert_adapter<T: CoreAdapter<Error = OpError>>() {}
    assert_adapter::<collab::client::ControlClient>();
}

#[test]
fn cli_dispatches_typed_core_requests_instead_of_http_paths() {
    let main = std::fs::read_to_string("src/main.rs").unwrap();
    assert!(main.contains("CoreRequest::Attach"));
    assert!(main.contains("CoreRequest::Wait"));
    assert!(main.contains(".execute(request)"));
    assert!(!main.contains("post_control("));
    assert!(!main.contains("get_control("));
}

#[test]
fn attachment_lifecycle_types_have_stable_serialized_shapes() {
    let attachment = Attachment {
        attachment_id: "att-1".into(),
        agent_kind: "codex".into(),
        tui_session_id: None,
        pid: 42,
        attached_at_epoch_secs: 100,
        last_heartbeat_epoch_secs: 101,
        collaboration_state: CollaborationState::Inactive,
        active: false,
    };
    assert_eq!(
        serde_json::to_value(attachment).unwrap()["active"],
        serde_json::json!(false)
    );
    let serialized = serde_json::to_value(Attachment {
        attachment_id: "att-2".into(),
        agent_kind: "codex".into(),
        tui_session_id: None,
        pid: 43,
        attached_at_epoch_secs: 100,
        last_heartbeat_epoch_secs: 101,
        collaboration_state: CollaborationState::PauseRequested,
        active: true,
    })
    .unwrap();
    assert_eq!(serialized["collaborationState"], "pause-requested");
    let mut invalid = serialized;
    invalid["collaborationState"] = serde_json::json!("half-paused");
    assert!(serde_json::from_value::<Attachment>(invalid).is_err());
    assert_eq!(
        serde_json::to_value(DetachRequest {
            attachment_id: Some("att-1".into()),
        })
        .unwrap(),
        serde_json::json!({ "attachmentId": "att-1" })
    );
    assert_eq!(
        serde_json::to_value(DetachResult {
            status: "detached".into(),
            attachment_id: Some("att-1".into()),
            active_attachment_count: 0,
        })
        .unwrap(),
        serde_json::json!({
            "status": "detached",
            "attachmentId": "att-1",
            "activeAttachmentCount": 0
        })
    );
    assert_eq!(
        serde_json::to_value(CloseResult {
            status: "closing".into(),
        })
        .unwrap(),
        serde_json::json!({ "status": "closing" })
    );
}
