//! Task 5.1 scripted scenarios：同一 attachment 連續處理兩筆 feedback，
//! 第二筆 verification failure 必須落為 failed。

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use collab::server::{self, ServerConfig};
use collab::session::{self, SessionFile};
use serde_json::Value;

static TEMP_DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_root() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "collab-loop-scenario-{}-{}",
        std::process::id(),
        TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn collab(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_collab"))
        .args(args)
        .output()
        .unwrap()
}

fn envelope(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn set_state(
    project: &str,
    attachment: &str,
    feedback: &str,
    expected: &str,
    state: &str,
    reason: Option<&str>,
) {
    let mut args = vec![
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
    ];
    if let Some(reason) = reason {
        args.extend(["--reason", reason]);
    }
    let output = collab(&args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

async fn submit(port: u16, text: &str) -> String {
    reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/__collab__/overlay/feedback"
        ))
        .json(&serde_json::json!({
            "kind": "textbox",
            "text": text,
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn file_hash(path: &std::path::Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    std::fs::read(path).unwrap().hash(&mut hasher);
    hasher.finish()
}

async fn submit_preview_draft(port: u16) -> Value {
    let before = "<!doctype html><html><head><title>Draft</title></head><body><main id=\"hero\"><h1>Hello</h1></main></body></html>";
    let after = "<!doctype html><html><head><title>Draft</title></head><body><main id=\"hero\"><h1>Welcome</h1></main></body></html>";
    reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{port}/__collab__/overlay/feedback"
        ))
        .json(&serde_json::json!({
            "kind": "preview-draft",
            "text": "Apply Preview Draft",
            "pageUrl": format!("http://127.0.0.1:{port}/index.html"),
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
            "elements": [{"selector": "#hero > h1", "tag": "h1"}],
            "previewDraft": {
                "selector": "#hero > h1",
                "beforeHtml": before,
                "afterHtml": after,
            },
        }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn preview_draft_submission_preserves_fixture_and_stays_pending_across_reload_boundary() {
    let root = temp_root();
    let fixture = root.join("index.html");
    std::fs::write(
        &fixture,
        "<!doctype html><html><head><title>Draft</title></head><body><main id=\"hero\"><h1>Hello</h1></main></body></html>",
    )
    .unwrap();
    let original_hash = file_hash(&fixture);
    let token = session::generate_token();
    let (commands, mut receiver) = collab::webview::command_channel();
    tokio::spawn(async move {
        while let Some(command) = receiver.recv().await {
            match command {
                collab::webview::WebviewCommand::SetCollaborationActive { respond, .. }
                | collab::webview::WebviewCommand::SyncFeedbackMarkers { respond, .. } => {
                    let _ = respond.send(Ok(()));
                }
                _ => {}
            }
        }
    });
    let server = server::start(ServerConfig {
        project_root: root.clone(),
        session_id: "sess-preview-draft".into(),
        token: token.clone(),
        commands,
    })
    .await
    .unwrap();
    let mut session = SessionFile::new(root.clone(), fixture.clone(), server.port, token);
    session.session_id = "sess-preview-draft".into();
    session::write_session_file(&session).unwrap();
    let project = root.to_str().unwrap();

    let attach = collab(&["attach", "--project", project, "--agent", "codex"]);
    assert!(attach.status.success());
    let attachment = envelope(&attach)["data"]["attachment"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_string();

    let submitted = submit_preview_draft(server.port).await;
    let feedback_id = submitted["id"].as_str().unwrap();
    assert_eq!(file_hash(&fixture), original_hash);

    let record = collab::feedback::read_record(&root, feedback_id).unwrap();
    assert_eq!(record.kind, "preview-draft");
    assert_eq!(record.state, "pending");

    let after_reload_boundary = collab::feedback::read_record(&root, feedback_id).unwrap();
    assert_eq!(after_reload_boundary.state, "pending");
    assert_eq!(file_hash(&fixture), original_hash);

    std::fs::write(
        &fixture,
        "<!doctype html><html><head><title>Draft</title></head><body><main id=\"hero\"><h1>Newer source</h1></main></body></html>",
    )
    .unwrap();
    let newer_hash = file_hash(&fixture);
    let wait = collab(&[
        "wait",
        "--project",
        project,
        "--attachment",
        &attachment,
        "--json",
    ]);
    assert_eq!(envelope(&wait)["data"]["item"]["id"], feedback_id);
    set_state(
        project,
        &attachment,
        feedback_id,
        "pending",
        "acknowledged",
        None,
    );
    set_state(
        project,
        &attachment,
        feedback_id,
        "acknowledged",
        "working",
        None,
    );
    set_state(
        project,
        &attachment,
        feedback_id,
        "working",
        "failed",
        Some("current source differs from beforeHtml"),
    );
    let conflicted = collab::feedback::read_record(&root, feedback_id).unwrap();
    assert_eq!(conflicted.state, "failed");
    assert_eq!(
        conflicted.failure_reason.as_deref(),
        Some("current source differs from beforeHtml")
    );
    assert_eq!(file_hash(&fixture), newer_hash);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_scripted_loop_resolves_first_feedback_then_fails_second() {
    let root = temp_root();
    let token = session::generate_token();
    let (commands, mut receiver) = collab::webview::command_channel();
    tokio::spawn(async move {
        while let Some(command) = receiver.recv().await {
            match command {
                collab::webview::WebviewCommand::SetCollaborationActive { respond, .. }
                | collab::webview::WebviewCommand::SyncFeedbackMarkers { respond, .. } => {
                    let _ = respond.send(Ok(()));
                }
                _ => {}
            }
        }
    });
    let server = server::start(ServerConfig {
        project_root: root.clone(),
        session_id: "sess-loop".into(),
        token: token.clone(),
        commands,
    })
    .await
    .unwrap();
    let mut session = SessionFile::new(root.clone(), root.join("index.html"), server.port, token);
    session.session_id = "sess-loop".into();
    session::write_session_file(&session).unwrap();
    let project = root.to_str().unwrap();

    let attach = collab(&["attach", "--project", project, "--agent", "codex"]);
    assert!(attach.status.success());
    let attachment = envelope(&attach)["data"]["attachment"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_string();

    let first = submit(server.port, "first request").await;
    let wait = collab(&[
        "wait",
        "--project",
        project,
        "--attachment",
        &attachment,
        "--json",
    ]);
    assert_eq!(envelope(&wait)["data"]["item"]["id"], first);
    set_state(
        project,
        &attachment,
        &first,
        "pending",
        "acknowledged",
        None,
    );
    set_state(
        project,
        &attachment,
        &first,
        "acknowledged",
        "working",
        None,
    );
    set_state(project, &attachment, &first, "working", "resolved", None);

    let second = submit(server.port, "second request").await;
    let wait = collab(&[
        "wait",
        "--project",
        project,
        "--attachment",
        &attachment,
        "--json",
    ]);
    assert_eq!(envelope(&wait)["data"]["item"]["id"], second);
    set_state(
        project,
        &attachment,
        &second,
        "pending",
        "acknowledged",
        None,
    );
    set_state(
        project,
        &attachment,
        &second,
        "acknowledged",
        "working",
        None,
    );
    set_state(
        project,
        &attachment,
        &second,
        "working",
        "failed",
        Some("screenshot verification mismatch"),
    );

    let first_record = collab::feedback::read_record(&root, &first).unwrap();
    let second_record = collab::feedback::read_record(&root, &second).unwrap();
    assert_eq!(first_record.state, "resolved");
    assert_eq!(second_record.state, "failed");
    assert_eq!(
        second_record.failure_reason.as_deref(),
        Some("screenshot verification mismatch")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn second_feedback_only_delivered_after_first_confirmed_terminal() {
    let root = temp_root();
    let token = session::generate_token();
    let (commands, mut receiver) = collab::webview::command_channel();
    tokio::spawn(async move {
        while let Some(command) = receiver.recv().await {
            match command {
                collab::webview::WebviewCommand::SetCollaborationActive { respond, .. }
                | collab::webview::WebviewCommand::SyncFeedbackMarkers { respond, .. } => {
                    let _ = respond.send(Ok(()));
                }
                _ => {}
            }
        }
    });
    let server = server::start(ServerConfig {
        project_root: root.clone(),
        session_id: "sess-gate".into(),
        token: token.clone(),
        commands,
    })
    .await
    .unwrap();
    let mut session = SessionFile::new(root.clone(), root.join("index.html"), server.port, token);
    session.session_id = "sess-gate".into();
    session::write_session_file(&session).unwrap();
    let project = root.to_str().unwrap();

    let attach = collab(&["attach", "--project", project, "--agent", "codex"]);
    assert!(attach.status.success());
    let attachment = envelope(&attach)["data"]["attachment"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_string();

    let first = submit(server.port, "gate first").await;
    let second = submit(server.port, "gate second").await;

    // Wait delivers the first item.
    let wait = collab(&[
        "wait",
        "--project",
        project,
        "--attachment",
        &attachment,
        "--json",
    ]);
    assert_eq!(envelope(&wait)["data"]["item"]["id"], first);
    set_state(
        project,
        &attachment,
        &first,
        "pending",
        "acknowledged",
        None,
    );
    set_state(
        project,
        &attachment,
        &first,
        "acknowledged",
        "working",
        None,
    );

    // While first is working with an active lease, queue must not deliver second.
    let blocked = collab::feedback::lease_next(&root, &attachment);
    assert!(
        blocked.is_ok_and(|v| v.is_none()),
        "second must not be delivered while first has an active lease"
    );

    // Resolve first → terminal confirmation complete.
    set_state(project, &attachment, &first, "working", "resolved", None);

    // Now the second item should be deliverable.
    let wait = collab(&[
        "wait",
        "--project",
        project,
        "--attachment",
        &attachment,
        "--json",
    ]);
    assert_eq!(envelope(&wait)["data"]["item"]["id"], second);
}

#[tokio::test(flavor = "multi_thread")]
async fn interruption_before_terminal_recovers_to_pending() {
    let root = temp_root();
    let token = session::generate_token();
    let (commands, mut receiver) = collab::webview::command_channel();
    tokio::spawn(async move {
        while let Some(command) = receiver.recv().await {
            match command {
                collab::webview::WebviewCommand::SetCollaborationActive { respond, .. }
                | collab::webview::WebviewCommand::SyncFeedbackMarkers { respond, .. } => {
                    let _ = respond.send(Ok(()));
                }
                _ => {}
            }
        }
    });
    let server = server::start(ServerConfig {
        project_root: root.clone(),
        session_id: "sess-recovery".into(),
        token: token.clone(),
        commands,
    })
    .await
    .unwrap();
    let mut session = SessionFile::new(root.clone(), root.join("index.html"), server.port, token);
    session.session_id = "sess-recovery".into();
    session::write_session_file(&session).unwrap();
    let project = root.to_str().unwrap();

    let attach = collab(&["attach", "--project", project, "--agent", "codex"]);
    assert!(attach.status.success());
    let attachment = envelope(&attach)["data"]["attachment"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_string();

    let item = submit(server.port, "recovery test").await;
    let wait = collab(&[
        "wait",
        "--project",
        project,
        "--attachment",
        &attachment,
        "--json",
    ]);
    assert_eq!(envelope(&wait)["data"]["item"]["id"], item);
    set_state(project, &attachment, &item, "pending", "acknowledged", None);
    set_state(project, &attachment, &item, "acknowledged", "working", None);

    // Simulate crash: release the attachment's leases (as detach would).
    let released = collab::feedback::release_owner_leases(&root, &attachment).unwrap();
    assert_eq!(released, 1);

    // Record should be pending with recovery metadata.
    let record = collab::feedback::read_record(&root, &item).unwrap();
    assert_eq!(record.state, "pending");
    assert!(record.lease.is_none());
    let recovery = record.recovery.unwrap();
    assert_eq!(recovery.previous_state.as_str(), "working");
    assert_eq!(recovery.previous_owner.as_deref(), Some(&*attachment));
}
