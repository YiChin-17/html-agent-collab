//! Tauri main-thread WebView command adapter。
//! design「Tauri main-thread WebView command adapter」：axum handlers 與
//! watcher 不得直接持有非 Send/Sync 的 WKWebView；它們透過 bounded channel
//! 送出 reload、eval、snapshot 命令，以 one-shot channel 取回結果。
//! 所有 unsafe pointer 不得離開本模組的 macOS adapter。

use std::path::PathBuf;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

/// bounded queue 容量；佇列滿時 caller 立即收到 `busy`，不得無限累積。
pub const COMMAND_QUEUE_CAPACITY: usize = 16;

#[derive(Debug)]
pub enum WebviewCommand {
    SetCollaborationActive {
        active: bool,
        respond: oneshot::Sender<Result<(), CommandError>>,
    },
    SyncFeedbackMarkers {
        snapshot: crate::core::FeedbackMarkerSnapshot,
        respond: oneshot::Sender<Result<(), CommandError>>,
    },
    Reload {
        respond: oneshot::Sender<Result<(), CommandError>>,
    },
    Eval {
        expression: String,
        respond: oneshot::Sender<Result<Value, CommandError>>,
    },
    Snapshot {
        output_path: PathBuf,
        respond: oneshot::Sender<Result<PathBuf, CommandError>>,
    },
}

/// design「Failure modes」對應的 command 錯誤。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    JavascriptError(String),
    UnsupportedResult(String),
    SnapshotFailed(String),
    Internal(String),
}

impl CommandError {
    pub fn code(&self) -> &'static str {
        match self {
            CommandError::JavascriptError(_) => "javascript-error",
            CommandError::UnsupportedResult(_) => "unsupported-result",
            CommandError::SnapshotFailed(_) => "snapshot-failed",
            CommandError::Internal(_) => "internal-error",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            CommandError::JavascriptError(m)
            | CommandError::UnsupportedResult(m)
            | CommandError::SnapshotFailed(m)
            | CommandError::Internal(m) => m,
        }
    }
}

pub type CommandSender = mpsc::Sender<WebviewCommand>;
pub type CommandReceiver = mpsc::Receiver<WebviewCommand>;

pub fn command_channel() -> (CommandSender, CommandReceiver) {
    mpsc::channel(COMMAND_QUEUE_CAPACITY)
}

fn write_png_bytes_atomically(
    output_path: &std::path::Path,
    png_bytes: &[u8],
) -> Result<PathBuf, CommandError> {
    crate::session::write_artifact_bytes(output_path, png_bytes)
        .map_err(|error| CommandError::SnapshotFailed(format!("cannot publish snapshot: {error}")))
}

/// 將 eval 語意包在頁面內執行的 wrapper：
/// - `undefined`/`null` → JSON null
/// - exception → `javascript-error`
/// - DOM node、function、symbol、無法 stringify → `unsupported-result`
///
/// wrapper 一律回傳 JSON 字串，Rust 端只需解析 NSString。
pub fn build_eval_script(expression: &str) -> String {
    let quoted = serde_json::to_string(expression).expect("string is always serializable");
    format!(
        r#"(() => {{
  try {{
    const __value = (0, eval)({quoted});
    if (__value === undefined || __value === null) return JSON.stringify({{ ok: true, value: null }});
    if (typeof Node !== "undefined" && __value instanceof Node)
      return JSON.stringify({{ ok: false, code: "unsupported-result", message: "result is a DOM node; serialize the fields you need explicitly" }});
    if (typeof __value === "function" || typeof __value === "symbol")
      return JSON.stringify({{ ok: false, code: "unsupported-result", message: "result of type " + typeof __value + " is not JSON-serializable" }});
    let __json;
    try {{ __json = JSON.stringify(__value); }}
    catch (err) {{ return JSON.stringify({{ ok: false, code: "unsupported-result", message: String(err && err.message || err) }}); }}
    if (__json === undefined)
      return JSON.stringify({{ ok: false, code: "unsupported-result", message: "result is not JSON-serializable" }});
    return JSON.stringify({{ ok: true, value: JSON.parse(__json) }});
  }} catch (err) {{
    return JSON.stringify({{ ok: false, code: "javascript-error", message: err && err.message ? String(err.message) : String(err) }});
  }}
}})()"#
    )
}

