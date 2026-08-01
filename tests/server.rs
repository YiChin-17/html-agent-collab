//! Task 2.1 驗證：temporary-root integration tests、unauthorized request tests
//! 與 session 檔案權限 assertion。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use collab::server;
use collab::session::{self, SessionFile};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// start server with a test session id；回傳 receiver 以維持 command channel 開啟。
async fn start_server(
    project_root: PathBuf,
    token: String,
) -> (server::RunningServer, tokio::task::JoinHandle<()>) {
    let (running, mut receiver) = start_server_manual(project_root, token).await;
    let consumer = tokio::spawn(async move {
        use collab::webview::{CommandError, WebviewCommand};
        while let Some(command) = receiver.recv().await {
            match command {
                WebviewCommand::SetCollaborationActive { respond, .. }
                | WebviewCommand::SyncFeedbackMarkers { respond, .. }
                | WebviewCommand::ToggleOfflinePaint { respond }
                | WebviewCommand::Reload { respond } => {
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::Eval { respond, .. } => {
                    let _ = respond.send(Ok(serde_json::Value::Null));
                }
                WebviewCommand::Snapshot { respond, .. } => {
                    let _ = respond.send(Err(CommandError::SnapshotFailed(
                        "snapshot not configured for this test".into(),
                    )));
                }
                WebviewCommand::CapturePainting { respond, .. } => {
                    let _ = respond.send(Err(CommandError::SnapshotFailed(
                        "painting capture is not configured for this test".into(),
                    )));
                }
            }
        }
    });
    (running, consumer)
}

#[test]
fn marker_command_payload_is_revisioned_bounded_and_element_only() {
    use collab::core::{FeedbackMarkerSnapshot, FeedbackMarkerSummary};
    use collab::webview::WebviewCommand;

    let (respond, _receive) = tokio::sync::oneshot::channel();
    let command = WebviewCommand::SyncFeedbackMarkers {
        snapshot: FeedbackMarkerSnapshot {
            revision: 42,
            items: vec![FeedbackMarkerSummary {
                id: "fb-0001-00000001".into(),
                state: collab::feedback::FeedbackState::Failed,
                text: "heading".into(),
                failure_reason: Some("verification failed".into()),
                element: serde_json::json!({"selector": "#hero", "tag": "h1", "attributes": {"id": "hero"}}),
            }],
        },
        respond,
    };

    let WebviewCommand::SyncFeedbackMarkers { snapshot, .. } = command else {
        panic!("expected marker command");
    };
    assert_eq!(snapshot.revision, 42);
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(
        snapshot.items[0].state,
        collab::feedback::FeedbackState::Failed
    );
    assert_eq!(
        snapshot.items[0].failure_reason.as_deref(),
        Some("verification failed")
    );
}

async fn start_server_manual(
    project_root: PathBuf,
    token: String,
) -> (server::RunningServer, collab::webview::CommandReceiver) {
    let (commands, receiver) = collab::webview::command_channel();
    let running = server::start(server::ServerConfig {
        project_root,
        session_id: "test-session".into(),
        token,
        commands,
    })
    .await
    .expect("failed to start server");
    (running, receiver)
}

static TEMP_DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

/// 每個測試一個獨立 temporary project root。
fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "collab-server-test-{}-{}-{}",
        name,
        std::process::id(),
        TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("failed to create temp root");
    dir
}

/// 最小 HTTP/1.1 client：回傳 (status code, response 全文)。
/// 手寫 raw request 以確保 path 不被 client 端 normalize。
async fn raw_request(port: u16, method: &str, path: &str, auth: Option<&str>) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("failed to connect to loopback server");
    let auth_header = match auth {
        Some(token) => format!("Authorization: Bearer {token}\r\n"),
        None => String::new(),
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth_header}Connection: close\r\nContent-Length: 0\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write failed");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read failed");
    let text = String::from_utf8_lossy(&response).to_string();
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, text)
}

async fn raw_request_with_host(
    port: u16,
    method: &str,
    path: &str,
    host: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("failed to connect to loopback server");
    let host_header = host
        .map(|value| format!("Host: {value}\r\n"))
        .unwrap_or_default();
    let body = body.unwrap_or_default();
    let content_type = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/json\r\n"
    };
    let request = format!(
        "{method} {path} HTTP/1.1\r\n{host_header}{content_type}Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write failed");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read failed");
    let text = String::from_utf8_lossy(&response).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    (status, text)
}

#[tokio::test]
async fn rejects_foreign_host_before_static_control_and_feedback_routes() {
    let root = temp_root("foreign-host");
    std::fs::write(root.join("index.html"), "<h1>private project</h1>").unwrap();
    let (running, _commands) = start_server(root.clone(), "test-token".into()).await;
    let foreign_host = format!("evil.example:{}", running.port);
    let feedback = serde_json::json!({
        "kind": "textbox",
        "text": "untrusted feedback",
        "pageUrl": "http://evil.example/",
        "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
    })
    .to_string();

    for (method, path, body) in [
        ("GET", "/", None),
        ("GET", "/__collab__/control/status", None),
        (
            "POST",
            "/__collab__/overlay/feedback",
            Some(feedback.as_str()),
        ),
    ] {
        let (status, response) =
            raw_request_with_host(running.port, method, path, Some(&foreign_host), body).await;
        assert_eq!(status, 403, "unexpected response for {path}: {response}");
        assert_eq!(response_json(&response)["code"], "forbidden-host");
    }

    assert!(collab::feedback::list_records(&root).unwrap().is_empty());
}

#[tokio::test]
async fn allows_loopback_hostnames_and_rejects_missing_host() {
    let root = temp_root("host-boundaries");
    std::fs::write(root.join("index.html"), "<h1>loopback project</h1>").unwrap();
    let (running, _commands) = start_server(root, "test-token".into()).await;

    for host in [
        format!("127.0.0.1:{}", running.port),
        format!("localhost:{}", running.port),
    ] {
        let (status, response) =
            raw_request_with_host(running.port, "GET", "/", Some(&host), None).await;
        assert_eq!(status, 200, "unexpected response for {host}: {response}");
        assert!(response.contains("loopback project"));
    }

    let (status, response) = raw_request_with_host(running.port, "GET", "/", None, None).await;
    assert_eq!(status, 403, "unexpected response without Host: {response}");
    assert_eq!(response_json(&response)["code"], "forbidden-host");
}

#[tokio::test]
async fn serves_index_and_relative_assets() {
    let root = temp_root("assets");
    std::fs::write(
        root.join("index.html"),
        "<!doctype html><link rel=\"stylesheet\" href=\"style.css\"><h1>hello-collab</h1>",
    )
    .unwrap();
    std::fs::write(root.join("style.css"), "h1{color:red}").unwrap();

    let (running, _commands) = start_server(root, "test-token".into()).await;

    let (status, body) = raw_request(running.port, "GET", "/", None).await;
    assert_eq!(status, 200);
    assert!(body.contains("hello-collab"));

    let (status, body) = raw_request(running.port, "GET", "/style.css", None).await;
    assert_eq!(status, 200);
    assert!(body.to_ascii_lowercase().contains("content-type: text/css"));
    assert!(body.contains("h1{color:red}"));
}

#[tokio::test]
async fn rejects_normalized_path_traversal() {
    let parent = temp_root("traversal-parent");
    let root = parent.join("project");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("index.html"), "<h1>inside</h1>").unwrap();
    let secret = "TOP-SECRET-OUTSIDE-ROOT";
    std::fs::write(parent.join("secret.txt"), secret).unwrap();

    let (running, _commands) = start_server(root, "test-token".into()).await;

    for path in [
        "/../secret.txt",
        "/%2e%2e/secret.txt",
        "/%2E%2E%2Fsecret.txt",
        "/sub/../../secret.txt",
        "/sub/%2e%2e/%2e%2e/secret.txt",
    ] {
        let (status, body) = raw_request(running.port, "GET", path, None).await;
        assert!(
            status >= 400,
            "expected HTTP error for {path}, got {status}"
        );
        assert!(
            !body.contains(secret),
            "traversal path {path} leaked file content outside root"
        );
    }
}

#[tokio::test]
async fn rejects_session_registry_path_variants_without_leaking_token() {
    let root = temp_root("session-registry-static");
    let token = "STATICALLY-EXPOSED-CONTROL-TOKEN";
    std::fs::create_dir_all(root.join(".collab/nested")).unwrap();
    std::fs::write(
        root.join(".collab/session.json"),
        serde_json::json!({"token": token}).to_string(),
    )
    .unwrap();
    std::fs::write(root.join(".collab/nested/any.json"), token).unwrap();

    let (running, _commands) = start_server(root, token.into()).await;

    for path in [
        "/.collab/session.json",
        "/.collab/",
        "/.collab/nested/any.json",
        "/.COLLAB/session.json",
        "/%2ecollab/session.json",
    ] {
        let (status, body) = raw_request(running.port, "GET", path, None).await;
        assert!(
            (400..500).contains(&status),
            "expected client error for {path}, got {status}: {body}"
        );
        assert!(
            !body.contains(token),
            "registry path {path} leaked the control token"
        );
    }
}

