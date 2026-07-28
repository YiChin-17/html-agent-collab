//! CLI 端的 session discovery 與 control service client。
//! spec「Common agent CLI contract」：Claude Code 與 Codex 共用同一套
//! discovery 規則、JSON envelope 與 error codes。

use std::path::Path;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::core::{
    AttachResult, CloseResult, CollaborationControlResult, CoreAdapter, CoreRequest, CoreResult,
    DetachResult, EvalResult, FeedbackResult, ScreenshotResult, StatusResult, WaitResult,
};
pub use crate::core::{Envelope, OpError};
use crate::session::{self, SessionFile};

pub const CONTROL_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
pub const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
pub const CONTROL_WAIT_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
pub const ATTACHMENT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

/// 從 `start` 往上走 ancestor 目錄，收集所有 process 仍存活的 preview session。
/// 巢狀 project root 各自有 preview 時會回傳多筆（→ `ambiguous-preview`）。
pub fn discover_live_sessions(start: &Path) -> Vec<SessionFile> {
    let mut found = Vec::new();
    let start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut current: Option<&Path> = Some(&start);
    while let Some(dir) = current {
        if let Ok(session) = session::read_session_file(dir)
            && process_alive(session.pid)
            && session_is_healthy(&session)
        {
            found.push(session);
        }
        current = dir.parent();
    }
    found
}

pub(crate) fn session_is_healthy(session: &SessionFile) -> bool {
    let client = match ControlClient::new(session.clone()) {
        Ok(client) => client,
        Err(_) => return false,
    };
    let health = match client.get("/health") {
        Ok(health) => health,
        Err(_) => return false,
    };
    health.get("status").and_then(Value::as_str) == Some("ok")
        && health.get("sessionId").and_then(Value::as_str) == Some(session.session_id.as_str())
}

fn process_alive(pid: u32) -> bool {
    // kill(pid, 0)：不送 signal，只檢查 process 是否存在（EPERM 也視為存在）。
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// 依 discovery 規則解析出唯一目標 session。
/// 0 個 → `preview-not-running`（含 recovery instruction）；
/// 多個且未指定 `--session` → `ambiguous-preview`（details 列出候選）。
pub fn resolve_session(project: &Path, session_id: Option<&str>) -> Result<SessionFile, OpError> {
    let candidates = discover_live_sessions(project);
    select_session(project, session_id, candidates)
}

fn select_session(
    project: &Path,
    session_id: Option<&str>,
    candidates: Vec<SessionFile>,
) -> Result<SessionFile, OpError> {
    match session_id {
        Some(id) => {
            if candidates.is_empty() {
                return Err(OpError::new(
                    "preview-not-running",
                    format!(
                        "no active preview found from {}; run `collab open <project>` first",
                        project.display()
                    ),
                ));
            }
            candidates
                .iter()
                .find(|session| session.session_id == id)
                .cloned()
                .ok_or_else(|| {
                    OpError::new(
                        "preview-session-mismatch",
                        format!(
                            "preview session {id} does not match a live preview in {}",
                            project.display()
                        ),
                    )
                    .with_details(json!({
                        "requestedSessionId": id,
                        "candidateSessionIds": candidates
                            .iter()
                            .take(8)
                            .map(|session| session.session_id.clone())
                            .collect::<Vec<_>>(),
                    }))
                })
        }
        None => match candidates.len() {
            0 => Err(OpError::new(
                "preview-not-running",
                format!(
                    "no active preview found from {}; run `collab open <project>` first",
                    project.display()
                ),
            )),
            1 => Ok(candidates.into_iter().next().unwrap()),
            _ => Err(OpError::new(
                "ambiguous-preview",
                "multiple active previews match this project; select one with --session <id>",
            )
            .with_details(json!({
                "candidates": candidates
                    .iter()
                    .map(|s| json!({
                        "sessionId": s.session_id,
                        "projectRoot": s.project_root,
                        "port": s.port,
                    }))
                    .collect::<Vec<_>>(),
            }))),
        },
    }
}

/// control service 的 blocking HTTP client；token 只進 Authorization header，
/// 不得出現在任何回傳資料中。
pub struct ControlClient {
    session: SessionFile,
    runtime: tokio::runtime::Runtime,
    http: reqwest::Client,
    wait_http: reqwest::Client,
}

pub enum InterruptibleResponse {
    Completed(Value),
    Interrupted,
}

impl ControlClient {
    pub fn new(session: SessionFile) -> Result<Self, OpError> {
        Self::with_timeouts(session, CONTROL_REQUEST_TIMEOUT, CONTROL_WAIT_TIMEOUT)
    }

    fn with_timeouts(
        session: SessionFile,
        request_timeout: Duration,
        wait_timeout: Duration,
    ) -> Result<Self, OpError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| OpError::new("internal-error", format!("failed to start runtime: {e}")))?;
        let http = reqwest::Client::builder()
            .connect_timeout(CONTROL_CONNECT_TIMEOUT)
            .timeout(request_timeout)
            .build()
            .map_err(|error| {
                OpError::new(
                    "internal-error",
                    format!("failed to configure control HTTP client: {error}"),
                )
            })?;
        let wait_http = reqwest::Client::builder()
            .connect_timeout(CONTROL_CONNECT_TIMEOUT)
            .timeout(wait_timeout)
            .build()
            .map_err(|error| {
                OpError::new(
                    "internal-error",
                    format!("failed to configure wait HTTP client: {error}"),
                )
            })?;
        Ok(ControlClient {
            session,
            runtime,
            http,
            wait_http,
        })
    }

    pub fn session(&self) -> &SessionFile {
        &self.session
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/__collab__{path}", self.session.port)
    }

    /// GET（無 token 的 read-only endpoints）。
    pub fn get(&self, path: &str) -> Result<Value, OpError> {
        let url = self.url(path);
        self.runtime.block_on(async {
            let response = self
                .http
                .get(&url)
                .send()
                .await
                .map_err(unreachable_error)?;
            parse_response(response).await
        })
    }

    /// GET control operation（帶 token）。
    pub fn get_control(&self, path: &str) -> Result<Value, OpError> {
        let url = self.url(path);
        self.runtime.block_on(async {
            let response = self
                .http
                .get(&url)
                .bearer_auth(&self.session.token)
                .send()
                .await
                .map_err(unreachable_error)?;
            parse_response(response).await
        })
    }

    /// POST control operation（帶 token）。
    pub fn post_control(&self, path: &str, body: Value) -> Result<Value, OpError> {
        let url = self.url(path);
        self.runtime.block_on(async {
            let response = self
                .http
                .post(&url)
                .bearer_auth(&self.session.token)
                .json(&body)
                .send()
                .await
                .map_err(unreachable_error)?;
            parse_response(response).await
        })
    }

    pub fn post_control_interruptible(
        &self,
        path: &str,
        body: Value,
    ) -> Result<InterruptibleResponse, OpError> {
        let url = self.url(path);
        let heartbeat_url = self.url("/control/heartbeat");
        self.runtime.block_on(async {
            let request = self
                .wait_http
                .post(&url)
                .bearer_auth(&self.session.token)
                .json(&body)
                .send();
            tokio::pin!(request);
            let mut heartbeat = tokio::time::interval_at(
                tokio::time::Instant::now() + ATTACHMENT_HEARTBEAT_INTERVAL,
                ATTACHMENT_HEARTBEAT_INTERVAL,
            );
            loop {
                tokio::select! {
                    response = &mut request => {
                        let response = response.map_err(unreachable_error)?;
                        return parse_response(response)
                            .await
                            .map(InterruptibleResponse::Completed);
                    }
                    heartbeat_response = async {
                        heartbeat.tick().await;
                        self.http
                            .post(&heartbeat_url)
                            .bearer_auth(&self.session.token)
                            .json(&body)
                            .send()
                            .await
                    } => {
                        let response = heartbeat_response.map_err(unreachable_error)?;
                        parse_response(response).await?;
                    }
                    interrupted = tokio::signal::ctrl_c() => {
                        interrupted.map_err(|error| {
                            OpError::new("internal-error", format!("failed to listen for SIGINT: {error}"))
                        })?;
                        return Ok(InterruptibleResponse::Interrupted);
                    }
                }
            }
        })
    }
}

