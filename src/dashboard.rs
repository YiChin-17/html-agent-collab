//! Native collaboration dashboard 的 bounded view model、reducer 與 channel。
//! AppKit objects 僅存在於本模組的 macOS adapter；server 只接觸 typed data。

use tokio::sync::{mpsc, oneshot, watch};

use crate::core::{
    Attachment, CollaborationState, DashboardAction, DashboardActionError,
    DashboardAttachmentSummary, DashboardFeedbackCounts, DashboardFeedbackSummary,
    DashboardRuntimeState, DashboardSnapshot, FeedbackMarkerSnapshot, FeedbackMarkerSummary,
};
use crate::feedback::{FeedbackKind, FeedbackRecord, FeedbackState};

pub const DASHBOARD_FEEDBACK_LIMIT: usize = 50;
pub const DASHBOARD_ACTION_CAPACITY: usize = 16;
pub const FEEDBACK_MARKER_LIMIT: usize = 50;
pub const CONNECT_SKILL_NAME: &str = "$preview-collaboration-connect";

pub fn build_marker_snapshot(revision: u64, records: &[FeedbackRecord]) -> FeedbackMarkerSnapshot {
    let mut records = records
        .iter()
        .filter(|record| {
            record.kind == FeedbackKind::ElementComment && record.state != FeedbackState::Resolved
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    FeedbackMarkerSnapshot {
        revision,
        items: records
            .into_iter()
            .take(FEEDBACK_MARKER_LIMIT)
            .filter_map(|record| {
                record
                    .elements
                    .as_array()
                    .and_then(|elements| elements.first())
                    .cloned()
                    .map(|element| FeedbackMarkerSummary {
                        id: record.id.clone(),
                        state: record.state,
                        text: shorten(&record.text, 160),
                        failure_reason: record.failure_reason.clone(),
                        element,
                    })
            })
            .collect(),
    }
}

pub fn build_snapshot(
    revision: u64,
    runtime_state: DashboardRuntimeState,
    preview_session_id: &str,
    attachments: &[Attachment],
    records: &[FeedbackRecord],
    selected_attachment_id: Option<&str>,
    error: Option<String>,
) -> DashboardSnapshot {
    let attachments = attachments
        .iter()
        .filter(|attachment| attachment.collaboration_state.is_connected())
        .take(crate::server::ATTACHMENT_CAPACITY)
        .map(|attachment| DashboardAttachmentSummary {
            attachment_id: attachment.attachment_id.clone(),
            agent_kind: attachment.agent_kind.clone(),
            collaboration_state: attachment.collaboration_state,
        })
        .collect::<Vec<_>>();
    let selected_attachment_id = match selected_attachment_id {
        Some(selected) => attachments
            .iter()
            .any(|attachment| attachment.attachment_id == selected)
            .then(|| selected.to_string()),
        None => (attachments.len() == 1).then(|| attachments[0].attachment_id.clone()),
    };
    let mut feedback_counts = DashboardFeedbackCounts::default();
    for record in records {
        match record.state {
            FeedbackState::Pending => feedback_counts.pending += 1,
            FeedbackState::Acknowledged => feedback_counts.acknowledged += 1,
            FeedbackState::Working => feedback_counts.working += 1,
            FeedbackState::Resolved => feedback_counts.resolved += 1,
            FeedbackState::Failed => feedback_counts.failed += 1,
        }
    }
    let mut records = records.iter().collect::<Vec<_>>();
    records.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    let feedback_items = records
        .into_iter()
        .take(DASHBOARD_FEEDBACK_LIMIT)
        .map(|record| DashboardFeedbackSummary {
            id: record.id.clone(),
            kind: record.kind,
            state: record.state,
            text: shorten(&record.text, 160),
            orphaned: record.orphaned,
            failure_reason: record.failure_reason.clone(),
            attachment_id: record.lease.as_ref().map(|lease| lease.owner.clone()),
        })
        .collect();
    DashboardSnapshot {
        revision,
        runtime_state,
        preview_session_id: preview_session_id.to_string(),
        attachments,
        selected_attachment_id,
        feedback_counts,
        feedback_items,
        error,
    }
}

fn shorten(text: &str, limit: usize) -> String {
    let mut characters = text.chars();
    let shortened = characters.by_ref().take(limit).collect::<String>();
    if characters.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

#[derive(Clone)]
pub struct DashboardPublisher {
    snapshots: watch::Sender<DashboardSnapshot>,
}

impl DashboardPublisher {
    pub fn publish(&self, snapshot: DashboardSnapshot) {
        self.snapshots.send_replace(snapshot);
    }

    pub fn current(&self) -> DashboardSnapshot {
        self.snapshots.borrow().clone()
    }
}

#[derive(Clone)]
pub struct DashboardHandle {
    pub snapshots: watch::Receiver<DashboardSnapshot>,
    actions: mpsc::Sender<DashboardActionRequest>,
}

impl DashboardHandle {
    pub fn try_submit(
        &self,
        action: DashboardAction,
    ) -> Result<oneshot::Receiver<Result<u64, DashboardActionError>>, DashboardActionError> {
        let (respond, receive) = oneshot::channel();
        self.actions
            .try_send(DashboardActionRequest { action, respond })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => DashboardActionError::Busy,
                mpsc::error::TrySendError::Closed(_) => {
                    DashboardActionError::Internal("Dashboard action channel is closed".into())
                }
            })?;
        Ok(receive)
    }
}

#[derive(Debug)]
pub struct DashboardActionRequest {
    pub action: DashboardAction,
    respond: oneshot::Sender<Result<u64, DashboardActionError>>,
}

impl DashboardActionRequest {
    pub fn respond(self, result: Result<u64, DashboardActionError>) {
        let _ = self.respond.send(result);
    }
}

pub type DashboardActionReceiver = mpsc::Receiver<DashboardActionRequest>;

pub fn channel(
    initial: DashboardSnapshot,
) -> (DashboardPublisher, DashboardHandle, DashboardActionReceiver) {
    let (snapshots, snapshot_receiver) = watch::channel(initial);
    let (actions, action_receiver) = mpsc::channel(DASHBOARD_ACTION_CAPACITY);
    (
        DashboardPublisher { snapshots },
        DashboardHandle {
            snapshots: snapshot_receiver,
            actions,
        },
        action_receiver,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardControl {
    Pause,
    Resume,
    Stop,
    Close,
}

#[derive(Debug, Clone)]
struct PendingAction {
    action: DashboardAction,
    required_revision: Option<u64>,
}

pub struct DashboardReducer {
    snapshot: DashboardSnapshot,
    pending: Option<PendingAction>,
    banner: Option<String>,
    connect_handoff_open: bool,
}

impl DashboardReducer {
    pub fn new(snapshot: DashboardSnapshot) -> Self {
        Self {
            snapshot,
            pending: None,
            banner: None,
            connect_handoff_open: false,
        }
    }

    pub fn snapshot(&self) -> &DashboardSnapshot {
        &self.snapshot
    }

    pub fn banner(&self) -> Option<&str> {
        self.banner.as_deref()
    }

    pub fn clear_banner(&mut self) {
        self.banner = None;
    }

    pub fn pending_action(&self) -> Option<&DashboardAction> {
        self.pending.as_ref().map(|pending| &pending.action)
    }

    pub fn dashboard_visible(&self) -> bool {
        self.snapshot.runtime_state == DashboardRuntimeState::Running
    }

    pub fn feedback_tools_visible(&self) -> bool {
        self.snapshot
            .attachments
            .iter()
            .any(|attachment| attachment.collaboration_state == CollaborationState::Active)
    }

    pub fn connect_agent_available(&self) -> bool {
        self.snapshot.runtime_state == DashboardRuntimeState::Running
            && self.snapshot.attachments.is_empty()
    }

    pub fn connect_handoff_open(&self) -> bool {
        self.connect_agent_available() && self.connect_handoff_open
    }

    pub fn toggle_connect_handoff(&mut self) {
        self.connect_handoff_open = self.connect_agent_available() && !self.connect_handoff_open;
        self.banner = None;
    }

    pub fn connect_command(&self) -> Option<String> {
        self.connect_agent_available()
            .then(|| format!("{CONNECT_SKILL_NAME} {}", self.snapshot.preview_session_id))
    }

    pub fn record_connect_copy_result(&mut self, result: Result<(), String>) {
        self.banner = result.err();
    }

    pub fn select(&mut self, attachment_id: Option<&str>) {
        self.snapshot.selected_attachment_id = attachment_id
            .filter(|selected| {
                self.snapshot.attachments.iter().any(|attachment| {
                    attachment.attachment_id == *selected
                        && attachment.collaboration_state.is_connected()
                })
            })
            .map(str::to_string);
    }

    pub fn action_for(
        &self,
        control: DashboardControl,
    ) -> Result<DashboardAction, DashboardActionError> {
        if control == DashboardControl::Close {
            return Ok(DashboardAction::Close);
        }
        let selected = self
            .snapshot
            .selected_attachment_id
            .as_deref()
            .ok_or(DashboardActionError::SelectionRequired)?;
        let attachment = self
            .snapshot
            .attachments
            .iter()
            .find(|attachment| attachment.attachment_id == selected)
            .ok_or(DashboardActionError::AttachmentNotFound)?;
        let attachment_id = attachment.attachment_id.clone();
        match (control, attachment.collaboration_state) {
            (DashboardControl::Pause, CollaborationState::Active) => {
                Ok(DashboardAction::Pause { attachment_id })
            }
            (DashboardControl::Resume, CollaborationState::Paused) => {
                Ok(DashboardAction::Resume { attachment_id })
            }
            (DashboardControl::Stop, state) if state.is_connected() => {
                Ok(DashboardAction::Stop { attachment_id })
            }
            _ => Err(DashboardActionError::AttachmentInactive),
        }
    }

    pub fn begin_action(&mut self, action: DashboardAction) -> Result<(), DashboardActionError> {
        if self.pending.is_some() {
            return Err(DashboardActionError::Busy);
        }
        self.banner = None;
        self.pending = Some(PendingAction {
            action,
            required_revision: None,
        });
        Ok(())
    }

    pub fn complete_action(&mut self, result: Result<u64, DashboardActionError>) {
        match result {
            Ok(revision) => {
                if let Some(pending) = &mut self.pending {
                    pending.required_revision = Some(revision);
                }
                self.finish_pending_if_confirmed();
            }
            Err(error) => {
                self.banner = Some(error.message());
                self.pending = None;
            }
        }
    }

    pub fn apply_snapshot(&mut self, mut snapshot: DashboardSnapshot) {
        if snapshot.revision <= self.snapshot.revision {
            return;
        }
        if snapshot.error.is_some() && snapshot.feedback_items.is_empty() {
            snapshot.feedback_items = self.snapshot.feedback_items.clone();
            snapshot.feedback_counts = self.snapshot.feedback_counts.clone();
        }
        if snapshot.selected_attachment_id.is_none()
            && let Some(selected) = self.snapshot.selected_attachment_id.as_ref()
            && snapshot.attachments.iter().any(|attachment| {
                attachment.attachment_id == *selected
                    && attachment.collaboration_state.is_connected()
            })
        {
            snapshot.selected_attachment_id = Some(selected.clone());
        }
        if snapshot
            .selected_attachment_id
            .as_ref()
            .is_none_or(|selected| {
                !snapshot.attachments.iter().any(|attachment| {
                    attachment.attachment_id == *selected
                        && attachment.collaboration_state.is_connected()
                })
            })
        {
            snapshot.selected_attachment_id = None;
        }
        self.banner = snapshot.error.clone();
        self.snapshot = snapshot;
        if !self.connect_agent_available() {
            self.connect_handoff_open = false;
        }
        self.finish_pending_if_confirmed();
    }

    pub fn selected_state_text(&self) -> &'static str {
        match self
            .snapshot
            .selected_attachment_id
            .as_ref()
            .and_then(|selected| {
                self.snapshot
                    .attachments
                    .iter()
                    .find(|attachment| attachment.attachment_id == *selected)
            })
            .map(|attachment| attachment.collaboration_state)
        {
            Some(CollaborationState::Active) => "Active",
            Some(CollaborationState::PauseRequested) => "Pausing after current feedback",
            Some(CollaborationState::Paused) => "Paused",
            Some(CollaborationState::Inactive) | None => "No agent connected",
        }
    }

    fn finish_pending_if_confirmed(&mut self) {
        let confirmed = self.pending.as_ref().is_some_and(|pending| {
            pending.required_revision.is_some_and(|required| {
                self.snapshot.revision >= required
                    && action_matches(&pending.action, &self.snapshot)
            })
        });
        if confirmed {
            self.pending = None;
        }
    }
}

fn action_matches(action: &DashboardAction, snapshot: &DashboardSnapshot) -> bool {
    let state = |attachment_id: &str| {
        snapshot
            .attachments
            .iter()
            .find(|attachment| attachment.attachment_id == attachment_id)
            .map(|attachment| attachment.collaboration_state)
    };
    match action {
        DashboardAction::Pause { attachment_id } => matches!(
            state(attachment_id),
            Some(CollaborationState::PauseRequested | CollaborationState::Paused)
        ),
        DashboardAction::Resume { attachment_id } => {
            state(attachment_id) == Some(CollaborationState::Active)
        }
        DashboardAction::Stop { attachment_id } => state(attachment_id).is_none(),
        DashboardAction::Close => snapshot.runtime_state == DashboardRuntimeState::Closed,
    }
}

pub fn install(
    window: &tauri::WebviewWindow<tauri::Wry>,
    dashboard: DashboardHandle,
    command_sender: crate::webview::CommandSender,
    draft_states: tokio::sync::watch::Receiver<crate::draft_panel::DraftPanelState>,
    project_root: std::path::PathBuf,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return macos::install(
        window,
        dashboard,
        command_sender,
        draft_states,
        project_root,
    );

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (
            window,
            dashboard,
            command_sender,
            draft_states,
            project_root,
        );
        Err("native collaboration dashboard requires macOS".into())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{OnceCell, RefCell};
    use std::sync::{Arc, Mutex};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSAccessibility, NSAlert, NSAlertFirstButtonReturn, NSButton, NSColor, NSPasteboard,
        NSPasteboardTypeString, NSPopUpButton, NSPopover, NSStackView, NSTextField,
        NSTitlebarAccessoryViewController, NSUserInterfaceLayoutOrientation, NSView,
        NSViewController, NSWindow,
    };
    use objc2_foundation::{NSArray, NSEdgeInsets, NSObjectProtocol, NSRectEdge, NSSize, NSString};

    use crate::core::{CollaborationState, DashboardRuntimeState, DashboardSnapshot};

    use super::{DashboardControl, DashboardHandle, DashboardReducer};

    #[derive(Clone, Copy)]
    struct NativeDashboardPointers {
        state_label: usize,
        error_label: usize,
        selector: usize,
        pause: usize,
        resume: usize,
        stop: usize,
        connect: usize,
        info_button: usize,
        info_popover: usize,
        info_counts_label: usize,
        info_items_stack: usize,
        connect_popover: usize,
        connect_preview_id: usize,
        connect_command: usize,
        connect_copy: usize,
        connect_error_label: usize,
        draft: usize,
    }

    struct PopoverRetains {
        _info: Retained<NSPopover>,
        _connect: Retained<NSPopover>,
    }

    struct DashboardControllerIvars {
        dashboard: DashboardHandle,
        reducer: Arc<Mutex<DashboardReducer>>,
        window: tauri::WebviewWindow<tauri::Wry>,
        pointers: OnceCell<NativeDashboardPointers>,
        popover_retains: OnceCell<PopoverRetains>,
        draft_panel: RefCell<crate::draft_panel::DraftPanelController>,
    }

    define_class!(
        #[unsafe(super = NSTitlebarAccessoryViewController)]
        #[thread_kind = MainThreadOnly]
        #[ivars = DashboardControllerIvars]
        struct DashboardController;

        unsafe impl NSObjectProtocol for DashboardController {}

        impl DashboardController {
            #[unsafe(method(selectAttachment:))]
            fn select_attachment(&self, sender: &NSPopUpButton) {
                let mut reducer = self.ivars().reducer.lock().unwrap();
                let offset = usize::from(reducer.snapshot().attachments.len() > 1);
                let selected = usize::try_from(sender.indexOfSelectedItem())
                    .ok()
                    .and_then(|index| index.checked_sub(offset))
                    .and_then(|index| reducer.snapshot().attachments.get(index))
                    .map(|attachment| attachment.attachment_id.clone());
                reducer.select(selected.as_deref());
                drop(reducer);
                self.render();
            }

            #[unsafe(method(pause:))]
            fn pause(&self, _sender: &NSButton) {
                self.submit_control(DashboardControl::Pause);
            }

            #[unsafe(method(resume:))]
            fn resume(&self, _sender: &NSButton) {
                self.submit_control(DashboardControl::Resume);
            }

            #[unsafe(method(stop:))]
            fn stop(&self, _sender: &NSButton) {
                self.submit_control(DashboardControl::Stop);
            }

            #[unsafe(method(toggleDraft:))]
            fn toggle_draft(&self, sender: &NSButton) {
                let ns_window = match self.ivars().window.ns_window() {
                    Ok(window) => window.cast::<NSWindow>(),
                    Err(error) => {
                        self.fail(crate::core::DashboardActionError::Internal(format!(
                            "cannot open Preview Draft: {error}"
                        )));
                        return;
                    }
                };
                let Some(ns_window) = (unsafe { ns_window.as_ref() }) else {
                    self.fail(crate::core::DashboardActionError::Internal(
                        "cannot open Preview Draft".into(),
                    ));
                    return;
                };
                let target: &objc2::runtime::AnyObject = self.as_ref();
                match self
                    .ivars()
                    .draft_panel
                    .borrow_mut()
                    .toggle(ns_window, target, sender)
                {
                    Ok(_) => {
                        self.ivars().reducer.lock().unwrap().clear_banner();
                        self.render();
                    }
                    Err(error) => {
                        self.fail(crate::core::DashboardActionError::Internal(error));
                    }
                }
            }

            #[unsafe(method(draftUndo:))]
            fn draft_undo(&self, _sender: &NSButton) {
                self.run_draft_action("undo");
            }

            #[unsafe(method(draftRedo:))]
            fn draft_redo(&self, _sender: &NSButton) {
                self.run_draft_action("redo");
            }

            #[unsafe(method(draftReset:))]
            fn draft_reset(&self, _sender: &NSButton) {
                self.run_draft_action("reset");
            }

            #[unsafe(method(draftApply:))]
            fn draft_apply(&self, _sender: &NSButton) {
                self.run_draft_action("submit");
            }

            #[unsafe(method(connectAgent:))]
            fn connect_agent(&self, sender: &NSButton) {
                self.ivars()
                    .reducer
                    .lock()
                    .unwrap()
                    .toggle_connect_handoff();
                let pointers = *self.ivars().pointers.get().expect("dashboard pointers installed");
                let popover = unsafe { (pointers.connect_popover as *mut NSPopover).as_ref() };
                if let Some(popover) = popover {
                    if self.ivars().reducer.lock().unwrap().connect_handoff_open() {
                        let view: &NSView = sender.as_ref();
                        popover.showRelativeToRect_ofView_preferredEdge(
                            view.bounds(),
                            view,
                            NSRectEdge::MaxY,
                        );
                    } else {
                        unsafe { popover.performClose(None) };
                    }
                }
            }

            #[unsafe(method(showInfo:))]
            fn show_info(&self, sender: &NSButton) {
                let pointers = *self.ivars().pointers.get().expect("dashboard pointers installed");
                let popover = unsafe { (pointers.info_popover as *mut NSPopover).as_ref() };
                if let Some(popover) = popover {
                    if popover.isShown() {
                        unsafe { popover.performClose(None) };
                    } else {
                        let view: &NSView = sender.as_ref();
                        popover.showRelativeToRect_ofView_preferredEdge(
                            view.bounds(),
                            view,
                            NSRectEdge::MaxY,
                        );
                    }
                }
            }

            #[unsafe(method(copyConnectCommand:))]
            fn copy_connect_command(&self, _sender: &NSButton) {
                let command = self.ivars().reducer.lock().unwrap().connect_command();
                let result = command
                    .ok_or_else(|| "Connect agent is not available".to_string())
                    .and_then(|command| {
                        let pasteboard = NSPasteboard::generalPasteboard();
                        pasteboard.clearContents();
                        pasteboard
                            .setString_forType(
                                &NSString::from_str(&command),
                                unsafe { NSPasteboardTypeString },
                            )
                            .then_some(())
                            .ok_or_else(|| "Could not copy command to the pasteboard".to_string())
                    });
                let pointers = *self.ivars().pointers.get().expect("dashboard pointers installed");
                let error_label =
                    unsafe { (pointers.connect_error_label as *mut NSTextField).as_ref() };
                if let Some(error_label) = error_label {
                    match &result {
                        Ok(()) => {
                            error_label.setStringValue(&NSString::from_str(""));
                            error_label.setHidden(true);
                        }
                        Err(message) => {
                            error_label.setStringValue(&NSString::from_str(message));
                            error_label.setHidden(false);
                        }
                    }
                }
                self.ivars()
                    .reducer
                    .lock()
                    .unwrap()
                    .record_connect_copy_result(result);
            }

            #[unsafe(method(close:))]
            fn close(&self, _sender: &NSButton) {
                let ns_window = match self.ivars().window.ns_window() {
                    Ok(window) => window.cast::<NSWindow>(),
                    Err(error) => {
                        self.fail(crate::core::DashboardActionError::Internal(format!(
                            "cannot open Close preview confirmation: {error}"
                        )));
                        return;
                    }
                };
                let Some(ns_window) = (unsafe { ns_window.as_ref() }) else {
                    self.fail(crate::core::DashboardActionError::Internal(
                        "cannot open Close preview confirmation".into(),
                    ));
                    return;
                };
                let dashboard = self.ivars().dashboard.clone();
                let reducer = self.ivars().reducer.clone();
                let window = self.ivars().window.clone();
                let pointers = *self.ivars().pointers.get().expect("dashboard pointers installed");
                confirm_close(
                    ns_window,
                    move || {
                        submit_action(
                            dashboard,
                            reducer,
                            window,
                            pointers,
                            crate::core::DashboardAction::Close,
                        )
                    },
                    self.mtm(),
                );
            }
        }
    );

    impl DashboardController {
        fn new(
            mtm: MainThreadMarker,
            dashboard: DashboardHandle,
            reducer: Arc<Mutex<DashboardReducer>>,
            window: tauri::WebviewWindow<tauri::Wry>,
            command_sender: crate::webview::CommandSender,
            draft_states: tokio::sync::watch::Receiver<crate::draft_panel::DraftPanelState>,
            project_root: std::path::PathBuf,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(DashboardControllerIvars {
                dashboard,
                reducer,
                window: window.clone(),
                pointers: OnceCell::new(),
                popover_retains: OnceCell::new(),
                draft_panel: RefCell::new(crate::draft_panel::DraftPanelController::new(
                    command_sender,
                    window.clone(),
                    draft_states,
                    project_root,
                )),
            });
            unsafe { msg_send![super(this), init] }
        }

        fn submit_control(&self, control: DashboardControl) {
            let action = match self.ivars().reducer.lock().unwrap().action_for(control) {
                Ok(action) => action,
                Err(error) => {
                    self.fail(error);
                    return;
                }
            };
            submit_action(
                self.ivars().dashboard.clone(),
                self.ivars().reducer.clone(),
                self.ivars().window.clone(),
                *self
                    .ivars()
                    .pointers
                    .get()
                    .expect("dashboard pointers installed"),
                action,
            );
        }

        fn run_draft_action(&self, operation: &str) {
            match self
                .ivars()
                .draft_panel
                .borrow_mut()
                .perform_action(operation)
            {
                Ok(()) => {
                    self.ivars().reducer.lock().unwrap().clear_banner();
                    self.render();
                }
                Err(error) => {
                    self.fail(crate::core::DashboardActionError::Internal(error));
                }
            }
        }

        fn fail(&self, error: crate::core::DashboardActionError) {
            self.ivars()
                .reducer
                .lock()
                .unwrap()
                .complete_action(Err(error));
            self.render();
        }

        fn render(&self) {
            let pointers = *self
                .ivars()
                .pointers
                .get()
                .expect("dashboard pointers installed");
            let (snapshot, pending, banner) = presentation(&self.ivars().reducer);
            update_snapshot(pointers, &snapshot, pending, banner.as_deref());
        }
    }

    pub(super) fn install(
        window: &tauri::WebviewWindow<tauri::Wry>,
        dashboard: DashboardHandle,
        command_sender: crate::webview::CommandSender,
        draft_states: tokio::sync::watch::Receiver<crate::draft_panel::DraftPanelState>,
        project_root: std::path::PathBuf,
    ) -> Result<(), String> {
        let mtm = MainThreadMarker::new().ok_or_else(|| {
            "dashboard installation must run on the AppKit main thread".to_string()
        })?;
        let ns_window = window
            .ns_window()
            .map_err(|error| format!("cannot access preview NSWindow: {error}"))?
            .cast::<NSWindow>();
        let ns_window = unsafe {
            ns_window
                .as_ref()
                .ok_or_else(|| "preview NSWindow pointer is null".to_string())?
        };

        let snapshot = dashboard.snapshots.borrow().clone();
        let reducer = Arc::new(Mutex::new(DashboardReducer::new(snapshot.clone())));
        let controller = DashboardController::new(
            mtm,
            dashboard.clone(),
            reducer.clone(),
            window.clone(),
            command_sender,
            draft_states,
            project_root,
        );
        let state_text = snapshot
            .selected_attachment_id
            .as_ref()
            .and_then(|selected| {
                snapshot
                    .attachments
                    .iter()
                    .find(|attachment| attachment.attachment_id == *selected)
            })
            .map_or("No agent connected".to_string(), |attachment| {
                format!(
                    "{} — {:?}",
                    attachment.agent_kind, attachment.collaboration_state
                )
            });

        let state_label = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(&state_text),
                None::<&objc2::runtime::AnyObject>,
                None,
                mtm,
            )
        };
        state_label.setEnabled(false);
        state_label.setAccessibilityLabel(Some(&NSString::from_str("Collaboration state")));
        let error_label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        error_label.setAccessibilityLabel(Some(&NSString::from_str("Dashboard error")));
        let selector = NSPopUpButton::new(mtm);
        selector.setAccessibilityLabel(Some(&NSString::from_str("Collaboration attachment")));
        unsafe {
            selector.setTarget(Some(&controller));
            selector.setAction(Some(sel!(selectAttachment:)));
        }
        let pause = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Pause"),
                Some(&controller),
                Some(sel!(pause:)),
                mtm,
            )
        };
        let draft = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Draft"),
                Some(&controller),
                Some(sel!(toggleDraft:)),
                mtm,
            )
        };
        draft.setAccessibilityLabel(Some(&NSString::from_str("Preview Draft")));
        let resume = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Resume"),
                Some(&controller),
                Some(sel!(resume:)),
                mtm,
            )
        };
        let stop = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Stop collaboration"),
                Some(&controller),
                Some(sel!(stop:)),
                mtm,
            )
        };
        let connect = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Connect agent"),
                Some(&controller),
                Some(sel!(connectAgent:)),
                mtm,
            )
        };

        // Info button — opens feedback popover
        let info_button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Info"),
                Some(&controller),
                Some(sel!(showInfo:)),
                mtm,
            )
        };
        info_button.setAccessibilityLabel(Some(&NSString::from_str("Feedback info")));
        info_button.setHidden(snapshot.feedback_items.is_empty());

        // Info popover content
        let info_counts_label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        info_counts_label.setAccessibilityLabel(Some(&NSString::from_str("Feedback counts")));
        let info_items_stack = NSStackView::stackViewWithViews(&NSArray::from_slice(&[]), mtm);
        info_items_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        info_items_stack.setSpacing(4.0);
        let info_content_views: [&NSView; 2] = [&info_counts_label, &info_items_stack];
        let info_content_stack =
            NSStackView::stackViewWithViews(&NSArray::from_slice(&info_content_views), mtm);
        info_content_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        info_content_stack.setSpacing(8.0);
        info_content_stack.setFrameSize(NSSize::new(360.0, 200.0));
        let info_vc = NSViewController::new(mtm);
        info_vc.setView(&info_content_stack);
        let info_popover = NSPopover::new(mtm);
        info_popover.setContentViewController(Some(&info_vc));
        info_popover.setContentSize(NSSize::new(360.0, 200.0));
        info_popover.setBehavior(objc2_app_kit::NSPopoverBehavior::Transient);

        // Connect agent popover content
        let connect_preview_id = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        connect_preview_id.setAccessibilityLabel(Some(&NSString::from_str("Preview ID")));
        let connect_command = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        connect_command.setAccessibilityLabel(Some(&NSString::from_str("Connect command")));
        connect_command.setSelectable(true);
        let connect_copy = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Copy command"),
                Some(&controller),
                Some(sel!(copyConnectCommand:)),
                mtm,
            )
        };
        let connect_workspace = NSTextField::labelWithString(
            &NSString::from_str("Open the agent conversation in the same project workspace"),
            mtm,
        );
        let connect_error_label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        connect_error_label.setAccessibilityLabel(Some(&NSString::from_str("Copy error")));
        connect_error_label.setHidden(true);
        let connect_content_views: [&NSView; 5] = [
            &connect_preview_id,
            &connect_command,
            &connect_copy,
            &connect_workspace,
            &connect_error_label,
        ];
        let connect_content_stack =
            NSStackView::stackViewWithViews(&NSArray::from_slice(&connect_content_views), mtm);
        connect_content_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        connect_content_stack.setSpacing(8.0);
        connect_content_stack.setFrameSize(NSSize::new(400.0, 140.0));
        let connect_vc = NSViewController::new(mtm);
        connect_vc.setView(&connect_content_stack);
        let connect_popover = NSPopover::new(mtm);
        connect_popover.setContentViewController(Some(&connect_vc));
        connect_popover.setContentSize(NSSize::new(400.0, 140.0));
        connect_popover.setBehavior(objc2_app_kit::NSPopoverBehavior::Transient);

        let close = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Close preview"),
                Some(&controller),
                Some(sel!(close:)),
                mtm,
            )
        };
        let views: [&NSView; 10] = [
            &state_label,
            &error_label,
            &selector,
            &info_button,
            &draft,
            &pause,
            &resume,
            &stop,
            &connect,
            &close,
        ];
        let control_row = NSStackView::stackViewWithViews(&NSArray::from_slice(&views), mtm);
        control_row.setSpacing(8.0);
        control_row.setEdgeInsets(NSEdgeInsets {
            top: 0.0,
            left: 8.0,
            bottom: 0.0,
            right: 0.0,
        });
        control_row.setFrameSize(NSSize::new(680.0, 32.0));
        control_row.setAccessibilityLabel(Some(&NSString::from_str("Collaboration dashboard")));

        controller.setView(&control_row);
        controller.setAutomaticallyAdjustsSize(true);
        ns_window.addTitlebarAccessoryViewController(&controller);

        let pointers = NativeDashboardPointers {
            state_label: Retained::as_ptr(&state_label) as usize,
            error_label: Retained::as_ptr(&error_label) as usize,
            selector: Retained::as_ptr(&selector) as usize,
            pause: Retained::as_ptr(&pause) as usize,
            resume: Retained::as_ptr(&resume) as usize,
            stop: Retained::as_ptr(&stop) as usize,
            connect: Retained::as_ptr(&connect) as usize,
            info_button: Retained::as_ptr(&info_button) as usize,
            info_popover: Retained::as_ptr(&info_popover) as usize,
            info_counts_label: Retained::as_ptr(&info_counts_label) as usize,
            info_items_stack: Retained::as_ptr(&info_items_stack) as usize,
            connect_popover: Retained::as_ptr(&connect_popover) as usize,
            connect_preview_id: Retained::as_ptr(&connect_preview_id) as usize,
            connect_command: Retained::as_ptr(&connect_command) as usize,
            connect_copy: Retained::as_ptr(&connect_copy) as usize,
            connect_error_label: Retained::as_ptr(&connect_error_label) as usize,
            draft: Retained::as_ptr(&draft) as usize,
        };
        controller
            .ivars()
            .pointers
            .set(pointers)
            .map_err(|_| "dashboard native pointers were already installed".to_string())?;
        let _ = controller.ivars().popover_retains.set(PopoverRetains {
            _info: info_popover,
            _connect: connect_popover,
        });
        update_snapshot(pointers, &snapshot, false, snapshot.error.as_deref());

        let window = window.clone();
        let mut snapshots = dashboard.snapshots;
        tauri::async_runtime::spawn(async move {
            while snapshots.changed().await.is_ok() {
                let snapshot = snapshots.borrow_and_update().clone();
                reducer.lock().unwrap().apply_snapshot(snapshot);
                let (snapshot, pending, banner) = presentation(&reducer);
                if window
                    .run_on_main_thread(move || {
                        update_snapshot(pointers, &snapshot, pending, banner.as_deref())
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        Ok(())
    }

    fn update_snapshot(
        pointers: NativeDashboardPointers,
        snapshot: &DashboardSnapshot,
        pending: bool,
        banner: Option<&str>,
    ) {
        let state_label = unsafe { (pointers.state_label as *mut NSButton).as_ref() };
        let error_label = unsafe { (pointers.error_label as *mut NSTextField).as_ref() };
        let selector = unsafe { (pointers.selector as *mut NSPopUpButton).as_ref() };
        let pause = unsafe { (pointers.pause as *mut NSButton).as_ref() };
        let resume = unsafe { (pointers.resume as *mut NSButton).as_ref() };
        let stop = unsafe { (pointers.stop as *mut NSButton).as_ref() };
        let connect = unsafe { (pointers.connect as *mut NSButton).as_ref() };
        let info_button = unsafe { (pointers.info_button as *mut NSButton).as_ref() };
        let draft = unsafe { (pointers.draft as *mut NSButton).as_ref() };
        let info_counts_label =
            unsafe { (pointers.info_counts_label as *mut NSTextField).as_ref() };
        let info_items_stack = unsafe { (pointers.info_items_stack as *mut NSStackView).as_ref() };
        let connect_preview_id =
            unsafe { (pointers.connect_preview_id as *mut NSTextField).as_ref() };
        let connect_command = unsafe { (pointers.connect_command as *mut NSTextField).as_ref() };
        let connect_copy = unsafe { (pointers.connect_copy as *mut NSButton).as_ref() };
        let (
            Some(state_label),
            Some(error_label),
            Some(selector),
            Some(pause),
            Some(resume),
            Some(stop),
            Some(connect),
            Some(info_button),
            Some(draft),
            Some(info_counts_label),
            Some(info_items_stack),
            Some(connect_preview_id),
            Some(connect_command),
            Some(connect_copy),
        ) = (
            state_label,
            error_label,
            selector,
            pause,
            resume,
            stop,
            connect,
            info_button,
            draft,
            info_counts_label,
            info_items_stack,
            connect_preview_id,
            connect_command,
            connect_copy,
        )
        else {
            return;
        };

        let selected = snapshot
            .selected_attachment_id
            .as_ref()
            .and_then(|selected| {
                snapshot
                    .attachments
                    .iter()
                    .find(|attachment| attachment.attachment_id == *selected)
            });
        let state_text = if snapshot.runtime_state == DashboardRuntimeState::Closed {
            "Closing preview".to_string()
        } else {
            selected.map_or_else(
                || {
                    if snapshot.attachments.len() > 1 {
                        "Select an attachment".into()
                    } else {
                        "No agent connected".into()
                    }
                },
                |attachment| match attachment.collaboration_state {
                    CollaborationState::Active => format!("{} — Active", attachment.agent_kind),
                    CollaborationState::PauseRequested => {
                        format!("{} — Pausing after current feedback", attachment.agent_kind)
                    }
                    CollaborationState::Paused => format!("{} — Paused", attachment.agent_kind),
                    CollaborationState::Inactive => "No agent connected".into(),
                },
            )
        };
        let state_text = if pending {
            format!("{state_text} — Updating…")
        } else {
            state_text
        };
        state_label.setTitle(&NSString::from_str(&state_text));
        let tint = match selected.map(|a| a.collaboration_state) {
            Some(CollaborationState::Active) => (0.85, 0.94, 0.85),
            Some(CollaborationState::PauseRequested) => (1.0, 0.95, 0.80),
            Some(CollaborationState::Paused) => (1.0, 0.90, 0.80),
            _ => (0.92, 0.92, 0.92),
        };
        state_label.setBezelColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
            tint.0, tint.1, tint.2, 1.0,
        )));

        let error = banner.or(snapshot.error.as_deref()).unwrap_or("");
        error_label.setStringValue(&NSString::from_str(error));
        error_label.setHidden(error.is_empty());

        // Info button: visible only when feedback records exist
        let has_feedback = !snapshot.feedback_items.is_empty();
        info_button.setHidden(!has_feedback);
        let draft_available = snapshot.runtime_state == DashboardRuntimeState::Running
            && snapshot
                .attachments
                .iter()
                .any(|attachment| attachment.collaboration_state == CollaborationState::Active);
        draft.setHidden(!draft_available);
        draft.setEnabled(draft_available && !pending);

        // Update info popover content
        let counts_text = format!(
            "{} pending \u{00B7} {} working \u{00B7} {} resolved \u{00B7} {} failed",
            snapshot.feedback_counts.pending,
            snapshot.feedback_counts.working,
            snapshot.feedback_counts.resolved,
            snapshot.feedback_counts.failed
        );
        info_counts_label.setStringValue(&NSString::from_str(&counts_text));

        // Rebuild feedback items list in popover
        while info_items_stack.arrangedSubviews().count() > 0 {
            if let Some(subview) = info_items_stack.arrangedSubviews().firstObject() {
                let subview: Retained<NSView> = unsafe { Retained::cast_unchecked(subview) };
                info_items_stack.removeArrangedSubview(&subview);
                subview.removeFromSuperview();
            }
        }
        for item in &snapshot.feedback_items {
            let mut line = format!("{:?} — {}", item.kind, item.state);
            if item.state == crate::feedback::FeedbackState::Failed
                && let Some(reason) = &item.failure_reason
            {
                line.push_str(&format!(", reason: {reason}"));
            }
            let mtm = MainThreadMarker::new().unwrap();
            let label = NSTextField::labelWithString(&NSString::from_str(&line), mtm);
            label.setSelectable(true);
            info_items_stack.addArrangedSubview(&label);
        }

        selector.removeAllItems();
        let has_placeholder = snapshot.attachments.len() > 1;
        if has_placeholder {
            selector.addItemWithTitle(&NSString::from_str("Select attachment"));
        }
        for attachment in &snapshot.attachments {
            selector.addItemWithTitle(&NSString::from_str(&format!(
                "{} — {:?}",
                attachment.agent_kind, attachment.collaboration_state
            )));
        }
        if let Some(selected) = snapshot.selected_attachment_id.as_ref()
            && let Some(index) = snapshot
                .attachments
                .iter()
                .position(|attachment| attachment.attachment_id == *selected)
        {
            selector.selectItemAtIndex((index + usize::from(has_placeholder)) as isize);
        } else if has_placeholder {
            selector.selectItemAtIndex(0);
        }
        selector.setHidden(snapshot.attachments.len() < 2);
        selector.setEnabled(!pending);

        let state = selected.map(|attachment| attachment.collaboration_state);
        pause.setHidden(state != Some(CollaborationState::Active));
        resume.setHidden(state != Some(CollaborationState::Paused));
        stop.setHidden(!state.is_some_and(CollaborationState::is_connected));
        pause.setEnabled(!pending);
        resume.setEnabled(!pending);
        stop.setEnabled(!pending);
        let connect_available = snapshot.runtime_state == DashboardRuntimeState::Running
            && snapshot.attachments.is_empty();
        connect.setHidden(!connect_available);
        connect.setEnabled(!pending);

        // Update connect popover content
        connect_preview_id.setStringValue(&NSString::from_str(&format!(
            "Preview ID {}",
            snapshot.preview_session_id
        )));
        connect_command.setStringValue(&NSString::from_str(&format!(
            "$preview-collaboration-connect {}",
            snapshot.preview_session_id
        )));
        connect_copy.setEnabled(!pending);
    }

    fn presentation(
        reducer: &Arc<Mutex<DashboardReducer>>,
    ) -> (DashboardSnapshot, bool, Option<String>) {
        let reducer = reducer.lock().unwrap();
        (
            reducer.snapshot().clone(),
            reducer.pending_action().is_some(),
            reducer.banner().map(str::to_string),
        )
    }

    fn submit_action(
        dashboard: DashboardHandle,
        reducer: Arc<Mutex<DashboardReducer>>,
        window: tauri::WebviewWindow<tauri::Wry>,
        pointers: NativeDashboardPointers,
        action: crate::core::DashboardAction,
    ) {
        {
            let mut reducer = reducer.lock().unwrap();
            if let Err(error) = reducer.begin_action(action.clone()) {
                reducer.complete_action(Err(error));
                let snapshot = reducer.snapshot().clone();
                let banner = reducer.banner().map(str::to_string);
                drop(reducer);
                update_snapshot(pointers, &snapshot, false, banner.as_deref());
                return;
            }
        }
        let (snapshot, pending, banner) = presentation(&reducer);
        update_snapshot(pointers, &snapshot, pending, banner.as_deref());

        let completion = match dashboard.try_submit(action) {
            Ok(completion) => completion,
            Err(error) => {
                reducer.lock().unwrap().complete_action(Err(error));
                let (snapshot, pending, banner) = presentation(&reducer);
                update_snapshot(pointers, &snapshot, pending, banner.as_deref());
                return;
            }
        };
        tauri::async_runtime::spawn(async move {
            let result = match completion.await {
                Ok(result) => result,
                Err(_) => Err(crate::core::DashboardActionError::Internal(
                    "Dashboard action response channel closed".into(),
                )),
            };
            reducer.lock().unwrap().complete_action(result);
            let (snapshot, pending, banner) = presentation(&reducer);
            let _ = window.run_on_main_thread(move || {
                update_snapshot(pointers, &snapshot, pending, banner.as_deref())
            });
        });
    }

    fn confirm_close(
        window: &NSWindow,
        confirmed: impl FnOnce() + Send + 'static,
        mtm: MainThreadMarker,
    ) {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("Close preview?"));
        alert.setInformativeText(&NSString::from_str(
            "Close preview stops every collaboration attachment.",
        ));
        alert.addButtonWithTitle(&NSString::from_str("Close preview"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        let confirmed = std::sync::Mutex::new(Some(confirmed));
        let completion = RcBlock::new(move |response| {
            if response == NSAlertFirstButtonReturn
                && let Some(confirmed) = confirmed.lock().unwrap().take()
            {
                confirmed();
            }
        });
        alert.beginSheetModalForWindow_completionHandler(window, Some(&completion));
    }
}