#[tokio::test]
async fn rejects_static_symlink_that_resolves_outside_project_root() {
    use std::os::unix::fs::symlink;

    let parent = temp_root("static-symlink-parent");
    let root = parent.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let secret = "OUTSIDE-PROJECT-SECRET";
    std::fs::write(parent.join("secret.txt"), secret).unwrap();
    symlink(parent.join("secret.txt"), root.join("leak.txt")).unwrap();

    let (running, _commands) = start_server(root, "test-token".into()).await;
    let (status, body) = raw_request(running.port, "GET", "/leak.txt", None).await;

    assert!(
        (400..500).contains(&status),
        "expected client error for out-of-root symlink, got {status}: {body}"
    );
    assert!(!body.contains(secret), "symlink leaked out-of-root content");
}

#[tokio::test]
async fn control_close_requires_valid_token_and_stop_route_is_removed() {
    let root = temp_root("auth");
    std::fs::write(root.join("index.html"), "<h1>auth</h1>").unwrap();
    let token = session::generate_token();
    let (running, _commands) = start_server(root, token.clone()).await;

    // 無 token 與錯誤 token 都必須被拒絕，且不執行 close。
    let (status, body) = raw_request(running.port, "POST", "/__collab__/control/close", None).await;
    assert_eq!(status, 401);
    assert!(body.contains("unauthorized"));

    let (status, _) = raw_request(
        running.port,
        "POST",
        "/__collab__/control/close",
        Some("wrong-token"),
    )
    .await;
    assert_eq!(status, 401);

    for operation in ["pause", "resume"] {
        let (status, body) = raw_post_json(
            running.port,
            &format!("/__collab__/control/{operation}"),
            "{}",
        )
        .await;
        assert_eq!(status, 401, "{operation}: {body}");
        assert!(body.contains("unauthorized"));
    }

    // 被拒絕的 close 不得生效：server 仍在服務。
    let (status, _) = raw_request(running.port, "GET", "/__collab__/health", None).await;
    assert_eq!(status, 200);

    let (status, _) = raw_request(
        running.port,
        "POST",
        "/__collab__/control/stop",
        Some(&token),
    )
    .await;
    assert!(
        matches!(status, 404 | 405),
        "legacy stop route must not be an active control operation"
    );

    // 正確 token 觸發 graceful shutdown。
    let (status, body) = raw_request(
        running.port,
        "POST",
        "/__collab__/control/close",
        Some(&token),
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains("closing"));

    tokio::time::timeout(Duration::from_secs(5), running.task)
        .await
        .expect("server did not shut down after authorized close")
        .expect("server task panicked")
        .expect("server returned io error");
}

#[tokio::test]
async fn health_response_does_not_leak_token() {
    let root = temp_root("health");
    let token = session::generate_token();
    let (running, _commands) = start_server(root, token.clone()).await;

    let (status, body) = raw_request(running.port, "GET", "/__collab__/health", None).await;
    assert_eq!(status, 200);
    assert!(
        !body.contains(&token),
        "health response leaked control token"
    );
}

#[tokio::test]
async fn token_free_status_omits_attachment_identity_and_details() {
    let root = temp_root("minimal-status");
    let token = "minimal-status-token";
    let (running, _commands) = start_server(root, token.into()).await;
    attach_agent(running.port, token, "codex").await;

    let (status, body) = raw_request(running.port, "GET", "/__collab__/status", None).await;

    assert_eq!(status, 200);
    let payload = response_json(&body);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["collaborationActive"], true);
    assert!(payload.get("sessionId").is_none());
    assert!(payload.get("attachments").is_none());
    assert!(!body.contains("codex"));
    assert!(!body.contains(&std::process::id().to_string()));
}

#[tokio::test]
async fn full_command_queue_returns_busy_immediately() {
    use collab::webview::{COMMAND_QUEUE_CAPACITY, WebviewCommand, command_channel};

    let root = temp_root("busy");
    let token = session::generate_token();
    let (commands, _receiver) = command_channel();
    let running = server::start(server::ServerConfig {
        project_root: root,
        session_id: "test-session".into(),
        token: token.clone(),
        commands: commands.clone(),
    })
    .await
    .unwrap();

    // 佔滿 bounded queue（receiver 存活但不消化）。
    for _ in 0..COMMAND_QUEUE_CAPACITY {
        let (respond, _receive) = tokio::sync::oneshot::channel();
        commands
            .try_send(WebviewCommand::Reload { respond })
            .expect("queue should accept up to capacity");
    }

    // queue 滿時必須立即回 busy，不得無限累積。
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(2),
        raw_request(
            running.port,
            "POST",
            "/__collab__/control/reload",
            Some(&token),
        ),
    )
    .await
    .expect("busy response must be immediate");
    assert_eq!(status, 503);
    assert!(body.contains("\"busy\""), "unexpected body: {body}");
}

#[tokio::test]
async fn closed_command_channel_is_internal_error() {
    let root = temp_root("closed-channel");
    let token = session::generate_token();
    let (running, receiver) = start_server_manual(root, token.clone()).await;
    drop(receiver);

    let (status, body) = raw_request(
        running.port,
        "POST",
        "/__collab__/control/reload",
        Some(&token),
    )
    .await;
    assert_eq!(status, 500);
    assert!(body.contains("internal-error"), "unexpected body: {body}");
}

/// 帶 JSON body 的 raw POST（無 Authorization）。
async fn raw_post_json(port: u16, path: &str, body: &str) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("failed to connect");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response).to_string();
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, text)
}

async fn raw_control_json(port: u16, path: &str, token: &str, body: &str) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("failed to connect");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response).to_string();
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, text)
}

fn response_json(response: &str) -> serde_json::Value {
    serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap().trim()).unwrap()
}

fn textbox_feedback(text: &str) -> String {
    serde_json::json!({
        "kind": "textbox",
        "text": text,
        "pageUrl": "http://127.0.0.1/",
        "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
    })
    .to_string()
}

fn assert_no_feedback_artifacts(root: &std::path::Path) {
    assert!(collab::feedback::list_records(root).unwrap().is_empty());
    let feedback_dir = collab::feedback::feedback_dir(root);
    assert!(
        !feedback_dir.exists() || std::fs::read_dir(feedback_dir).unwrap().next().is_none(),
        "rejected feedback must not publish artifacts"
    );
}

