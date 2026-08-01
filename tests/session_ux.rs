use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde_json::Value;

static TEMP_DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

fn collab() -> Command {
    Command::new(env!("CARGO_BIN_EXE_collab"))
}

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "collab-session-ux-{name}-{}-{}",
        std::process::id(),
        TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn parse_envelope(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not a JSON envelope: {error}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

struct PreviewGuard {
    root: PathBuf,
}

impl Drop for PreviewGuard {
    fn drop(&mut self) {
        let _ = collab()
            .args(["close", "--project", self.root.to_str().unwrap()])
            .output();
    }
}

#[test]
fn background_open_returns_opened_then_reuses_the_healthy_preview() {
    let root = temp_root("background");
    let entry = root.join("landing.html");
    std::fs::write(&entry, "<!doctype html><title>landing</title>").unwrap();
    let _guard = PreviewGuard { root: root.clone() };

    let opened = collab()
        .args(["open", entry.to_str().unwrap(), "--background"])
        .output()
        .expect("failed to invoke background open");
    assert!(opened.status.success(), "stderr: {:?}", opened.stderr);
    let opened = parse_envelope(&opened);
    assert_eq!(opened["ok"], true);
    assert_eq!(opened["data"]["status"], "opened");
    assert_eq!(opened["data"]["projectRoot"], root.to_str().unwrap());
    assert_eq!(
        opened["data"]["entryFile"],
        entry.canonicalize().unwrap().to_str().unwrap()
    );
    assert!(opened["data"]["sessionId"].as_str().is_some());
    assert!(opened["data"]["port"].as_u64().is_some());
    let session_before = collab::session::read_session_file(&root).unwrap();
    assert!(process_alive(session_before.pid));

    let reused = collab()
        .args([
            "open",
            root.join("./landing.html").to_str().unwrap(),
            "--background",
        ])
        .output()
        .expect("failed to invoke reused background open");
    assert!(reused.status.success(), "stderr: {:?}", reused.stderr);
    let reused = parse_envelope(&reused);
    assert_eq!(reused["data"]["status"], "reused");
    for identity in ["sessionId", "projectRoot", "entryFile", "port"] {
        assert_eq!(opened["data"][identity], reused["data"][identity]);
    }
    let session_after = collab::session::read_session_file(&root).unwrap();
    assert_eq!(session_before.session_id, session_after.session_id);
    assert_eq!(session_before.port, session_after.port);
    assert_eq!(session_before.pid, session_after.pid);
    assert!(process_alive(session_after.pid));
}

#[test]
fn background_preview_survives_launcher_completion() {
    let root = temp_root("launcher-completion");
    let entry = root.join("index.html");
    std::fs::write(&entry, "<!doctype html><title>launcher completion</title>").unwrap();
    let _guard = PreviewGuard { root: root.clone() };

    let mut launcher_command = Command::new(env!("CARGO_BIN_EXE_collab"));
    launcher_command
        .args(["open", entry.to_str().unwrap(), "--background"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        launcher_command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let launcher = launcher_command
        .spawn()
        .expect("failed to invoke one-shot background launcher");
    let launcher_pid = launcher.id();
    let opened = launcher.wait_with_output().unwrap();
    assert!(opened.status.success(), "stderr: {:?}", opened.stderr);
    let opened = parse_envelope(&opened);
    assert_eq!(opened["data"]["status"], "opened");

    let cleanup_result = unsafe { libc::kill(-(launcher_pid as libc::pid_t), libc::SIGTERM) };
    assert!(
        cleanup_result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
        "failed to clean up launcher process group"
    );

    let project = root.to_str().unwrap();
    let status = collab()
        .args(["status", "--project", project])
        .output()
        .expect("failed to query preview status after launcher completion");
    assert!(status.status.success(), "stderr: {:?}", status.stderr);
    assert_eq!(
        parse_envelope(&status)["data"]["sessionId"],
        opened["data"]["sessionId"]
    );

    let attach = collab()
        .args(["attach", "--project", project, "--agent", "session-ux-test"])
        .output()
        .expect("failed to attach after launcher completion");
    assert!(attach.status.success(), "stderr: {:?}", attach.stderr);

    let eval = collab()
        .args(["eval", "--project", project, "document.title"])
        .output()
        .expect("failed to evaluate after launcher completion");
    assert!(eval.status.success(), "stderr: {:?}", eval.stderr);

    let screenshot = collab()
        .args(["screenshot", "--project", project])
        .output()
        .expect("failed to capture screenshot after launcher completion");
    assert!(
        screenshot.status.success(),
        "stderr: {:?}",
        screenshot.stderr
    );

    let reused = collab()
        .args(["open", entry.to_str().unwrap(), "--background"])
        .output()
        .expect("failed to reuse preview after launcher completion");
    assert!(reused.status.success(), "stderr: {:?}", reused.stderr);
    let reused = parse_envelope(&reused);
    assert_eq!(reused["data"]["status"], "reused");
    assert_eq!(reused["data"]["sessionId"], opened["data"]["sessionId"]);
}

#[test]
fn background_start_timeout_kills_only_the_spawned_child() {
    let root = temp_root("timeout");
    let entry = root.join("index.html");
    std::fs::write(&entry, "<!doctype html>").unwrap();
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let mut unrelated = Command::new("sleep").arg("30").spawn().unwrap();
    let child_pid = child.id();
    let unrelated_pid = unrelated.id();

    let error = collab::preview::wait_for_background_session(
        &root,
        &entry.canonicalize().unwrap(),
        &mut child,
        Duration::from_millis(50),
    )
    .unwrap_err();

    assert_eq!(error.code, "preview-start-timeout");
    let envelope = serde_json::to_value(collab::core::Envelope::failure(error)).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "preview-start-timeout");
    assert!(!process_alive(child_pid));
    assert!(process_alive(unrelated_pid));
    assert!(collab::session::read_session_file(&root).is_err());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}

#[test]
fn background_child_failure_includes_captured_stderr() {
    let root = temp_root("stderr");
    let entry = root.join("index.html");
    std::fs::write(&entry, "<!doctype html>").unwrap();
    let mut child = Command::new("sh")
        .args(["-c", "echo startup-diagnostic >&2; exit 7"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let error = collab::preview::wait_for_background_session(
        &root,
        &entry.canonicalize().unwrap(),
        &mut child,
        Duration::from_secs(2),
    )
    .unwrap_err();

    assert_eq!(error.code, "preview-start-failed");
    assert!(error.message.contains("startup-diagnostic"));
}

#[test]
fn background_readiness_rejects_mismatched_session_identity() {
    let root = temp_root("background-identity-mismatch");
    let entry = root.join("index.html");
    std::fs::write(&entry, "<!doctype html>").unwrap();
    let entry = entry.canonicalize().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (commands, _receiver) = collab::webview::command_channel();
    let running = runtime
        .block_on(collab::server::start(collab::server::ServerConfig {
            project_root: root.clone(),
            session_id: "actual-server-session".into(),
            token: "identity-token".into(),
            commands,
        }))
        .unwrap();
    let mut registry = collab::session::SessionFile::new(
        root.clone(),
        entry.clone(),
        running.port,
        "identity-token".into(),
    );
    registry.session_id = "stale-registry-session".into();
    collab::session::write_session_file(&registry).unwrap();
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();

    let error = collab::preview::wait_for_background_session(
        &root,
        &entry,
        &mut child,
        Duration::from_millis(100),
    )
    .unwrap_err();

    assert_eq!(error.code, "preview-start-timeout");
    assert!(!process_alive(child.id()));
    running.task.abort();
}

fn process_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[test]
fn pause_and_resume_help_define_same_attachment_workflow() {
    let help = collab()
        .arg("--help")
        .output()
        .expect("failed to inspect collaboration CLI");
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(
        help.lines()
            .any(|line| line.trim_start().starts_with("pause "))
    );
    assert!(
        help.lines()
            .any(|line| line.trim_start().starts_with("resume "))
    );

    for operation in ["pause", "resume"] {
        let output = collab()
            .args([operation, "--help"])
            .output()
            .expect("failed to inspect collaboration lifecycle help");
        assert!(output.status.success());
        let output = String::from_utf8_lossy(&output.stdout);
        assert!(output.contains("--attachment"));
        assert!(output.contains("--project"));
        assert!(output.contains("--session"));
    }
}

// spec「Inactive collaboration overlay state」+「Feedback tools follow collaboration
// availability」：collaboration 可用性與離線 Paint 開關是兩個獨立狀態，離線 Paint
// 不得暴露 Element、Note、Send paint 或 feedback editor。
#[test]
fn overlay_separates_collaboration_availability_from_offline_paint() {
    let overlay = std::fs::read_to_string("web/overlay.js").unwrap();

    for contract in [
        "function setActive(active)",
        "function toggleOfflinePaint()",
        "function closeOfflinePaint()",
        "state.offlinePaint",
        "function paintingAvailable()",
        "toggleOfflinePaint: toggleOfflinePaint",
        "offlinePaintOpen: function ()",
        // collaboration 專屬控制項只在 active 時出現。
        r#"ui.commentButton.style.display = state.active ? "" : "none";"#,
        r#"ui.noteButton.style.display = state.active ? "" : "none";"#,
        "state.active && (state.mode === \"paint\" || state.marks.length)",
        // 離線 marks 不得走任何提交路徑。
        "if (!state.active) return Promise.resolve(null);",
    ] {
        assert!(
            overlay.contains(contract),
            "missing offline paint separation contract: {contract}"
        );
    }
}

// spec「Dashboard and feedback toolbar have separate visibility lifecycles」：
// dashboard 跟隨 preview runtime，頁面 collaboration toolbar 跟隨 collaboration 可用性。
#[test]
fn dashboard_and_offline_paint_follow_separate_lifecycles() {
    let dashboard = std::fs::read_to_string("src/dashboard.rs").unwrap();

    for contract in [
        "pub fn dashboard_visible(&self)",
        "pub fn feedback_tools_visible(&self)",
        "pub fn offline_paint_available(&self)",
        "DashboardRuntimeState::Running",
    ] {
        assert!(
            dashboard.contains(contract),
            "missing separate lifecycle contract: {contract}"
        );
    }
}

#[test]
fn acceptance_script_covers_stopped_preview_connection_handoff() {
    let script = std::fs::read_to_string("scripts/session-ux-acceptance.sh").unwrap();

    for contract in [
        "preview-collaboration-connect",
        "connect-after-stop",
        "different-conversation",
        "new-attachment-feedback",
    ] {
        assert!(
            script.contains(contract),
            "acceptance transcript is missing {contract}"
        );
    }
}
