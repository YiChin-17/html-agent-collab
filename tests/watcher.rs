//! Task 2.4 驗證：simulated watcher tests。
//! 真實 save-to-render 一秒時限由 macOS timing harness 驗證。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use collab::watcher::{self, DEBOUNCE};
use collab::webview::{WebviewCommand, command_channel};
use tokio::sync::mpsc;

static TEMP_DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "collab-watcher-test-{}-{}-{}",
        name,
        std::process::id(),
        TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("failed to create temp root");
    dir.canonicalize().unwrap()
}

async fn expect_reload(commands: &mut collab::webview::CommandReceiver, timeout: Duration) {
    let command = tokio::time::timeout(timeout, commands.recv())
        .await
        .expect("expected a reload command in time")
        .expect("command channel closed");
    match command {
        WebviewCommand::Reload { respond } => {
            let _ = respond.send(Ok(()));
        }
        other => panic!("expected Reload, got {other:?}"),
    }
}

#[tokio::test]
async fn burst_of_events_collapses_to_single_reload() {
    let (event_tx, event_rx) = mpsc::channel(64);
    let (command_tx, mut command_rx) = command_channel();
    tokio::spawn(watcher::run_debouncer(event_rx, command_tx));

    // 模擬 editor save burst：5 個事件間隔 < 200ms。
    for _ in 0..5 {
        event_tx.send(()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    expect_reload(&mut command_rx, DEBOUNCE * 10).await;

    // burst 結束後不得再有第二次 reload。
    let extra = tokio::time::timeout(DEBOUNCE * 3, command_rx.recv()).await;
    assert!(extra.is_err(), "burst must collapse to exactly one reload");
}

#[tokio::test]
async fn separate_saves_each_trigger_reload() {
    let (event_tx, event_rx) = mpsc::channel(64);
    let (command_tx, mut command_rx) = command_channel();
    tokio::spawn(watcher::run_debouncer(event_rx, command_tx));

    event_tx.send(()).await.unwrap();
    expect_reload(&mut command_rx, DEBOUNCE * 10).await;

    tokio::time::sleep(DEBOUNCE * 2).await;
    event_tx.send(()).await.unwrap();
    expect_reload(&mut command_rx, DEBOUNCE * 10).await;
}

#[tokio::test]
async fn fs_write_triggers_reload_but_collab_writes_do_not() {
    let root = temp_root("fs-events");

    let (event_tx, event_rx) = mpsc::channel(64);
    let (command_tx, mut command_rx) = command_channel();
    let _watcher = watcher::start_watcher(&root, event_tx).expect("failed to start watcher");
    tokio::spawn(watcher::run_debouncer(event_rx, command_tx));

    // 先以一般 project file event 確認 watcher ready，避免把延後送達的
    // setup event 誤認成後續 `.collab` 寫入造成的 reload。
    std::fs::write(root.join("index.html"), "<h1>v1</h1>").unwrap();
    expect_reload(&mut command_rx, Duration::from_secs(1)).await;

    std::fs::create_dir_all(root.join(".collab/screenshots")).unwrap();
    let setup_quiet = tokio::time::timeout(DEBOUNCE * 3, command_rx.recv()).await;
    assert!(
        setup_quiet.is_err(),
        ".collab setup must not trigger reload"
    );

    // .collab 下的寫入不得觸發 reload（避免 snapshot/session 自我迴圈）。
    std::fs::write(root.join(".collab/screenshots/shot.png"), [0u8; 16]).unwrap();
    let none = tokio::time::timeout(Duration::from_secs(2), command_rx.recv()).await;
    assert!(none.is_err(), ".collab writes must not trigger reload");

    // 專案檔案寫入必須在一秒內觸發 reload。
    std::fs::write(root.join("index.html"), "<h1>v2</h1>").unwrap();
    expect_reload(&mut command_rx, Duration::from_secs(1)).await;
}