async fn attach_agent(port: u16, token: &str, agent: &str) -> String {
    let payload = serde_json::json!({
        "agentKind": agent,
        "pid": std::process::id(),
    })
    .to_string();
    let (status, body) =
        raw_control_json(port, "/__collab__/control/attach", token, &payload).await;
    assert_eq!(status, 200, "body: {body}");
    response_json(&body)["attachment"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn collaboration_control(
    port: u16,
    token: &str,
    operation: &str,
    attachment_id: &str,
) -> (u16, serde_json::Value) {
    let payload = serde_json::json!({"attachmentId": attachment_id}).to_string();
    let (status, body) = raw_control_json(
        port,
        &format!("/__collab__/control/{operation}"),
        token,
        &payload,
    )
    .await;
    (status, response_json(&body))
}

#[tokio::test]
async fn pause_transition_table_handles_idle_owned_terminal_and_resume() {
    let root = temp_root("pause-transitions");
    let token = "pause-transitions-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;
    let attachment_id = attach_agent(running.port, token, "codex").await;

    let (status, paused) =
        collaboration_control(running.port, token, "pause", &attachment_id).await;
    assert_eq!(status, 200, "{paused}");
    assert_eq!(paused["status"], "paused");
    assert_eq!(paused["collaborationState"], "paused");

    let (status, resumed) =
        collaboration_control(running.port, token, "resume", &attachment_id).await;
    assert_eq!(status, 200, "{resumed}");
    assert_eq!(resumed["status"], "resumed");
    assert_eq!(resumed["attachmentId"], attachment_id);

    let record = collab::feedback::store(
        &root,
        collab::feedback::validate(serde_json::json!({
            "kind": "textbox",
            "text": "feedback A",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0}
        }))
        .unwrap(),
    )
    .unwrap();
    collab::feedback::lease_next(&root, &attachment_id).unwrap();

    let (status, pending) =
        collaboration_control(running.port, token, "pause", &attachment_id).await;
    assert_eq!(status, 200, "{pending}");
    assert_eq!(pending["status"], "pause-requested");
    assert_eq!(pending["collaborationState"], "pause-requested");

    let (status, resume_pending) =
        collaboration_control(running.port, token, "resume", &attachment_id).await;
    assert_eq!(status, 409, "{resume_pending}");
    assert_eq!(resume_pending["code"], "pause-pending");

    collab::feedback::transition(
        &root,
        &record.id,
        "pending",
        "acknowledged",
        &attachment_id,
        None,
    )
    .unwrap();
    collab::feedback::transition(
        &root,
        &record.id,
        "acknowledged",
        "working",
        &attachment_id,
        None,
    )
    .unwrap();
    let terminal = serde_json::json!({
        "attachmentId": attachment_id,
        "expectedState": "working",
        "state": "resolved"
    })
    .to_string();
    let (status, body) = raw_control_json(
        running.port,
        &format!("/__collab__/control/feedback/{}/state", record.id),
        token,
        &terminal,
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (status, body) = raw_request(
        running.port,
        "GET",
        "/__collab__/control/status",
        Some(token),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        response_json(&body)["attachments"][0]["collaborationState"],
        "paused"
    );
}

#[tokio::test]
async fn paused_collaboration_rejects_overlay_without_persisting_artifacts() {
    let root = temp_root("pause-overlay-gate");
    let token = "pause-overlay-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;
    let attachment_id = attach_agent(running.port, token, "codex").await;
    let (status, _) = collaboration_control(running.port, token, "pause", &attachment_id).await;
    assert_eq!(status, 200);

    let payload = serde_json::json!({
        "kind": "painting",
        "text": "feedback B",
        "pageUrl": "http://127.0.0.1/",
        "viewport": {
            "width": 800, "height": 600, "scrollX": 0, "scrollY": 0,
            "documentWidth": 800, "documentHeight": 1200,
            "captureRegions": [{"x": 0, "y": 0, "width": 800, "height": 600}],
        },
        "elements": [],
        "marks": [{"type": "rect", "x": 1, "y": 1, "width": 10, "height": 10}],
        "svg": "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"
    })
    .to_string();
    let (status, body) =
        raw_post_json(running.port, "/__collab__/overlay/feedback", &payload).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(response_json(&body)["code"], "collaboration-paused");
    assert_no_feedback_artifacts(&root);
}

#[tokio::test]
async fn overlay_feedback_rejects_zero_connected_attachments_without_artifacts() {
    let root = temp_root("overlay-zero-connected");
    let (running, _commands) = start_server(root.clone(), "test-token".into()).await;

    let (status, body) = raw_post_json(
        running.port,
        "/__collab__/overlay/feedback",
        &textbox_feedback("must be rejected"),
    )
    .await;

    assert_eq!(status, 409, "{body}");
    assert_eq!(response_json(&body)["code"], "collaboration-inactive");
    assert_no_feedback_artifacts(&root);
}

#[tokio::test]
async fn overlay_feedback_rejects_all_inactive_attachments_without_artifacts() {
    let root = temp_root("overlay-all-inactive");
    let token = "overlay-all-inactive-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;
    let attachment_id = attach_agent(running.port, token, "codex").await;
    let (status, detached) =
        collaboration_control(running.port, token, "detach", &attachment_id).await;
    assert_eq!(status, 200, "{detached}");

    let (status, body) = raw_post_json(
        running.port,
        "/__collab__/overlay/feedback",
        &textbox_feedback("must remain rejected"),
    )
    .await;

    assert_eq!(status, 409, "{body}");
    assert_eq!(response_json(&body)["code"], "collaboration-inactive");
    assert_no_feedback_artifacts(&root);
}

#[tokio::test]
async fn overlay_feedback_accepts_when_another_attachment_remains_active() {
    let root = temp_root("overlay-active-and-paused");
    let token = "overlay-active-and-paused-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;
    let paused_id = attach_agent(running.port, token, "paused").await;
    let _active_id = attach_agent(running.port, token, "active").await;
    let (status, paused) = collaboration_control(running.port, token, "pause", &paused_id).await;
    assert_eq!(status, 200, "{paused}");

    let (status, body) = raw_post_json(
        running.port,
        "/__collab__/overlay/feedback",
        &textbox_feedback("active attachment accepts this"),
    )
    .await;

    assert_eq!(status, 200, "{body}");
    assert_eq!(collab::feedback::list_records(&root).unwrap().len(), 1);
}

#[tokio::test]
async fn pause_requested_detach_and_lease_expiry_recover_current_feedback() {
    let root = temp_root("pause-lease-loss");
    let token = "pause-lease-loss-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;
    let first_attachment = attach_agent(running.port, token, "first").await;
    let first = collab::feedback::store(
        &root,
        collab::feedback::validate(serde_json::json!({
            "kind": "textbox",
            "text": "detach recovery",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0}
        }))
        .unwrap(),
    )
    .unwrap();
    collab::feedback::lease_next(&root, &first_attachment).unwrap();
    let (status, paused) =
        collaboration_control(running.port, token, "pause", &first_attachment).await;
    assert_eq!(status, 200);
    assert_eq!(paused["status"], "pause-requested");
    let detach = serde_json::json!({"attachmentId": first_attachment}).to_string();
    let (status, _) =
        raw_control_json(running.port, "/__collab__/control/detach", token, &detach).await;
    assert_eq!(status, 200);
    let recovered = collab::feedback::read_record(&root, &first.id).unwrap();
    assert_eq!(recovered.state, "pending");
    assert!(recovered.lease.is_none());

    let second_attachment = attach_agent(running.port, token, "second").await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    collab::feedback::lease_next_at(&root, &second_attachment, now, Duration::from_millis(40))
        .unwrap();
    let (status, pending) =
        collaboration_control(running.port, token, "pause", &second_attachment).await;
    assert_eq!(status, 200);
    assert_eq!(pending["status"], "pause-requested");
    tokio::time::sleep(Duration::from_millis(60)).await;
    let heartbeat = serde_json::json!({"attachmentId": second_attachment}).to_string();
    let (status, body) = raw_control_json(
        running.port,
        "/__collab__/control/heartbeat",
        token,
        &heartbeat,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let recovered = collab::feedback::read_record(&root, &first.id).unwrap();
    assert_eq!(recovered.state, "pending");
    assert!(recovered.lease.is_none());
    let (status, body) = raw_request(
        running.port,
        "GET",
        "/__collab__/control/status",
        Some(token),
    )
    .await;
    assert_eq!(status, 200);
    let status = response_json(&body);
    assert_eq!(
        status["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|attachment| attachment["attachmentId"] == second_attachment)
            .unwrap()["collaborationState"],
        "paused"
    );
}

#[tokio::test]
async fn concurrent_resume_and_stop_leave_one_inactive_result() {
    let root = temp_root("resume-stop-race");
    let token = "resume-stop-race-token";
    let (running, _commands) = start_server(root, token.into()).await;
    let attachment_id = attach_agent(running.port, token, "codex").await;
    let (status, _) = collaboration_control(running.port, token, "pause", &attachment_id).await;
    assert_eq!(status, 200);

    let resume_body = serde_json::json!({"attachmentId": attachment_id}).to_string();
    let stop_body = resume_body.clone();
    let port = running.port;
    let resume = tokio::spawn(async move {
        raw_control_json(port, "/__collab__/control/resume", token, &resume_body).await
    });
    let stop = tokio::spawn(async move {
        raw_control_json(port, "/__collab__/control/detach", token, &stop_body).await
    });
    let (resume, stop) = tokio::join!(resume, stop);
    let (resume_status, _) = resume.unwrap();
    let (stop_status, stop_body) = stop.unwrap();
    assert!(matches!(resume_status, 200 | 409));
    assert_eq!(stop_status, 200, "{stop_body}");

    let (status, body) = raw_request(
        running.port,
        "GET",
        "/__collab__/control/status",
        Some(token),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        response_json(&body)["attachments"][0]["collaborationState"],
        "inactive"
    );
}

async fn complete_active_command(
    commands: &mut collab::webview::CommandReceiver,
    expected_active: bool,
) {
    let collab::webview::WebviewCommand::SetCollaborationActive { active, respond } =
        commands.recv().await.unwrap()
    else {
        panic!("expected collaboration-active command");
    };
    assert_eq!(active, expected_active);
    respond.send(Ok(())).unwrap();
}

async fn complete_marker_command(commands: &mut collab::webview::CommandReceiver) {
    let collab::webview::WebviewCommand::SyncFeedbackMarkers { respond, .. } =
        commands.recv().await.unwrap()
    else {
        panic!("expected feedback-marker command");
    };
    respond.send(Ok(())).unwrap();
}

#[tokio::test]
async fn overlay_feedback_endpoint_validates_and_persists() {
    let root = temp_root("overlay-feedback");
    let token = "test-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;
    attach_agent(running.port, token, "codex").await;

    // 合法 element-comment → 200 + pending record 落盤。
    let valid = serde_json::json!({
        "kind": "element-comment",
        "text": "make it blue",
        "pageUrl": "http://127.0.0.1/",
        "viewport": {"width": 1200, "height": 800, "scrollX": 0, "scrollY": 10},
        "elements": [{"selector": "#hero", "tag": "h1"}],
    })
    .to_string();
    let (status, body) = raw_post_json(running.port, "/__collab__/overlay/feedback", &valid).await;
    assert_eq!(status, 200, "body: {body}");
    let json_body = body.split("\r\n\r\n").nth(1).unwrap();
    let response: serde_json::Value = serde_json::from_str(json_body.trim()).unwrap();
    let id = response["id"].as_str().unwrap();
    assert_eq!(response["state"], "pending");

    let record = collab::feedback::read_record(&root, id).unwrap();
    assert_eq!(record.kind, "element-comment");
    assert_eq!(record.state, "pending");
    assert_eq!(record.elements[0]["selector"], "#hero");

    // schema 違反 → 400 invalid-request，不落盤。
    let invalid = serde_json::json!({
        "kind": "emoji-reaction",
        "text": "x",
        "pageUrl": "http://127.0.0.1/",
        "viewport": {},
    })
    .to_string();
    let (status, body) =
        raw_post_json(running.port, "/__collab__/overlay/feedback", &invalid).await;
    assert_eq!(status, 400);
    assert!(body.contains("invalid-request"));
}

#[tokio::test]
async fn overlay_draft_state_endpoint_publishes_only_bounded_state() {
    let root = temp_root("overlay-draft-state");
    let (running, _commands) = start_server(root, "test-token".into()).await;
    let mut draft_states = running.draft_states.clone();
    let valid = serde_json::json!({
        "status": "editing",
        "pageUrl": format!("http://127.0.0.1:{}/index.html", running.port),
        "selector": "#hero",
        "focusHtml": "<h1>Hello</h1>",
        "originalHtml": "<!doctype html><html><head></head><body><h1>Hello</h1></body></html>",
        "currentHtml": "<!doctype html><html><head></head><body><h1>Welcome</h1></body></html>",
        "dirty": true,
        "undoDepth": 1,
        "redoDepth": 0,
        "error": null
    })
    .to_string();

    let (status, body) =
        raw_post_json(running.port, "/__collab__/overlay/draft-state", &valid).await;
    assert_eq!(status, 200, "{body}");
    draft_states.changed().await.unwrap();
    assert_eq!(
        draft_states.borrow_and_update().selector.as_deref(),
        Some("#hero")
    );

    let invalid = valid.replace("\"undoDepth\":1", "\"undoDepth\":51");
    let (status, body) =
        raw_post_json(running.port, "/__collab__/overlay/draft-state", &invalid).await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("invalid-request"));
}

/// Task 3.2 attachment existence checks：painting 送出後 PNG 與 SVG 均存在，
/// record 的 attachments 為 absolute paths。snapshot 由 stub consumer 產生。
#[tokio::test]
async fn painting_feedback_persists_png_and_svg_attachments() {
    use collab::webview::WebviewCommand;

    let root = temp_root("painting");
    let (running, mut commands) = start_server_manual(root.clone(), "test-token".into()).await;

    // stub consumer：模擬 WebKit snapshot 寫入 PNG。
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            match command {
                WebviewCommand::SetCollaborationActive { respond, .. }
                | WebviewCommand::SyncFeedbackMarkers { respond, .. }
                | WebviewCommand::ToggleOfflinePaint { respond } => {
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::CapturePainting {
                    output_paths,
                    respond,
                    ..
                } => {
                    for path in &output_paths {
                        std::fs::write(path, b"\x89PNG-stub").unwrap();
                    }
                    let _ = respond.send(Ok(output_paths));
                }
                _ => {}
            }
        }
    });
    attach_agent(running.port, "test-token", "codex").await;

    let payload = serde_json::json!({
        "kind": "painting",
        "text": "Move these cards upward",
        "pageUrl": "http://127.0.0.1/",
        "viewport": {
            "width": 1200, "height": 800, "scrollX": 0, "scrollY": 0,
            "documentWidth": 1200, "documentHeight": 4000,
            "captureRegions": [{"x": 0, "y": 0, "width": 1200, "height": 800}],
        },
        "elements": [
            {"selector": "#card-a", "overlapRatio": 0.82},
            {"selector": "#section", "overlapRatio": 0.64},
            {"selector": "#card-b", "overlapRatio": 0.31},
        ],
        "marks": [{"type": "rect", "x": 10, "y": 10, "width": 100, "height": 60}],
        "svg": "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect x=\"10\" y=\"10\" width=\"100\" height=\"60\"/></svg>",
    })
    .to_string();
    let (status, body) =
        raw_post_json(running.port, "/__collab__/overlay/feedback", &payload).await;
    assert_eq!(status, 200, "body: {body}");
    let json_body = body.split("\r\n\r\n").nth(1).unwrap();
    let response: serde_json::Value = serde_json::from_str(json_body.trim()).unwrap();
    let id = response["id"].as_str().unwrap();

    let record = collab::feedback::read_record(&root, id).unwrap();
    assert_eq!(record.kind, "painting");
    assert_eq!(record.text, "Move these cards upward");
    assert_eq!(record.attachments.len(), 2);
    for attachment in &record.attachments {
        assert!(
            std::path::Path::new(attachment).is_absolute(),
            "attachment must be absolute: {attachment}"
        );
        assert!(
            std::path::Path::new(attachment).exists(),
            "attachment must exist: {attachment}"
        );
    }
    assert!(record.attachments[0].ends_with(".png"));
    assert!(record.attachments[1].ends_with(".svg"));
    assert_eq!(record.marks[0]["type"], "rect");
    // spec Example「Overlap ordering」：0.82 → 0.64 → 0.31。
    assert_eq!(record.elements[0]["selector"], "#card-a");
    assert_eq!(record.elements[1]["selector"], "#section");
    assert_eq!(record.elements[2]["selector"], "#card-b");
}

