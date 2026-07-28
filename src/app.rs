//! Tauri application lifecycle：啟動 loopback server、寫入 session file、
//! 建立唯一一個 WKWebView preview 視窗。
//! spec「Native macOS preview runtime」要求 exactly one WKWebView，且不得
//! 啟動 Chromium-family renderer。

use std::error::Error;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

use crate::dashboard;
use crate::server;
use crate::session::{self, SessionFile};
use crate::watcher;
use crate::webview;

/// preview 視窗的唯一 label；後續 tasks（reload、eval、snapshot）都以此定位視窗。
pub const PREVIEW_WINDOW_LABEL: &str = "preview";

struct SessionRegistryGuard {
    project_root: PathBuf,
    session_id: String,
}

impl SessionRegistryGuard {
    fn new(session: &SessionFile) -> Self {
        Self {
            project_root: session.project_root.clone(),
            session_id: session.session_id.clone(),
        }
    }
}

impl Drop for SessionRegistryGuard {
    fn drop(&mut self) {
        if let Err(error) =
            session::remove_session_file_if_owned(&self.project_root, &self.session_id)
        {
            eprintln!("cannot clean preview session registry: {error}");
        }
    }
}

fn server_task_error(result: Result<io::Result<()>, tokio::task::JoinError>) -> Option<String> {
    match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("preview server failed: {error}")),
        Err(error) => Some(format!("preview server task failed: {error}")),
    }
}

/// 啟動 server 與 Tauri app 並開啟單一 preview 視窗，blocking 直到視窗關閉
/// 或 control stop。
pub fn run(
    project_root: PathBuf,
    entry_file: PathBuf,
    startup_lock: session::StartupLock,
) -> Result<(), Box<dyn Error>> {
    let token = session::generate_token();
    let session_id = session::generate_session_id();
    let runtime = tokio::runtime::Runtime::new()?;
    let (command_tx, command_rx) = webview::command_channel();
    let watcher_command_tx = command_tx.clone();
    let draft_command_tx = command_tx.clone();
    let running = runtime.block_on(server::start(server::ServerConfig {
        project_root: project_root.clone(),
        session_id: session_id.clone(),
        token: token.clone(),
        commands: command_tx,
    }))?;

    let mut session_file = SessionFile::new(
        project_root.clone(),
        entry_file.clone(),
        running.port,
        token,
    );
    session_file.session_id = session_id;
    session::write_session_file(&session_file)?;
    let _registry_guard = SessionRegistryGuard::new(&session_file);
    drop(startup_lock);

    // token 不得出現在正常輸出；只印不含敏感資訊的 preview URL。
    let preview_url = initial_preview_url(running.port, &project_root, &entry_file)?;
    println!("preview: {preview_url}");

    let dashboard_handle = running.dashboard.clone();
    let draft_states = running.draft_states.clone();
    let server_task = running.task;
    let server_failure = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let setup_server_failure = server_failure.clone();
    let exit_project_root = session_file.project_root.clone();
    let exit_session_id = session_file.session_id.clone();
    tauri::Builder::default()
        .setup(move |app| {
            let window = WebviewWindowBuilder::new(
                app,
                PREVIEW_WINDOW_LABEL,
                WebviewUrl::External(preview_url.clone()),
            )
            .title(format!("collab preview — {}", entry_file.display()))
            .inner_size(1200.0, 800.0)
            // reload 前後保留 scroll position（spec「Preserve collaboration
            // UI across reload」）。
            .initialization_script(watcher::SCROLL_PRESERVE_SCRIPT)
            // Shadow DOM feedback overlay：每次 navigation 注入一次。
            .initialization_script(crate::overlay::overlay_script())
            .build()?;

            dashboard::install(
                &window,
                dashboard_handle,
                draft_command_tx,
                draft_states,
                project_root.clone(),
            )
            .map_err(|error| format!("cannot create native collaboration dashboard: {error}"))?;

            // bounded queue 的消化端：把 reload/eval/snapshot 派送到 main thread。
            runtime.spawn(webview::run_command_consumer(window, command_rx));

            // 檔案監看 → 200ms debounce → 同一 WKWebView reload。
            // watcher 建立失敗是明確錯誤（不默默改用 polling）。
            let (event_tx, event_rx) = tokio::sync::mpsc::channel(watcher::EVENT_QUEUE_CAPACITY);
            let watcher = watcher::start_watcher(&project_root, event_tx)
                .map_err(|e| format!("cannot watch project files: {e}"))?;
            // watcher 必須存活整個 app lifetime；交給 Tauri state 持有。
            app.manage(std::sync::Mutex::new(watcher));
            runtime.spawn(watcher::run_debouncer(event_rx, watcher_command_tx));

            // control stop（graceful shutdown 完成）→ 結束 Tauri app。
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let error = server_task_error(runtime.block_on(server_task));
                let exit_code = if let Some(error) = error {
                    eprintln!("{error}");
                    *setup_server_failure.lock().unwrap() = Some(error);
                    1
                } else {
                    0
                };
                handle.exit(exit_code);
            });
            Ok(())
        })
        .build(tauri::generate_context!())?
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event
                && let Err(error) =
                    session::remove_session_file_if_owned(&exit_project_root, &exit_session_id)
            {
                eprintln!("cannot clean preview session registry on exit: {error}");
            }
        });
    if let Some(error) = server_failure.lock().unwrap().take() {
        return Err(io::Error::other(error).into());
    }
    Ok(())
}

pub fn initial_preview_url(
    port: u16,
    project_root: &Path,
    entry_file: &Path,
) -> io::Result<tauri::Url> {
    let relative = entry_file.strip_prefix(project_root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "entry {} is outside project root {}",
                entry_file.display(),
                project_root.display()
            ),
        )
    })?;
    let mut url = tauri::Url::parse(&format!("http://127.0.0.1:{port}/")).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid preview URL: {error}"),
        )
    })?;
    {
        let mut segments = url.path_segments_mut().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "preview URL cannot be a base")
        })?;
        for component in relative.components() {
            let std::path::Component::Normal(component) = component else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("entry path {} is not normalized", relative.display()),
                ));
            };
            segments.push(&component.to_string_lossy());
        }
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_preview_url_targets_the_resolved_entry_file() {
        let root =
            std::env::temp_dir().join(format!("collab-app-entry-url-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let entry = root.join("landing page.html");

        let url = initial_preview_url(43123, &root, &entry).unwrap();

        assert_eq!(url.as_str(), "http://127.0.0.1:43123/landing%20page.html");
    }

    #[test]
    fn registry_guard_cleans_owned_session_after_setup_failure() {
        let root =
            std::env::temp_dir().join(format!("collab-app-registry-guard-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut session =
            SessionFile::new(root.clone(), root.join("index.html"), 1234, "token".into());
        session.session_id = "owned-session".into();
        session::write_session_file(&session).unwrap();

        drop(SessionRegistryGuard::new(&session));

        assert!(session::read_session_file(&root).is_err());
    }

    #[tokio::test]
    async fn server_task_io_failure_is_reported() {
        let task = tokio::spawn(async { Err(io::Error::other("server failed")) });

        let error = server_task_error(task.await);

        assert!(error.unwrap().contains("server failed"));
    }
}
