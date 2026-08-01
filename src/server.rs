//! axum loopback control service。
//! spec「Local HTTP project serving」：loopback-only origin 服務專案目錄，
//! 阻擋 URL decode 與 normalization 後仍指向 root 之外的請求。
//! spec「Local control boundary」：state-changing operations 需要 per-preview
//! token；health/status 為不含敏感資訊的最小 read-only response。

use std::io;
use std::net::Ipv4Addr;
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::{Notify, watch};
use tower_http::services::ServeDir;

use crate::core::{
    AttachRequest, Attachment, CollaborationControlRequest, CollaborationControlResult,
    CollaborationState, DashboardAction, DashboardActionError, DashboardRuntimeState,
    DetachRequest, DetachResult, EvalRequest, FeedbackStateRequest, WaitRequest,
};
use crate::webview::{CommandError, WebviewCommand};

/// control 路徑前綴；同名的專案檔案會被 control routes 遮蔽（documented boundary）。
pub const CONTROL_PREFIX: &str = "/__collab__";
pub const ATTACHMENT_CAPACITY: usize = 32;
pub const CONSOLE_BUFFER_CAPACITY: usize = 0;
pub const RESPONSE_BUFFER_CAPACITY: usize = 1;
pub const CONTROL_BODY_LIMIT_BYTES: usize = 1024 * 1024;
pub const WEBVIEW_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
pub const ATTACHMENT_EXPIRY: Duration = Duration::from_secs(60);

pub struct ServerConfig {
    pub project_root: PathBuf,
    pub session_id: String,
    pub token: String,
    /// main-thread WebView command queue（bounded；滿時回 `busy`）。
    pub commands: crate::webview::CommandSender,
}

#[derive(Clone)]
struct ControlState {
    token: Arc<String>,
    session_id: Arc<String>,
    project_root: Arc<PathBuf>,
    shutdown_tx: watch::Sender<bool>,
    attachments: Arc<Mutex<Vec<AttachmentState>>>,
    attachment_lifecycle: Arc<tokio::sync::Mutex<()>>,
    feedback_lock: Arc<tokio::sync::Mutex<()>>,
    feedback_notify: Arc<Notify>,
    lifecycle_notify: Arc<Notify>,
    commands: crate::webview::CommandSender,
    dashboard: crate::dashboard::DashboardPublisher,
    dashboard_revision: Arc<AtomicU64>,
    draft_states: watch::Sender<crate::draft_panel::DraftPanelState>,
}

#[derive(Clone)]
struct HostValidationState {
    allowed_hosts: [String; 2],
}

#[derive(Clone)]
struct StaticValidationState {
    canonical_project_root: Arc<PathBuf>,
}

struct AttachmentState {
    attachment: Attachment,
    stop_tx: watch::Sender<bool>,
}

pub struct RunningServer {
    pub port: u16,
    /// serve loop 的 join handle；graceful shutdown（stop 之後所有 in-flight
    /// response 送畢）完成時 resolve。
    pub task: tokio::task::JoinHandle<io::Result<()>>,
    pub dashboard: crate::dashboard::DashboardHandle,
    pub draft_states: watch::Receiver<crate::draft_panel::DraftPanelState>,
}

/// 在隨機 loopback port 啟動 project serving 與 control service。
/// 只綁 127.0.0.1，不得綁任何非 loopback interface。
pub async fn start(config: ServerConfig) -> io::Result<RunningServer> {
    let canonical_project_root = config.project_root.canonicalize()?;
    let feedback_lock = Arc::new(tokio::sync::Mutex::new(()));
    serialized_feedback_io(
        Arc::new(config.project_root.clone()),
        feedback_lock.clone(),
        crate::feedback::recover_expired_leases,
    )
    .await
    .map_err(|error| io::Error::other(format!("cannot recover feedback queue: {error}")))?;
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let dashboard_shutdown_rx = shutdown_tx.subscribe();
    let initial_dashboard = crate::dashboard::build_snapshot(
        0,
        DashboardRuntimeState::Running,
        &config.session_id,
        &[],
        &[],
        None,
        None,
    );
    let (dashboard, dashboard_handle, dashboard_actions) =
        crate::dashboard::channel(initial_dashboard);
    let (draft_states, draft_state_receiver) =
        watch::channel(crate::draft_panel::DraftPanelState::default());
    let state = ControlState {
        token: Arc::new(config.token),
        session_id: Arc::new(config.session_id),
        project_root: Arc::new(config.project_root.clone()),
        shutdown_tx,
        attachments: Arc::new(Mutex::new(Vec::new())),
        attachment_lifecycle: Arc::new(tokio::sync::Mutex::new(())),
        feedback_lock,
        feedback_notify: Arc::new(Notify::new()),
        lifecycle_notify: Arc::new(Notify::new()),
        commands: config.commands,
        dashboard,
        dashboard_revision: Arc::new(AtomicU64::new(0)),
        draft_states,
    };
    publish_dashboard_snapshot(&state, DashboardRuntimeState::Running).await;
    tokio::spawn(run_dashboard_actions(
        state.clone(),
        dashboard_actions,
        dashboard_shutdown_rx,
    ));

    let control = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/status", get(status))
        .route("/control/status", get(control_status))
        .route("/control/attach", post(control_attach))
        .route("/control/heartbeat", post(control_heartbeat))
        .route("/control/detach", post(control_detach))
        .route("/control/pause", post(control_pause))
        .route("/control/resume", post(control_resume))
        .route("/control/close", post(control_close))
        .route("/control/reload", post(control_reload))
        .route("/control/eval", post(control_eval))
        .route("/control/screenshot", post(control_screenshot))
        .route("/control/wait", post(control_wait))
        .route("/control/feedback/lease", post(control_feedback_lease))
        .route("/control/feedback/{id}", get(control_feedback_show))
        .route(
            "/control/feedback/{id}/state",
            post(control_feedback_set_state),
        )
        .route(
            "/overlay/feedback",
            post(overlay_feedback).get(overlay_feedback_list),
        )
        .route(
            "/overlay/feedback/reconcile",
            post(overlay_feedback_reconcile),
        )
        .route("/overlay/draft-state", post(overlay_draft_state))
        .with_state(state);

    let host_validation = HostValidationState {
        allowed_hosts: [format!("127.0.0.1:{port}"), format!("localhost:{port}")],
    };
    let static_validation = StaticValidationState {
        canonical_project_root: Arc::new(canonical_project_root.clone()),
    };
    let app = Router::new()
        .nest(CONTROL_PREFIX, control)
        .fallback_service(ServeDir::new(canonical_project_root))
        .layer(DefaultBodyLimit::max(CONTROL_BODY_LIMIT_BYTES))
        .layer(middleware::from_fn_with_state(
            static_validation,
            validate_static_path,
        ))
        .layer(middleware::from_fn_with_state(
            host_validation,
            validate_host,
        ));

    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.changed().await;
            })
            .await
    });

    Ok(RunningServer {
        port,
        task,
        dashboard: dashboard_handle,
        draft_states: draft_state_receiver,
    })
}

async fn overlay_draft_state(
    State(state): State<ControlState>,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    match crate::draft_panel::validate_state(body.0) {
        Ok(draft_state) => {
            state.draft_states.send_replace(draft_state);
            (StatusCode::OK, Json(json!({ "status": "published" })))
        }
        Err(message) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "code": "invalid-request", "message": message })),
        ),
    }
}

async fn blocking_io<T, E, F>(operation: F) -> Result<T, E>
where
    T: Send + 'static,
    E: From<io::Error> + Send + 'static,
    F: FnOnce() -> Result<T, E> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| {
            E::from(io::Error::other(format!(
                "blocking I/O task failed: {error}"
            )))
        })?
}

async fn serialized_feedback_io<T, E, F>(
    project_root: Arc<PathBuf>,
    feedback_lock: Arc<tokio::sync::Mutex<()>>,
    operation: F,
) -> Result<T, E>
where
    T: Send + 'static,
    E: From<io::Error> + Send + 'static,
    F: FnOnce(&FsPath) -> Result<T, E> + Send + 'static,
{
    blocking_io(move || {
        let _guard = feedback_lock.blocking_lock();
        operation(&project_root)
    })
    .await
}

async fn feedback_io<T, E, F>(state: &ControlState, operation: F) -> Result<T, E>
where
    T: Send + 'static,
    E: From<io::Error> + Send + 'static,
    F: FnOnce(&FsPath) -> Result<T, E> + Send + 'static,
{
    serialized_feedback_io(
        state.project_root.clone(),
        state.feedback_lock.clone(),
        operation,
    )
    .await
}

async fn publish_dashboard_snapshot(
    state: &ControlState,
    runtime_state: DashboardRuntimeState,
) -> u64 {
    let revision = state.dashboard_revision.fetch_add(1, Ordering::Relaxed) + 1;
    let attachments = state
        .attachments
        .lock()
        .unwrap()
        .iter()
        .map(|entry| entry.attachment.clone())
        .collect::<Vec<_>>();
    let previous = state.dashboard.current();
    let selected = previous.selected_attachment_id.as_deref();
    let view = feedback_io(state, |root| {
        crate::feedback::dashboard_view(root, crate::dashboard::DASHBOARD_FEEDBACK_LIMIT)
    })
    .await;
    let mut snapshot = match view {
        Ok(view) => {
            let mut snapshot = crate::dashboard::build_snapshot(
                revision,
                runtime_state,
                &state.session_id,
                &attachments,
                &view.records,
                selected,
                None,
            );
            snapshot.feedback_counts = crate::core::DashboardFeedbackCounts {
                pending: view.counts.pending,
                acknowledged: view.counts.acknowledged,
                working: view.counts.working,
                resolved: view.counts.resolved,
                failed: view.counts.failed,
            };
            snapshot
        }
        Err(error) => crate::dashboard::build_snapshot(
            revision,
            runtime_state,
            &state.session_id,
            &attachments,
            &[],
            selected,
            Some(format!("feedback storage unavailable: {error}")),
        ),
    };
    if snapshot.error.is_some() {
        snapshot.feedback_counts = previous.feedback_counts;
        snapshot.feedback_items = previous.feedback_items;
    }
    state.dashboard.publish(snapshot);
    revision
}