#[tokio::test]
async fn painting_publish_failure_removes_unpublished_artifacts() {
    use collab::webview::WebviewCommand;

    let root = temp_root("painting-cleanup");
    let (running, mut commands) = start_server_manual(root.clone(), "test-token".into()).await;
    let feedback_dir = collab::feedback::feedback_dir(&root);
    let observed_id = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let worker_id = observed_id.clone();
    tokio::spawn(async move {
        let WebviewCommand::SetCollaborationActive { respond, .. } = commands.recv().await.unwrap()
        else {
            panic!("expected collaboration-active command");
        };
        respond.send(Ok(())).unwrap();
        let WebviewCommand::CapturePainting {
            output_paths,
            respond,
            ..
        } = commands.recv().await.unwrap()
        else {
            panic!("expected capture painting command");
        };
        for path in &output_paths {
            std::fs::write(path, b"\x89PNG-stub").unwrap();
        }
        let id = record_id_from_png(&output_paths[0]);
        // record 寫入失敗：以目錄佔用 JSON 路徑。
        let dir = output_paths[0].parent().unwrap().to_path_buf();
        std::fs::create_dir_all(dir.join(format!("{id}.json"))).unwrap();
        *worker_id.lock().unwrap() = Some(id);
        let _ = respond.send(Ok(output_paths));
    });
    attach_agent(running.port, "test-token", "codex").await;

    let payload = serde_json::json!({
        "kind": "painting",
        "text": "cleanup this failure",
        "pageUrl": "http://127.0.0.1/",
        "viewport": {
            "width": 800, "height": 600, "scrollX": 0, "scrollY": 0,
            "documentWidth": 800, "documentHeight": 1800,
            "captureRegions": [
                {"x": 0, "y": 0, "width": 800, "height": 600},
                {"x": 0, "y": 1200, "width": 800, "height": 600},
            ],
        },
        "elements": [],
        "marks": [{"type": "line", "x": 1, "y": 2}],
        "svg": "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
    })
    .to_string();

    let (status, _) = raw_post_json(running.port, "/__collab__/overlay/feedback", &payload).await;

    assert_eq!(status, 500);
    let id = observed_id.lock().unwrap().clone().unwrap();
    // 本次建立的每一張 PNG 與 SVG 都必須被清掉，且不得留下 unpublished record。
    assert!(!feedback_dir.join(format!("{id}-0.png")).exists());
    assert!(!feedback_dir.join(format!("{id}-1.png")).exists());
    assert!(!feedback_dir.join(format!("{id}.svg")).exists());
    assert!(!feedback_dir.join(format!("{id}.json.tmp")).exists());
}

fn record_id_from_png(path: &std::path::Path) -> String {
    let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
    stem.rsplit_once('-')
        .expect("png attachments are named <id>-<index>.png")
        .0
        .to_string()
}

