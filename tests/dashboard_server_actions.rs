use std::path::PathBuf;
use std::time::Duration;

use collab::core::{
    CollaborationState, DashboardAction, DashboardActionError, DashboardRuntimeState,
};
use collab::server::{self, ServerConfig};
use collab::webview::{CommandError, WebviewCommand};

fn temp_root() -> PathBuf {
    named_temp_root("actions")
}

fn named_temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("collab-dashboard-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

async fn wait_for_snapshot(
    snapshots: &mut tokio::sync::watch::Receiver<collab::core::DashboardSnapshot>,
    predicate: impl Fn(&collab::core::DashboardSnapshot) -> bool,
) -> collab::core::DashboardSnapshot {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = snapshots.borrow_and_update().clone();
            if predicate(&snapshot) {
                return snapshot;
            }
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .expect("dashboard snapshot did not reach expected state")
}

// spec「Native Offline Paint command is lifecycle-gated」：server 在
// attachment lifecycle 邊界內重新確認 zero connected，才把 toggle 送給 overlay。
#[tokio::test]
async fn offline_paint_eligibility_is_rechecked_inside_the_attachment_lifecycle() {
    let root = named_temp_root("offline-paint");
    let token = "offline-paint-token";
    let (commands, mut receiver) = collab::webview::command_channel();
    let (toggles, mut observed_toggles) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(command) = receiver.recv().await {
            match command {
                WebviewCommand::ToggleOfflinePaint { respond } => {
                    let _ = toggles.send(());
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::SetCollaborationActive { respond, .. }
                | WebviewCommand::SyncFeedbackMarkers { respond, .. }
                | WebviewCommand::Reload { respond } => {
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::Eval { respond, .. } => {
                    let _ = respond.send(Ok(serde_json::Value::Null));
                }
                WebviewCommand::Snapshot { respond, .. } => {
                    let _ = respond.send(Err(CommandError::SnapshotFailed("not used".into())));
                }
                WebviewCommand::CapturePainting { respond, .. } => {
                    let _ = respond.send(Err(CommandError::SnapshotFailed("not used".into())));
                }
            }
        }
    });
    let running = server::start(ServerConfig {
        project_root: root,
        session_id: "offline-paint-session".into(),
        token: token.into(),
        commands,
    })
    .await
    .unwrap();
    let dashboard = running.dashboard.clone();
    let mut snapshots = dashboard.snapshots.clone();

    dashboard
        .try_submit(DashboardAction::ToggleOfflinePaint)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert!(
        observed_toggles.recv().await.is_some(),
        "zero-connected toggle must reach the overlay"
    );

    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/__collab__/control/attach",
            running.port
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({"agentKind": "codex", "pid": 42}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());

    let rejected = dashboard
        .try_submit(DashboardAction::ToggleOfflinePaint)
        .unwrap()
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(rejected, DashboardActionError::OfflinePaintUnavailable);
    assert!(
        observed_toggles.try_recv().is_err(),
        "a connected attachment must stop the toggle before it reaches the overlay"
    );
    let connected =
        wait_for_snapshot(&mut snapshots, |snapshot| !snapshot.attachments.is_empty()).await;
    assert_eq!(
        connected.attachments[0].collaboration_state,
        CollaborationState::Active
    );

    dashboard
        .try_submit(DashboardAction::Close)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    running.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn native_actions_run_on_the_server_lifecycle_owner_and_publish_completion_revisions() {
    let root = temp_root();
    let token = "dashboard-action-token";
    let (commands, mut receiver) = collab::webview::command_channel();
    tokio::spawn(async move {
        while let Some(command) = receiver.recv().await {
            match command {
                WebviewCommand::SetCollaborationActive { respond, .. }
                | WebviewCommand::SyncFeedbackMarkers { respond, .. }
                | WebviewCommand::ToggleOfflinePaint { respond }
                | WebviewCommand::Reload { respond } => {
                    let _ = respond.send(Ok(()));
                }
                WebviewCommand::Eval { respond, .. } => {
                    let _ = respond.send(Ok(serde_json::Value::Null));
                }
                WebviewCommand::Snapshot { respond, .. } => {
                    let _ = respond.send(Err(CommandError::SnapshotFailed("not used".into())));
                }
                WebviewCommand::CapturePainting { respond, .. } => {
                    let _ = respond.send(Err(CommandError::SnapshotFailed("not used".into())));
                }
            }
        }
    });
    let running = server::start(ServerConfig {
        project_root: root,
        session_id: "dashboard-session".into(),
        token: token.into(),
        commands,
    })
    .await
    .unwrap();
    let dashboard = running.dashboard.clone();
    let mut snapshots = dashboard.snapshots.clone();

    let response = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/__collab__/control/attach",
            running.port
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({"agentKind": "codex", "pid": 42}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let body: serde_json::Value = response.json().await.unwrap();
    let attachment_id = body["attachment"]["attachmentId"].as_str().unwrap();

    let pause = dashboard
        .try_submit(DashboardAction::Pause {
            attachment_id: attachment_id.into(),
        })
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    let paused = wait_for_snapshot(&mut snapshots, |snapshot| {
        snapshot.revision >= pause
            && snapshot.attachments[0].collaboration_state == CollaborationState::Paused
    })
    .await;
    assert_eq!(paused.revision, pause);

    let resume = dashboard
        .try_submit(DashboardAction::Resume {
            attachment_id: attachment_id.into(),
        })
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    wait_for_snapshot(&mut snapshots, |snapshot| {
        snapshot.revision >= resume
            && snapshot.attachments[0].collaboration_state == CollaborationState::Active
    })
    .await;

    let stop = dashboard
        .try_submit(DashboardAction::Stop {
            attachment_id: attachment_id.into(),
        })
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    wait_for_snapshot(&mut snapshots, |snapshot| {
        snapshot.revision >= stop && snapshot.attachments.is_empty()
    })
    .await;

    let missing = dashboard
        .try_submit(DashboardAction::Pause {
            attachment_id: "missing".into(),
        })
        .unwrap()
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(missing, DashboardActionError::AttachmentNotFound);

    let close_revision = dashboard
        .try_submit(DashboardAction::Close)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    wait_for_snapshot(&mut snapshots, |snapshot| {
        snapshot.revision >= close_revision
            && snapshot.runtime_state == DashboardRuntimeState::Closed
    })
    .await;
    running.task.await.unwrap().unwrap();
    tokio::task::yield_now().await;
    assert_eq!(
        dashboard.try_submit(DashboardAction::Close).unwrap_err(),
        DashboardActionError::Internal("Dashboard action channel is closed".into())
    );
    snapshots.borrow_and_update();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), snapshots.changed())
            .await
            .expect("dashboard snapshot channel remained open after close")
            .is_err()
    );
}
