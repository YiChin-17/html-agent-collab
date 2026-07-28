//! notify 檔案監看與直接 reload。
//! design「notify 檔案監看與直接 reload」：macOS FSEvents backend 監看 project
//! root，editor save burst debounce 200ms 後經 main-thread adapter reload 現有
//! WKWebView；watcher 建立失敗回明確錯誤，不默默切到 polling。

use std::path::Path;
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::webview::{CommandSender, WebviewCommand};

/// editor save burst 的 debounce 窗口（spec: 200 milliseconds）。
pub const DEBOUNCE: Duration = Duration::from_millis(200);
pub const EVENT_QUEUE_CAPACITY: usize = 256;

/// 事件路徑是否應觸發 reload。
/// 排除 `.collab/`（session/screenshot 寫入會造成自我 reload 迴圈）與 `.git/`。
pub fn is_relevant_path(project_root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(project_root) else {
        return false;
    };
    !relative.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        name == ".collab" || name == ".git"
    })
}

fn is_relevant_event(project_root: &Path, event: &Event) -> bool {
    let kind_relevant = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    );
    kind_relevant
        && event
            .paths
            .iter()
            .any(|path| is_relevant_path(project_root, path))
}

/// 啟動 recursive watcher；相關事件以 marker 送入 debouncer channel。
/// 回傳的 watcher 必須存活整個 app lifetime，否則監看停止。
pub fn start_watcher(
    project_root: &Path,
    events: mpsc::Sender<()>,
) -> notify::Result<RecommendedWatcher> {
    let root = project_root.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        match result {
            Ok(event) => {
                if is_relevant_event(&root, &event) {
                    // queue 滿代表 debouncer 已有 pending burst，丟棄不影響語意。
                    let _ = events.try_send(());
                }
            }
            Err(e) => eprintln!("file watcher error: {e}"),
        }
    })?;
    watcher.watch(project_root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

/// burst debounce：第一個事件後等到連續 200ms 無新事件，才送出一次 Reload。
pub async fn run_debouncer(mut events: mpsc::Receiver<()>, commands: CommandSender) {
    loop {
        if events.recv().await.is_none() {
            return;
        }
        loop {
            match tokio::time::timeout(DEBOUNCE, events.recv()).await {
                Ok(Some(())) => continue,
                Ok(None) => return,
                Err(_) => break,
            }
        }
        let (respond, receive) = tokio::sync::oneshot::channel();
        // queue 滿時放棄本次 reload（下一個檔案事件會再觸發）。
        if commands
            .try_send(WebviewCommand::Reload { respond })
            .is_ok()
        {
            let _ = receive.await;
        }
    }
}

/// 每次 navigation 都注入：以 sessionStorage 保存/恢復 scroll position。
/// sessionStorage 在同一 WKWebView reload 間保留，未送出 overlay draft（task 3.x）
/// 也走同一機制。
pub const SCROLL_PRESERVE_SCRIPT: &str = r#"(function () {
  try {
    var KEY = "__collab_scroll__";
    var saved = sessionStorage.getItem(KEY);
    if (saved) {
      var pos = JSON.parse(saved);
      var restore = function () { window.scrollTo(pos[0], pos[1]); };
      if (document.readyState === "complete") restore();
      else window.addEventListener("load", restore, { once: true });
    }
    var save = function () {
      sessionStorage.setItem(KEY, JSON.stringify([window.scrollX, window.scrollY]));
    };
    window.addEventListener("scroll", save, { passive: true });
    window.addEventListener("pagehide", save);
  } catch (e) {}
})();"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn collab_and_git_paths_are_ignored() {
        let root = PathBuf::from("/tmp/project");
        assert!(is_relevant_path(&root, &root.join("index.html")));
        assert!(is_relevant_path(&root, &root.join("assets/app.js")));
        assert!(!is_relevant_path(&root, &root.join(".collab/session.json")));
        assert!(!is_relevant_path(
            &root,
            &root.join(".collab/screenshots/x.png")
        ));
        assert!(!is_relevant_path(&root, &root.join(".git/index")));
        assert!(!is_relevant_path(&root, Path::new("/elsewhere/file.html")));
    }
}