/// spec「Multiple capture regions are published」：一筆 feedback 依序帶三張 PNG，
/// 最後才是 editable SVG，`viewport.captureRegions[n]` 與 `attachments[n]` 逐一對應。
#[tokio::test]
async fn painting_publishes_ordered_multi_region_attachments() {
    use collab::webview::WebviewCommand;

    let root = temp_root("painting-multi-region");
    let (running, mut commands) = start_server_manual(root.clone(), "test-token".into()).await;
    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<f64>::new()));
    let worker = observed.clone();
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            match command {
                WebviewCommand::SetCollaborationActive { respond, .. }
                | WebviewCommand::SyncFeedbackMarkers { respond, .. }
                | WebviewCommand::ToggleOfflinePaint { respond } => {
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::CapturePainting {
                    regions,
                    output_paths,
                    respond,
                } => {
                    assert_eq!(regions.len(), output_paths.len());
                    *worker.lock().unwrap() = regions.iter().map(|region| region.y).collect();
                    for path in &output_paths {
                        std::fs::write(path, b"\x89PNG-stub").unwrap();
                    }
                    let _ = respond.send(Ok(output_paths));
                }
                _ => {}
            }
        }
    });
    attach_agent(running.port, "test-token", "codex").await;

    let payload = serde_json::json!({
        "kind": "painting",
        "text": "three regions",
        "pageUrl": "http://127.0.0.1/",
        "viewport": {
            "width": 1200, "height": 800, "scrollX": 0, "scrollY": 0,
            "documentWidth": 1200, "documentHeight": 6000,
            "captureRegions": [
                {"x": 0, "y": 0, "width": 1200, "height": 800},
                {"x": 0, "y": 2000, "width": 1200, "height": 800},
                {"x": 0, "y": 4000, "width": 1200, "height": 800},
            ],
        },
        "elements": [],
        "marks": [{"type": "rect", "x": 10, "y": 10, "width": 100, "height": 60}],
        "svg": "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
    })
    .to_string();

    let (status, body) =
        raw_post_json(running.port, "/__collab__/overlay/feedback", &payload).await;
    assert_eq!(status, 200, "body: {body}");
    let json_body = body.split("\r\n\r\n").nth(1).unwrap();
    let response: serde_json::Value = serde_json::from_str(json_body.trim()).unwrap();
    let id = response["id"].as_str().unwrap();

    assert_eq!(*observed.lock().unwrap(), vec![0.0, 2000.0, 4000.0]);
    let record = collab::feedback::read_record(&root, id).unwrap();
    assert_eq!(record.attachments.len(), 4);
    for (index, attachment) in record.attachments.iter().take(3).enumerate() {
        assert!(
            attachment.ends_with(&format!("{id}-{index}.png")),
            "attachment {index} must be the indexed PNG: {attachment}"
        );
        assert!(std::path::Path::new(attachment).exists());
    }
    assert!(record.attachments[3].ends_with(".svg"));
    assert_eq!(record.viewport["captureRegions"][2]["y"], 4000.0);
}

/// spec「Server rejects an invalid capture plan」：malformed plan 以 HTTP 400
/// `invalid-request` 拒絕，且不得建立任何 JSON、PNG 或 SVG。
#[tokio::test]
async fn painting_with_an_invalid_capture_plan_creates_no_artifact() {
    use collab::webview::WebviewCommand;

    let root = temp_root("painting-invalid-plan");
    let (running, mut commands) = start_server_manual(root.clone(), "test-token".into()).await;
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            match command {
                WebviewCommand::SetCollaborationActive { respond, .. }
                | WebviewCommand::SyncFeedbackMarkers { respond, .. }
                | WebviewCommand::ToggleOfflinePaint { respond } => {
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::CapturePainting { .. } => {
                    panic!("an invalid capture plan must never reach the WebView");
                }
                _ => {}
            }
        }
    });
    attach_agent(running.port, "test-token", "codex").await;

    let nine = (0..9)
        .map(|index| serde_json::json!({"x": 0, "y": index * 400, "width": 1200, "height": 800}))
        .collect::<Vec<_>>();
    for regions in [
        serde_json::json!([]),
        serde_json::Value::from(nine),
        serde_json::json!([{"x": 0, "y": 0, "width": 1200, "height": 0}]),
        serde_json::json!([{"x": 0, "y": 5800, "width": 1200, "height": 800}]),
    ] {
        let payload = serde_json::json!({
            "kind": "painting",
            "text": "invalid plan",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {
                "width": 1200, "height": 800, "scrollX": 0, "scrollY": 0,
                "documentWidth": 1200, "documentHeight": 6000,
                "captureRegions": regions,
            },
            "elements": [],
            "marks": [{"type": "rect", "x": 10, "y": 10, "width": 100, "height": 60}],
            "svg": "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
        })
        .to_string();

        let (status, body) =
            raw_post_json(running.port, "/__collab__/overlay/feedback", &payload).await;
        assert_eq!(status, 400, "regions {regions} body: {body}");
        assert!(body.contains("invalid-request"), "body: {body}");
    }

    let feedback_dir = collab::feedback::feedback_dir(&root);
    let created = std::fs::read_dir(&feedback_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(created, 0, "a rejected capture plan created artifacts");
}

/// spec「Any native capture or restoration fails」：任一 region 失敗即回
/// `snapshot-failed`，並清除本次已建立的全部 PNG 與 SVG。
#[tokio::test]
async fn painting_partial_capture_failure_removes_every_artifact() {
    use collab::webview::{CommandError, WebviewCommand};

    let root = temp_root("painting-partial-capture");
    let (running, mut commands) = start_server_manual(root.clone(), "test-token".into()).await;
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            match command {
                WebviewCommand::SetCollaborationActive { respond, .. }
                | WebviewCommand::SyncFeedbackMarkers { respond, .. }
                | WebviewCommand::ToggleOfflinePaint { respond } => {
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::CapturePainting {
                    output_paths,
                    respond,
                    ..
                } => {
                    // 第一張成功、第二張失敗：模擬 scroll restoration 中途失敗。
                    std::fs::write(&output_paths[0], b"\x89PNG-stub").unwrap();
                    let _ = respond.send(Err(CommandError::SnapshotFailed(
                        "second region could not be captured".into(),
                    )));
                }
                _ => {}
            }
        }
    });
    attach_agent(running.port, "test-token", "codex").await;

    let payload = serde_json::json!({
        "kind": "painting",
        "text": "partial capture",
        "pageUrl": "http://127.0.0.1/",
        "viewport": {
            "width": 1200, "height": 800, "scrollX": 0, "scrollY": 0,
            "documentWidth": 1200, "documentHeight": 4000,
            "captureRegions": [
                {"x": 0, "y": 0, "width": 1200, "height": 800},
                {"x": 0, "y": 3000, "width": 1200, "height": 800},
            ],
        },
        "elements": [],
        "marks": [{"type": "rect", "x": 10, "y": 10, "width": 100, "height": 60}],
        "svg": "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
    })
    .to_string();

    let (status, body) =
        raw_post_json(running.port, "/__collab__/overlay/feedback", &payload).await;

    assert_eq!(status, 500, "body: {body}");
    assert!(body.contains("snapshot-failed"), "body: {body}");
    let feedback_dir = collab::feedback::feedback_dir(&root);
    let leftovers = std::fs::read_dir(&feedback_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "partial capture left artifacts behind");
}

#[tokio::test]
async fn painting_rejects_symlinked_feedback_directory_without_writing_target() {
    use collab::webview::WebviewCommand;
    use std::os::unix::fs::symlink;

    let root = temp_root("painting-directory-symlink");
    let target = root.join("attacker-controlled");
    std::fs::create_dir_all(&target).unwrap();
    let victim = target.join("victim.txt");
    let original = b"painting directory victim";
    std::fs::write(&victim, original).unwrap();
    std::fs::create_dir_all(root.join(collab::session::SESSION_DIR)).unwrap();
    symlink(&target, collab::feedback::feedback_dir(&root)).unwrap();
    let (running, mut commands) = start_server_manual(root.clone(), "test-token".into()).await;
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            match command {
                WebviewCommand::SetCollaborationActive { respond, .. }
                | WebviewCommand::SyncFeedbackMarkers { respond, .. }
                | WebviewCommand::ToggleOfflinePaint { respond } => {
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::CapturePainting {
                    output_paths,
                    respond,
                    ..
                } => {
                    for path in &output_paths {
                        std::fs::write(path, b"\x89PNG-stub").unwrap();
                    }
                    let _ = respond.send(Ok(output_paths));
                }
                _ => {}
            }
        }
    });
    attach_agent(running.port, "test-token", "codex").await;
    let payload = serde_json::json!({
        "kind": "painting",
        "text": "must reject symlinked feedback directory",
        "pageUrl": "http://127.0.0.1/",
        "viewport": {
            "width": 800, "height": 600, "scrollX": 0, "scrollY": 0,
            "documentWidth": 800, "documentHeight": 1200,
            "captureRegions": [{"x": 0, "y": 0, "width": 800, "height": 600}],
        },
        "elements": [],
        "marks": [{"type": "line", "x": 1, "y": 2}],
        "svg": "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
    })
    .to_string();

    let (status, body) =
        raw_post_json(running.port, "/__collab__/overlay/feedback", &payload).await;

    assert_eq!(status, 500, "{body}");
    assert_eq!(std::fs::read(&victim).unwrap(), original);
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
}