impl CoreAdapter for ControlClient {
    type Error = OpError;

    fn execute(&self, request: CoreRequest) -> Result<CoreResult, Self::Error> {
        match request {
            CoreRequest::Attach(request) => {
                let response = self.post_control("/control/attach", serialize_request(request)?)?;
                decode_response::<AttachResult>(response).map(CoreResult::Attach)
            }
            CoreRequest::Status => {
                let mut response = self.get_control("/control/status")?;
                if let Some(map) = response.as_object_mut() {
                    map.insert("projectRoot".into(), json!(self.session.project_root));
                    map.insert("entryFile".into(), json!(self.session.entry_file));
                    map.insert("port".into(), json!(self.session.port));
                }
                decode_response::<StatusResult>(response).map(CoreResult::Status)
            }
            CoreRequest::Detach(request) => {
                let response = self.post_control("/control/detach", serialize_request(request)?)?;
                decode_response::<DetachResult>(response).map(CoreResult::Detach)
            }
            CoreRequest::Pause(request) => {
                let response = self.post_control("/control/pause", serialize_request(request)?)?;
                decode_response::<CollaborationControlResult>(response)
                    .map(CoreResult::CollaborationControl)
            }
            CoreRequest::Resume(request) => {
                let response = self.post_control("/control/resume", serialize_request(request)?)?;
                decode_response::<CollaborationControlResult>(response)
                    .map(CoreResult::CollaborationControl)
            }
            CoreRequest::Close => {
                let response = self.post_control("/control/close", json!({}))?;
                decode_response::<CloseResult>(response).map(CoreResult::Close)
            }
            CoreRequest::Screenshot => {
                let response = self.post_control("/control/screenshot", json!({}))?;
                decode_response::<ScreenshotResult>(response).map(CoreResult::Screenshot)
            }
            CoreRequest::Eval(request) => {
                let response = self.post_control("/control/eval", serialize_request(request)?)?;
                decode_response::<EvalResult>(response).map(CoreResult::Eval)
            }
            CoreRequest::Wait(request) => match self
                .post_control_interruptible("/control/wait", serialize_request(request)?)?
            {
                InterruptibleResponse::Completed(response) => {
                    decode_response::<WaitResult>(response).map(CoreResult::Wait)
                }
                InterruptibleResponse::Interrupted => Ok(CoreResult::Interrupted),
            },
            CoreRequest::FeedbackShow(request) => {
                let response =
                    self.get_control(&format!("/control/feedback/{}", request.feedback_id))?;
                decode_response::<FeedbackResult>(response).map(CoreResult::Feedback)
            }
            CoreRequest::FeedbackSetState(request) => {
                let path = format!("/control/feedback/{}/state", request.feedback_id);
                let response = self.post_control(&path, serialize_request(request)?)?;
                decode_response::<FeedbackResult>(response).map(CoreResult::Feedback)
            }
        }
    }
}