async fn publish_feedback_mutation(state: &ControlState) {
    let revision = publish_dashboard_snapshot(state, DashboardRuntimeState::Running).await;
    let records = match feedback_io(state, |root| {
        crate::feedback::marker_records(root, crate::dashboard::FEEDBACK_MARKER_LIMIT)
    })
    .await
    {
        Ok(records) => records,
        Err(error) => {
            eprintln!("cannot build feedback marker snapshot: {error}");
            return;
        }
    };
    let snapshot = crate::dashboard::build_marker_snapshot(revision, &records);
    let (respond, receive) = tokio::sync::oneshot::channel();
    if let Err((_, body)) = submit_command(
        state,
        WebviewCommand::SyncFeedbackMarkers { snapshot, respond },
        receive,
    )
    .await
    {
        eprintln!("cannot sync feedback markers: {}", body.0);
    }
}

async fn run_dashboard_actions(
    state: ControlState,
    mut actions: crate::dashboard::DashboardActionReceiver,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let request = tokio::select! {
            request = actions.recv() => {
                let Some(request) = request else {
                    break;
                };
                request
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
        };
        let response = match &request.action {
            DashboardAction::Pause { attachment_id } => {
                pause_attachment(
                    &state,
                    CollaborationControlRequest {
                        attachment_id: Some(attachment_id.clone()),
                    },
                )
                .await
            }
            DashboardAction::Resume { attachment_id } => {
                resume_attachment(
                    &state,
                    CollaborationControlRequest {
                        attachment_id: Some(attachment_id.clone()),
                    },
                )
                .await
            }
            DashboardAction::Stop { attachment_id } => {
                detach_attachment(
                    &state,
                    DetachRequest {
                        attachment_id: Some(attachment_id.clone()),
                    },
                )
                .await
            }
            DashboardAction::ToggleOfflinePaint => match toggle_offline_paint(&state).await {
                Ok(()) => (StatusCode::OK, Json(json!({ "status": "toggled" }))),
                Err(response) => response,
            },
            DashboardAction::Close => match close_all_attachments(&state).await {
                Ok(()) => {
                    let revision =
                        publish_dashboard_snapshot(&state, DashboardRuntimeState::Closed).await;
                    let _ = state.shutdown_tx.send(true);
                    request.respond(Ok(revision));
                    break;
                }
                Err(response) => response,
            },
        };
        let result = if response.0.is_success() {
            Ok(state.dashboard_revision.load(Ordering::Relaxed))
        } else {
            Err(dashboard_action_error(&response.1.0))
        };
        request.respond(result);
    }
}

fn dashboard_action_error(body: &Value) -> DashboardActionError {
    let code = body
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("internal-error");
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Dashboard action failed")
        .to_string();
    match code {
        "busy" => DashboardActionError::Busy,
        "attachment-not-found" => DashboardActionError::AttachmentNotFound,
        "attachment-inactive" | "pause-pending" => DashboardActionError::AttachmentInactive,
        "offline-paint-unavailable" => DashboardActionError::OfflinePaintUnavailable,
        "feedback-storage-error" => DashboardActionError::Storage(message),
        _ => DashboardActionError::Internal(message),
    }
}

async fn validate_static_path(
    State(state): State<StaticValidationState>,
    request: Request,
    next: Next,
) -> Response {
    let raw_path = request.uri().path();
    if raw_path == CONTROL_PREFIX || raw_path.starts_with(&format!("{CONTROL_PREFIX}/")) {
        return next.run(request).await;
    }

    let relative_path = match validated_project_relative_path(raw_path) {
        Ok(path) => path,
        Err(message) => return invalid_static_path(message).into_response(),
    };

    let target = state.canonical_project_root.join(&relative_path);
    match target.canonicalize() {
        Ok(canonical_target) if !canonical_target.starts_with(&*state.canonical_project_root) => {
            return forbidden_static_path("static path escapes the project root").into_response();
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => {
            return forbidden_static_path("static path cannot be resolved safely").into_response();
        }
    }

    next.run(request).await
}

fn validated_project_relative_path(path: &str) -> Result<PathBuf, &'static str> {
    let decoded_path = decode_uri_path(path)?;
    let relative_path = decoded_path.trim_start_matches('/');
    for component in FsPath::new(relative_path).components() {
        match component {
            Component::Normal(name)
                if name
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(crate::session::SESSION_DIR)) =>
            {
                return Err("session registry is not available through static serving");
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("static path escapes the project root");
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(PathBuf::from(relative_path))
}

/// 將目前 preview URL path 解析為 project root 內既有的實體資源。
pub fn resolve_project_resource(project_root: &FsPath, uri_path: &str) -> io::Result<PathBuf> {
    let canonical_project_root = project_root.canonicalize()?;
    let relative_path = validated_project_relative_path(uri_path)
        .map_err(|message| io::Error::new(io::ErrorKind::PermissionDenied, message))?;
    let target = canonical_project_root.join(relative_path).canonicalize()?;
    if !target.starts_with(&canonical_project_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "static path escapes the project root",
        ));
    }
    Ok(target)
}

fn decode_uri_path(path: &str) -> Result<String, &'static str> {
    fn hex_value(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let Some(high) = bytes.get(index + 1).and_then(|byte| hex_value(*byte)) else {
            return Err("static path contains invalid percent encoding");
        };
        let Some(low) = bytes.get(index + 2).and_then(|byte| hex_value(*byte)) else {
            return Err("static path contains invalid percent encoding");
        };
        decoded.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| "static path is not valid UTF-8")
}

#[cfg(test)]
mod draft_source_tests {
    use super::resolve_project_resource;

    #[test]
    fn resolves_only_existing_project_contained_resources() {
        let root = std::env::temp_dir().join(format!(
            "collab-draft-source-resolver-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("pages")).unwrap();
        std::fs::write(root.join("pages/index.html"), "<!doctype html>").unwrap();
        std::fs::create_dir_all(root.join(".collab")).unwrap();
        std::fs::write(root.join(".collab/session.json"), "{}").unwrap();

        assert_eq!(
            resolve_project_resource(&root, "/pages/index.html").unwrap(),
            root.join("pages/index.html").canonicalize().unwrap()
        );
        assert!(resolve_project_resource(&root, "/../outside.html").is_err());
        assert!(resolve_project_resource(&root, "/%2e%2e/outside.html").is_err());
        assert!(resolve_project_resource(&root, "/.collab/session.json").is_err());
        assert!(resolve_project_resource(&root, "/missing.html").is_err());
    }
}

fn forbidden_static_path(message: &'static str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "code": "forbidden-static-path", "message": message })),
    )
}

fn invalid_static_path(message: &'static str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "code": "invalid-static-path", "message": message })),
    )
}

async fn validate_host(
    State(state): State<HostValidationState>,
    request: Request,
    next: Next,
) -> Response {
    let allowed = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| state.allowed_hosts.iter().any(|allowed| allowed == host));
    if !allowed {
        return forbidden_host().into_response();
    }
    next.run(request).await
}

fn forbidden_host() -> (StatusCode, Json<Value>) {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "code": "forbidden-host",
            "message": "request Host is not allowed for this preview session",
        })),
    )
}

/// 無 token 的最小 read-only response；不得包含 token 或其他敏感資訊。
async fn health(State(state): State<ControlState>) -> Json<Value> {
    Json(json!({ "status": "ok", "sessionId": *state.session_id }))
}

async fn metrics(State(state): State<ControlState>) -> Json<Value> {
    let _ = expire_stale_attachments(&state).await;
    let attachments = state.attachments.lock().unwrap();
    let attachment_count = attachments.len();
    let active_attachment_count = attachments
        .iter()
        .filter(|attachment| attachment.attachment.active)
        .count();
    let command_capacity = state.commands.max_capacity();
    Json(json!({
        "attachmentCount": attachment_count,
        "activeAttachmentCount": active_attachment_count,
        "attachmentCapacity": ATTACHMENT_CAPACITY,
        "consoleItems": 0,
        "consoleCapacity": CONSOLE_BUFFER_CAPACITY,
        "eventQueueCapacity": crate::watcher::EVENT_QUEUE_CAPACITY,
        "responseBufferCapacity": RESPONSE_BUFFER_CAPACITY,
        "webviewCommandQueued": command_capacity.saturating_sub(state.commands.capacity()),
        "webviewCommandCapacity": command_capacity,
        "feedbackMemoryItems": 0,
        "feedbackViewLimit": crate::feedback::FEEDBACK_VIEW_LIMIT,
        "controlBodyLimitBytes": CONTROL_BODY_LIMIT_BYTES,
    }))
}

/// 無 token 的最小 read-only status；不含 token 或檔案路徑。
async fn status(State(state): State<ControlState>) -> Json<Value> {
    let _ = expire_stale_attachments(&state).await;
    let collaboration_active = state
        .attachments
        .lock()
        .unwrap()
        .iter()
        .any(|attachment| attachment.attachment.collaboration_state.accepts_feedback());
    Json(json!({
        "status": "ok",
        "collaborationActive": collaboration_active,
    }))
}

async fn control_status(
    State(state): State<ControlState>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let _ = expire_stale_attachments(&state).await;
    let attachments = state
        .attachments
        .lock()
        .unwrap()
        .iter()
        .map(|attachment| attachment.attachment.clone())
        .collect::<Vec<_>>();
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "sessionId": *state.session_id,
            "attachments": attachments,
        })),
    )
}

