//! Preview background start/reuse orchestration.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;

use crate::core::OpError;
use crate::session::{self, ResolvedEntry, SessionFile};

pub const BACKGROUND_START_TIMEOUT: Duration = Duration::from_secs(10);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);
const STARTUP_STDERR_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundOpenResult {
    pub status: String,
    pub session_id: String,
    pub project_root: PathBuf,
    pub entry_file: PathBuf,
    pub port: u16,
}

impl BackgroundOpenResult {
    fn from_session(status: &str, session: &SessionFile) -> Self {
        BackgroundOpenResult {
            status: status.to_string(),
            session_id: session.session_id.clone(),
            project_root: session.project_root.clone(),
            entry_file: session.entry_file.clone(),
            port: session.port,
        }
    }
}

pub fn open_background(entry: &Path) -> Result<BackgroundOpenResult, OpError> {
    let resolved =
        session::resolve_entry(entry).map_err(|error| OpError::new(error.code, error.message))?;
    if let Some(existing) = healthy_session(&resolved.project_root) {
        return reuse_or_conflict(&resolved, &existing);
    }

    let executable = std::env::current_exe().map_err(|error| {
        OpError::new(
            "internal-error",
            format!("cannot locate collab executable: {error}"),
        )
    })?;
    let mut command = Command::new(executable);
    command
        .arg("open")
        .arg(&resolved.entry_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command.spawn().map_err(|error| {
        OpError::new(
            "preview-start-failed",
            format!("cannot start background preview: {error}"),
        )
    })?;
    let session = wait_for_background_session(
        &resolved.project_root,
        &resolved.entry_file,
        &mut child,
        BACKGROUND_START_TIMEOUT,
    )?;
    Ok(BackgroundOpenResult::from_session("opened", &session))
}

pub fn wait_for_background_session(
    project_root: &Path,
    expected_entry: &Path,
    child: &mut Child,
    timeout: Duration,
) -> Result<SessionFile, OpError> {
    let child_pid = child.id();
    let mut stderr_reader = child.stderr.take().map(|mut stderr| {
        std::thread::spawn(move || {
            let mut captured = Vec::with_capacity(STARTUP_STDERR_LIMIT);
            let mut buffer = [0_u8; 4096];
            while let Ok(read) = stderr.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                let remaining = STARTUP_STDERR_LIMIT.saturating_sub(captured.len());
                captured.extend_from_slice(&buffer[..read.min(remaining)]);
            }
            captured
        })
    });
    let started = Instant::now();
    loop {
        if let Some(session) = healthy_session(project_root) {
            if session.entry_file == expected_entry {
                return Ok(session);
            }
            terminate_child(child);
            return Err(entry_conflict(expected_entry, &session));
        }
        if let Some(status) = child.try_wait().map_err(|error| {
            OpError::new(
                "preview-start-failed",
                format!("cannot inspect background preview process: {error}"),
            )
        })? {
            let diagnostics = take_startup_stderr(&mut stderr_reader);
            let detail = diagnostics
                .filter(|value| !value.is_empty())
                .map(|value| format!("; stderr: {value}"))
                .unwrap_or_default();
            return Err(OpError::new(
                "preview-start-failed",
                format!("background preview exited before becoming ready: {status}{detail}"),
            ));
        }
        if started.elapsed() >= timeout {
            terminate_child(child);
            remove_child_session(project_root, expected_entry, child_pid);
            return Err(OpError::new(
                "preview-start-timeout",
                format!(
                    "background preview for {} did not become healthy within {} ms",
                    expected_entry.display(),
                    timeout.as_millis()
                ),
            ));
        }
        std::thread::sleep(READY_POLL_INTERVAL.min(timeout.saturating_sub(started.elapsed())));
    }
}

fn take_startup_stderr(reader: &mut Option<std::thread::JoinHandle<Vec<u8>>>) -> Option<String> {
    let captured = reader.take()?.join().ok()?;
    Some(String::from_utf8_lossy(&captured).trim().to_string())
}

fn healthy_session(project_root: &Path) -> Option<SessionFile> {
    let session = session::read_session_file(project_root).ok()?;
    crate::client::session_is_healthy(&session).then_some(session)
}

fn reuse_or_conflict(
    resolved: &ResolvedEntry,
    existing: &SessionFile,
) -> Result<BackgroundOpenResult, OpError> {
    if existing.entry_file != resolved.entry_file {
        return Err(entry_conflict(&resolved.entry_file, existing));
    }
    Ok(BackgroundOpenResult::from_session("reused", existing))
}

fn entry_conflict(requested_entry: &Path, existing: &SessionFile) -> OpError {
    OpError::new(
        "entry-conflict",
        format!(
            "preview session {} already owns {}",
            existing.session_id,
            existing.entry_file.display()
        ),
    )
    .with_details(json!({
        "currentEntry": existing.entry_file,
        "requestedEntry": requested_entry,
    }))
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn remove_child_session(project_root: &Path, expected_entry: &Path, child_pid: u32) {
    let Ok(session) = session::read_session_file(project_root) else {
        return;
    };
    if session.pid == child_pid && session.entry_file == expected_entry {
        let _ = session::remove_session_file_if_owned(project_root, &session.session_id);
    }
}
