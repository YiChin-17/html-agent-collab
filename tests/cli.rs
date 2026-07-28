//! CLI exit-code tests（task 1.2 驗證項目）。
//! 成功開窗屬 GUI 行為，由 macOS process-tree manual assertion 覆蓋，不在此測。

use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

use collab::session::{self, SessionFile};
use serde_json::Value;

static TEMP_DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

fn collab() -> Command {
    Command::new(env!("CARGO_BIN_EXE_collab"))
}

fn temp_root(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "collab-cli-entry-{name}-{}-{}",
        std::process::id(),
        TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

struct LivePreview {
    _runtime: tokio::runtime::Runtime,
    _running: collab::server::RunningServer,
    _commands: collab::webview::CommandReceiver,
}

fn start_live_preview(root: &std::path::Path, entry: &std::path::Path) -> LivePreview {
    start_live_preview_with_id(root, entry, &session::generate_session_id())
}

fn start_live_preview_with_id(
    root: &std::path::Path,
    entry: &std::path::Path,
    session_id: &str,
) -> LivePreview {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let token = session::generate_token();
    let (commands, receiver) = collab::webview::command_channel();
    let running = runtime
        .block_on(collab::server::start(collab::server::ServerConfig {
            project_root: root.to_path_buf(),
            session_id: session_id.to_string(),
            token: token.clone(),
            commands,
        }))
        .unwrap();
    let mut file = SessionFile::new(root.to_path_buf(), entry.to_path_buf(), running.port, token);
    file.session_id = session_id.to_string();
    session::write_session_file(&file).unwrap();
    LivePreview {
        _runtime: runtime,
        _running: running,
        _commands: receiver,
    }
}

#[test]
fn explicit_session_selection_distinguishes_mismatch_from_absence() {
    let root = temp_root("session-mismatch");
    let entry = root.join("index.html");
    std::fs::write(&entry, "<!doctype html>").unwrap();
    let _preview = start_live_preview_with_id(&root, &entry, "fedcba9876543210");

    let output = collab()
        .args([
            "status",
            "--project",
            root.to_str().unwrap(),
            "--session",
            "0123456789abcdef",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let error = error_envelope(&output)["error"].clone();
    assert_eq!(error["code"], "preview-session-mismatch");
    assert_eq!(error["details"]["requestedSessionId"], "0123456789abcdef");
    assert_eq!(
        error["details"]["candidateSessionIds"],
        serde_json::json!(["fedcba9876543210"])
    );
}

#[test]
fn explicit_session_discovery_stays_within_workspace_ancestors() {
    let parent = temp_root("workspace-boundary");
    let preview_root = parent.join("preview-project");
    let other_root = parent.join("other-project");
    std::fs::create_dir_all(&preview_root).unwrap();
    std::fs::create_dir_all(&other_root).unwrap();
    let entry = preview_root.join("index.html");
    std::fs::write(&entry, "<!doctype html>").unwrap();
    let _preview = start_live_preview_with_id(&preview_root, &entry, "0123456789abcdef");

    let sibling_only = collab()
        .args([
            "status",
            "--project",
            other_root.to_str().unwrap(),
            "--session",
            "0123456789abcdef",
        ])
        .output()
        .unwrap();

    assert!(!sibling_only.status.success());
    assert_eq!(
        error_envelope(&sibling_only)["error"]["code"],
        "preview-not-running"
    );
}

fn error_envelope(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not a JSON envelope: {e}; got {stdout:?}"));
    assert_eq!(envelope["ok"], false);
    envelope
}

#[test]
fn open_missing_project_exits_nonzero() {
    let output = collab()
        .args(["open", "/nonexistent/collab-test-project"])
        .output()
        .expect("failed to run collab binary");
    assert!(!output.status.success());
    let envelope = error_envelope(&output);
    assert_eq!(envelope["error"]["code"], "invalid-entry");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cannot resolve preview entry"),
        "unexpected error: {envelope}"
    );
}

#[test]
fn open_non_html_file_exits_with_invalid_entry() {
    let file_path = std::env::temp_dir().join("collab-cli-test-not-html.txt");
    std::fs::write(&file_path, "not html").expect("failed to write fixture file");

    let output = collab()
        .args(["open", file_path.to_str().unwrap()])
        .output()
        .expect("failed to run collab binary");
    assert!(!output.status.success());
    let envelope = error_envelope(&output);
    assert_eq!(envelope["error"]["code"], "invalid-entry");

    std::fs::remove_file(&file_path).ok();
}

#[test]
fn open_directory_without_index_exits_with_invalid_entry() {
    let root = temp_root("missing-index");

    let output = collab()
        .args(["open", root.to_str().unwrap()])
        .output()
        .expect("failed to run collab binary");

    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error"]["code"], "invalid-entry");
}

#[test]
fn open_rejects_another_entry_owned_by_the_same_project_root() {
    let root = temp_root("conflict");
    let current_entry = root.join("index.html");
    let requested_entry = root.join("about.html");
    std::fs::write(&current_entry, "<!doctype html>").unwrap();
    std::fs::write(&requested_entry, "<!doctype html>").unwrap();
    let _preview = start_live_preview(&root, &current_entry.canonicalize().unwrap());

    let output = collab()
        .args(["open", requested_entry.to_str().unwrap()])
        .output()
        .expect("failed to run collab binary");

    assert!(!output.status.success());
    let envelope = error_envelope(&output);
    assert_eq!(envelope["error"]["code"], "entry-conflict");
    assert_eq!(
        envelope["error"]["details"]["currentEntry"],
        current_entry.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(
        envelope["error"]["details"]["requestedEntry"],
        requested_entry.canonicalize().unwrap().to_str().unwrap()
    );
}

#[test]
fn open_same_canonical_entry_does_not_start_a_second_preview() {
    let root = temp_root("same-entry");
    let entry = root.join("index.html");
    std::fs::write(&entry, "<!doctype html>").unwrap();
    let _preview = start_live_preview(&root, &entry.canonicalize().unwrap());

    let output = collab()
        .args(["open", entry.to_str().unwrap()])
        .output()
        .expect("failed to run collab binary");

    assert!(!output.status.success());
    assert_eq!(
        error_envelope(&output)["error"]["code"],
        "preview-already-running"
    );
}

#[test]
fn no_subcommand_exits_nonzero_with_usage() {
    let output = collab().output().expect("failed to run collab binary");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage"), "unexpected stderr: {stderr}");
}