/// 記錄一個 agent attachment（state-changing，需 token）。
async fn control_attach(
    State(state): State<ControlState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let request: AttachRequest = match serde_json::from_value(body.0) {
        Ok(request) => request,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "code": "invalid-request",
                    "message": format!("invalid attach payload: {e}"),
                })),
            );
        }
    };
    let _lifecycle = state.attachment_lifecycle.lock().await;
    if let Err(response) = expire_stale_attachments_locked(&state, epoch_secs()).await {
        return response;
    }
    let now = epoch_secs();
    let attachment = Attachment {
        attachment_id: crate::session::generate_session_id(),
        agent_kind: request.agent_kind,
        tui_session_id: request.tui_session_id,
        pid: request.pid,
        attached_at_epoch_secs: now,
        last_heartbeat_epoch_secs: now,
        collaboration_state: CollaborationState::Active,
        active: true,
    };
    let (stop_tx, _) = watch::channel(false);
    let activate = {
        let mut attachments = state.attachments.lock().unwrap();
        if attachments.len() == ATTACHMENT_CAPACITY {
            let Some(inactive) = attachments
                .iter()
                .position(|attachment| !attachment.attachment.collaboration_state.is_connected())
            else {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "code": "attachment-capacity",
                        "message": "attachment registry is full of active attachments",
                    })),
                );
            };
            attachments.remove(inactive);
        }
        !attachments
            .iter()
            .any(|attachment| attachment.attachment.collaboration_state.accepts_feedback())
    };
    if activate && let Err(response) = set_collaboration_active(&state, true).await {
        return response;
    }
    state.attachments.lock().unwrap().push(AttachmentState {
        attachment: attachment.clone(),
        stop_tx,
    });
    publish_dashboard_snapshot(&state, DashboardRuntimeState::Running).await;
    (
        StatusCode::OK,
        Json(json!({
            "previewSessionId": *state.session_id,
            "attachment": attachment,
        })),
    )
}

async fn control_heartbeat(
    State(state): State<ControlState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let request: WaitRequest = match serde_json::from_value(body.0) {
        Ok(request) => request,
        Err(error) => return invalid_request(format!("invalid heartbeat payload: {error}")),
    };
    match heartbeat_attachment(&state, &request.attachment_id).await {
        Ok(AttachmentActivity::Active) => {}
        Ok(AttachmentActivity::Inactive) => return attachment_inactive(&request.attachment_id),
        Ok(AttachmentActivity::Missing) => return attachment_not_found(&request.attachment_id),
        Err(response) => return response,
    }
    let owner = request.attachment_id.clone();
    let renewed_leases = match feedback_io(&state, move |root| {
        crate::feedback::renew_owner_leases(root, &owner)
    })
    .await
    {
        Ok(count) => count,
        Err(error) => return feedback_error(error),
    };
    if renewed_leases > 0 {
        publish_dashboard_snapshot(&state, DashboardRuntimeState::Running).await;
    }
    (
        StatusCode::OK,
        Json(json!({ "status": "ok", "renewedLeases": renewed_leases })),
    )
}

async fn control_detach(
    State(state): State<ControlState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let request: DetachRequest = match serde_json::from_value(body.0) {
        Ok(request) => request,
        Err(error) => return invalid_request(format!("invalid detach payload: {error}")),
    };
    detach_attachment(&state, request).await
}

async fn control_pause(
    State(state): State<ControlState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let request: CollaborationControlRequest = match serde_json::from_value(body.0) {
        Ok(request) => request,
        Err(error) => return invalid_request(format!("invalid pause payload: {error}")),
    };
    pause_attachment(&state, request).await
}

async fn control_resume(
    State(state): State<ControlState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let request: CollaborationControlRequest = match serde_json::from_value(body.0) {
        Ok(request) => request,
        Err(error) => return invalid_request(format!("invalid resume payload: {error}")),
    };
    resume_attachment(&state, request).await
}

async fn control_close(
    State(state): State<ControlState>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    if let Err(response) = close_all_attachments(&state).await {
        return response;
    }
    publish_dashboard_snapshot(&state, DashboardRuntimeState::Closed).await;
    let _ = state.shutdown_tx.send(true);
    (StatusCode::OK, Json(json!({ "status": "closing" })))
}

async fn control_reload(
    State(state): State<ControlState>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let (respond, receive) = tokio::sync::oneshot::channel();
    match submit_command(&state, WebviewCommand::Reload { respond }, receive).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "status": "reloading" }))),
        Err(response) => response,
    }
}

async fn control_eval(
    State(state): State<ControlState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let request: EvalRequest = match serde_json::from_value(body.0) {
        Ok(request) => request,
        Err(error) => return invalid_request(format!("invalid eval payload: {error}")),
    };
    let (respond, receive) = tokio::sync::oneshot::channel();
    let command = WebviewCommand::Eval {
        expression: request.expression,
        respond,
    };
    match submit_command(&state, command, receive).await {
        Ok(value) => (StatusCode::OK, Json(json!({ "value": value }))),
        Err(response) => response,
    }
}

async fn control_screenshot(
    State(state): State<ControlState>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let project_root = state.project_root.clone();
    let screenshots = match blocking_io::<PathBuf, io::Error, _>(move || {
        crate::session::prepare_private_subdir(&project_root, "screenshots")
    })
    .await
    {
        Ok(path) => path,
        Err(error) => {
            return internal_error(format!("cannot prepare screenshots directory: {error}"));
        }
    };
    let output_path = screenshots.join(format!("snapshot-{}.png", unique_snapshot_stamp()));
    let (respond, receive) = tokio::sync::oneshot::channel();
    let command = WebviewCommand::Snapshot {
        output_path,
        respond,
    };
    match submit_command(&state, command, receive).await {
        Ok(path) => (StatusCode::OK, Json(json!({ "path": path }))),
        Err(response) => response,
    }
}

async fn control_wait(
    State(state): State<ControlState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let request: WaitRequest = match serde_json::from_value(body.0) {
        Ok(request) => request,
        Err(error) => return invalid_request(format!("invalid wait payload: {error}")),
    };
    match heartbeat_attachment(&state, &request.attachment_id).await {
        Ok(AttachmentActivity::Active) => {}
        Ok(AttachmentActivity::Inactive) => return collaboration_stop(),
        Ok(AttachmentActivity::Missing) => return attachment_not_found(&request.attachment_id),
        Err(response) => return response,
    }
    let mut stop_rx = match attachment_stop_receiver(&state, &request.attachment_id) {
        AttachmentAccess::Active(receiver) => receiver,
        AttachmentAccess::Inactive => return collaboration_stop(),
        AttachmentAccess::Missing => return attachment_not_found(&request.attachment_id),
    };
    loop {
        if *stop_rx.borrow() {
            return collaboration_stop();
        }

        let notified = state.feedback_notify.notified();
        tokio::pin!(notified);
        let lifecycle_notified = state.lifecycle_notify.notified();
        tokio::pin!(lifecycle_notified);
        let _lifecycle = state.attachment_lifecycle.lock().await;
        if let Err(response) = expire_stale_attachments_locked(&state, epoch_secs()).await {
            return response;
        }
        let collaboration_state = attachment_collaboration_state(&state, &request.attachment_id);
        let lease_result = match collaboration_state {
            Some(CollaborationState::Active) => {
                let owner = request.attachment_id.clone();
                feedback_io(&state, move |root| {
                    let item = crate::feedback::lease_next(root, &owner)?;
                    let delay = if item.is_none() {
                        crate::feedback::time_until_next_lease_expiry(root)?
                    } else {
                        None
                    };
                    Ok::<_, crate::feedback::QueueError>((item, delay))
                })
                .await
            }
            Some(CollaborationState::PauseRequested | CollaborationState::Paused) => {
                Ok((None, None))
            }
            Some(CollaborationState::Inactive) => return collaboration_stop(),
            None => return attachment_not_found(&request.attachment_id),
        };
        drop(_lifecycle);
        let lease_delay = match lease_result {
            Ok((Some(item), _)) => {
                publish_dashboard_snapshot(&state, DashboardRuntimeState::Running).await;
                return (
                    StatusCode::OK,
                    Json(json!({ "event": "feedback", "item": item })),
                );
            }
            Ok((None, delay)) => delay,
            Err(error) => return feedback_error(error),
        };
        let lease_timer = tokio::time::sleep(
            lease_delay.unwrap_or_else(|| std::time::Duration::from_secs(24 * 60 * 60)),
        );
        tokio::pin!(lease_timer);

        tokio::select! {
            _ = &mut notified => {}
            _ = &mut lifecycle_notified => {}
            changed = stop_rx.changed() => {
                if changed.is_err() || *stop_rx.borrow() {
                    return collaboration_stop();
                }
            }
            _ = &mut lease_timer, if lease_delay.is_some() => {}
        }
    }
}

async fn control_feedback_lease(
    State(state): State<ControlState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    let request: WaitRequest = match serde_json::from_value(body.0) {
        Ok(request) => request,
        Err(error) => return invalid_request(format!("invalid feedback lease payload: {error}")),
    };
    match heartbeat_attachment(&state, &request.attachment_id).await {
        Ok(AttachmentActivity::Active) => {}
        Ok(AttachmentActivity::Inactive) => return attachment_inactive(&request.attachment_id),
        Ok(AttachmentActivity::Missing) => return attachment_not_found(&request.attachment_id),
        Err(response) => return response,
    }

    let _lifecycle = state.attachment_lifecycle.lock().await;
    if attachment_collaboration_state(&state, &request.attachment_id)
        != Some(CollaborationState::Active)
    {
        return attachment_inactive(&request.attachment_id);
    }
    let owner = request.attachment_id;
    match feedback_io(&state, move |root| {
        crate::feedback::lease_next(root, &owner)
    })
    .await
    {
        Ok(item) => {
            if item.is_some() {
                publish_dashboard_snapshot(&state, DashboardRuntimeState::Running).await;
            }
            (StatusCode::OK, Json(json!({ "item": item })))
        }
        Err(error) => feedback_error(error),
    }
}

async fn control_feedback_show(
    State(state): State<ControlState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    if let Err(message) = crate::feedback::validate_feedback_id(&id) {
        return invalid_feedback_id(message);
    }
    let feedback_id = id.clone();
    match feedback_io(&state, move |root| {
        crate::feedback::read_record(root, &feedback_id)
    })
    .await
    {
        Ok(item) => (StatusCode::OK, Json(json!({ "item": item }))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            feedback_error(crate::feedback::QueueError::NotFound(id))
        }
        Err(error) => feedback_error(crate::feedback::QueueError::Io(error)),
    }
}