/// Task 3.3：list 與 reconcile endpoints——只翻 orphaned 旗標、payload 原樣保留。
#[tokio::test]
async fn feedback_list_and_reconcile_preserve_payload() {
    let root = temp_root("reconcile");
    let token = "test-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;
    attach_agent(running.port, token, "codex").await;

    let valid = serde_json::json!({
        "kind": "element-comment",
        "text": "keep me",
        "pageUrl": "http://127.0.0.1/",
        "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        "elements": [{"selector": "#gone", "tag": "p", "attributes": {"id": "gone"}}],
    })
    .to_string();
    let (status, body) = raw_post_json(running.port, "/__collab__/overlay/feedback", &valid).await;
    assert_eq!(status, 200);
    let id =
        serde_json::from_str::<serde_json::Value>(body.split("\r\n\r\n").nth(1).unwrap().trim())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

    // list 回傳 pending 項目。
    let (status, body) =
        raw_request(running.port, "GET", "/__collab__/overlay/feedback", None).await;
    assert_eq!(status, 200);
    assert!(body.contains(&id));

    // reconcile → orphaned=true，state 與 payload 不變。
    let reconcile = serde_json::json!({"id": id, "orphaned": true}).to_string();
    let (status, _) = raw_post_json(
        running.port,
        "/__collab__/overlay/feedback/reconcile",
        &reconcile,
    )
    .await;
    assert_eq!(status, 200);
    let record = collab::feedback::read_record(&root, &id).unwrap();
    assert!(record.orphaned);
    assert_eq!(
        record.state, "pending",
        "reconcile must not change lifecycle state"
    );
    assert_eq!(record.text, "keep me");
    assert_eq!(record.elements[0]["selector"], "#gone");

    // 未知 id → 404。
    let missing = serde_json::json!({"id": "fb-0-deadbeef", "orphaned": true}).to_string();
    let (status, body) = raw_post_json(
        running.port,
        "/__collab__/overlay/feedback/reconcile",
        &missing,
    )
    .await;
    assert_eq!(status, 404);
    assert!(body.contains("feedback-not-found"));
}

#[tokio::test]
async fn feedback_id_entry_points_reject_unsafe_values() {
    let root = temp_root("unsafe-feedback-id");
    let token = "unsafe-feedback-id-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;

    let mut escaped_record = collab::feedback::prepare(
        collab::feedback::validate(serde_json::json!({
            "kind": "textbox",
            "text": "must stay unchanged",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        }))
        .unwrap(),
    );
    escaped_record.id = "../session".into();
    std::fs::create_dir_all(root.join(".collab/feedback")).unwrap();
    let escaped_path = root.join(".collab/session.json");
    let escaped_body = serde_json::to_vec_pretty(&escaped_record).unwrap();
    std::fs::write(&escaped_path, &escaped_body).unwrap();

    for id in ["../session", "/etc/passwd", "fb-1/../../x"] {
        let payload = serde_json::json!({"id": id, "orphaned": true}).to_string();
        let (status, body) = raw_post_json(
            running.port,
            "/__collab__/overlay/feedback/reconcile",
            &payload,
        )
        .await;
        assert_eq!(status, 400, "unexpected response for {id}: {body}");
        assert_eq!(response_json(&body)["code"], "invalid-feedback-id");
    }
    assert_eq!(std::fs::read(&escaped_path).unwrap(), escaped_body);

    let (status, body) = raw_request(
        running.port,
        "GET",
        "/__collab__/control/feedback/not-an-id",
        Some(token),
    )
    .await;
    assert_eq!(status, 400, "unexpected show response: {body}");
    assert_eq!(response_json(&body)["code"], "invalid-feedback-id");

    let (status, body) = raw_control_json(
        running.port,
        "/__collab__/control/feedback/not-an-id/state",
        token,
        "{}",
    )
    .await;
    assert_eq!(status, 400, "unexpected set-state response: {body}");
    assert_eq!(response_json(&body)["code"], "invalid-feedback-id");
}

