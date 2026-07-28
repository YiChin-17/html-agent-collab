//! Transport-neutral collaboration operation contract.
//! CLI/HTTP is one adapter; future adapters reuse these request/result types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::feedback::FeedbackRecord;
use crate::feedback::{FeedbackKind, FeedbackState};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CollaborationState {
    Active,
    PauseRequested,
    Paused,
    Inactive,
}

impl CollaborationState {
    pub fn is_connected(self) -> bool {
        !matches!(self, Self::Inactive)
    }

    pub fn accepts_feedback(self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub attachment_id: String,
    pub agent_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tui_session_id: Option<String>,
    pub pid: u32,
    pub attached_at_epoch_secs: u64,
    pub last_heartbeat_epoch_secs: u64,
    pub collaboration_state: CollaborationState,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DashboardRuntimeState {
    Running,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DashboardAttachmentSummary {
    pub attachment_id: String,
    pub agent_kind: String,
    pub collaboration_state: CollaborationState,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DashboardFeedbackCounts {
    pub pending: usize,
    pub acknowledged: usize,
    pub working: usize,
    pub resolved: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DashboardFeedbackSummary {
    pub id: String,
    pub kind: FeedbackKind,
    pub state: FeedbackState,
    pub text: String,
    pub orphaned: bool,
    pub failure_reason: Option<String>,
    pub attachment_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSnapshot {
    pub revision: u64,
    pub runtime_state: DashboardRuntimeState,
    pub preview_session_id: String,
    pub attachments: Vec<DashboardAttachmentSummary>,
    pub selected_attachment_id: Option<String>,
    pub feedback_counts: DashboardFeedbackCounts,
    pub feedback_items: Vec<DashboardFeedbackSummary>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum DashboardAction {
    Pause { attachment_id: String },
    Resume { attachment_id: String },
    Stop { attachment_id: String },
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashboardActionError {
    Busy,
    SelectionRequired,
    AttachmentNotFound,
    AttachmentInactive,
    Storage(String),
    Internal(String),
}

impl DashboardActionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::SelectionRequired => "selection-required",
            Self::AttachmentNotFound => "attachment-not-found",
            Self::AttachmentInactive => "attachment-inactive",
            Self::Storage(_) => "storage-error",
            Self::Internal(_) => "internal-error",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Busy => "Dashboard action queue is busy".into(),
            Self::SelectionRequired => "Select an attachment first".into(),
            Self::AttachmentNotFound => "The selected attachment no longer exists".into(),
            Self::AttachmentInactive => "The selected attachment is inactive".into(),
            Self::Storage(message) | Self::Internal(message) => message.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackMarkerSummary {
    pub id: String,
    pub state: FeedbackState,
    pub text: String,
    pub failure_reason: Option<String>,
    pub element: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackMarkerSnapshot {
    pub revision: u64,
    pub items: Vec<FeedbackMarkerSummary>,
}

impl Attachment {
    pub fn set_collaboration_state(&mut self, state: CollaborationState) {
        self.collaboration_state = state;
        self.active = state.is_connected();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttachRequest {
    pub agent_kind: String,
    #[serde(default)]
    pub tui_session_id: Option<String>,
    pub pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WaitRequest {
    pub attachment_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DetachRequest {
    #[serde(default)]
    pub attachment_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationControlRequest {
    #[serde(default)]
    pub attachment_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EvalRequest {
    pub expression: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackShowRequest {
    pub feedback_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackStateRequest {
    pub feedback_id: String,
    pub attachment_id: String,
    pub expected_state: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", content = "request", rename_all = "kebab-case")]
pub enum CoreRequest {
    Attach(AttachRequest),
    Status,
    Detach(DetachRequest),
    Pause(CollaborationControlRequest),
    Resume(CollaborationControlRequest),
    Close,
    Screenshot,
    Eval(EvalRequest),
    Wait(WaitRequest),
    FeedbackShow(FeedbackShowRequest),
    FeedbackSetState(FeedbackStateRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttachResult {
    pub preview_session_id: String,
    pub attachment: Attachment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatusResult {
    pub status: String,
    pub session_id: String,
    pub attachments: Vec<Attachment>,
    pub project_root: PathBuf,
    pub entry_file: PathBuf,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DetachResult {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_id: Option<String>,
    pub active_attachment_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationControlResult {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_id: Option<String>,
    pub collaboration_state: CollaborationState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloseResult {
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvalResult {
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenshotResult {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WaitResult {
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<FeedbackRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedbackResult {
    pub item: FeedbackRecord,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CoreResult {
    Attach(AttachResult),
    Status(StatusResult),
    Detach(DetachResult),
    CollaborationControl(CollaborationControlResult),
    Close(CloseResult),
    Screenshot(ScreenshotResult),
    Eval(EvalResult),
    Wait(WaitResult),
    Feedback(FeedbackResult),
    Interrupted,
}

impl CoreResult {
    pub fn into_data(self) -> Value {
        match self {
            CoreResult::Attach(value) => serde_json::to_value(value),
            CoreResult::Status(value) => serde_json::to_value(value),
            CoreResult::Detach(value) => serde_json::to_value(value),
            CoreResult::CollaborationControl(value) => serde_json::to_value(value),
            CoreResult::Close(value) => serde_json::to_value(value),
            CoreResult::Screenshot(value) => serde_json::to_value(value),
            CoreResult::Eval(value) => serde_json::to_value(value),
            CoreResult::Wait(value) => serde_json::to_value(value),
            CoreResult::Feedback(value) => serde_json::to_value(value),
            CoreResult::Interrupted => Ok(serde_json::json!({ "event": "interrupted" })),
        }
        .expect("core results are always serializable")
    }

    pub fn is_interrupted(&self) -> bool {
        matches!(self, CoreResult::Interrupted)
    }
}

pub trait CoreAdapter {
    type Error;

    fn execute(&self, request: CoreRequest) -> Result<CoreResult, Self::Error>;
}

#[derive(Debug, Serialize)]
pub struct Envelope {
    pub ok: bool,
    pub data: Option<Value>,
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl Envelope {
    pub fn success(data: Value) -> Self {
        Envelope {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn failure(error: OpError) -> Self {
        Envelope {
            ok: false,
            data: None,
            error: Some(ErrorBody {
                code: error.code,
                message: error.message,
                details: error.details,
            }),
        }
    }
}

#[derive(Debug)]
pub struct OpError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

impl OpError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        OpError {
            code: code.to_string(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}