async fn control_feedback_set_state(
    State(state): State<ControlState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&state, &headers) {
        return unauthorized();
    }
    if let Err(message) = crate::feedback::validate_feedback_id(&id) {
        return invalid_feedback_id(message);
    }
    let mut payload = body.0;
    if let Some(map) = payload.as_object_mut() {
        map.insert("feedbackId".into(), json!(id));
    }
    let request: FeedbackStateRequest = match serde_json::from_value(payload) {
        Ok(request) => request,
        Err(error) => return invalid_request(format!("invalid feedback state payload: {error}")),
    };
    match heartbeat_attachment(&state, &request.attachment_id).await {
        Ok(AttachmentActivity::Active) => {}
        Ok(AttachmentActivity::Inactive) => return attachment_inactive(&request.attachment_id),
        Ok(AttachmentActivity::Missing) => return attachment_not_found(&request.attachment_id),
        Err(response) => return response,
    }

    let _lifecycle = state.attachment_lifecycle.lock().await;
    if let Err(response) = expire_stale_attachments_locked(&state, epoch_secs()).await {
        return response;
    }
    match attachment_collaboration_state(&state, &request.attachment_id) {
        Some(CollaborationState::Active | CollaborationState::PauseRequested) => {}
        Some(CollaborationState::Paused | CollaborationState::Inactive) => {
            return attachment_inactive(&request.attachment_id);
        }
        None => return attachment_not_found(&request.attachment_id),
    }
    let feedback_id = id.clone();
    let attachment_id = request.attachment_id.clone();
    let result = feedback_io(&state, move |root| {
        crate::feedback::transition(
            root,
            &feedback_id,
            &request.expected_state,
            &request.state,
            &request.attachment_id,
            request.reason.as_deref(),
        )
    })
    .await;
    match result {
        Ok(item) => {
            if matches!(
                item.state,
                crate::feedback::FeedbackState::Resolved | crate::feedback::FeedbackState::Failed
            ) && let Some(attachment) = state
                .attachments
                .lock()
                .unwrap()
                .iter_mut()
                .find(|entry| entry.attachment.attachment_id == attachment_id)
                && attachment.attachment.collaboration_state == CollaborationState::PauseRequested
            {
                attachment
                    .attachment
                    .set_collaboration_state(CollaborationState::Paused);
                state.lifecycle_notify.notify_waiters();
            }
            state.feedback_notify.notify_one();
            publish_feedback_mutation(&state).await;
            (StatusCode::OK, Json(json!({ "item": item })))
        }
        Err(error) => feedback_error(error),
    }
}

/// try_send 進 bounded queue 並等待 one-shot 結果。
/// queue 滿 → `busy`（caller 可延遲重試）；channel 關閉或 sender 遺失 → internal error。
async fn submit_command<T>(
    state: &ControlState,
    command: WebviewCommand,
    receive: tokio::sync::oneshot::Receiver<Result<T, CommandError>>,
) -> Result<T, (StatusCode, Json<Value>)> {
    submit_command_with_timeout(state, command, receive, WEBVIEW_COMMAND_TIMEOUT).await
}

async fn submit_command_with_timeout<T>(
    state: &ControlState,
    command: WebviewCommand,
    receive: tokio::sync::oneshot::Receiver<Result<T, CommandError>>,
    timeout: Duration,
) -> Result<T, (StatusCode, Json<Value>)> {
    use tokio::sync::mpsc::error::TrySendError;
    match state.commands.try_send(command) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "code": "busy",
                    "message": "webview command queue is full; retry shortly",
                })),
            ));
        }
        Err(TrySendError::Closed(_)) => {
            return Err(command_channel_closed());
        }
    }
    match tokio::time::timeout(timeout, receive).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => Err((
            command_error_status(&error),
            Json(json!({
                "code": error.code(),
                "message": error.message(),
            })),
        )),
        Ok(Err(_)) => Err(command_channel_closed()),
        Err(_) => Err((
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({
                "code": "timeout",
                "message": "webview command did not complete before the timeout",
            })),
        )),
    }
}

fn command_error_status(error: &CommandError) -> StatusCode {
    match error {
        CommandError::JavascriptError(_) | CommandError::UnsupportedResult(_) => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        CommandError::SnapshotFailed(_) | CommandError::Internal(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn command_channel_closed() -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "code": "internal-error",
            "message": "webview command channel is unavailable",
        })),
    )
}

fn unique_snapshot_stamp() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{millis}-{}", COUNTER.fetch_add(1, Ordering::SeqCst))
}

/// overlay 專用提交 endpoint。頁面沒有 control token（design 風險節：token 不
/// 暴露於頁面），以 loopback-only ＋ schema 驗證作為邊界。
async fn overlay_feedback(
    State(state): State<ControlState>,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    let mut incoming = match crate::feedback::validate(body.0) {
        Ok(incoming) => incoming,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "code": "invalid-request", "message": message })),
            );
        }
    };

    let _lifecycle = state.attachment_lifecycle.lock().await;
    if let Err(response) = expire_stale_attachments_locked(&state, epoch_secs()).await {
        return response;
    }
    let (has_connected, has_active) = {
        let attachments = state.attachments.lock().unwrap();
        (
            attachments
                .iter()
                .any(|entry| entry.attachment.collaboration_state.is_connected()),
            attachments
                .iter()
                .any(|entry| entry.attachment.collaboration_state.accepts_feedback()),
        )
    };
    if !has_active {
        let (code, message) = if has_connected {
            (
                "collaboration-paused",
                "collaboration is paused; resume before submitting feedback",
            )
        } else {
            (
                "collaboration-inactive",
                "collaboration has no active attachment; attach before submitting feedback",
            )
        };
        return (
            StatusCode::CONFLICT,
            Json(json!({ "code": code, "message": message })),
        );
    }

    if incoming.kind == "painting" {
        return overlay_painting_feedback(&state, incoming).await;
    }

    incoming.svg = None;
    let result = feedback_io(&state, move |root| crate::feedback::store(root, incoming)).await;
    match result {
        Ok(record) => {
            state.feedback_notify.notify_one();
            publish_feedback_mutation(&state).await;
            (
                StatusCode::OK,
                Json(json!({ "id": record.id, "state": record.state })),
            )
        }
        Err(e) => internal_error(format!("cannot persist feedback: {e}")),
    }
}

/// painting：附件（editable SVG ＋ native snapshot PNG）完成後才寫入 record
/// （design「Persistent feedback queue」：附件完成後才發布）。
async fn overlay_painting_feedback(
    state: &ControlState,
    mut incoming: crate::feedback::IncomingFeedback,
) -> (StatusCode, Json<Value>) {
    let svg_markup = incoming.svg.take().expect("validated painting has svg");
    let regions = std::mem::take(&mut incoming.capture_regions);
    let mut record = crate::feedback::prepare(incoming);

    let prepare_root = state.project_root.clone();
    let prepare_id = record.id.clone();
    let svg_path = match blocking_io::<PathBuf, io::Error, _>(move || {
        let dir =
            crate::session::prepare_private_subdir(&prepare_root, crate::feedback::FEEDBACK_DIR)?;
        let path = dir.join(format!("{prepare_id}.svg"));
        crate::session::write_artifact_bytes(&path, svg_markup.as_bytes())
    })
    .await
    {
        Ok(path) => path,
        Err(e) => return internal_error(format!("cannot write svg attachment: {e}")),
    };

    let dir = svg_path
        .parent()
        .expect("prepared SVG path has a parent")
        .to_path_buf();
    // `viewport.captureRegions[n]` 對應 `attachments[n]`；SVG 永遠是最後一個。
    let png_paths = (0..regions.len())
        .map(|index| dir.join(format!("{}-{index}.png", record.id)))
        .collect::<Vec<_>>();
    let (respond, receive) = tokio::sync::oneshot::channel();
    let command = WebviewCommand::CapturePainting {
        regions,
        output_paths: png_paths.clone(),
        respond,
    };
    match submit_command(state, command, receive).await {
        Ok(written) => {
            record.attachments = written
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .chain(std::iter::once(svg_path.to_string_lossy().into_owned()))
                .collect();
        }
        Err(response) => {
            // 不留下 partial artifact 當成功結果：本次建立的每一張 PNG 與 SVG 都清掉。
            remove_painting_artifacts(&png_paths, &svg_path).await;
            return response;
        }
    }

    let record_to_write = record.clone();
    let result = feedback_io(state, move |root| {
        crate::feedback::write_record(root, &record_to_write)
    })
    .await;
    match result {
        Ok(()) => {
            state.feedback_notify.notify_one();
            publish_feedback_mutation(state).await;
            (
                StatusCode::OK,
                Json(json!({
                "id": record.id,
                "state": record.state,
                "attachments": record.attachments,
                })),
            )
        }
        Err(e) => {
            remove_painting_artifacts(&png_paths, &svg_path).await;
            internal_error(format!("cannot persist feedback: {e}"))
        }
    }
}