/// 解析 wrapper 回傳的 JSON payload。
pub fn parse_eval_payload(raw: &str) -> Result<Value, CommandError> {
    let payload: Value = serde_json::from_str(raw).map_err(|e| {
        CommandError::Internal(format!("eval wrapper returned unparseable payload: {e}"))
    })?;
    if payload.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(payload.get("value").cloned().unwrap_or(Value::Null));
    }
    let message = payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown evaluation error")
        .to_string();
    match payload.get("code").and_then(Value::as_str) {
        Some("unsupported-result") => Err(CommandError::UnsupportedResult(message)),
        Some("javascript-error") => Err(CommandError::JavascriptError(message)),
        other => Err(CommandError::Internal(format!(
            "eval wrapper returned unknown code {other:?}: {message}"
        ))),
    }
}

/// 在 background task 消化 command queue，逐一 dispatch 到 main-thread WKWebView。
/// 完成回覆由 WebKit completion handler 經 one-shot channel 送回。
pub async fn run_command_consumer(
    window: tauri::WebviewWindow<tauri::Wry>,
    mut receiver: CommandReceiver,
) {
    while let Some(command) = receiver.recv().await {
        execute(&window, command);
    }
}

fn execute(window: &tauri::WebviewWindow<tauri::Wry>, command: WebviewCommand) {
    #[cfg(target_os = "macos")]
    macos::execute(window, command);

    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        match command {
            WebviewCommand::SetCollaborationActive { respond, .. } => {
                let _ = respond.send(Err(CommandError::Internal("unsupported platform".into())));
            }
            WebviewCommand::SyncFeedbackMarkers { respond, .. } => {
                let _ = respond.send(Err(CommandError::Internal("unsupported platform".into())));
            }
            WebviewCommand::Reload { respond } => {
                let _ = respond.send(Err(CommandError::Internal("unsupported platform".into())));
            }
            WebviewCommand::Eval { respond, .. } => {
                let _ = respond.send(Err(CommandError::Internal("unsupported platform".into())));
            }
            WebviewCommand::Snapshot { respond, .. } => {
                let _ = respond.send(Err(CommandError::Internal("unsupported platform".into())));
            }
        }
    }
}

/// macOS adapter：唯一允許接觸 WKWebView unsafe pointer 的地方。
/// completion handler 結束後即不再持有任何 NSImage/NSData reference
/// （Retained 於 closure 結尾釋放）。
#[cfg(target_os = "macos")]
mod macos {
    use std::sync::Mutex;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
    use objc2_foundation::{NSDictionary, NSError, NSString};
    use objc2_web_kit::WKWebView;

    use super::{CommandError, WebviewCommand, build_eval_script, parse_eval_payload};

