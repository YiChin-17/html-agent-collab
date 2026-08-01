//! Task 4.2：interruptible long-poll、stop event 與 SIGINT lease preservation。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use collab::server::{self, RunningServer, ServerConfig};
use collab::session::{self, SessionFile};
use serde_json::Value;

static TEMP_DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "collab-wait-test-{name}-{}-{}",
        std::process::id(),
        TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

async fn start_preview(root: &Path) -> (RunningServer, SessionFile) {
    let token = session::generate_token();
    let (commands, mut receiver) = collab::webview::command_channel();
    tokio::spawn(async move {
        while let Some(command) = receiver.recv().await {
            match command {
                collab::webview::WebviewCommand::SetCollaborationActive { respond, .. } => {
                    let _ = respond.send(Ok(()));
                }
                collab::webview::WebviewCommand::SyncFeedbackMarkers { respond, .. } => {
                    let _ = respond.send(Ok(()));
                }
                collab::webview::WebviewCommand::ToggleOfflinePaint { respond } => {
                    let _ = respond.send(Ok(()));
                }
                collab::webview::WebviewCommand::CapturePainting { respond, .. } => {
                    let _ = respond.send(Err(collab::webview::CommandError::SnapshotFailed(
                        "painting capture is not used by wait tests".into(),
                    )));
                }
                collab::webview::WebviewCommand::Reload { respond } => {
                    let _ = respond.send(Ok(()));
                }
                collab::webview::WebviewCommand::Eval { respond, .. } => {
                    let _ = respond.send(Ok(serde_json::Value::Null));
                }
                collab::webview::WebviewCommand::Snapshot { respond, .. } => {
                    let _ = respond.send(Err(collab::webview::CommandError::Internal(
                        "snapshot is not used by wait tests".into(),
                    )));
                }
            }
        }
    });
    let running = server::start(ServerConfig {
        project_root: root.to_path_buf(),
        session_id: "sess-wait".into(),
        token: token.clone(),
        commands,
    })
    .await
    .unwrap();
    let mut file = SessionFile::new(
        root.to_path_buf(),
        root.join("index.html"),
        running.port,
        token,
    );
    file.session_id = "sess-wait".into();
    session::write_session_file(&file).unwrap();
    (running, file)
}

fn collab(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_collab"))
        .args(args)
        .output()
        .unwrap()
}