/// design「Failure modes」：任一 capture、restoration 或 record write 失敗，
/// 都必須移除本次建立的全部 PNG 與 SVG，不得留下未發布的 artifact。
async fn remove_painting_artifacts(png_paths: &[PathBuf], svg_path: &FsPath) {
    let mut cleanup_paths = png_paths.to_vec();
    cleanup_paths.push(svg_path.to_path_buf());
    let cleanup_result = blocking_io::<(), io::Error, _>(move || {
        for path in cleanup_paths {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    })
    .await;
    if let Err(cleanup_error) = cleanup_result {
        eprintln!("cannot clean unpublished painting artifacts: {cleanup_error}");
    }
}

/// overlay 讀取非 terminal 的 feedback（reload reconciliation 用）。
async fn overlay_feedback_list(State(state): State<ControlState>) -> (StatusCode, Json<Value>) {
    let project_root = state.project_root.clone();
    match blocking_io::<_, crate::feedback::QueueError, _>(move || {
        crate::feedback::marker_records(&project_root, crate::dashboard::FEEDBACK_MARKER_LIMIT)
    })
    .await
    {
        Ok(records) => (
            StatusCode::OK,
            Json(json!({
                "revision": state.dashboard_revision.load(Ordering::Relaxed),
                "items": records,
            })),
        ),
        Err(error) => internal_error(format!("cannot list feedback: {error}")),
    }
}

/// reload reconciliation：只允許翻 `orphaned` 旗標，不允許改 lifecycle state。
async fn overlay_feedback_reconcile(
    State(state): State<ControlState>,
    body: Json<Value>,
) -> (StatusCode, Json<Value>) {
    let (Some(id), Some(orphaned)) = (
        body.0.get("id").and_then(Value::as_str),
        body.0.get("orphaned").and_then(Value::as_bool),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "code": "invalid-request",
                "message": "reconcile payload requires string `id` and boolean `orphaned`",
            })),
        );
    };
    if let Err(message) = crate::feedback::validate_feedback_id(id) {
        return invalid_feedback_id(message);
    }
    let feedback_id = id.to_string();
    let result = feedback_io(&state, move |root| {
        crate::feedback::set_orphaned(root, &feedback_id, orphaned)
    })
    .await;
    match result {
        Ok(record) => {
            publish_feedback_mutation(&state).await;
            (
                StatusCode::OK,
                Json(
                    json!({ "id": record.id, "orphaned": record.orphaned, "state": record.state }),
                ),
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "code": "feedback-not-found",
                "message": format!("no feedback item {id}"),
            })),
        ),
        Err(e) => internal_error(format!("cannot update feedback: {e}")),
    }
}

fn internal_error(message: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "code": "internal-error", "message": message })),
    )
}

fn invalid_request(message: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "code": "invalid-request", "message": message })),
    )
}

fn invalid_feedback_id(message: &'static str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "code": "invalid-feedback-id", "message": message })),
    )
}

enum AttachmentActivity {
    Active,
    Inactive,
    Missing,
}

enum AttachmentAccess {
    Active(watch::Receiver<bool>),
    Inactive,
    Missing,
}

async fn expire_stale_attachments(
    state: &ControlState,
) -> Result<usize, (StatusCode, Json<Value>)> {
    expire_stale_attachments_at(state, epoch_secs()).await
}

async fn expire_stale_attachments_at(
    state: &ControlState,
    now: u64,
) -> Result<usize, (StatusCode, Json<Value>)> {
    let _lifecycle = state.attachment_lifecycle.lock().await;
    expire_stale_attachments_locked(state, now).await
}

async fn expire_stale_attachments_locked(
    state: &ControlState,
    now: u64,
) -> Result<usize, (StatusCode, Json<Value>)> {
    let (active_ids, stale_ids) = {
        let attachments = state.attachments.lock().unwrap();
        let active_ids = attachments
            .iter()
            .filter(|attachment| attachment.attachment.collaboration_state.accepts_feedback())
            .map(|attachment| attachment.attachment.attachment_id.clone())
            .collect::<Vec<_>>();
        let stale_ids = attachments
            .iter()
            .filter(|attachment| {
                attachment.attachment.collaboration_state.is_connected()
                    && now.saturating_sub(attachment.attachment.last_heartbeat_epoch_secs)
                        > ATTACHMENT_EXPIRY.as_secs()
            })
            .map(|attachment| attachment.attachment.attachment_id.clone())
            .collect::<Vec<_>>();
        (active_ids, stale_ids)
    };
    if !active_ids.is_empty()
        && active_ids
            .iter()
            .all(|attachment_id| stale_ids.contains(attachment_id))
    {
        set_collaboration_active(state, false).await?;
    }
    if stale_ids.is_empty() {
        return Ok(0);
    }
    for stale_owner in &stale_ids {
        let owner = stale_owner.clone();
        feedback_io(state, move |root| {
            crate::feedback::release_owner_leases(root, &owner)
        })
        .await
        .map_err(|error: crate::feedback::QueueError| {
            internal_error(format!("cannot release feedback leases: {error}"))
        })?;
    }
    {
        let mut attachments = state.attachments.lock().unwrap();
        for attachment in attachments
            .iter_mut()
            .filter(|attachment| stale_ids.contains(&attachment.attachment.attachment_id))
        {
            attachment
                .attachment
                .set_collaboration_state(CollaborationState::Inactive);
            let _ = attachment.stop_tx.send(true);
        }
    }
    state.lifecycle_notify.notify_waiters();
    publish_feedback_mutation(state).await;
    Ok(stale_ids.len())
}

async fn heartbeat_attachment(
    state: &ControlState,
    attachment_id: &str,
) -> Result<AttachmentActivity, (StatusCode, Json<Value>)> {
    let _lifecycle = state.attachment_lifecycle.lock().await;
    let now = epoch_secs();
    expire_stale_attachments_locked(state, now).await?;
    let activity = state
        .attachments
        .lock()
        .unwrap()
        .iter_mut()
        .find(|attachment| attachment.attachment.attachment_id == attachment_id)
        .map_or(AttachmentActivity::Missing, |attachment| {
            if attachment.attachment.collaboration_state.is_connected() {
                attachment.attachment.last_heartbeat_epoch_secs = now;
                AttachmentActivity::Active
            } else {
                AttachmentActivity::Inactive
            }
        });
    if matches!(
        attachment_collaboration_state(state, attachment_id),
        Some(CollaborationState::PauseRequested)
    ) {
        let owner = attachment_id.to_string();
        let has_current_lease = feedback_io(state, move |root| {
            crate::feedback::owner_has_live_lease(root, &owner)
        })
        .await
        .map_err(feedback_error)?;
        if !has_current_lease {
            {
                if let Some(attachment) = state
                    .attachments
                    .lock()
                    .unwrap()
                    .iter_mut()
                    .find(|entry| entry.attachment.attachment_id == attachment_id)
                {
                    attachment
                        .attachment
                        .set_collaboration_state(CollaborationState::Paused);
                }
            }
            state.lifecycle_notify.notify_waiters();
            publish_dashboard_snapshot(state, DashboardRuntimeState::Running).await;
        }
    }
    Ok(activity)
}

fn attachment_collaboration_state(
    state: &ControlState,
    attachment_id: &str,
) -> Option<CollaborationState> {
    state
        .attachments
        .lock()
        .unwrap()
        .iter()
        .find(|entry| entry.attachment.attachment_id == attachment_id)
        .map(|entry| entry.attachment.collaboration_state)
}

fn attachment_stop_receiver(state: &ControlState, attachment_id: &str) -> AttachmentAccess {
    state
        .attachments
        .lock()
        .unwrap()
        .iter()
        .find(|attachment| attachment.attachment.attachment_id == attachment_id)
        .map_or(AttachmentAccess::Missing, |attachment| {
            if attachment.attachment.collaboration_state.is_connected() {
                AttachmentAccess::Active(attachment.stop_tx.subscribe())
            } else {
                AttachmentAccess::Inactive
            }
        })
}

fn select_connected_attachment(
    attachments: &[AttachmentState],
    requested_id: Option<String>,
) -> Result<Option<usize>, (StatusCode, Json<Value>)> {
    if let Some(attachment_id) = requested_id {
        let Some(index) = attachments
            .iter()
            .position(|entry| entry.attachment.attachment_id == attachment_id)
        else {
            return Err(attachment_not_found(&attachment_id));
        };
        if !attachments[index]
            .attachment
            .collaboration_state
            .is_connected()
        {
            return Err(attachment_inactive(&attachment_id));
        }
        return Ok(Some(index));
    }

    let connected = attachments
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.attachment.collaboration_state.is_connected())
        .map(|(index, entry)| (index, entry.attachment.attachment_id.clone()))
        .collect::<Vec<_>>();
    match connected.as_slice() {
        [] => Ok(None),
        [(index, _)] => Ok(Some(*index)),
        _ => Err((
            StatusCode::CONFLICT,
            Json(json!({
                "code": "ambiguous-attachment",
                "message": "multiple collaboration attachments exist; select one explicitly",
                "details": {
                    "candidateAttachmentIds": connected
                        .into_iter()
                        .map(|(_, attachment_id)| attachment_id)
                        .collect::<Vec<_>>(),
                },
            })),
        )),
    }
}

async fn pause_attachment(
    state: &ControlState,
    request: CollaborationControlRequest,
) -> (StatusCode, Json<Value>) {
    let _lifecycle = state.attachment_lifecycle.lock().await;
    if let Err(response) = expire_stale_attachments_locked(state, epoch_secs()).await {
        return response;
    }
    let selected = {
        let attachments = state.attachments.lock().unwrap();
        match select_connected_attachment(&attachments, request.attachment_id) {
            Ok(selected) => selected,
            Err(response) => return response,
        }
    };
    let Some(selected) = selected else {
        return collaboration_control_result(CollaborationControlResult {
            status: "already-paused".into(),
            attachment_id: None,
            collaboration_state: CollaborationState::Paused,
        });
    };
    let (attachment_id, current_state, deactivate) = {
        let attachments = state.attachments.lock().unwrap();
        let selected = &attachments[selected].attachment;
        (
            selected.attachment_id.clone(),
            selected.collaboration_state,
            selected.collaboration_state.accepts_feedback()
                && attachments
                    .iter()
                    .filter(|entry| entry.attachment.collaboration_state.accepts_feedback())
                    .count()
                    == 1,
        )
    };
    match current_state {
        CollaborationState::Paused => {
            return collaboration_control_result(CollaborationControlResult {
                status: "already-paused".into(),
                attachment_id: Some(attachment_id),
                collaboration_state: CollaborationState::Paused,
            });
        }
        CollaborationState::PauseRequested => {
            return collaboration_control_result(CollaborationControlResult {
                status: "pause-requested".into(),
                attachment_id: Some(attachment_id),
                collaboration_state: CollaborationState::PauseRequested,
            });
        }
        CollaborationState::Inactive => return attachment_inactive(&attachment_id),
        CollaborationState::Active => {}
    }
    let owner = attachment_id.clone();
    let has_current_lease = match feedback_io(state, move |root| {
        crate::feedback::owner_has_live_lease(root, &owner)
    })
    .await
    {
        Ok(has_lease) => has_lease,
        Err(error) => return feedback_error(error),
    };
    if deactivate && let Err(response) = set_collaboration_active(state, false).await {
        return response;
    }
    let collaboration_state = if has_current_lease {
        CollaborationState::PauseRequested
    } else {
        CollaborationState::Paused
    };
    {
        let mut attachments = state.attachments.lock().unwrap();
        let selected = attachments
            .iter_mut()
            .find(|entry| entry.attachment.attachment_id == attachment_id)
            .expect("attachment lifecycle lock preserves selected attachment");
        selected
            .attachment
            .set_collaboration_state(collaboration_state);
    }
    state.lifecycle_notify.notify_waiters();
    publish_dashboard_snapshot(state, DashboardRuntimeState::Running).await;
    collaboration_control_result(CollaborationControlResult {
        status: match collaboration_state {
            CollaborationState::PauseRequested => "pause-requested",
            CollaborationState::Paused => "paused",
            _ => unreachable!(),
        }
        .into(),
        attachment_id: Some(attachment_id),
        collaboration_state,
    })
}