fn serialize_request<T: serde::Serialize>(request: T) -> Result<Value, OpError> {
    serde_json::to_value(request).map_err(|error| {
        OpError::new(
            "internal-error",
            format!("cannot serialize core request: {error}"),
        )
    })
}

fn decode_response<T: DeserializeOwned>(response: Value) -> Result<T, OpError> {
    serde_json::from_value(response).map_err(|error| {
        OpError::new(
            "invalid-response",
            format!("control service returned an invalid core result: {error}"),
        )
    })
}

fn unreachable_error(e: reqwest::Error) -> OpError {
    if e.is_timeout() {
        return OpError::new("timeout", format!("preview control request timed out: {e}"));
    }
    OpError::new(
        "preview-not-running",
        format!("preview control service unreachable: {e}; run `collab open <project>` first"),
    )
}

async fn parse_response(response: reqwest::Response) -> Result<Value, OpError> {
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(body);
    }
    let code = body
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("control-error")
        .to_string();
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("control service returned an error")
        .to_string();
    Err(OpError {
        code,
        message,
        details: body.get("details").cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn delayed_http_response(
        delay: Duration,
        body: &'static str,
    ) -> (u16, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            std::thread::sleep(delay);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });
        (port, worker)
    }

    fn test_session(port: u16) -> SessionFile {
        SessionFile::new(
            PathBuf::from("/tmp/control-client-timeout"),
            PathBuf::from("/tmp/control-client-timeout/index.html"),
            port,
            "test-token".into(),
        )
    }

    fn candidate(session_id: &str) -> SessionFile {
        let mut session = test_session(1);
        session.session_id = session_id.into();
        session
    }

    #[test]
    fn explicit_selection_reports_local_candidate_mismatch() {
        let error = select_session(
            Path::new("/workspace/project"),
            Some("0123456789abcdef"),
            vec![candidate("fedcba9876543210")],
        )
        .unwrap_err();

        assert_eq!(error.code, "preview-session-mismatch");
        assert_eq!(
            error.details,
            Some(json!({
                "requestedSessionId": "0123456789abcdef",
                "candidateSessionIds": ["fedcba9876543210"],
            }))
        );
    }

    #[test]
    fn explicit_selection_with_zero_candidates_is_not_running() {
        let error = select_session(
            Path::new("/workspace/project"),
            Some("0123456789abcdef"),
            vec![],
        )
        .unwrap_err();

        assert_eq!(error.code, "preview-not-running");
        assert_eq!(error.details, None);
    }

    #[test]
    fn ordinary_request_times_out_when_peer_never_responds() {
        let (port, worker) = delayed_http_response(Duration::from_millis(250), "{}");
        let client = ControlClient::with_timeouts(
            test_session(port),
            Duration::from_millis(40),
            Duration::from_secs(1),
        )
        .unwrap();
        let started = Instant::now();

        let error = client.get("/health").unwrap_err();

        assert_eq!(error.code, "timeout");
        assert!(started.elapsed() < Duration::from_millis(200));
        worker.join().unwrap();
    }

    #[test]
    fn wait_request_uses_its_long_poll_timeout_policy() {
        let body = r#"{"event":"collaboration.stop"}"#;
        let (port, worker) = delayed_http_response(Duration::from_millis(100), body);
        let client = ControlClient::with_timeouts(
            test_session(port),
            Duration::from_millis(20),
            Duration::from_millis(500),
        )
        .unwrap();

        let response = client
            .post_control_interruptible("/control/wait", json!({}))
            .unwrap();

        assert!(matches!(
            response,
            InterruptibleResponse::Completed(value)
                if value["event"] == "collaboration.stop"
        ));
        worker.join().unwrap();
    }

    #[test]
    fn current_process_is_alive() {
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn resolve_with_no_sessions_is_preview_not_running() {
        let dir = std::env::temp_dir().join(format!("collab-client-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let error = resolve_session(&dir, None).unwrap_err();
        assert_eq!(error.code, "preview-not-running");
        assert!(
            error.message.contains("collab open"),
            "must include recovery instruction"
        );
    }
}