fn parse_envelope(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be a JSON envelope: {error}; got {:?}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn attach(project: &str, agent: &str) -> String {
    let output = collab(&["attach", "--project", project, "--agent", agent]);
    assert!(output.status.success());
    parse_envelope(&output)["data"]["attachment"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_string()
}

fn spawn_wait(project: &str, attachment: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_collab"))
        .args([
            "wait",
            "--project",
            project,
            "--attachment",
            attachment,
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn wait_help_documents_the_interrupted_exit_status() {
    let output = collab(&["wait", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("interrupted"));
    assert!(help.contains("130"));
}

#[tokio::test(flavor = "multi_thread")]
async fn wait_returns_submitted_feedback_and_leases_it_once() {
    let root = temp_root("feedback");
    let (_server, session) = start_preview(&root).await;
    let project = root.to_str().unwrap();
    let attachment = attach(project, "codex");
    let child = spawn_wait(project, &attachment);

    tokio::time::sleep(Duration::from_millis(100)).await;
    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/__collab__/overlay/feedback",
            session.port
        ))
        .json(&serde_json::json!({
            "kind": "textbox",
            "text": "make the title shorter",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let submitted: Value = response.json().await.unwrap();

    let output = tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    let envelope = parse_envelope(&output);
    assert_eq!(envelope["data"]["event"], "feedback");
    assert_eq!(envelope["data"]["item"]["id"], submitted["id"]);
    assert_eq!(envelope["data"]["item"]["lease"]["owner"], attachment);
    assert!(
        collab::feedback::lease_next(&root, "another-attachment")
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn detach_wakes_wait_with_collaboration_stop_without_shutting_down_preview() {
    let root = temp_root("stop");
    let (server, session) = start_preview(&root).await;
    let project = root.to_str().unwrap();
    let attachment = attach(project, "claude-code");
    let child = spawn_wait(project, &attachment);

    tokio::time::sleep(Duration::from_millis(100)).await;
    let detach = collab(&["detach", "--project", project, "--attachment", &attachment]);
    assert!(detach.status.success());
    assert_eq!(parse_envelope(&detach)["data"]["status"], "detached");

    let output = tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        parse_envelope(&output)["data"]["event"],
        "collaboration.stop"
    );
    let health = reqwest::Client::new()
        .get(format!(
            "http://127.0.0.1:{}/__collab__/health",
            session.port
        ))
        .send()
        .await
        .unwrap();
    assert!(health.status().is_success());

    let close = collab(&["close", "--project", project]);
    assert!(close.status.success());
    tokio::time::timeout(Duration::from_secs(5), server.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn sigint_returns_130_without_stealing_or_releasing_another_lease() {
    let root = temp_root("sigint");
    let (_server, _session) = start_preview(&root).await;
    let project = root.to_str().unwrap();
    let owner = attach(project, "claude-code");
    let waiting = attach(project, "codex");

    let feedback = collab::feedback::store(
        &root,
        collab::feedback::validate(serde_json::json!({
            "kind": "textbox",
            "text": "keep this queued",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        }))
        .unwrap(),
    )
    .unwrap();
    collab::feedback::lease_next(&root, &owner)
        .unwrap()
        .expect("first attachment should own the lease");

    let child = spawn_wait(project, &waiting);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let pid = child.id();
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGINT) }, 0);

    let output = tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert_eq!(parse_envelope(&output)["data"]["event"], "interrupted");

    let preserved = collab::feedback::read_record(&root, &feedback.id).unwrap();
    assert_eq!(preserved.state, "pending");
    assert_eq!(
        preserved.lease.as_ref().map(|lease| lease.owner.as_str()),
        Some(owner.as_str())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_after_current_keeps_wait_and_attachment_until_resume() {
    let root = temp_root("pause-resume");
    let (_server, session) = start_preview(&root).await;
    let project = root.to_str().unwrap();
    let attachment = attach(project, "codex");

    let feedback_a = collab::feedback::store(
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
    let first_wait = spawn_wait(project, &attachment);
    let first_output = tokio::task::spawn_blocking(move || first_wait.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(first_output.status.success());
    assert_eq!(
        parse_envelope(&first_output)["data"]["item"]["id"],
        feedback_a.id
    );

    for (expected, state) in [("pending", "acknowledged"), ("acknowledged", "working")] {
        let output = collab(&[
            "feedback",
            "set-state",
            "--project",
            project,
            &feedback_a.id,
            state,
            "--expected",
            expected,
            "--attachment",
            &attachment,
        ]);
        assert!(output.status.success(), "stderr: {:?}", output.stderr);
    }

    let pause = collab(&["pause", "--project", project, "--attachment", &attachment]);
    assert!(pause.status.success());
    assert_eq!(parse_envelope(&pause)["data"]["status"], "pause-requested");

    let resolved = collab(&[
        "feedback",
        "set-state",
        "--project",
        project,
        &feedback_a.id,
        "resolved",
        "--expected",
        "working",
        "--attachment",
        &attachment,
    ]);
    assert!(resolved.status.success(), "stderr: {:?}", resolved.stderr);

    let mut paused_wait = spawn_wait(project, &attachment);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(paused_wait.try_wait().unwrap().is_none());

    let rejected = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/__collab__/overlay/feedback",
            session.port
        ))
        .json(&serde_json::json!({
            "kind": "textbox",
            "text": "feedback B before resume",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        rejected.json::<Value>().await.unwrap()["code"],
        "collaboration-paused"
    );
    assert!(paused_wait.try_wait().unwrap().is_none());

    let resume = collab(&["resume", "--project", project, "--attachment", &attachment]);
    assert!(resume.status.success());
    assert_eq!(parse_envelope(&resume)["data"]["attachmentId"], attachment);

    let submitted = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/__collab__/overlay/feedback",
            session.port
        ))
        .json(&serde_json::json!({
            "kind": "textbox",
            "text": "feedback B after resume",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0}
        }))
        .send()
        .await
        .unwrap();
    assert!(submitted.status().is_success());
    let feedback_b: Value = submitted.json().await.unwrap();

    let paused_output = tokio::task::spawn_blocking(move || paused_wait.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(paused_output.status.success());
    let paused_output = parse_envelope(&paused_output);
    assert_eq!(paused_output["data"]["item"]["id"], feedback_b["id"]);
    assert_eq!(paused_output["data"]["item"]["lease"]["owner"], attachment);

    let status = parse_envelope(&collab(&["status", "--project", project]));
    assert_eq!(status["data"]["attachments"].as_array().unwrap().len(), 1);
    assert_eq!(status["data"]["attachments"][0]["attachmentId"], attachment);
}