async fn resume_attachment(
    state: &ControlState,
    request: CollaborationControlRequest,
) -> (StatusCode, Json<Value>) {
    let _lifecycle = state.attachment_lifecycle.lock().await;
    if let Err(response) = expire_stale_attachments_locked(state, epoch_secs()).await {
        return response;
    }
    let selected = {
        let attachments = state.attachments.lock().unwrap();
        match select_connected_attachment(&attachments, request.attachment_id) {
            Ok(selected) => selected,
            Err(response) => return response,
        }
    };
    let Some(selected) = selected else {
        return collaboration_control_result(CollaborationControlResult {
            status: "already-active".into(),
            attachment_id: None,
            collaboration_state: CollaborationState::Active,
        });
    };
    let (attachment_id, current_state, activate) = {
        let attachments = state.attachments.lock().unwrap();
        let selected = &attachments[selected].attachment;
        (
            selected.attachment_id.clone(),
            selected.collaboration_state,
            !attachments
                .iter()
                .any(|entry| entry.attachment.collaboration_state.accepts_feedback()),
        )
    };
    match current_state {
        CollaborationState::Active => {
            return collaboration_control_result(CollaborationControlResult {
                status: "already-active".into(),
                attachment_id: Some(attachment_id),
                collaboration_state: CollaborationState::Active,
            });
        }
        CollaborationState::PauseRequested => {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "code": "pause-pending",
                    "message": "current feedback must reach resolved or failed before resume",
                })),
            );
        }
        CollaborationState::Inactive => return attachment_inactive(&attachment_id),
        CollaborationState::Paused => {}
    }
    if activate && let Err(response) = set_collaboration_active(state, true).await {
        return response;
    }
    {
        let mut attachments = state.attachments.lock().unwrap();
        let selected = attachments
            .iter_mut()
            .find(|entry| entry.attachment.attachment_id == attachment_id)
            .expect("attachment lifecycle lock preserves selected attachment");
        selected
            .attachment
            .set_collaboration_state(CollaborationState::Active);
    }
    state.lifecycle_notify.notify_waiters();
    state.feedback_notify.notify_waiters();
    publish_dashboard_snapshot(state, DashboardRuntimeState::Running).await;
    collaboration_control_result(CollaborationControlResult {
        status: "resumed".into(),
        attachment_id: Some(attachment_id),
        collaboration_state: CollaborationState::Active,
    })
}

async fn detach_attachment(
    state: &ControlState,
    request: DetachRequest,
) -> (StatusCode, Json<Value>) {
    let _lifecycle = state.attachment_lifecycle.lock().await;
    if let Err(response) = expire_stale_attachments_locked(state, epoch_secs()).await {
        return response;
    }
    let (attachment_id, deactivate) = {
        let attachments = state.attachments.lock().unwrap();
        let selected = match request.attachment_id {
            Some(attachment_id) => {
                let Some(index) = attachments
                    .iter()
                    .position(|attachment| attachment.attachment.attachment_id == attachment_id)
                else {
                    return attachment_not_found(&attachment_id);
                };
                index
            }
            None => {
                let active = attachments
                    .iter()
                    .enumerate()
                    .filter(|(_, attachment)| {
                        attachment.attachment.collaboration_state.is_connected()
                    })
                    .map(|(index, attachment)| (index, attachment.attachment.attachment_id.clone()))
                    .collect::<Vec<_>>();
                match active.as_slice() {
                    [] => {
                        return detach_result(DetachResult {
                            status: "already-detached".into(),
                            attachment_id: None,
                            active_attachment_count: 0,
                        });
                    }
                    [(index, _)] => *index,
                    _ => {
                        return (
                            StatusCode::CONFLICT,
                            Json(json!({
                                "code": "ambiguous-attachment",
                                "message": "multiple active attachments exist; select one explicitly",
                                "details": {
                                    "candidateAttachmentIds": active
                                        .into_iter()
                                        .map(|(_, attachment_id)| attachment_id)
                                        .collect::<Vec<_>>(),
                                },
                            })),
                        );
                    }
                }
            }
        };

        let attachment_id = attachments[selected].attachment.attachment_id.clone();
        if !attachments[selected]
            .attachment
            .collaboration_state
            .is_connected()
        {
            let active_attachment_count = attachments
                .iter()
                .filter(|attachment| attachment.attachment.active)
                .count();
            return detach_result(DetachResult {
                status: "already-detached".into(),
                attachment_id: Some(attachment_id),
                active_attachment_count,
            });
        }
        let deactivate = attachments[selected]
            .attachment
            .collaboration_state
            .accepts_feedback()
            && attachments
                .iter()
                .filter(|attachment| attachment.attachment.collaboration_state.accepts_feedback())
                .count()
                == 1;
        (attachment_id, deactivate)
    };
    if deactivate && let Err(response) = set_collaboration_active(state, false).await {
        return response;
    }
    let owner = attachment_id.clone();
    if let Err(error) = feedback_io(state, move |root| {
        crate::feedback::release_owner_leases(root, &owner)
    })
    .await
    {
        return feedback_error(error);
    }
    let active_attachment_count = {
        let mut attachments = state.attachments.lock().unwrap();
        let selected = attachments
            .iter()
            .position(|attachment| attachment.attachment.attachment_id == attachment_id)
            .expect("attachment lifecycle lock preserves selected attachment");
        attachments[selected]
            .attachment
            .set_collaboration_state(CollaborationState::Inactive);
        let _ = attachments[selected].stop_tx.send(true);
        attachments
            .iter()
            .filter(|attachment| attachment.attachment.active)
            .count()
    };
    state.lifecycle_notify.notify_waiters();
    publish_feedback_mutation(state).await;
    detach_result(DetachResult {
        status: "detached".into(),
        attachment_id: Some(attachment_id),
        active_attachment_count,
    })
}

/// spec「Native Offline Paint command is lifecycle-gated」：eligibility 檢查與
/// attachment lifecycle 變更共用同一把鎖，因此 attach 與 toggle 只有一種可觀察順序；
/// 先取得鎖的一方決定結果，成功的 attach 一定會關閉並清除離線 Paint。
async fn toggle_offline_paint(state: &ControlState) -> Result<(), (StatusCode, Json<Value>)> {
    let _lifecycle = state.attachment_lifecycle.lock().await;
    if *state.shutdown_tx.borrow() {
        return Err(offline_paint_unavailable());
    }
    let connected = {
        let attachments = state.attachments.lock().unwrap();
        attachments
            .iter()
            .any(|attachment| attachment.attachment.collaboration_state.is_connected())
    };
    if connected {
        return Err(offline_paint_unavailable());
    }
    let (respond, receive) = tokio::sync::oneshot::channel();
    submit_command(
        state,
        WebviewCommand::ToggleOfflinePaint { respond },
        receive,
    )
    .await
}

fn offline_paint_unavailable() -> (StatusCode, Json<Value>) {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "code": "offline-paint-unavailable",
            "message": "Offline Paint is available only while no agent is connected",
        })),
    )
}

async fn set_collaboration_active(
    state: &ControlState,
    active: bool,
) -> Result<(), (StatusCode, Json<Value>)> {
    let (respond, receive) = tokio::sync::oneshot::channel();
    submit_command(
        state,
        WebviewCommand::SetCollaborationActive { active, respond },
        receive,
    )
    .await
}

fn detach_result(result: DetachResult) -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(serde_json::to_value(result).expect("detach result is serializable")),
    )
}

fn collaboration_control_result(result: CollaborationControlResult) -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(serde_json::to_value(result).expect("collaboration control result is serializable")),
    )
}

async fn close_all_attachments(state: &ControlState) -> Result<(), (StatusCode, Json<Value>)> {
    let _lifecycle = state.attachment_lifecycle.lock().await;
    let (has_active, connected_ids) = {
        let attachments = state.attachments.lock().unwrap();
        let has_active = attachments
            .iter()
            .any(|attachment| attachment.attachment.collaboration_state.accepts_feedback());
        let connected_ids = attachments
            .iter()
            .filter(|attachment| attachment.attachment.collaboration_state.is_connected())
            .map(|attachment| attachment.attachment.attachment_id.clone())
            .collect::<Vec<_>>();
        (has_active, connected_ids)
    };
    for connected_owner in connected_ids {
        feedback_io(state, move |root| {
            crate::feedback::release_owner_leases(root, &connected_owner)
        })
        .await
        .map_err(feedback_error)?;
    }
    if has_active {
        set_collaboration_active(state, false).await?;
    }
    let mut attachments = state.attachments.lock().unwrap();
    for attachment in attachments.iter_mut() {
        if attachment.attachment.collaboration_state.is_connected() {
            attachment
                .attachment
                .set_collaboration_state(CollaborationState::Inactive);
            let _ = attachment.stop_tx.send(true);
        }
    }
    drop(attachments);
    state.lifecycle_notify.notify_waiters();
    Ok(())
}

fn collaboration_stop() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({ "event": "collaboration.stop" })),
    )
}