#[tokio::test]
async fn feedback_control_endpoints_enforce_compare_and_lease_owner() {
    let root = temp_root("feedback-control");
    let token = "feedback-control-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;

    let attach = serde_json::json!({
        "agentKind": "codex",
        "pid": std::process::id(),
    })
    .to_string();
    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/attach", token, &attach).await;
    assert_eq!(status, 200);
    let attachment_id =
        serde_json::from_str::<serde_json::Value>(body.split("\r\n\r\n").nth(1).unwrap().trim())
            .unwrap()["attachment"]["attachmentId"]
            .as_str()
            .unwrap()
            .to_string();

    let payload = serde_json::json!({
        "kind": "textbox",
        "text": "update the headline",
        "pageUrl": "http://127.0.0.1/",
        "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
    })
    .to_string();
    let (status, body) =
        raw_post_json(running.port, "/__collab__/overlay/feedback", &payload).await;
    assert_eq!(status, 200);
    let feedback_id =
        serde_json::from_str::<serde_json::Value>(body.split("\r\n\r\n").nth(1).unwrap().trim())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

    let lease = serde_json::json!({ "attachmentId": attachment_id }).to_string();
    let (status, body) = raw_control_json(
        running.port,
        "/__collab__/control/feedback/lease",
        token,
        &lease,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains(&feedback_id));

    let conflict = serde_json::json!({
        "attachmentId": attachment_id,
        "expectedState": "acknowledged",
        "state": "working",
    })
    .to_string();
    let (status, body) = raw_control_json(
        running.port,
        &format!("/__collab__/control/feedback/{feedback_id}/state"),
        token,
        &conflict,
    )
    .await;
    assert_eq!(status, 409);
    assert!(body.contains("state-conflict"));

    let acknowledge = serde_json::json!({
        "attachmentId": attachment_id,
        "expectedState": "pending",
        "state": "acknowledged",
    })
    .to_string();
    let (status, body) = raw_control_json(
        running.port,
        &format!("/__collab__/control/feedback/{feedback_id}/state"),
        token,
        &acknowledge,
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    assert!(body.contains("\"acknowledged\""));

    let (status, body) = raw_request(
        running.port,
        "GET",
        &format!("/__collab__/control/feedback/{feedback_id}"),
        Some(token),
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains("\"acknowledged\""));
}

#[tokio::test]
async fn detach_supports_selected_single_zero_and_ambiguous_active_attachments() {
    let root = temp_root("detach-selection");
    let token = "detach-selection-token";
    let (running, _commands) = start_server(root, token.into()).await;

    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/detach", token, "{}").await;
    assert_eq!(status, 200);
    let zero = response_json(&body);
    assert_eq!(zero["status"], "already-detached");
    assert_eq!(zero["activeAttachmentCount"], 0);
    assert!(zero.get("attachmentId").is_none());

    let only = attach_agent(running.port, token, "only").await;
    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/detach", token, "{}").await;
    assert_eq!(status, 200);
    let single = response_json(&body);
    assert_eq!(single["status"], "detached");
    assert_eq!(single["attachmentId"], only);
    assert_eq!(single["activeAttachmentCount"], 0);

    let repeat = serde_json::json!({ "attachmentId": only }).to_string();
    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/detach", token, &repeat).await;
    assert_eq!(status, 200);
    assert_eq!(response_json(&body)["status"], "already-detached");

    let first = attach_agent(running.port, token, "first").await;
    let second = attach_agent(running.port, token, "second").await;
    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/detach", token, "{}").await;
    assert_eq!(status, 409);
    let ambiguous = response_json(&body);
    assert_eq!(ambiguous["code"], "ambiguous-attachment");
    let candidates = ambiguous["details"]["candidateAttachmentIds"]
        .as_array()
        .unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().any(|candidate| candidate == &first));
    assert!(candidates.iter().any(|candidate| candidate == &second));

    let selected = serde_json::json!({ "attachmentId": first }).to_string();
    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/detach", token, &selected).await;
    assert_eq!(status, 200);
    let selected = response_json(&body);
    assert_eq!(selected["status"], "detached");
    assert_eq!(selected["activeAttachmentCount"], 1);

    let (status, body) = raw_request(
        running.port,
        "GET",
        "/__collab__/control/status",
        Some(token),
    )
    .await;
    assert_eq!(status, 200);
    let status = response_json(&body);
    let attachments = status["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 3);
    assert_eq!(
        attachments
            .iter()
            .find(|attachment| attachment["attachmentId"] == first)
            .unwrap()["active"],
        false
    );
    assert_eq!(
        attachments
            .iter()
            .find(|attachment| attachment["attachmentId"] == second)
            .unwrap()["active"],
        true
    );
    let (health, _) = raw_request(running.port, "GET", "/__collab__/health", None).await;
    assert_eq!(health, 200, "detach must preserve the preview server");
}

#[tokio::test]
async fn connect_after_stop_creates_a_new_attachment_and_reactivates_dashboard() {
    let root = temp_root("connect-after-stop");
    let token = "connect-after-stop-token";
    let (running, _commands) = start_server(root, token.into()).await;
    let mut snapshots = running.dashboard.snapshots.clone();

    let old_attachment = attach_agent(running.port, token, "codex").await;
    let stop = serde_json::json!({ "attachmentId": old_attachment }).to_string();
    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/detach", token, &stop).await;
    assert_eq!(status, 200, "{body}");
    snapshots.changed().await.unwrap();
    let stopped = snapshots.borrow_and_update().clone();
    assert!(stopped.attachments.is_empty());
    assert_eq!(stopped.preview_session_id, "test-session");

    let new_attachment = attach_agent(running.port, token, "codex").await;
    assert_ne!(new_attachment, old_attachment);
    snapshots.changed().await.unwrap();
    let connected = snapshots.borrow_and_update().clone();
    assert_eq!(connected.attachments.len(), 1);
    assert_eq!(connected.attachments[0].attachment_id, new_attachment);
    assert_eq!(
        connected.attachments[0].collaboration_state,
        collab::core::CollaborationState::Active
    );

    let (status, body) = raw_request(
        running.port,
        "GET",
        "/__collab__/control/status",
        Some(token),
    )
    .await;
    assert_eq!(status, 200);
    let attachments = response_json(&body)["attachments"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(attachments.len(), 2);
    assert_eq!(attachments[0]["attachmentId"], old_attachment);
    assert_eq!(attachments[0]["active"], false);
    assert_eq!(attachments[1]["attachmentId"], new_attachment);
    assert_eq!(attachments[1]["active"], true);
}

#[tokio::test]
async fn inactive_attachment_cannot_wait_lease_or_mutate_feedback() {
    let root = temp_root("inactive-attachment");
    let token = "inactive-attachment-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;
    let attachment = attach_agent(running.port, token, "codex").await;

    let feedback = collab::feedback::store(
        &root,
        collab::feedback::validate(serde_json::json!({
            "kind": "textbox",
            "text": "keep pending",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        }))
        .unwrap(),
    )
    .unwrap();
    let detach = serde_json::json!({ "attachmentId": attachment }).to_string();
    let (status, _) =
        raw_control_json(running.port, "/__collab__/control/detach", token, &detach).await;
    assert_eq!(status, 200);

    let wait = serde_json::json!({ "attachmentId": attachment }).to_string();
    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/wait", token, &wait).await;
    assert_eq!(status, 200);
    assert_eq!(response_json(&body)["event"], "collaboration.stop");

    let (status, body) = raw_control_json(
        running.port,
        "/__collab__/control/feedback/lease",
        token,
        &wait,
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(response_json(&body)["code"], "attachment-inactive");

    let mutation = serde_json::json!({
        "attachmentId": attachment,
        "expectedState": "pending",
        "state": "acknowledged",
    })
    .to_string();
    let (status, body) = raw_control_json(
        running.port,
        &format!("/__collab__/control/feedback/{}/state", feedback.id),
        token,
        &mutation,
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(response_json(&body)["code"], "attachment-inactive");
    let unchanged = collab::feedback::read_record(&root, &feedback.id).unwrap();
    assert_eq!(unchanged.state, "pending");
    assert!(unchanged.lease.is_none());
}

#[tokio::test]
async fn first_attach_and_last_detach_drive_bounded_overlay_active_commands() {
    let root = temp_root("overlay-active-commands");
    let token = "overlay-active-commands-token";
    let (running, mut commands) = start_server_manual(root, token.into()).await;

    let first_attach = tokio::spawn(attach_agent(running.port, token, "first"));
    complete_active_command(&mut commands, true).await;
    let first = first_attach.await.unwrap();

    let second = attach_agent(running.port, token, "second").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), commands.recv())
            .await
            .is_err(),
        "second active attachment must not issue another activation command"
    );

    let detach_first = serde_json::json!({ "attachmentId": first }).to_string();
    let port = running.port;
    let first_detach = tokio::spawn(async move {
        raw_control_json(port, "/__collab__/control/detach", token, &detach_first).await
    });
    complete_marker_command(&mut commands).await;
    let (status, _) = first_detach.await.unwrap();
    assert_eq!(status, 200);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), commands.recv())
            .await
            .is_err(),
        "detaching a non-final attachment must keep the overlay active"
    );

    let detach_second = serde_json::json!({ "attachmentId": second }).to_string();
    let port = running.port;
    let last_detach = tokio::spawn(async move {
        raw_control_json(port, "/__collab__/control/detach", token, &detach_second).await
    });
    complete_active_command(&mut commands, false).await;
    complete_marker_command(&mut commands).await;
    let (status, _) = last_detach.await.unwrap();
    assert_eq!(status, 200);

    let reattach = tokio::spawn(attach_agent(running.port, token, "reattached"));
    complete_active_command(&mut commands, true).await;
    reattach.await.unwrap();
}

