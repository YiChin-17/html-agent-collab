//! Task 5.2：無任何 MCP 設定時，CLI adapter 仍涵蓋完整 core workflow。

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use collab::server::{self, ServerConfig};
use collab::session::{self, SessionFile};
use collab::webview::WebviewCommand;
use serde_json::Value;

static TEMP_DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_root() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "collab-no-mcp-{}-{}",
        std::process::id(),
        TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn collab_without_mcp(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_collab"));
    command.args(args);
    for key in [
        "MCP_CONFIG",
        "MCP_SERVERS",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
    ] {
        command.env_remove(key);
    }
    command.output().unwrap()
}

fn envelope(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn set_state(project: &str, attachment: &str, feedback: &str, expected: &str, state: &str) {
    let output = collab_without_mcp(&[
        "feedback",
        "set-state",
        "--project",
        project,
        feedback,
        state,
        "--expected",
        expected,
        "--attachment",
        attachment,
    ]);
    assert!(output.status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn clean_environment_runs_the_full_cli_core_workflow() {
    let root = temp_root();
    let token = session::generate_token();
    let (commands, mut receiver) = collab::webview::command_channel();
    let server = server::start(ServerConfig {
        project_root: root.clone(),
        session_id: "sess-no-mcp".into(),
        token: token.clone(),
        commands,
    })
    .await
    .unwrap();
    let mut session = SessionFile::new(root.clone(), root.join("index.html"), server.port, token);
    session.session_id = "sess-no-mcp".into();
    session::write_session_file(&session).unwrap();
    let project = root.to_str().unwrap();
    let snapshot_path = root.join(".collab/screenshots/no-mcp.png");
    let expected_snapshot = snapshot_path.clone();

    tokio::spawn(async move {
        while let Some(command) = receiver.recv().await {
            match command {
                WebviewCommand::SetCollaborationActive { respond, .. } => {
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::SyncFeedbackMarkers { respond, .. } => {
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::ToggleOfflinePaint { respond } => {
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::CapturePainting { respond, .. } => {
                    let _ = respond.send(Err(collab::webview::CommandError::SnapshotFailed(
                        "painting capture is not used by this test".into(),
                    )));
                }
                WebviewCommand::Eval {
                    expression,
                    respond,
                } => {
                    let _ = respond.send(Ok(serde_json::json!({
                        "expression": expression,
                        "mcp": false
                    })));
                }
                WebviewCommand::Snapshot { respond, .. } => {
                    let _ = respond.send(Ok(expected_snapshot.clone()));
                }
                WebviewCommand::Reload { respond } => {
                    let _ = respond.send(Ok(()));
                }
            }
        }
    });

    let attach = collab_without_mcp(&["attach", "--project", project, "--agent", "codex"]);
    assert!(attach.status.success());
    let attachment = envelope(&attach)["data"]["attachment"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_string();

    let status = collab_without_mcp(&["status", "--project", project]);
    assert!(status.status.success());
    assert_eq!(envelope(&status)["data"]["sessionId"], "sess-no-mcp");

    let submitted: Value = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/__collab__/overlay/feedback",
            server.port
        ))
        .json(&serde_json::json!({
            "kind": "textbox",
            "text": "clean environment request",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let feedback = submitted["id"].as_str().unwrap();

    let wait = collab_without_mcp(&[
        "wait",
        "--project",
        project,
        "--attachment",
        &attachment,
        "--json",
    ]);
    assert!(wait.status.success());
    assert_eq!(envelope(&wait)["data"]["item"]["id"], feedback);

    let show = collab_without_mcp(&["feedback", "show", "--project", project, feedback]);
    assert!(show.status.success());
    set_state(project, &attachment, feedback, "pending", "acknowledged");
    set_state(project, &attachment, feedback, "acknowledged", "working");

    let eval = collab_without_mcp(&["eval", "--project", project, "document.title"]);
    assert!(eval.status.success());
    assert_eq!(envelope(&eval)["data"]["value"]["mcp"], false);

    let screenshot = collab_without_mcp(&["screenshot", "--project", project]);
    assert!(screenshot.status.success());
    assert_eq!(
        envelope(&screenshot)["data"]["path"].as_str().unwrap(),
        snapshot_path.to_str().unwrap()
    );

    set_state(project, &attachment, feedback, "working", "resolved");
    let detach = collab_without_mcp(&["detach", "--project", project, "--attachment", &attachment]);
    assert!(detach.status.success());
    let status = collab_without_mcp(&["status", "--project", project]);
    assert!(status.status.success());
    let close = collab_without_mcp(&["close", "--project", project]);
    assert!(close.status.success());
}