fn attachment_inactive(attachment_id: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "code": "attachment-inactive",
            "message": format!("attachment {attachment_id} is no longer active"),
        })),
    )
}

fn attachment_not_found(attachment_id: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "code": "attachment-not-found",
            "message": format!("attachment {attachment_id} is not registered with this preview"),
        })),
    )
}

fn feedback_error(error: crate::feedback::QueueError) -> (StatusCode, Json<Value>) {
    use crate::feedback::QueueError;

    match error {
        QueueError::NotFound(id) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "code": "feedback-not-found",
                "message": format!("no feedback item {id}"),
            })),
        ),
        QueueError::CompareMismatch { expected, actual } => (
            StatusCode::CONFLICT,
            Json(json!({
                "code": "state-conflict",
                "message": format!("expected state {expected}, but current state is {actual}"),
                "details": { "expected": expected, "actual": actual },
            })),
        ),
        QueueError::InvalidTransition { from, to } => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "code": "invalid-feedback-transition",
                "message": format!("invalid feedback state transition {from} -> {to}"),
                "details": { "from": from, "to": to },
            })),
        ),
        QueueError::MissingFailureReason => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "code": "missing-failure-reason",
                "message": "failed feedback requires a non-empty reason",
            })),
        ),
        QueueError::LeaseRequired => (
            StatusCode::CONFLICT,
            Json(json!({
                "code": "lease-required",
                "message": "feedback item has no active lease",
            })),
        ),
        QueueError::LeaseOwnerMismatch { expected, actual } => (
            StatusCode::CONFLICT,
            Json(json!({
                "code": "lease-conflict",
                "message": format!("feedback lease belongs to {actual}, not {expected}"),
                "details": { "expected": expected, "actual": actual },
            })),
        ),
        QueueError::Io(error) => {
            internal_error(format!("feedback queue operation failed: {error}"))
        }
        QueueError::Storage { path, message } => internal_error(format!(
            "feedback storage operation failed at {}: {message}",
            path.display()
        )),
    }
}

fn authorized(state: &ControlState, headers: &HeaderMap) -> bool {
    let expected = format!("Bearer {}", state.token);
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|presented| constant_time_eq(presented.as_bytes(), expected.as_bytes()))
}