    pub(super) fn execute(window: &tauri::WebviewWindow<tauri::Wry>, command: WebviewCommand) {
        let result = match command {
            WebviewCommand::SetCollaborationActive { active, respond } => {
                window.with_webview(move |platform| {
                    let webview = unsafe { &*platform.inner().cast::<WKWebView>() };
                    let script = NSString::from_str(if active {
                        "window.__collabOverlay && window.__collabOverlay.setActive(true);"
                    } else {
                        "window.__collabOverlay && window.__collabOverlay.setActive(false);"
                    });
                    let respond = Mutex::new(Some(respond));
                    let handler =
                        RcBlock::new(move |_value: *mut AnyObject, error: *mut NSError| {
                            let Some(respond) = respond.lock().unwrap().take() else {
                                return;
                            };
                            let outcome = unsafe { error.as_ref() }.map_or(Ok(()), |error| {
                                Err(CommandError::JavascriptError(
                                    error.localizedDescription().to_string(),
                                ))
                            });
                            let _ = respond.send(outcome);
                        });
                    unsafe {
                        webview.evaluateJavaScript_completionHandler(&script, Some(&handler));
                    }
                })
            }
            WebviewCommand::SyncFeedbackMarkers { snapshot, respond } => {
                window.with_webview(move |platform| {
                    let webview = unsafe { &*platform.inner().cast::<WKWebView>() };
                    let payload = serde_json::to_string(&snapshot)
                        .expect("marker snapshot is always serializable");
                    let script = NSString::from_str(&format!(
                        "window.__collabOverlay && window.__collabOverlay.syncFeedbackMarkers({payload});"
                    ));
                    let respond = Mutex::new(Some(respond));
                    let handler =
                        RcBlock::new(move |_value: *mut AnyObject, error: *mut NSError| {
                            let Some(respond) = respond.lock().unwrap().take() else {
                                return;
                            };
                            let outcome = unsafe { error.as_ref() }.map_or(Ok(()), |error| {
                                Err(CommandError::JavascriptError(
                                    error.localizedDescription().to_string(),
                                ))
                            });
                            let _ = respond.send(outcome);
                        });
                    unsafe {
                        webview.evaluateJavaScript_completionHandler(&script, Some(&handler));
                    }
                })
            }
            WebviewCommand::Reload { respond } => window.with_webview(move |platform| {
                // with_webview closure 在 main thread 執行。
                let webview = unsafe { &*platform.inner().cast::<WKWebView>() };
                unsafe {
                    webview.reload();
                }
                let _ = respond.send(Ok(()));
            }),
            WebviewCommand::Eval {
                expression,
                respond,
            } => {
                window.with_webview(move |platform| {
                    let webview = unsafe { &*platform.inner().cast::<WKWebView>() };
                    let script = NSString::from_str(&build_eval_script(&expression));
                    // completion block 的型別是 Fn，oneshot sender 以 Mutex<Option> 取出一次。
                    let respond = Mutex::new(Some(respond));
                    let handler =
                        RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
                            let Some(respond) = respond.lock().unwrap().take() else {
                                return;
                            };
                            let outcome = if let Some(error) = unsafe { error.as_ref() } {
                                Err(CommandError::JavascriptError(
                                    error.localizedDescription().to_string(),
                                ))
                            } else if let Some(value) = unsafe { value.as_ref() } {
                                match value.downcast_ref::<NSString>() {
                                    Some(raw) => parse_eval_payload(&raw.to_string()),
                                    None => Err(CommandError::Internal(
                                        "eval wrapper did not return a string payload".into(),
                                    )),
                                }
                            } else {
                                Err(CommandError::Internal(
                                    "eval completed without result or error".into(),
                                ))
                            };
                            let _ = respond.send(outcome);
                        });
                    unsafe {
                        webview.evaluateJavaScript_completionHandler(&script, Some(&handler));
                    }
                })
            }
            WebviewCommand::Snapshot {
                output_path,
                respond,
            } => {
                window.with_webview(move |platform| {
                    let webview = unsafe { &*platform.inner().cast::<WKWebView>() };
                    let respond = Mutex::new(Some(respond));
                    let handler = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
                        let Some(respond) = respond.lock().unwrap().take() else {
                            return;
                        };
                        let outcome = if let Some(error) = unsafe { error.as_ref() } {
                            Err(CommandError::SnapshotFailed(
                                error.localizedDescription().to_string(),
                            ))
                        } else if let Some(image) = unsafe { image.as_ref() } {
                            write_png(image, &output_path)
                        } else {
                            Err(CommandError::SnapshotFailed(
                                "WebKit returned neither image nor error".into(),
                            ))
                        };
                        let _ = respond.send(outcome);
                    });
                    unsafe {
                        // configuration None → 目前可見 viewport（含注入 overlay）。
                        webview.takeSnapshotWithConfiguration_completionHandler(None, &handler);
                    }
                })
            }
        };
        if let Err(e) = result {
            // with_webview 本身失敗（視窗已消失等）；oneshot sender 已被移入
            // closure，無法補送——caller 端以 oneshot RecvError 收到 internal error。
            eprintln!("webview command dispatch failed: {e}");
        }
    }

    /// NSImage → PNG bytes → temp file + rename（避免留下 partial PNG 當作成功結果）。
    fn write_png(
        image: &NSImage,
        output_path: &std::path::Path,
    ) -> Result<std::path::PathBuf, CommandError> {
        let png_bytes = png_data(image)?;
        super::write_png_bytes_atomically(output_path, &png_bytes)
    }

    fn png_data(image: &NSImage) -> Result<Vec<u8>, CommandError> {
        unsafe {
            let tiff = image.TIFFRepresentation().ok_or_else(|| {
                CommandError::SnapshotFailed("NSImage has no TIFF representation".into())
            })?;
            let rep: Retained<NSBitmapImageRep> = NSBitmapImageRep::imageRepWithData(&tiff)
                .ok_or_else(|| {
                    CommandError::SnapshotFailed("cannot build bitmap representation".into())
                })?;
            let png = rep
                .representationUsingType_properties(
                    NSBitmapImageFileType::PNG,
                    &NSDictionary::new(),
                )
                .ok_or_else(|| CommandError::SnapshotFailed("cannot encode PNG".into()))?;
            Ok(png.to_vec())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn eval_script_embeds_expression_as_json_string() {
        let script = build_eval_script(r#"document.title + "x""#);
        assert!(script.contains(r#"(0, eval)("document.title + \"x\"")"#));
    }

    #[test]
    fn parse_payload_success_value() {
        let value = parse_eval_payload(r#"{"ok":true,"value":{"a":[1,2],"b":null}}"#).unwrap();
        assert_eq!(value["a"][1], 2);
    }

    #[test]
    fn parse_payload_undefined_becomes_null() {
        let value = parse_eval_payload(r#"{"ok":true,"value":null}"#).unwrap();
        assert_eq!(value, Value::Null);
    }

    #[test]
    fn parse_payload_javascript_error() {
        let error =
            parse_eval_payload(r#"{"ok":false,"code":"javascript-error","message":"boom"}"#)
                .unwrap_err();
        assert_eq!(error.code(), "javascript-error");
        assert_eq!(error.message(), "boom");
    }

    #[test]
    fn parse_payload_unsupported_result() {
        let error = parse_eval_payload(
            r#"{"ok":false,"code":"unsupported-result","message":"result is a DOM node; serialize the fields you need explicitly"}"#,
        )
        .unwrap_err();
        assert_eq!(error.code(), "unsupported-result");
    }

    #[test]
    fn parse_payload_garbage_is_internal_error() {
        let error = parse_eval_payload("not json").unwrap_err();
        assert_eq!(error.code(), "internal-error");
    }

    #[test]
    fn failed_png_publish_removes_temporary_file() {
        let root = std::env::temp_dir().join(format!("collab-png-cleanup-{}", std::process::id()));
        let output = root.join("snapshot.png");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&output).unwrap();

        assert!(write_png_bytes_atomically(&output, b"png").is_err());
        assert!(!output.with_extension("png.tmp").exists());
    }

    #[test]
    fn png_publish_rejects_symlinked_screenshots_directory() {
        let root = std::env::temp_dir().join(format!(
            "collab-png-directory-symlink-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(crate::session::SESSION_DIR)).unwrap();
        let target = root.join("attacker-controlled");
        std::fs::create_dir_all(&target).unwrap();
        let victim = target.join("victim.txt");
        let original = b"screenshots directory victim";
        std::fs::write(&victim, original).unwrap();
        let screenshots = root.join(crate::session::SESSION_DIR).join("screenshots");
        symlink(&target, &screenshots).unwrap();

        assert!(write_png_bytes_atomically(&screenshots.join("snapshot.png"), b"png").is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), original);
        assert!(!target.join("snapshot.png").exists());
    }

    #[test]
    fn png_publish_does_not_follow_preplaced_temporary_symlink() {
        let root = std::env::temp_dir().join(format!(
            "collab-png-temporary-symlink-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let output = root.join("snapshot.png");
        let victim = root.join("victim.txt");
        let original = b"png temporary symlink victim";
        std::fs::write(&victim, original).unwrap();
        symlink(&victim, output.with_extension("png.tmp")).unwrap();

        assert_eq!(
            write_png_bytes_atomically(&output, b"complete png").unwrap(),
            output
        );

        assert_eq!(std::fs::read(&victim).unwrap(), original);
        assert_eq!(std::fs::read(&output).unwrap(), b"complete png");
    }

    #[test]
    fn png_publish_atomically_replaces_final_symlink_with_private_file() {
        let root =
            std::env::temp_dir().join(format!("collab-png-final-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let output = root.join("snapshot.png");
        let victim = root.join("victim.txt");
        let original = b"png final symlink victim";
        std::fs::write(&victim, original).unwrap();
        symlink(&victim, &output).unwrap();

        write_png_bytes_atomically(&output, b"complete png").unwrap();

        assert_eq!(std::fs::read(&victim).unwrap(), original);
        assert_eq!(std::fs::read(&output).unwrap(), b"complete png");
        assert!(
            !std::fs::symlink_metadata(&output)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