#[tokio::test]
async fn failed_overlay_activation_does_not_register_attachment() {
    use collab::webview::{CommandError, WebviewCommand};

    let root = temp_root("overlay-activation-failure");
    let token = "overlay-activation-failure-token";
    let (running, mut commands) = start_server_manual(root, token.into()).await;
    let port = running.port;
    let attach = tokio::spawn(async move {
        let payload = serde_json::json!({
            "agentKind": "codex",
            "pid": std::process::id(),
        })
        .to_string();
        raw_control_json(port, "/__collab__/control/attach", token, &payload).await
    });
    let WebviewCommand::SetCollaborationActive {
        active: true,
        respond,
    } = commands.recv().await.unwrap()
    else {
        panic!("expected collaboration activation command");
    };
    respond
        .send(Err(CommandError::JavascriptError("overlay failed".into())))
        .unwrap();

    let (status, body) = attach.await.unwrap();

    assert_eq!(status, 422);
    assert_eq!(response_json(&body)["code"], "javascript-error");
    let (status, body) = raw_request(
        running.port,
        "GET",
        "/__collab__/control/status",
        Some(token),
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        response_json(&body)["attachments"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn full_overlay_command_queue_keeps_last_attachment_active_and_returns_busy() {
    use collab::webview::{COMMAND_QUEUE_CAPACITY, WebviewCommand, command_channel};

    let root = temp_root("overlay-busy");
    let token = "overlay-busy-token";
    let (commands, mut receiver) = command_channel();
    let running = server::start(server::ServerConfig {
        project_root: root,
        session_id: "overlay-busy-session".into(),
        token: token.into(),
        commands: commands.clone(),
    })
    .await
    .unwrap();

    let attach = tokio::spawn(attach_agent(running.port, token, "codex"));
    complete_active_command(&mut receiver, true).await;
    let attachment = attach.await.unwrap();
    for _ in 0..COMMAND_QUEUE_CAPACITY {
        let (respond, _receive) = tokio::sync::oneshot::channel();
        commands
            .try_send(WebviewCommand::Reload { respond })
            .expect("queue should fill to capacity");
    }

    let detach = serde_json::json!({ "attachmentId": attachment }).to_string();
    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/detach", token, &detach).await;
    assert_eq!(status, 503);
    assert_eq!(response_json(&body)["code"], "busy");

    let (status, body) = raw_request(
        running.port,
        "GET",
        "/__collab__/control/status",
        Some(token),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(response_json(&body)["attachments"][0]["active"], true);
}

#[tokio::test]
async fn close_wakes_all_attachment_waiters_then_shuts_down_runtime() {
    let root = temp_root("close-waiters");
    let token = "close-waiters-token";
    let (running, _commands) = start_server(root, token.into()).await;
    let first = attach_agent(running.port, token, "first").await;
    let second = attach_agent(running.port, token, "second").await;

    let first_wait = tokio::spawn({
        let body = serde_json::json!({ "attachmentId": first }).to_string();
        async move { raw_control_json(running.port, "/__collab__/control/wait", token, &body).await }
    });
    let second_wait = tokio::spawn({
        let body = serde_json::json!({ "attachmentId": second }).to_string();
        async move { raw_control_json(running.port, "/__collab__/control/wait", token, &body).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/close", token, "{}").await;
    assert_eq!(status, 200);
    assert_eq!(response_json(&body)["status"], "closing");

    for waiter in [first_wait, second_wait] {
        let (status, body) = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("waiter was not awakened")
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(response_json(&body)["event"], "collaboration.stop");
    }
    tokio::time::timeout(Duration::from_secs(5), running.task)
        .await
        .expect("server did not shut down after close")
        .expect("server task panicked")
        .expect("server returned io error");
}

#[tokio::test]
async fn close_releases_feedback_lease_for_immediate_reacquisition_after_restart() {
    let root = temp_root("close-lease-release");
    let token = "close-lease-release-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;
    let owner = attach_agent(running.port, token, "codex").await;

    let feedback = collab::feedback::store(
        &root,
        collab::feedback::validate(serde_json::json!({
            "kind": "textbox",
            "text": "release this lease on close",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        }))
        .unwrap(),
    )
    .unwrap();
    let lease = serde_json::json!({ "attachmentId": owner }).to_string();
    let (status, body) = raw_control_json(
        running.port,
        "/__collab__/control/feedback/lease",
        token,
        &lease,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains(&feedback.id));
    collab::feedback::transition(&root, &feedback.id, "pending", "acknowledged", &owner, None)
        .unwrap();
    collab::feedback::transition(&root, &feedback.id, "acknowledged", "working", &owner, None)
        .unwrap();

    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/close", token, "{}").await;
    assert_eq!(status, 200);
    assert_eq!(response_json(&body)["status"], "closing");
    tokio::time::timeout(Duration::from_secs(5), running.task)
        .await
        .expect("server did not shut down after close")
        .expect("server task panicked")
        .expect("server returned io error");

    let after_close = collab::feedback::read_record(&root, &feedback.id).unwrap();
    assert_eq!(
        after_close.state, "pending",
        "feedback must be pending after preview close"
    );
    assert!(
        after_close.lease.is_none(),
        "lease must be cleared after preview close"
    );
    assert_eq!(
        after_close
            .recovery
            .as_ref()
            .and_then(|recovery| recovery.previous_owner.as_deref()),
        Some(owner.as_str())
    );

    let restart_token = "close-lease-release-restart-token";
    let (restarted, _commands) = start_server(root, restart_token.into()).await;
    let new_owner = attach_agent(restarted.port, restart_token, "new-agent").await;
    let new_lease = serde_json::json!({ "attachmentId": new_owner }).to_string();
    let (status, body) = raw_control_json(
        restarted.port,
        "/__collab__/control/feedback/lease",
        restart_token,
        &new_lease,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        response_json(&body)["item"]["id"],
        feedback.id,
        "new attachment must acquire the same feedback without advancing clock"
    );
}

#[tokio::test]
async fn resource_metrics_and_attachment_registry_stay_bounded() {
    let root = temp_root("resource-metrics");
    let token = "resource-metrics-token";
    let (running, _commands) = start_server(root, token.into()).await;

    let mut attachment_ids = Vec::new();
    for index in 0..server::ATTACHMENT_CAPACITY {
        let attach = serde_json::json!({
            "agentKind": format!("agent-{index}"),
            "pid": std::process::id(),
        })
        .to_string();
        let (status, body) =
            raw_control_json(running.port, "/__collab__/control/attach", token, &attach).await;
        assert_eq!(status, 200);
        attachment_ids.push(
            response_json(&body)["attachment"]["attachmentId"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let overflow = serde_json::json!({
        "agentKind": "overflow",
        "pid": std::process::id(),
    })
    .to_string();
    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/attach", token, &overflow).await;
    assert_eq!(status, 503);
    assert_eq!(response_json(&body)["code"], "attachment-capacity");

    let (status, body) = raw_request(
        running.port,
        "GET",
        "/__collab__/control/status",
        Some(token),
    )
    .await;
    assert_eq!(status, 200);
    let full = response_json(&body);
    let retained = full["attachments"].as_array().unwrap();
    assert_eq!(retained.len(), server::ATTACHMENT_CAPACITY);
    assert!(
        retained
            .iter()
            .all(|attachment| attachment["active"] == true)
    );
    assert!(
        retained
            .iter()
            .all(|attachment| attachment["agentKind"] != "overflow")
    );

    for attachment_id in attachment_ids.iter().take(3) {
        let detach = serde_json::json!({ "attachmentId": attachment_id }).to_string();
        let (status, _) =
            raw_control_json(running.port, "/__collab__/control/detach", token, &detach).await;
        assert_eq!(status, 200);
    }
    for index in 0..3 {
        attach_agent(running.port, token, &format!("replacement-{index}")).await;
    }

    let (status, body) = raw_request(
        running.port,
        "GET",
        "/__collab__/control/status",
        Some(token),
    )
    .await;
    assert_eq!(status, 200);
    let status_json: serde_json::Value =
        serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap().trim()).unwrap();
    let attachments = status_json["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), server::ATTACHMENT_CAPACITY);
    assert_eq!(attachments.first().unwrap()["agentKind"], "agent-3");
    assert!(
        attachments
            .iter()
            .all(|attachment| attachment["active"] == true)
    );

    let (status, body) = raw_request(running.port, "GET", "/__collab__/metrics", None).await;
    assert_eq!(status, 200);
    let metrics: serde_json::Value =
        serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap().trim()).unwrap();
    assert_eq!(metrics["attachmentCount"], server::ATTACHMENT_CAPACITY);
    assert_eq!(
        metrics["activeAttachmentCount"],
        server::ATTACHMENT_CAPACITY
    );
    assert_eq!(metrics["feedbackMemoryItems"], 0);
    assert_eq!(metrics["consoleItems"], 0);
    assert!(
        metrics["webviewCommandQueued"].as_u64().unwrap()
            <= metrics["webviewCommandCapacity"].as_u64().unwrap()
    );
}

#[tokio::test]
async fn detach_releases_feedback_lease_for_immediate_reacquisition() {
    let root = temp_root("detach-lease-release");
    let token = "detach-lease-release-token";
    let (running, _commands) = start_server(root.clone(), token.into()).await;

    let owner = attach_agent(running.port, token, "codex").await;

    let fb = collab::feedback::store(
        &root,
        collab::feedback::validate(serde_json::json!({
            "kind": "textbox",
            "text": "fix the button",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        }))
        .unwrap(),
    )
    .unwrap();

    let lease = serde_json::json!({ "attachmentId": owner }).to_string();
    let (status, body) = raw_control_json(
        running.port,
        "/__collab__/control/feedback/lease",
        token,
        &lease,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains(&fb.id));

    collab::feedback::transition(&root, &fb.id, "pending", "acknowledged", &owner, None).unwrap();
    collab::feedback::transition(&root, &fb.id, "acknowledged", "working", &owner, None).unwrap();

    let detach = serde_json::json!({ "attachmentId": owner }).to_string();
    let (status, body) =
        raw_control_json(running.port, "/__collab__/control/detach", token, &detach).await;
    assert_eq!(status, 200);
    assert_eq!(response_json(&body)["status"], "detached");

    let after_detach = collab::feedback::read_record(&root, &fb.id).unwrap();
    assert_eq!(
        after_detach.state, "pending",
        "feedback must be pending after owner detach"
    );
    assert!(
        after_detach.lease.is_none(),
        "lease must be cleared after owner detach"
    );
    let recovery = after_detach
        .recovery
        .as_ref()
        .expect("recovery metadata must exist");
    assert_eq!(recovery.previous_owner.as_deref(), Some(owner.as_str()));
    let recovery_count_after_detach = recovery.count;

    let new_owner = attach_agent(running.port, token, "new-agent").await;
    let new_lease = serde_json::json!({ "attachmentId": new_owner }).to_string();
    let (status, body) = raw_control_json(
        running.port,
        "/__collab__/control/feedback/lease",
        token,
        &new_lease,
    )
    .await;
    assert_eq!(status, 200);
    let leased_item = response_json(&body);
    assert_eq!(
        leased_item["item"]["id"], fb.id,
        "new attachment must acquire the same feedback without advancing clock"
    );

    let repeat_detach = serde_json::json!({ "attachmentId": owner }).to_string();
    let (status, body) = raw_control_json(
        running.port,
        "/__collab__/control/detach",
        token,
        &repeat_detach,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(response_json(&body)["status"], "already-detached");

    let after_repeat = collab::feedback::read_record(&root, &fb.id).unwrap();
    assert_eq!(
        after_repeat.recovery.as_ref().map(|r| r.count).unwrap_or(0),
        recovery_count_after_detach,
        "already-detached must not increase recovery count"
    );
}

#[test]
fn session_file_is_permission_restricted_and_round_trips() {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root("session");
    let written = SessionFile::new(
        root.clone(),
        root.join("index.html"),
        8080,
        session::generate_token(),
    );
    let path = session::write_session_file(&written).unwrap();

    let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        file_mode, 0o600,
        "session.json must be user read/write only"
    );

    let dir_mode = std::fs::metadata(root.join(session::SESSION_DIR))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, ".collab dir must be user-only");

    let read = session::read_session_file(&root).unwrap();
    assert_eq!(read, written);

    session::remove_session_file(&root).unwrap();
    assert!(session::read_session_file(&root).is_err());
}