fn constant_time_eq(presented: &[u8], expected: &[u8]) -> bool {
    if presented.len() != expected.len() {
        return false;
    }

    // 累積所有 byte 的差異，避免 token mismatch 位置影響提早返回時機。
    presented
        .iter()
        .zip(expected)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn unauthorized() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "code": "unauthorized",
            "message": "missing or invalid preview control token"
        })),
    )
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feedback_test_state(
        name: &str,
    ) -> (
        ControlState,
        crate::feedback::FeedbackRecord,
        crate::webview::CommandReceiver,
    ) {
        let root = std::env::temp_dir().join(format!(
            "collab-server-feedback-lock-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let record = crate::feedback::store(
            &root,
            crate::feedback::validate(json!({
                "kind": "textbox",
                "text": "serialize me",
                "pageUrl": "http://127.0.0.1/",
                "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
            }))
            .unwrap(),
        )
        .unwrap();
        let (shutdown_tx, _) = watch::channel(false);
        let (commands, commands_rx) = crate::webview::command_channel();
        let initial_dashboard = crate::dashboard::build_snapshot(
            0,
            DashboardRuntimeState::Running,
            "test-session",
            &[],
            &[],
            None,
            None,
        );
        let (dashboard, _dashboard_handle, _dashboard_actions) =
            crate::dashboard::channel(initial_dashboard);
        let (draft_states, _) = watch::channel(crate::draft_panel::DraftPanelState::default());
        let (stop_tx, _) = watch::channel(false);
        let attachment = AttachmentState {
            attachment: Attachment {
                attachment_id: "attachment-a".into(),
                agent_kind: "test".into(),
                tui_session_id: None,
                pid: std::process::id(),
                attached_at_epoch_secs: epoch_secs(),
                last_heartbeat_epoch_secs: epoch_secs(),
                collaboration_state: CollaborationState::Active,
                active: true,
            },
            stop_tx,
        };
        (
            ControlState {
                token: Arc::new("test-token".into()),
                session_id: Arc::new("test-session".into()),
                project_root: Arc::new(root),
                shutdown_tx,
                attachments: Arc::new(Mutex::new(vec![attachment])),
                attachment_lifecycle: Arc::new(tokio::sync::Mutex::new(())),
                feedback_lock: Arc::new(tokio::sync::Mutex::new(())),
                feedback_notify: Arc::new(Notify::new()),
                lifecycle_notify: Arc::new(Notify::new()),
                commands,
                dashboard,
                dashboard_revision: Arc::new(AtomicU64::new(0)),
                draft_states,
            },
            record,
            commands_rx,
        )
    }

    #[test]
    fn concurrent_reconcile_and_state_change_preserve_both_updates() {
        let (state, record, mut commands) = feedback_test_state("reconcile");
        let command_worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async move {
                for _ in 0..2 {
                    let command = commands.recv().await.unwrap();
                    let WebviewCommand::SyncFeedbackMarkers { respond, .. } = command else {
                        panic!("expected marker sync after persisted feedback mutation");
                    };
                    let _ = respond.send(Ok(()));
                }
            });
        });
        crate::feedback::lease_next(&state.project_root, "attachment-a")
            .unwrap()
            .unwrap();
        let guard = state.feedback_lock.clone().blocking_lock_owned();
        let reconcile_state = state.clone();
        let state_change_state = state.clone();
        let reconcile_id = record.id.clone();
        let feedback_id = record.id.clone();
        let final_id = record.id.clone();
        let (reconcile_tx, reconcile_rx) = std::sync::mpsc::channel();
        let reconcile_worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let response = runtime.block_on(overlay_feedback_reconcile(
                State(reconcile_state),
                Json(json!({"id": reconcile_id, "orphaned": true})),
            ));
            reconcile_tx.send(response.0).unwrap();
        });
        let (state_change_tx, state_change_rx) = std::sync::mpsc::channel();
        let state_change_worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let mut headers = HeaderMap::new();
            headers.insert(header::AUTHORIZATION, "Bearer test-token".parse().unwrap());
            let response = runtime.block_on(control_feedback_set_state(
                State(state_change_state),
                Path(feedback_id),
                headers,
                Json(json!({
                    "attachmentId": "attachment-a",
                    "expectedState": "pending",
                    "state": "acknowledged",
                })),
            ));
            state_change_tx.send(response.0).unwrap();
        });

        assert!(matches!(
            reconcile_rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(matches!(
            state_change_rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(guard);
        assert_eq!(
            reconcile_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            StatusCode::OK
        );
        assert_eq!(
            state_change_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            StatusCode::OK
        );
        reconcile_worker.join().unwrap();
        state_change_worker.join().unwrap();
        command_worker.join().unwrap();

        let final_record = crate::feedback::read_record(&state.project_root, &final_id).unwrap();
        assert!(final_record.orphaned);
        assert_eq!(final_record.state, "acknowledged");
    }

    #[test]
    fn webview_completion_wait_returns_timeout_error() {
        let (state, _record, _commands) = feedback_test_state("webview-timeout");
        let (respond, receive) = tokio::sync::oneshot::channel();
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let (status, Json(body)) = runtime
            .block_on(submit_command_with_timeout(
                &state,
                WebviewCommand::Reload { respond },
                receive,
                std::time::Duration::from_millis(25),
            ))
            .unwrap_err();

        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(body["code"], "timeout");
    }

    #[tokio::test]
    async fn stale_attachment_becomes_inactive_and_releases_capacity() {
        let (state, _record, _commands) = feedback_test_state("attachment-expiry");
        let now = epoch_secs();
        {
            let mut attachments = state.attachments.lock().unwrap();
            attachments[0].attachment.last_heartbeat_epoch_secs =
                now - ATTACHMENT_EXPIRY.as_secs() - 1;
            for index in 1..ATTACHMENT_CAPACITY {
                let (stop_tx, _) = watch::channel(false);
                attachments.push(AttachmentState {
                    attachment: Attachment {
                        attachment_id: format!("attachment-{index}"),
                        agent_kind: "test".into(),
                        tui_session_id: None,
                        pid: std::process::id(),
                        attached_at_epoch_secs: now,
                        last_heartbeat_epoch_secs: now,
                        collaboration_state: CollaborationState::Active,
                        active: true,
                    },
                    stop_tx,
                });
            }
        }

        assert_eq!(expire_stale_attachments_at(&state, now).await.unwrap(), 1);
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer test-token".parse().unwrap());
        let (status, _) = control_attach(
            State(state.clone()),
            headers,
            Json(json!({"agentKind": "replacement", "pid": std::process::id()})),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let attachments = state.attachments.lock().unwrap();
        assert_eq!(attachments.len(), ATTACHMENT_CAPACITY);
        assert!(attachments.iter().all(|entry| entry.attachment.active));
        assert!(
            attachments
                .iter()
                .all(|entry| entry.attachment.attachment_id != "attachment-a")
        );
    }

    #[tokio::test]
    async fn overlay_feedback_expires_stale_attachment_before_admission() {
        let (state, existing_record, mut commands) = feedback_test_state("overlay-stale-gate");
        std::fs::remove_file(
            crate::feedback::feedback_dir(&state.project_root)
                .join(format!("{}.json", existing_record.id)),
        )
        .unwrap();
        state.attachments.lock().unwrap()[0]
            .attachment
            .last_heartbeat_epoch_secs = epoch_secs() - ATTACHMENT_EXPIRY.as_secs() - 1;
        let command_worker = tokio::spawn(async move {
            while let Some(command) = commands.recv().await {
                match command {
                    WebviewCommand::SetCollaborationActive { respond, .. }
                    | WebviewCommand::SyncFeedbackMarkers { respond, .. } => {
                        let _ = respond.send(Ok(()));
                    }
                    _ => panic!("unexpected command while expiring stale attachment"),
                }
            }
        });

        let (status, Json(body)) = overlay_feedback(
            State(state.clone()),
            Json(json!({
                "kind": "textbox",
                "text": "stale attachment must not admit this",
                "pageUrl": "http://127.0.0.1/",
                "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
            })),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "collaboration-inactive");
        assert!(
            crate::feedback::list_records(&state.project_root)
                .unwrap()
                .is_empty()
        );
        drop(state);
        command_worker.abort();
    }

    #[tokio::test]
    async fn overlay_feedback_submission_holds_lifecycle_until_painting_is_published() {
        let (state, existing_record, mut commands) = feedback_test_state("overlay-submit-first");
        std::fs::remove_file(
            crate::feedback::feedback_dir(&state.project_root)
                .join(format!("{}.json", existing_record.id)),
        )
        .unwrap();
        let (snapshot_seen_tx, snapshot_seen_rx) = tokio::sync::oneshot::channel();
        let (publish_tx, publish_rx) = tokio::sync::oneshot::channel();
        let command_worker = tokio::spawn(async move {
            let mut snapshot_seen_tx = Some(snapshot_seen_tx);
            let mut publish_rx = Some(publish_rx);
            while let Some(command) = commands.recv().await {
                match command {
                    WebviewCommand::CapturePainting {
                        output_paths,
                        respond,
                        ..
                    } => {
                        for path in &output_paths {
                            std::fs::write(path, b"\x89PNG-stub").unwrap();
                        }
                        snapshot_seen_tx.take().unwrap().send(()).unwrap();
                        publish_rx.take().unwrap().await.unwrap();
                        respond.send(Ok(output_paths)).unwrap();
                    }
                    WebviewCommand::SetCollaborationActive { respond, .. }
                    | WebviewCommand::SyncFeedbackMarkers { respond, .. } => {
                        let _ = respond.send(Ok(()));
                    }
                    _ => panic!("unexpected command during submission-first ordering test"),
                }
            }
        });
        let submission_state = state.clone();
        let submission = tokio::spawn(async move {
            overlay_feedback(
                State(submission_state),
                Json(json!({
                    "kind": "painting",
                    "text": "publish before detach",
                    "pageUrl": "http://127.0.0.1/",
                    "viewport": {
                        "width": 800, "height": 600, "scrollX": 0, "scrollY": 0,
                        "documentWidth": 800, "documentHeight": 1200,
                        "captureRegions": [{"x": 0, "y": 0, "width": 800, "height": 600}],
                    },
                    "elements": [],
                    "marks": [{"type": "line", "x": 1, "y": 2}],
                    "svg": "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
                })),
            )
            .await
        });

        snapshot_seen_rx.await.unwrap();
        assert!(
            state.attachment_lifecycle.try_lock().is_err(),
            "submission must retain the lifecycle boundary while snapshot publication is pending"
        );
        let detach_state = state.clone();
        let detach = tokio::spawn(async move {
            detach_attachment(
                &detach_state,
                DetachRequest {
                    attachment_id: Some("attachment-a".into()),
                },
            )
            .await
        });
        publish_tx.send(()).unwrap();

        assert_eq!(submission.await.unwrap().0, StatusCode::OK);
        assert_eq!(detach.await.unwrap().0, StatusCode::OK);
        assert_eq!(
            crate::feedback::list_records(&state.project_root)
                .unwrap()
                .len(),
            1
        );
        command_worker.abort();
    }

    #[tokio::test]
    async fn overlay_feedback_rejects_when_detach_holds_lifecycle_first() {
        let (state, existing_record, mut commands) = feedback_test_state("overlay-detach-first");
        std::fs::remove_file(
            crate::feedback::feedback_dir(&state.project_root)
                .join(format!("{}.json", existing_record.id)),
        )
        .unwrap();
        let (detach_started_tx, detach_started_rx) = tokio::sync::oneshot::channel();
        let (finish_detach_tx, finish_detach_rx) = tokio::sync::oneshot::channel();
        let command_worker = tokio::spawn(async move {
            let mut detach_started_tx = Some(detach_started_tx);
            let mut finish_detach_rx = Some(finish_detach_rx);
            while let Some(command) = commands.recv().await {
                match command {
                    WebviewCommand::SetCollaborationActive {
                        active: false,
                        respond,
                    } => {
                        detach_started_tx.take().unwrap().send(()).unwrap();
                        finish_detach_rx.take().unwrap().await.unwrap();
                        respond.send(Ok(())).unwrap();
                    }
                    WebviewCommand::SyncFeedbackMarkers { respond, .. } => {
                        let _ = respond.send(Ok(()));
                    }
                    _ => panic!("unexpected command during detach-first ordering test"),
                }
            }
        });
        let detach_state = state.clone();
        let detach = tokio::spawn(async move {
            detach_attachment(
                &detach_state,
                DetachRequest {
                    attachment_id: Some("attachment-a".into()),
                },
            )
            .await
        });
        detach_started_rx.await.unwrap();
        let submission_state = state.clone();
        let submission = tokio::spawn(async move {
            overlay_feedback(
                State(submission_state),
                Json(json!({
                    "kind": "textbox",
                    "text": "detach must win",
                    "pageUrl": "http://127.0.0.1/",
                    "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
                })),
            )
            .await
        });
        finish_detach_tx.send(()).unwrap();

        assert_eq!(detach.await.unwrap().0, StatusCode::OK);
        let (status, Json(body)) = submission.await.unwrap();
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "collaboration-inactive");
        assert!(
            crate::feedback::list_records(&state.project_root)
                .unwrap()
                .is_empty()
        );
        command_worker.abort();
    }

    #[test]
    fn heartbeat_refreshes_attachment_and_renews_its_lease() {
        let (state, record, _commands) = feedback_test_state("attachment-heartbeat");
        let now_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let leased = crate::feedback::lease_next_at(
            &state.project_root,
            "attachment-a",
            now_millis,
            Duration::from_secs(60),
        )
        .unwrap()
        .unwrap();
        let previous_expiry = leased.lease.unwrap().expires_at;
        state.attachments.lock().unwrap()[0]
            .attachment
            .last_heartbeat_epoch_secs = epoch_secs() - 10;
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Bearer test-token".parse().unwrap());

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (status, Json(body)) = runtime.block_on(control_heartbeat(
            State(state.clone()),
            headers,
            Json(json!({"attachmentId": "attachment-a"})),
        ));

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["renewedLeases"], 1);
        let heartbeat_at = state.attachments.lock().unwrap()[0]
            .attachment
            .last_heartbeat_epoch_secs;
        assert!(epoch_secs().saturating_sub(heartbeat_at) <= 1);
        let renewed = crate::feedback::read_record(&state.project_root, &record.id).unwrap();
        assert!(renewed.lease.unwrap().expires_at > previous_expiry);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn serialized_feedback_io_waits_without_blocking_async_executor() {
        let (state, _record, _commands) = feedback_test_state("nonblocking-feedback-io");
        let guard = state.feedback_lock.clone().lock_owned().await;
        let worker_state = state.clone();
        let worker = tokio::spawn(async move {
            feedback_io::<(), crate::feedback::QueueError, _>(&worker_state, |_| Ok(())).await
        });

        tokio::time::timeout(
            Duration::from_millis(100),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .expect("waiting for serialized feedback I/O blocked the async executor");
        drop(guard);
        worker.await.unwrap().unwrap();
    }

    #[test]
    fn constant_time_token_comparison_accepts_identical_tokens() {
        assert!(constant_time_eq(
            b"Bearer control-token",
            b"Bearer control-token"
        ));
    }

    #[test]
    fn constant_time_token_comparison_rejects_different_lengths() {
        assert!(!constant_time_eq(
            b"Bearer control-token-extra",
            b"Bearer control-token"
        ));
    }

    #[test]
    fn constant_time_token_comparison_rejects_last_byte_difference() {
        assert!(!constant_time_eq(
            b"Bearer control-tokem",
            b"Bearer control-token"
        ));
    }

    #[test]
    fn unauthorized_response_shape_is_stable() {
        let (status, Json(body)) = unauthorized();

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body,
            json!({
                "code": "unauthorized",
                "message": "missing or invalid preview control token"
            })
        );
    }

    #[tokio::test]
    async fn stale_expiration_releases_unexpired_feedback_lease() {
        let (state, _record, mut commands) = feedback_test_state("stale-lease-release");
        let attachment_id = "attachment-a";

        let now_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let leased = crate::feedback::lease_next_at(
            &state.project_root,
            attachment_id,
            now_millis,
            crate::feedback::DEFAULT_LEASE_DURATION,
        )
        .unwrap()
        .unwrap();
        let feedback_id = leased.id.clone();
        let lease_expiry = leased.lease.as_ref().unwrap().expires_at;
        crate::feedback::transition(
            &state.project_root,
            &feedback_id,
            "pending",
            "acknowledged",
            attachment_id,
            None,
        )
        .unwrap();
        crate::feedback::transition(
            &state.project_root,
            &feedback_id,
            "acknowledged",
            "working",
            attachment_id,
            None,
        )
        .unwrap();

        let stale_at = epoch_secs() + ATTACHMENT_EXPIRY.as_secs() + 1;
        {
            let mut attachments = state.attachments.lock().unwrap();
            attachments[0].attachment.last_heartbeat_epoch_secs =
                stale_at - ATTACHMENT_EXPIRY.as_secs() - 1;
        }

        let port = tokio::spawn({
            let state = state.clone();
            async move {
                expire_stale_attachments_at(&state, stale_at).await.unwrap();
            }
        });
        let cmd = commands.recv().await.unwrap();
        if let crate::webview::WebviewCommand::SetCollaborationActive { respond, .. } = cmd {
            respond.send(Ok(())).unwrap();
        }
        port.await.unwrap();

        let after = crate::feedback::read_record(&state.project_root, &feedback_id).unwrap();
        assert_eq!(
            after.state, "pending",
            "stale attachment must release feedback lease (lease expiry {lease_expiry} still far future)"
        );
        assert!(after.lease.is_none());
        let recovery = after
            .recovery
            .as_ref()
            .expect("recovery metadata must exist");
        assert_eq!(recovery.previous_owner.as_deref(), Some(attachment_id));
    }
}
