//! Task 2.2 驗證：CLI snapshot tests（統一 JSON envelope）與
//! single/multiple session integration tests。
//! 測試在 in-process 啟動 control server 並寫入 session file（pid 為測試
//! process，故視為存活），再以 binary 執行 CLI 驗證 contract。

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU32, Ordering};

use collab::server::{self, RunningServer, ServerConfig};
use collab::session::{self, SessionFile};
use serde_json::Value;

static TEMP_DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "collab-cli-contract-{}-{}-{}",
        name,
        std::process::id(),
        TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("failed to create temp root");
    dir.canonicalize().unwrap()
}

/// 在 in-process runtime 啟動 server 並寫入該 root 的 session file。
/// 回傳的 receiver 讓測試決定 command queue 的消化方式。
async fn start_preview(
    root: &Path,
    session_id: &str,
) -> (RunningServer, SessionFile, tokio::task::JoinHandle<()>) {
    let (running, file, mut receiver) = start_preview_manual(root, session_id).await;
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
                    let _ = respond.send(Ok(Value::Null));
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
    (running, file, consumer)
}

async fn start_preview_manual(
    root: &Path,
    session_id: &str,
) -> (RunningServer, SessionFile, collab::webview::CommandReceiver) {
    let token = session::generate_token();
    let (commands, receiver) = collab::webview::command_channel();
    let running = server::start(ServerConfig {
        project_root: root.to_path_buf(),
        session_id: session_id.to_string(),
        token: token.clone(),
        commands,
    })
    .await
    .expect("failed to start server");
    let mut file = SessionFile::new(
        root.to_path_buf(),
        root.join("index.html"),
        running.port,
        token,
    );
    file.session_id = session_id.to_string();
    session::write_session_file(&file).expect("failed to write session file");
    (running, file, receiver)
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_rejects_registry_with_mismatched_server_identity() {
    let root = temp_root("identity-mismatch");
    let (_server, mut file, _commands) = start_preview(&root, "actual-server-session").await;
    file.session_id = "stale-registry-session".into();
    session::write_session_file(&file).unwrap();

    let discovery_root = root.clone();
    let sessions = tokio::task::spawn_blocking(move || {
        collab::client::discover_live_sessions(&discovery_root)
    })
    .await
    .unwrap();
    assert!(sessions.is_empty());
}

fn collab(args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_collab"))
        .args(args)
        .output()
        .expect("failed to run collab binary")
}

/// stdout 必須是單行統一 envelope：{ok, data, error}。
fn parse_envelope(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not a JSON envelope: {e}; got {stdout:?}"));
    let object = envelope.as_object().expect("envelope must be an object");
    assert!(object.contains_key("ok"), "envelope missing `ok`");
    assert!(object.contains_key("data"), "envelope missing `data`");
    assert!(object.contains_key("error"), "envelope missing `error`");
    envelope
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_has_identical_shape_for_claude_code_and_codex() {
    let root = temp_root("attach");
    let (_server, file, _commands) = start_preview(&root, "sess-attach-1").await;
    let project = root.to_str().unwrap();

    let claude = collab(&["attach", "--project", project, "--agent", "claude-code"]);
    let codex = collab(&[
        "attach",
        "--project",
        project,
        "--agent",
        "codex",
        "--tui-session",
        "codex-tui-42",
    ]);
    assert!(claude.status.success());
    assert!(codex.status.success());

    let claude_env = parse_envelope(&claude);
    let codex_env = parse_envelope(&codex);
    assert_eq!(claude_env["ok"], true);
    assert_eq!(codex_env["ok"], true);
    assert_eq!(claude_env["data"]["previewSessionId"], "sess-attach-1");
    assert_eq!(codex_env["data"]["previewSessionId"], "sess-attach-1");
    assert_eq!(claude_env["data"]["attachment"]["agentKind"], "claude-code");
    assert_eq!(codex_env["data"]["attachment"]["agentKind"], "codex");
    assert_eq!(
        codex_env["data"]["attachment"]["tuiSessionId"],
        "codex-tui-42"
    );

    // 兩個 agent 的 envelope top-level 與 data 欄位形狀一致（無 agent 分岔）。
    let claude_keys: Vec<_> = claude_env["data"].as_object().unwrap().keys().collect();
    let codex_keys: Vec<_> = codex_env["data"].as_object().unwrap().keys().collect();
    assert_eq!(claude_keys, codex_keys);

    // token 不得出現在任何命令輸出。
    for output in [&claude, &codex] {
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!all.contains(&file.token), "attach output leaked token");
    }

    // status 反映兩筆 attachment。
    let status = collab(&["status", "--project", project]);
    assert!(status.status.success());
    let status_env = parse_envelope(&status);
    assert_eq!(status_env["data"]["sessionId"], "sess-attach-1");
    assert_eq!(
        status_env["data"]["attachments"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn missing_preview_returns_preview_not_running_with_recovery() {
    let root = temp_root("no-preview");
    let project = root.to_str().unwrap();

    for args in [
        vec!["attach", "--project", project],
        vec!["status", "--project", project],
        vec!["detach", "--project", project],
        vec!["close", "--project", project],
        vec!["wait", "--project", project],
    ] {
        let output = collab(&args);
        assert!(!output.status.success(), "{args:?} should fail");
        let envelope = parse_envelope(&output);
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["error"]["code"], "preview-not-running", "{args:?}");
        assert!(
            envelope["error"]["message"]
                .as_str()
                .unwrap()
                .contains("collab open"),
            "error must include recovery instruction"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_previews_require_explicit_session_selection() {
    let parent = temp_root("nested");
    let child = parent.join("child");
    std::fs::create_dir_all(&child).unwrap();
    let (_parent_server, _, _parent_commands) = start_preview(&parent, "sess-parent").await;
    let (_child_server, _, _child_commands) = start_preview(&child, "sess-child").await;
    let child_project = child.to_str().unwrap();

    // 從 child 目錄 discovery 會同時匹配 child 與 parent 的 preview。
    let ambiguous = collab(&["status", "--project", child_project]);
    assert!(!ambiguous.status.success());
    let envelope = parse_envelope(&ambiguous);
    assert_eq!(envelope["error"]["code"], "ambiguous-preview");
    let candidates = envelope["error"]["details"]["candidates"]
        .as_array()
        .unwrap();
    assert_eq!(candidates.len(), 2);
    let ids: Vec<_> = candidates
        .iter()
        .map(|c| c["sessionId"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"sess-parent") && ids.contains(&"sess-child"));

    // 明確指定 session 後成功。
    let explicit = collab(&[
        "status",
        "--project",
        child_project,
        "--session",
        "sess-child",
    ]);
    assert!(explicit.status.success());
    let envelope = parse_envelope(&explicit);
    assert_eq!(envelope["data"]["sessionId"], "sess-child");
}

#[tokio::test(flavor = "multi_thread")]
async fn detach_preserves_status_and_close_shuts_down_server_via_cli() {
    let root = temp_root("lifecycle");
    let (server, _, _commands) = start_preview(&root, "sess-lifecycle").await;
    let project = root.to_str().unwrap();

    let attach = collab(&["attach", "--project", project, "--agent", "codex"]);
    assert!(attach.status.success());
    let attachment = parse_envelope(&attach)["data"]["attachment"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_string();

    let detach = collab(&["detach", "--project", project]);
    assert!(detach.status.success());
    let envelope = parse_envelope(&detach);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["status"], "detached");
    assert_eq!(envelope["data"]["attachmentId"], attachment);
    assert_eq!(envelope["data"]["activeAttachmentCount"], 0);

    let status = collab(&["status", "--project", project]);
    assert!(status.status.success());
    let status = parse_envelope(&status);
    assert_eq!(status["data"]["sessionId"], "sess-lifecycle");
    assert_eq!(
        status["data"]["entryFile"],
        root.join("index.html").to_str().unwrap()
    );
    assert_eq!(status["data"]["attachments"][0]["active"], false);

    let close = collab(&["close", "--project", project]);
    assert!(close.status.success());
    let envelope = parse_envelope(&close);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["status"], "closing");

    tokio::time::timeout(std::time::Duration::from_secs(5), server.task)
        .await
        .expect("server did not shut down after CLI close")
        .expect("server task panicked")
        .expect("server io error");

    let status = collab(&["status", "--project", project]);
    assert!(!status.status.success());
    assert_eq!(
        parse_envelope(&status)["error"]["code"],
        "preview-not-running"
    );
}

#[test]
fn lifecycle_help_lists_detach_and_close_but_not_stop() {
    let help = collab(&["--help"]);
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(
        help.lines()
            .any(|line| line.trim_start().starts_with("detach "))
    );
    assert!(
        help.lines()
            .any(|line| line.trim_start().starts_with("close "))
    );
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("stop "))
    );

    let removed = collab(&["stop", "--help"]);
    assert!(!removed.status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn detach_cli_reports_ambiguous_candidates_and_accepts_explicit_selection() {
    let root = temp_root("detach-ambiguous");
    let (_server, _, _commands) = start_preview(&root, "sess-detach-ambiguous").await;
    let project = root.to_str().unwrap();
    let first = collab(&["attach", "--project", project, "--agent", "claude-code"]);
    let second = collab(&["attach", "--project", project, "--agent", "codex"]);
    assert!(first.status.success());
    assert!(second.status.success());
    let first_id = parse_envelope(&first)["data"]["attachment"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_string();

    let ambiguous = collab(&["detach", "--project", project]);
    assert!(!ambiguous.status.success());
    let envelope = parse_envelope(&ambiguous);
    assert_eq!(envelope["error"]["code"], "ambiguous-attachment");
    assert_eq!(
        envelope["error"]["details"]["candidateAttachmentIds"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let selected = collab(&["detach", "--project", project, "--attachment", &first_id]);
    assert!(selected.status.success());
    assert_eq!(parse_envelope(&selected)["data"]["attachmentId"], first_id);

    let close = collab(&["close", "--project", project]);
    assert!(close.status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_and_resume_cli_preserve_attachment_identity() {
    let root = temp_root("pause-resume");
    let (_server, _, _commands) = start_preview(&root, "sess-pause-resume").await;
    let project = root.to_str().unwrap();
    let attach = collab(&[
        "attach",
        "--project",
        project,
        "--agent",
        "codex",
        "--tui-session",
        "tui-pause-resume",
    ]);
    assert!(attach.status.success());
    let attachment_id = parse_envelope(&attach)["data"]["attachment"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_string();

    let paused = collab(&[
        "pause",
        "--project",
        project,
        "--attachment",
        &attachment_id,
    ]);
    assert!(paused.status.success(), "stderr: {:?}", paused.stderr);
    let paused = parse_envelope(&paused);
    assert_eq!(paused["data"]["status"], "paused");
    assert_eq!(paused["data"]["attachmentId"], attachment_id);
    assert_eq!(paused["data"]["collaborationState"], "paused");

    let resumed = collab(&[
        "resume",
        "--project",
        project,
        "--attachment",
        &attachment_id,
    ]);
    assert!(resumed.status.success(), "stderr: {:?}", resumed.stderr);
    let resumed = parse_envelope(&resumed);
    assert_eq!(resumed["data"]["status"], "resumed");
    assert_eq!(resumed["data"]["attachmentId"], attachment_id);
    assert_eq!(resumed["data"]["collaborationState"], "active");

    let status = parse_envelope(&collab(&["status", "--project", project]));
    assert_eq!(status["data"]["attachments"].as_array().unwrap().len(), 1);
    assert_eq!(
        status["data"]["attachments"][0]["attachmentId"],
        attachment_id
    );
    assert_eq!(
        status["data"]["attachments"][0]["tuiSessionId"],
        "tui-pause-resume"
    );
    assert_eq!(
        status["data"]["attachments"][0]["collaborationState"],
        "active"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_cli_reports_ambiguous_candidates_without_mutating_attachments() {
    let root = temp_root("pause-ambiguous");
    let (_server, _, _commands) = start_preview(&root, "sess-pause-ambiguous").await;
    let project = root.to_str().unwrap();
    let first = collab(&["attach", "--project", project, "--agent", "codex"]);
    let second = collab(&["attach", "--project", project, "--agent", "claude-code"]);
    assert!(first.status.success() && second.status.success());

    let paused = collab(&["pause", "--project", project]);
    assert!(!paused.status.success());
    let paused = parse_envelope(&paused);
    assert_eq!(paused["error"]["code"], "ambiguous-attachment");
    assert_eq!(
        paused["error"]["details"]["candidateAttachmentIds"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let status = parse_envelope(&collab(&["status", "--project", project]));
    assert!(
        status["data"]["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|attachment| attachment["collaborationState"] == "active")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wait_requires_an_attachment_after_session_resolution() {
    let root = temp_root("pending-ops");
    let (_server, _, _commands) = start_preview(&root, "sess-pending").await;
    let project = root.to_str().unwrap();

    let args = vec!["wait", "--project", project, "--json"];
    let output = collab(&args);
    assert!(!output.status.success(), "{args:?} should exit non-zero");
    let envelope = parse_envelope(&output);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "attachment-required");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("collab attach")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn feedback_show_and_compare_set_state_use_cli_envelopes() {
    let root = temp_root("feedback");
    let (_server, _, _commands) = start_preview(&root, "sess-feedback").await;
    let project = root.to_str().unwrap();

    let feedback = collab::feedback::store(
        &root,
        collab::feedback::validate(serde_json::json!({
            "kind": "textbox",
            "text": "change the title",
            "pageUrl": "http://127.0.0.1/",
            "viewport": {"width": 800, "height": 600, "scrollX": 0, "scrollY": 0},
        }))
        .unwrap(),
    )
    .unwrap();

    let attach = collab(&["attach", "--project", project, "--agent", "codex"]);
    assert!(attach.status.success());
    let attach_envelope = parse_envelope(&attach);
    let attachment = attach_envelope["data"]["attachment"]["attachmentId"]
        .as_str()
        .unwrap()
        .to_string();
    collab::feedback::lease_next(&root, &attachment)
        .unwrap()
        .expect("pending feedback should be leased");

    let show = collab(&["feedback", "show", "--project", project, &feedback.id]);
    assert!(show.status.success());
    let show_envelope = parse_envelope(&show);
    assert_eq!(show_envelope["data"]["item"]["id"], feedback.id);
    assert_eq!(show_envelope["data"]["item"]["state"], "pending");

    let set_state = collab(&[
        "feedback",
        "set-state",
        "--project",
        project,
        &feedback.id,
        "acknowledged",
        "--expected",
        "pending",
        "--attachment",
        &attachment,
    ]);
    assert!(set_state.status.success());
    let state_envelope = parse_envelope(&set_state);
    assert_eq!(state_envelope["data"]["item"]["state"], "acknowledged");

    let stale_compare = collab(&[
        "feedback",
        "set-state",
        "--project",
        project,
        &feedback.id,
        "working",
        "--expected",
        "pending",
        "--attachment",
        &attachment,
    ]);
    assert!(!stale_compare.status.success());
    let conflict = parse_envelope(&stale_compare);
    assert_eq!(conflict["error"]["code"], "state-conflict");
    assert_eq!(conflict["error"]["details"]["actual"], "acknowledged");
}

/// eval 與 screenshot 的 CLI envelope（task 2.3）：以 stub consumer 取代
/// 真實 WKWebView 回覆 command，WebKit 行為由 macOS harness 驗證。
#[tokio::test(flavor = "multi_thread")]
async fn eval_and_screenshot_emit_envelopes_through_command_queue() {
    use collab::webview::{CommandError, WebviewCommand};

    let root = temp_root("eval-screenshot");
    let (_server, _, mut commands) = start_preview_manual(&root, "sess-eval").await;
    let project = root.to_str().unwrap().to_string();
    let snapshot_path = root.join(".collab/screenshots/stub.png");
    let stub_path = snapshot_path.clone();

    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
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
                    let _ = respond.send(Err(CommandError::SnapshotFailed(
                        "painting capture is not configured for this test".into(),
                    )));
                }
                WebviewCommand::Eval {
                    expression,
                    respond,
                } => {
                    let outcome = if expression == "1+1" {
                        Ok(serde_json::json!(2))
                    } else {
                        Err(CommandError::JavascriptError("boom".into()))
                    };
                    let _ = respond.send(outcome);
                }
                WebviewCommand::Snapshot { respond, .. } => {
                    let _ = respond.send(Ok(stub_path.clone()));
                }
                WebviewCommand::Reload { respond } => {
                    let _ = respond.send(Ok(()));
                }
            }
        }
    });

    let ok = tokio::task::spawn_blocking({
        let project = project.clone();
        move || collab(&["eval", "--project", &project, "1+1"])
    })
    .await
    .unwrap();
    assert!(ok.status.success());
    let envelope = parse_envelope(&ok);
    assert_eq!(envelope["data"]["value"], 2);

    let err = tokio::task::spawn_blocking({
        let project = project.clone();
        move || collab(&["eval", "--project", &project, "throw new Error('x')"])
    })
    .await
    .unwrap();
    assert!(!err.status.success());
    let envelope = parse_envelope(&err);
    assert_eq!(envelope["error"]["code"], "javascript-error");

    let shot = tokio::task::spawn_blocking({
        let project = project.clone();
        move || collab(&["screenshot", "--project", &project])
    })
    .await
    .unwrap();
    assert!(shot.status.success());
    let envelope = parse_envelope(&shot);
    assert_eq!(
        envelope["data"]["path"].as_str().unwrap(),
        snapshot_path.to_str().unwrap()
    );
}
