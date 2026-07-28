//! Preview Draft 的原生 AppKit split workspace。
//! 只重組既有 content view，不建立第二個 WKWebView。

pub const DRAFT_PREFERRED_WIDTH: f64 = 480.0;
pub const DRAFT_MINIMUM_WIDTH: f64 = 360.0;
pub const PREVIEW_MINIMUM_WIDTH: f64 = 640.0;
pub const MINIMUM_WORKSPACE_WIDTH: f64 = 1000.0;
pub const DEBOUNCE_MILLISECONDS: u64 = 250;
pub const DRAFT_BUTTON_HEIGHT: f64 = 22.0;
pub const DRAFT_CONTROL_SPACING: f64 = 6.0;
pub const DRAFT_HORIZONTAL_INSET: f64 = 8.0;
pub const TOOLBAR_STRIP_HEIGHT: f64 = 34.0;
pub const VALIDATION_STRIP_HEIGHT: f64 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlSyntaxKind {
    Doctype,
    Comment,
    TagDelimiter,
    TagName,
    AttributeName,
    AttributeValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtmlSyntaxSpan {
    pub kind: HtmlSyntaxKind,
    pub location: usize,
    pub length: usize,
}

pub fn html_syntax_spans(source: &str) -> Vec<HtmlSyntaxSpan> {
    let bytes = source.as_bytes();
    let utf16_offsets = utf16_offsets(source);
    let mut spans = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if starts_with_ascii_case(bytes, index, b"<!--") {
            let end = source[index + 4..]
                .find("-->")
                .map_or(bytes.len(), |offset| index + 4 + offset + 3);
            push_html_span(
                &mut spans,
                HtmlSyntaxKind::Comment,
                index,
                end,
                &utf16_offsets,
            );
            index = end;
            continue;
        }
        if starts_with_ascii_case(bytes, index, b"<!doctype") {
            let end = bytes[index..]
                .iter()
                .position(|byte| *byte == b'>')
                .map_or(bytes.len(), |offset| index + offset + 1);
            push_html_span(
                &mut spans,
                HtmlSyntaxKind::Doctype,
                index,
                end,
                &utf16_offsets,
            );
            index = end;
            continue;
        }
        if bytes[index] != b'<' {
            index += source[index..].chars().next().unwrap().len_utf8();
            continue;
        }

        let closing_tag = bytes.get(index + 1) == Some(&b'/');
        let delimiter_end = index + if closing_tag { 2 } else { 1 };
        push_html_span(
            &mut spans,
            HtmlSyntaxKind::TagDelimiter,
            index,
            delimiter_end,
            &utf16_offsets,
        );

        let mut cursor = delimiter_end;
        skip_ascii_whitespace(bytes, &mut cursor);
        let tag_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| is_html_name_byte(*byte))
        {
            cursor += 1;
        }
        if tag_start == cursor {
            index = delimiter_end;
            continue;
        }
        push_html_span(
            &mut spans,
            HtmlSyntaxKind::TagName,
            tag_start,
            cursor,
            &utf16_offsets,
        );
        let raw_text_closing = (!closing_tag
            && (source[tag_start..cursor].eq_ignore_ascii_case("style")
                || source[tag_start..cursor].eq_ignore_ascii_case("script")))
        .then(|| format!("</{}", &source[tag_start..cursor]));

        while cursor < bytes.len() {
            skip_ascii_whitespace(bytes, &mut cursor);
            if starts_with_ascii_case(bytes, cursor, b"/>") {
                push_html_span(
                    &mut spans,
                    HtmlSyntaxKind::TagDelimiter,
                    cursor,
                    cursor + 2,
                    &utf16_offsets,
                );
                cursor += 2;
                break;
            }
            if bytes.get(cursor) == Some(&b'>') {
                push_html_span(
                    &mut spans,
                    HtmlSyntaxKind::TagDelimiter,
                    cursor,
                    cursor + 1,
                    &utf16_offsets,
                );
                cursor += 1;
                break;
            }
            if cursor >= bytes.len() {
                break;
            }

            let attribute_start = cursor;
            while bytes.get(cursor).is_some_and(|byte| {
                !byte.is_ascii_whitespace() && !matches!(*byte, b'=' | b'>' | b'/')
            }) {
                cursor += source[cursor..].chars().next().unwrap().len_utf8();
            }
            if attribute_start == cursor {
                cursor += source[cursor..].chars().next().unwrap().len_utf8();
                continue;
            }
            push_html_span(
                &mut spans,
                HtmlSyntaxKind::AttributeName,
                attribute_start,
                cursor,
                &utf16_offsets,
            );

            skip_ascii_whitespace(bytes, &mut cursor);
            if bytes.get(cursor) != Some(&b'=') {
                continue;
            }
            cursor += 1;
            skip_ascii_whitespace(bytes, &mut cursor);
            let Some(quote @ (b'\'' | b'"')) = bytes.get(cursor).copied() else {
                while bytes
                    .get(cursor)
                    .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
                {
                    cursor += source[cursor..].chars().next().unwrap().len_utf8();
                }
                continue;
            };
            let value_start = cursor;
            cursor += 1;
            while bytes.get(cursor).is_some_and(|byte| *byte != quote) {
                cursor += source[cursor..].chars().next().unwrap().len_utf8();
            }
            if bytes.get(cursor) == Some(&quote) {
                cursor += 1;
            }
            push_html_span(
                &mut spans,
                HtmlSyntaxKind::AttributeValue,
                value_start,
                cursor,
                &utf16_offsets,
            );
        }

        index = cursor;
        if let Some(closing) = raw_text_closing {
            let closing = closing.as_bytes();
            index = (index..bytes.len())
                .find(|candidate| {
                    starts_with_ascii_case(bytes, *candidate, closing)
                        && bytes.get(candidate + closing.len()).is_some_and(|byte| {
                            byte.is_ascii_whitespace() || matches!(*byte, b'/' | b'>')
                        })
                })
                .unwrap_or(bytes.len());
        }
    }

    spans
}

fn utf16_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0; source.len() + 1];
    let mut utf16_offset = 0;
    for (byte_offset, character) in source.char_indices() {
        offsets[byte_offset] = utf16_offset;
        utf16_offset += character.len_utf16();
        offsets[byte_offset + character.len_utf8()] = utf16_offset;
    }
    offsets
}

fn push_html_span(
    spans: &mut Vec<HtmlSyntaxSpan>,
    kind: HtmlSyntaxKind,
    byte_start: usize,
    byte_end: usize,
    utf16_offsets: &[usize],
) {
    let location = utf16_offsets[byte_start];
    spans.push(HtmlSyntaxSpan {
        kind,
        location,
        length: utf16_offsets[byte_end] - location,
    });
}

fn starts_with_ascii_case(source: &[u8], offset: usize, expected: &[u8]) -> bool {
    source
        .get(offset..offset.saturating_add(expected.len()))
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(expected))
}

fn skip_ascii_whitespace(source: &[u8], offset: &mut usize) {
    while source.get(*offset).is_some_and(u8::is_ascii_whitespace) {
        *offset += 1;
    }
}

fn is_html_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftSourceRange {
    pub location: usize,
    pub length: usize,
}

pub fn unique_source_range(
    source_html: &str,
    focus_html: &str,
) -> Result<DraftSourceRange, &'static str> {
    if focus_html.is_empty() {
        return Err("source-location-unavailable");
    }
    let mut matches = source_html.match_indices(focus_html);
    let Some((byte_start, _)) = matches.next() else {
        return Err("source-location-unavailable");
    };
    if matches.next().is_some() {
        return Err("source-location-unavailable");
    }
    Ok(DraftSourceRange {
        location: source_html[..byte_start].encode_utf16().count(),
        length: focus_html.encode_utf16().count(),
    })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftPanelError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftPanelState {
    pub status: String,
    pub page_url: Option<String>,
    pub selector: Option<String>,
    pub focus_html: Option<String>,
    pub original_html: Option<String>,
    pub current_html: Option<String>,
    pub dirty: bool,
    pub undo_depth: usize,
    pub redo_depth: usize,
    pub error: Option<DraftPanelError>,
}

impl Default for DraftPanelState {
    fn default() -> Self {
        Self {
            status: "idle".into(),
            page_url: None,
            selector: None,
            focus_html: None,
            original_html: None,
            current_html: None,
            dirty: false,
            undo_depth: 0,
            redo_depth: 0,
            error: None,
        }
    }
}

pub fn validate_state(value: serde_json::Value) -> Result<DraftPanelState, String> {
    let state: DraftPanelState =
        serde_json::from_value(value).map_err(|error| format!("invalid draft state: {error}"))?;
    if !matches!(state.status.as_str(), "idle" | "editing" | "submitted") {
        return Err("draft state has an unknown status".into());
    }
    if state.undo_depth > 50 || state.redo_depth > 50 {
        return Err("draft state history exceeds its bounded limit".into());
    }
    for document in [&state.original_html, &state.current_html]
        .into_iter()
        .flatten()
    {
        if document.len() > crate::feedback::PREVIEW_DRAFT_DOCUMENT_LIMIT_BYTES {
            return Err("draft state document exceeds its bounded limit".into());
        }
    }
    if state.status != "idle"
        && (state.page_url.is_none()
            || state.original_html.is_none()
            || state.current_html.is_none())
    {
        return Err("active draft state requires page URL and complete HTML documents".into());
    }
    Ok(state)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DraftFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DraftPanelLayout {
    pub toolbar: DraftFrame,
    pub validation: DraftFrame,
    pub editor: DraftFrame,
}

pub fn draft_panel_layout(bounds: DraftFrame, validation_visible: bool) -> DraftPanelLayout {
    let validation_height = if validation_visible {
        VALIDATION_STRIP_HEIGHT
    } else {
        0.0
    };
    let editor_height = (bounds.height - TOOLBAR_STRIP_HEIGHT - validation_height).max(0.0);
    DraftPanelLayout {
        toolbar: DraftFrame {
            x: bounds.x + DRAFT_HORIZONTAL_INSET,
            y: bounds.y + bounds.height - (TOOLBAR_STRIP_HEIGHT + DRAFT_BUTTON_HEIGHT) / 2.0,
            width: (bounds.width - DRAFT_HORIZONTAL_INSET * 2.0).max(0.0),
            height: DRAFT_BUTTON_HEIGHT,
        },
        validation: DraftFrame {
            x: bounds.x + DRAFT_HORIZONTAL_INSET,
            y: bounds.y + editor_height,
            width: (bounds.width - DRAFT_HORIZONTAL_INSET * 2.0).max(0.0),
            height: validation_height,
        },
        editor: DraftFrame {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: editor_height,
        },
    }
}

pub fn split_position_bounds(content_width: f64) -> (f64, f64) {
    (
        PREVIEW_MINIMUM_WIDTH,
        (content_width - DRAFT_MINIMUM_WIDTH).max(PREVIEW_MINIMUM_WIDTH),
    )
}

pub fn initial_split_preview_width(content_width: f64) -> f64 {
    let (minimum, maximum) = split_position_bounds(content_width);
    (content_width * 0.6).clamp(minimum, maximum)
}

pub fn expanded_frame(
    saved_window_frame: DraftFrame,
    visible_frame: DraftFrame,
) -> Result<DraftFrame, &'static str> {
    if visible_frame.width < MINIMUM_WORKSPACE_WIDTH {
        return Err("insufficient-space");
    }

    let width = (saved_window_frame.width + DRAFT_PREFERRED_WIDTH)
        .max(MINIMUM_WORKSPACE_WIDTH)
        .min(visible_frame.width);
    let max_x = visible_frame.x + visible_frame.width - width;
    let x = saved_window_frame.x.min(max_x).max(visible_frame.x);
    Ok(DraftFrame {
        x,
        y: saved_window_frame.y,
        width,
        height: saved_window_frame.height,
    })
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
    use std::time::Duration;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSAccessibility, NSAutoresizingMaskOptions, NSButton, NSColor, NSControlSize,
        NSControlStateValueOff, NSControlStateValueOn, NSFont, NSFontWeightRegular,
        NSForegroundColorAttributeName, NSScreen, NSScrollView, NSSplitView, NSSplitViewDelegate,
        NSStackView, NSTextDelegate, NSTextField, NSTextView, NSTextViewDelegate,
        NSUserInterfaceLayoutOrientation, NSView, NSWindow,
    };
    use objc2_foundation::{
        NSArray, NSNotification, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString,
    };
    use tokio::sync::oneshot;

    use crate::webview::{CommandSender, WebviewCommand};

    use super::{
        DEBOUNCE_MILLISECONDS, DRAFT_BUTTON_HEIGHT, DRAFT_CONTROL_SPACING, DRAFT_PREFERRED_WIDTH,
        DraftFrame, draft_panel_layout, expanded_frame, initial_split_preview_width,
        split_position_bounds,
    };

    const DRAFT_STATUS_IDLE: u8 = 0;
    const DRAFT_STATUS_EDITING: u8 = 1;
    const DRAFT_STATUS_SUBMITTED: u8 = 2;

    struct DraftTextDelegateIvars {
        command_sender: CommandSender,
        window: tauri::WebviewWindow<tauri::Wry>,
        input_generation: Arc<AtomicU64>,
        input_pending: Arc<AtomicBool>,
        suppress_changes: Arc<AtomicBool>,
    }

    define_class!(
        #[unsafe(super = objc2_foundation::NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = DraftTextDelegateIvars]
        struct DraftTextDelegate;

        unsafe impl NSObjectProtocol for DraftTextDelegate {}

        unsafe impl NSTextDelegate for DraftTextDelegate {
            #[allow(non_snake_case)]
            #[unsafe(method(textDidChange:))]
            fn textDidChange(&self, notification: &NSNotification) {
                if self.ivars().suppress_changes.load(Ordering::SeqCst) {
                    return;
                }
                let Some(object) = notification.object() else {
                    return;
                };
                let editor = unsafe { (Retained::as_ptr(&object) as *const NSTextView).as_ref() };
                let Some(editor) = editor else {
                    return;
                };
                let html = editor.string().to_string();
                let editor_pointer = Retained::as_ptr(&object) as usize;
                let generation = self.ivars().input_generation.fetch_add(1, Ordering::SeqCst) + 1;
                self.ivars().input_pending.store(true, Ordering::SeqCst);
                let sender = self.ivars().command_sender.clone();
                let window = self.ivars().window.clone();
                let current_generation = self.ivars().input_generation.clone();
                let input_pending = self.ivars().input_pending.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(DEBOUNCE_MILLISECONDS)).await;
                    if current_generation.load(Ordering::SeqCst) != generation {
                        return;
                    }
                    let pending_on_error = input_pending.clone();
                    if window
                        .run_on_main_thread(move || {
                            if current_generation.load(Ordering::SeqCst) != generation {
                                return;
                            }
                            let Some(editor) =
                                (unsafe { (editor_pointer as *mut NSTextView).as_ref() })
                            else {
                                return;
                            };
                            apply_html_syntax(editor);
                            let (respond, receive) = oneshot::channel();
                            if sender
                                .try_send(WebviewCommand::Eval {
                                    expression: draft_apply_expression(&html),
                                    respond,
                                })
                                .is_err()
                            {
                                input_pending.store(false, Ordering::SeqCst);
                                return;
                            }
                            tauri::async_runtime::spawn(async move {
                                let _ = receive.await;
                                input_pending.store(false, Ordering::SeqCst);
                            });
                        })
                        .is_err()
                    {
                        pending_on_error.store(false, Ordering::SeqCst);
                    }
                });
            }
        }

        unsafe impl NSTextViewDelegate for DraftTextDelegate {}
    );

    impl DraftTextDelegate {
        fn new(
            mtm: MainThreadMarker,
            command_sender: CommandSender,
            window: tauri::WebviewWindow<tauri::Wry>,
            input_generation: Arc<AtomicU64>,
            input_pending: Arc<AtomicBool>,
            suppress_changes: Arc<AtomicBool>,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(DraftTextDelegateIvars {
                command_sender,
                window,
                input_generation,
                input_pending,
                suppress_changes,
            });
            unsafe { msg_send![super(this), init] }
        }
    }

    define_class!(
        #[unsafe(super = objc2_foundation::NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = ()]
        struct DraftSplitDelegate;

        unsafe impl NSObjectProtocol for DraftSplitDelegate {}

        unsafe impl NSSplitViewDelegate for DraftSplitDelegate {
            #[allow(non_snake_case)]
            #[unsafe(method(splitView:constrainMinCoordinate:ofSubviewAt:))]
            fn splitView_constrainMinCoordinate_ofSubviewAt(
                &self,
                _split_view: &NSSplitView,
                proposed_minimum_position: f64,
                _divider_index: isize,
            ) -> f64 {
                proposed_minimum_position.max(super::PREVIEW_MINIMUM_WIDTH)
            }

            #[allow(non_snake_case)]
            #[unsafe(method(splitView:constrainMaxCoordinate:ofSubviewAt:))]
            fn splitView_constrainMaxCoordinate_ofSubviewAt(
                &self,
                split_view: &NSSplitView,
                proposed_maximum_position: f64,
                _divider_index: isize,
            ) -> f64 {
                let content_width = split_view.bounds().size.width - split_view.dividerThickness();
                let (_, maximum) = split_position_bounds(content_width);
                proposed_maximum_position.min(maximum)
            }

            #[allow(non_snake_case)]
            #[unsafe(method(splitView:constrainSplitPosition:ofSubviewAt:))]
            fn splitView_constrainSplitPosition_ofSubviewAt(
                &self,
                split_view: &NSSplitView,
                proposed_position: f64,
                _divider_index: isize,
            ) -> f64 {
                let content_width = split_view.bounds().size.width - split_view.dividerThickness();
                let (minimum, maximum) = split_position_bounds(content_width);
                proposed_position.clamp(minimum, maximum)
            }
        }
    );

    impl DraftSplitDelegate {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            unsafe { msg_send![super(this), init] }
        }
    }

    struct NativeDraftWorkspace {
        saved_window_frame: NSRect,
        original_content_view: Retained<NSView>,
        original_webview_superview: Retained<NSView>,
        split_view: Retained<NSSplitView>,
        panel: Retained<NSView>,
        toolbar: Retained<NSStackView>,
        scroll_view: Retained<NSScrollView>,
        editor: Retained<NSTextView>,
        validation_message: Retained<NSTextField>,
        _text_delegate: Retained<DraftTextDelegate>,
        _split_delegate: Retained<DraftSplitDelegate>,
    }

    pub struct DraftPanelController {
        command_sender: CommandSender,
        draft_states: tokio::sync::watch::Receiver<super::DraftPanelState>,
        window: tauri::WebviewWindow<tauri::Wry>,
        project_root: PathBuf,
        workspace: Option<NativeDraftWorkspace>,
        non_blocking_error: Option<String>,
        poll_generation: Arc<AtomicU64>,
        input_generation: Arc<AtomicU64>,
        input_pending: Arc<AtomicBool>,
        suppress_changes: Arc<AtomicBool>,
        editor_pointer: Arc<AtomicUsize>,
        draft_dirty: Arc<AtomicBool>,
        draft_status: Arc<AtomicU8>,
    }

    impl DraftPanelController {
        pub fn new(
            command_sender: CommandSender,
            window: tauri::WebviewWindow<tauri::Wry>,
            draft_states: tokio::sync::watch::Receiver<super::DraftPanelState>,
            project_root: PathBuf,
        ) -> Self {
            Self {
                command_sender,
                draft_states,
                window,
                project_root,
                workspace: None,
                non_blocking_error: None,
                poll_generation: Arc::new(AtomicU64::new(0)),
                input_generation: Arc::new(AtomicU64::new(0)),
                input_pending: Arc::new(AtomicBool::new(false)),
                suppress_changes: Arc::new(AtomicBool::new(false)),
                editor_pointer: Arc::new(AtomicUsize::new(0)),
                draft_dirty: Arc::new(AtomicBool::new(false)),
                draft_status: Arc::new(AtomicU8::new(DRAFT_STATUS_IDLE)),
            }
        }

        pub fn is_open(&self) -> bool {
            self.workspace.is_some()
        }

        pub fn toggle(
            &mut self,
            window: &NSWindow,
            target: &AnyObject,
            draft_toggle: &NSButton,
        ) -> Result<bool, String> {
            if self.is_open() {
                if (self.draft_dirty.load(Ordering::SeqCst)
                    || self.input_pending.load(Ordering::SeqCst))
                    && self.draft_status.load(Ordering::SeqCst) == DRAFT_STATUS_EDITING
                {
                    let error =
                        "Apply, Reset, or continue editing the current Preview Draft".to_string();
                    self.non_blocking_error = Some(error.clone());
                    self.show_validation_message(&error);
                    draft_toggle.setState(NSControlStateValueOn);
                    return Ok(true);
                }
                let operation =
                    if self.draft_status.load(Ordering::SeqCst) == DRAFT_STATUS_SUBMITTED {
                        "setMode(null)"
                    } else {
                        "resetPreviewDraft()"
                    };
                self.dispatch(operation)?;
                self.restore_original_content_hierarchy(window);
                draft_toggle.setState(NSControlStateValueOff);
                return Ok(false);
            }

            let (page_url, source_html) = self.load_current_source()?;
            match self.install(window, target, &source_html) {
                Ok(()) => {
                    draft_toggle.setState(NSControlStateValueOn);
                    let load_expression = format!(
                        "loadPreviewDraftSource({{ pageUrl: {}, html: {} }})",
                        serde_json::to_string(&page_url).expect("page URL is serializable"),
                        serde_json::to_string(&source_html).expect("source HTML is serializable")
                    );
                    if let Err(error) = self
                        .dispatch(&load_expression)
                        .and_then(|()| self.dispatch("setMode('draft')"))
                    {
                        self.rollback_install(window, draft_toggle);
                        return Err(error);
                    }
                    self.start_state_updates();
                    Ok(true)
                }
                Err(error) => {
                    self.non_blocking_error = Some(error.clone());
                    self.rollback_install(window, draft_toggle);
                    Err(error)
                }
            }
        }

        pub fn perform_action(&mut self, operation: &str) -> Result<(), String> {
            let operation = match operation {
                "undo" => "undoPreviewDraft()",
                "redo" => "redoPreviewDraft()",
                "reset" => "resetPreviewDraft()",
                "submit" => "submitPreviewDraft()",
                _ => return Err("unknown Preview Draft operation".into()),
            };
            self.hide_validation_message();
            self.dispatch(operation)
        }

        pub fn editor_html(&self) -> Option<String> {
            self.workspace
                .as_ref()
                .map(|workspace| workspace.editor.string().to_string())
        }

        pub fn schedule_editor_apply(&mut self) -> Result<(), String> {
            let Some(html) = self.editor_html() else {
                return Err("Preview Draft editor is closed".into());
            };
            let delay = DEBOUNCE_MILLISECONDS;
            let expression = format!(
                "new Promise(resolve => setTimeout(() => resolve({}), {delay}))",
                draft_apply_expression(&html)
            );
            self.dispatch_expression(expression)
        }

        fn load_current_source(&self) -> Result<(String, String), String> {
            let page_url = self
                .window
                .url()
                .map_err(|error| format!("cannot read current preview URL: {error}"))?;
            let resource =
                crate::server::resolve_project_resource(&self.project_root, page_url.path())
                    .map_err(|error| format!("cannot load current preview source: {error}"))?;
            let is_html = resource
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
                });
            if !is_html {
                return Err("Preview Draft requires a local HTML resource".into());
            }
            let source_html = std::fs::read_to_string(&resource)
                .map_err(|error| format!("cannot read current UTF-8 HTML source: {error}"))?;
            if source_html.len() > crate::feedback::PREVIEW_DRAFT_DOCUMENT_LIMIT_BYTES {
                return Err("current HTML source exceeds the Preview Draft size limit".into());
            }
            Ok((page_url.to_string(), source_html))
        }

        fn install(
            &mut self,
            window: &NSWindow,
            target: &AnyObject,
            source_html: &str,
        ) -> Result<(), String> {
            let mtm = MainThreadMarker::new()
                .ok_or_else(|| "Preview Draft must open on the AppKit main thread".to_string())?;
            let saved_window_frame = window.frame();
            let screen: Retained<NSScreen> = window
                .screen()
                .ok_or_else(|| "Preview Draft cannot determine the visible screen".to_string())?;
            let visible_frame = screen.visibleFrame();
            let expanded = expanded_frame(
                DraftFrame {
                    x: saved_window_frame.origin.x,
                    y: saved_window_frame.origin.y,
                    width: saved_window_frame.size.width,
                    height: saved_window_frame.size.height,
                },
                DraftFrame {
                    x: visible_frame.origin.x,
                    y: visible_frame.origin.y,
                    width: visible_frame.size.width,
                    height: visible_frame.size.height,
                },
            )
            .map_err(str::to_string)?;
            let original_content_view = window.contentView().ok_or_else(|| {
                "Preview Draft cannot access the preview content view".to_string()
            })?;
            let original_webview_superview = original_content_view.clone();
            let panel_height = original_content_view.bounds().size.height;
            let initial_panel_layout = draft_panel_layout(
                DraftFrame {
                    x: 0.0,
                    y: 0.0,
                    width: DRAFT_PREFERRED_WIDTH,
                    height: panel_height,
                },
                false,
            );

            let split_view = NSSplitView::new(mtm);
            split_view.setVertical(true);
            split_view.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(expanded.width, original_content_view.bounds().size.height),
            ));
            let split_delegate = DraftSplitDelegate::new(mtm);
            split_view.setDelegate(Some(ProtocolObject::from_ref(&*split_delegate)));

            let editor = NSTextView::new(mtm);
            editor.setRichText(false);
            editor.setImportsGraphics(false);
            editor.setString(&NSString::from_str(source_html));
            editor.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
                13.0,
                unsafe { NSFontWeightRegular },
            )));
            apply_html_syntax(&editor);
            editor.setAccessibilityLabel(Some(&NSString::from_str("Preview Draft HTML")));
            editor.setFrameSize(NSSize::new(
                initial_panel_layout.editor.width,
                initial_panel_layout.editor.height,
            ));
            let text_delegate = DraftTextDelegate::new(
                mtm,
                self.command_sender.clone(),
                self.window.clone(),
                self.input_generation.clone(),
                self.input_pending.clone(),
                self.suppress_changes.clone(),
            );
            editor.setDelegate(Some(ProtocolObject::from_ref(&*text_delegate)));

            let scroll_view = NSScrollView::new(mtm);
            scroll_view.setHasVerticalScroller(true);
            scroll_view.setAutohidesScrollers(true);
            scroll_view.setDocumentView(Some(&editor));
            scroll_view.setFrame(draft_frame_to_ns_rect(initial_panel_layout.editor));
            scroll_view.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );

            let undo = toolbar_button("Undo", target, objc2::sel!(draftUndo:), mtm);
            let redo = toolbar_button("Redo", target, objc2::sel!(draftRedo:), mtm);
            let reset = toolbar_button("Reset", target, objc2::sel!(draftReset:), mtm);
            let apply = toolbar_button("Apply to source", target, objc2::sel!(draftApply:), mtm);
            let toolbar_views: [&NSView; 4] = [&undo, &redo, &reset, &apply];
            let toolbar =
                NSStackView::stackViewWithViews(&NSArray::from_slice(&toolbar_views), mtm);
            toolbar.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
            toolbar.setSpacing(DRAFT_CONTROL_SPACING);
            toolbar.setAccessibilityLabel(Some(&NSString::from_str("Preview Draft actions")));
            toolbar.setFrame(draft_frame_to_ns_rect(initial_panel_layout.toolbar));
            toolbar.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewMinYMargin,
            );

            let validation_message = NSTextField::labelWithString(&NSString::from_str(""), mtm);
            validation_message
                .setAccessibilityLabel(Some(&NSString::from_str("Preview Draft validation")));
            validation_message.setHidden(true);
            validation_message.setFrame(draft_frame_to_ns_rect(initial_panel_layout.validation));
            validation_message.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewMinYMargin,
            );

            let panel = NSView::new(mtm);
            panel.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(DRAFT_PREFERRED_WIDTH, panel_height),
            ));
            panel.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            panel.addSubview(&scroll_view);
            panel.addSubview(&toolbar);
            panel.addSubview(&validation_message);

            window.setFrame_display(
                NSRect::new(
                    NSPoint::new(expanded.x, expanded.y),
                    NSSize::new(expanded.width, expanded.height),
                ),
                true,
            );
            window.setContentView(Some(&split_view));
            split_view.addArrangedSubview(&original_content_view);
            split_view.addArrangedSubview(&panel);
            split_view.layoutSubtreeIfNeeded();
            let content_width = split_view.bounds().size.width - split_view.dividerThickness();
            let preview_width = initial_split_preview_width(content_width);
            split_view.setPosition_ofDividerAtIndex(preview_width, 0);
            split_view.layoutSubtreeIfNeeded();
            apply_draft_panel_layout(&panel, &toolbar, &validation_message, &scroll_view, false);

            self.workspace = Some(NativeDraftWorkspace {
                saved_window_frame,
                original_content_view,
                original_webview_superview,
                split_view,
                panel,
                toolbar,
                scroll_view,
                editor,
                validation_message,
                _text_delegate: text_delegate,
                _split_delegate: split_delegate,
            });
            let editor_pointer = self
                .workspace
                .as_ref()
                .map(|workspace| Retained::as_ptr(&workspace.editor) as usize)
                .unwrap_or(0);
            self.editor_pointer.store(editor_pointer, Ordering::SeqCst);
            self.draft_dirty.store(false, Ordering::SeqCst);
            self.draft_status.store(DRAFT_STATUS_IDLE, Ordering::SeqCst);
            Ok(())
        }

        fn rollback_install(&mut self, window: &NSWindow, draft_toggle: &NSButton) {
            self.restore_original_content_hierarchy(window);
            draft_toggle.setState(NSControlStateValueOff);
        }

        fn restore_original_content_hierarchy(&mut self, window: &NSWindow) {
            self.poll_generation.fetch_add(1, Ordering::SeqCst);
            self.input_generation.fetch_add(1, Ordering::SeqCst);
            self.input_pending.store(false, Ordering::SeqCst);
            self.editor_pointer.store(0, Ordering::SeqCst);
            let Some(workspace) = self.workspace.take() else {
                return;
            };
            workspace.original_content_view.removeFromSuperview();
            window.setContentView(Some(&workspace.original_content_view));
            window.setFrame_display(workspace.saved_window_frame, true);
            let _ = workspace.original_webview_superview;
            let _ = workspace.split_view;
        }

        fn start_state_updates(&self) {
            let generation = self.poll_generation.fetch_add(1, Ordering::SeqCst) + 1;
            let current_generation = self.poll_generation.clone();
            let mut states = self.draft_states.clone();
            let window = self.window.clone();
            let editor_pointer = self.editor_pointer.clone();
            let validation_message_pointer = self
                .workspace
                .as_ref()
                .map(|workspace| Retained::as_ptr(&workspace.validation_message) as usize)
                .unwrap_or(0);
            let panel_pointer = self
                .workspace
                .as_ref()
                .map(|workspace| Retained::as_ptr(&workspace.panel) as usize)
                .unwrap_or(0);
            let toolbar_pointer = self
                .workspace
                .as_ref()
                .map(|workspace| Retained::as_ptr(&workspace.toolbar) as usize)
                .unwrap_or(0);
            let scroll_view_pointer = self
                .workspace
                .as_ref()
                .map(|workspace| Retained::as_ptr(&workspace.scroll_view) as usize)
                .unwrap_or(0);
            let input_pending = self.input_pending.clone();
            let suppress_changes = self.suppress_changes.clone();
            let draft_dirty = self.draft_dirty.clone();
            let draft_status = self.draft_status.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    if states.changed().await.is_err() {
                        break;
                    }
                    if current_generation.load(Ordering::SeqCst) != generation {
                        break;
                    }
                    let state = states.borrow_and_update().clone();
                    let status = match state.status.as_str() {
                        "editing" => DRAFT_STATUS_EDITING,
                        "submitted" => DRAFT_STATUS_SUBMITTED,
                        _ => DRAFT_STATUS_IDLE,
                    };
                    draft_status.store(status, Ordering::SeqCst);
                    draft_dirty.store(state.dirty, Ordering::SeqCst);
                    let validation_error = state.error.as_ref().map(|error| error.message.clone());
                    let html = state.current_html.unwrap_or_default();
                    let focus_range = state
                        .focus_html
                        .as_deref()
                        .and_then(|focus_html| super::unique_source_range(&html, focus_html).ok());
                    let editor_pointer = editor_pointer.load(Ordering::SeqCst);
                    if editor_pointer == 0 {
                        continue;
                    }
                    let suppress_changes = suppress_changes.clone();
                    let current_generation = current_generation.clone();
                    let input_is_pending = input_pending.load(Ordering::SeqCst);
                    let _ = window.run_on_main_thread(move || {
                        if current_generation.load(Ordering::SeqCst) != generation {
                            return;
                        }
                        let Some(editor) =
                            (unsafe { (editor_pointer as *mut NSTextView).as_ref() })
                        else {
                            return;
                        };
                        if let Some(validation_message) =
                            unsafe { (validation_message_pointer as *mut NSTextField).as_ref() }
                        {
                            validation_message.setStringValue(&NSString::from_str(
                                validation_error.as_deref().unwrap_or_default(),
                            ));
                            validation_message.setHidden(validation_error.is_none());
                            if let (Some(panel), Some(toolbar), Some(scroll_view)) = unsafe {
                                (
                                    (panel_pointer as *mut NSView).as_ref(),
                                    (toolbar_pointer as *mut NSStackView).as_ref(),
                                    (scroll_view_pointer as *mut NSScrollView).as_ref(),
                                )
                            } {
                                apply_draft_panel_layout(
                                    panel,
                                    toolbar,
                                    validation_message,
                                    scroll_view,
                                    validation_error.is_some(),
                                );
                            }
                        }
                        editor.setEditable(status != DRAFT_STATUS_SUBMITTED);
                        if input_is_pending || validation_error.is_some() {
                            return;
                        }
                        if editor.string().to_string() != html {
                            suppress_changes.store(true, Ordering::SeqCst);
                            editor.setString(&NSString::from_str(&html));
                            suppress_changes.store(false, Ordering::SeqCst);
                        }
                        apply_html_syntax(editor);
                        if let Some(range) = focus_range {
                            let range = NSRange::new(range.location, range.length);
                            editor.setSelectedRange(range);
                            editor.scrollRangeToVisible(range);
                        }
                    });
                }
            });
        }

        fn dispatch(&mut self, operation: &str) -> Result<(), String> {
            self.dispatch_expression(format!(
                "window.__collabOverlay && window.__collabOverlay.{operation}"
            ))
        }

        fn dispatch_expression(&mut self, expression: String) -> Result<(), String> {
            let (respond, receive) = oneshot::channel();
            self.command_sender
                .try_send(WebviewCommand::Eval {
                    expression,
                    respond,
                })
                .map_err(|error| {
                    let message = match error {
                        tokio::sync::mpsc::error::TrySendError::Full(_) => {
                            "Preview Draft command queue is busy"
                        }
                        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                            "Preview Draft command queue is closed"
                        }
                    };
                    self.non_blocking_error = Some(message.into());
                    message.to_string()
                })?;
            tauri::async_runtime::spawn(async move {
                let _ = receive.await;
            });
            Ok(())
        }

        fn show_validation_message(&self, message: &str) {
            if let Some(workspace) = &self.workspace {
                workspace
                    .validation_message
                    .setStringValue(&NSString::from_str(message));
                workspace.validation_message.setHidden(false);
                apply_draft_panel_layout(
                    &workspace.panel,
                    &workspace.toolbar,
                    &workspace.validation_message,
                    &workspace.scroll_view,
                    true,
                );
            }
        }

        fn hide_validation_message(&self) {
            if let Some(workspace) = &self.workspace {
                workspace.validation_message.setHidden(true);
                apply_draft_panel_layout(
                    &workspace.panel,
                    &workspace.toolbar,
                    &workspace.validation_message,
                    &workspace.scroll_view,
                    false,
                );
            }
        }
    }

    fn toolbar_button(
        label: &str,
        target: &AnyObject,
        action: objc2::runtime::Sel,
        mtm: MainThreadMarker,
    ) -> Retained<NSButton> {
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(label),
                Some(target),
                Some(action),
                mtm,
            )
        };
        button.setAccessibilityLabel(Some(&NSString::from_str(label)));
        button.setControlSize(NSControlSize::Small);
        button.sizeToFit();
        button.setFrameSize(NSSize::new(button.frame().size.width, DRAFT_BUTTON_HEIGHT));
        button
    }

    fn apply_draft_panel_layout(
        panel: &NSView,
        toolbar: &NSStackView,
        validation_message: &NSTextField,
        scroll_view: &NSScrollView,
        validation_visible: bool,
    ) {
        let bounds = panel.bounds();
        let layout = draft_panel_layout(
            DraftFrame {
                x: bounds.origin.x,
                y: bounds.origin.y,
                width: bounds.size.width,
                height: bounds.size.height,
            },
            validation_visible,
        );
        toolbar.setFrame(draft_frame_to_ns_rect(layout.toolbar));
        validation_message.setFrame(draft_frame_to_ns_rect(layout.validation));
        scroll_view.setFrame(draft_frame_to_ns_rect(layout.editor));
    }

    fn draft_frame_to_ns_rect(frame: DraftFrame) -> NSRect {
        NSRect::new(
            NSPoint::new(frame.x, frame.y),
            NSSize::new(frame.width, frame.height),
        )
    }

    fn apply_html_syntax(editor: &NSTextView) {
        let source = editor.string().to_string();
        let source_length = source.encode_utf16().count();
        let selection = editor.selectedRange();
        let Some(layout_manager) = (unsafe { editor.layoutManager() }) else {
            return;
        };
        let full_range = NSRange::new(0, source_length);
        let foreground_color_attribute = unsafe { NSForegroundColorAttributeName };
        layout_manager
            .removeTemporaryAttribute_forCharacterRange(foreground_color_attribute, full_range);
        for span in super::html_syntax_spans(&source) {
            if span.location.saturating_add(span.length) > source_length {
                continue;
            }
            let color = match span.kind {
                super::HtmlSyntaxKind::Doctype => NSColor::systemPurpleColor(),
                super::HtmlSyntaxKind::Comment => NSColor::secondaryLabelColor(),
                super::HtmlSyntaxKind::TagDelimiter => NSColor::systemBlueColor(),
                super::HtmlSyntaxKind::TagName => NSColor::systemTealColor(),
                super::HtmlSyntaxKind::AttributeName => NSColor::systemOrangeColor(),
                super::HtmlSyntaxKind::AttributeValue => NSColor::systemGreenColor(),
            };
            unsafe {
                layout_manager.addTemporaryAttribute_value_forCharacterRange(
                    foreground_color_attribute,
                    &color,
                    NSRange::new(span.location, span.length),
                );
            }
        }
        if selection.location.saturating_add(selection.length) <= source_length {
            editor.setSelectedRange(selection);
        }
    }

    fn draft_apply_expression(html: &str) -> String {
        format!(
            "window.__collabOverlay.applyPreviewDraft({{ html: {} }})",
            serde_json::to_string(html).expect("editor text is serializable")
        )
    }
}

#[cfg(target_os = "macos")]
pub use macos::DraftPanelController;

#[cfg(not(target_os = "macos"))]
pub struct DraftPanelController;

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16_fragment(source: &str, span: HtmlSyntaxSpan) -> String {
        String::from_utf16(
            &source
                .encode_utf16()
                .skip(span.location)
                .take(span.length)
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn html_syntax_scanner_emits_ordered_non_overlapping_utf16_spans() {
        let source = "<!doctype html><!-- note --><h1 class=\"title\" data-label='台灣'>Hello</h1>";
        let spans = html_syntax_spans(source);
        let actual = spans
            .iter()
            .map(|span| (span.kind, utf16_fragment(source, *span)))
            .collect::<Vec<_>>();

        assert_eq!(
            actual,
            vec![
                (HtmlSyntaxKind::Doctype, "<!doctype html>".into()),
                (HtmlSyntaxKind::Comment, "<!-- note -->".into()),
                (HtmlSyntaxKind::TagDelimiter, "<".into()),
                (HtmlSyntaxKind::TagName, "h1".into()),
                (HtmlSyntaxKind::AttributeName, "class".into()),
                (HtmlSyntaxKind::AttributeValue, "\"title\"".into()),
                (HtmlSyntaxKind::AttributeName, "data-label".into()),
                (HtmlSyntaxKind::AttributeValue, "'台灣'".into()),
                (HtmlSyntaxKind::TagDelimiter, ">".into()),
                (HtmlSyntaxKind::TagDelimiter, "</".into()),
                (HtmlSyntaxKind::TagName, "h1".into()),
                (HtmlSyntaxKind::TagDelimiter, ">".into()),
            ]
        );
        assert!(
            spans
                .windows(2)
                .all(|pair| { pair[0].location + pair[0].length <= pair[1].location })
        );
        assert!(
            spans
                .iter()
                .all(|span| span.location + span.length <= source.encode_utf16().count())
        );
    }

    #[test]
    fn html_syntax_scanner_preserves_unicode_boundaries() {
        let source = "<p title=\"台灣\">界面</p>";
        let spans = html_syntax_spans(source);

        assert!(spans.iter().any(|span| {
            span.kind == HtmlSyntaxKind::AttributeValue
                && utf16_fragment(source, *span) == "\"台灣\""
        }));
        assert!(
            !spans
                .iter()
                .any(|span| utf16_fragment(source, *span).contains("界面"))
        );
    }

    #[test]
    fn html_syntax_scanner_keeps_incomplete_markup_in_bounds() {
        for source in ["<div class=\"open", "<!-- note", "<span data-label='台灣"] {
            let spans = html_syntax_spans(source);
            let source_length = source.encode_utf16().count();

            assert!(
                spans
                    .iter()
                    .all(|span| span.location + span.length <= source_length)
            );
            assert!(
                spans
                    .windows(2)
                    .all(|pair| { pair[0].location + pair[0].length <= pair[1].location })
            );
        }
    }

    #[test]
    fn html_syntax_scanner_leaves_incomplete_raw_text_content_unstyled() {
        let source = "<style>.note::before { content: \"<b>\"; }";
        let spans = html_syntax_spans(source);

        assert!(
            spans
                .iter()
                .all(|span| span.location + span.length <= "<style>".encode_utf16().count())
        );
    }

    #[test]
    fn html_syntax_scanner_ignores_invalid_raw_text_closing_prefixes() {
        for (opening, invalid_closing, valid_closing) in [
            ("<script>", "</script-not-a-tag>", "</script>"),
            ("<script>", "</scriptx>", "</script>"),
            ("<style>", "</style-note>", "</style>"),
            ("<style>", "</stylex>", "</style>"),
        ] {
            let source =
                format!("{opening}raw{invalid_closing}<b>inside</b>{valid_closing}<p>after</p>");
            let spans = html_syntax_spans(&source);
            let raw_text_start = opening.encode_utf16().count();
            let valid_closing_start = source
                .find(valid_closing)
                .expect("source contains a valid closing tag");
            let valid_closing_start = source[..valid_closing_start].encode_utf16().count();

            assert!(
                spans
                    .iter()
                    .filter(|span| span.location >= raw_text_start)
                    .all(|span| span.location >= valid_closing_start),
                "invalid closing prefix must remain unstyled: {invalid_closing}"
            );
            assert!(spans.iter().any(|span| {
                span.kind == HtmlSyntaxKind::TagName && utf16_fragment(&source, *span) == "p"
            }));
        }
    }

    #[test]
    fn html_syntax_scanner_accepts_raw_text_closing_tag_boundaries() {
        for (opening, closing) in [
            ("<script>", "</ScRiPt>"),
            ("<script>", "</ScRiPt\t>"),
            ("<script>", "</ScRiPt/>"),
            ("<style>", "</StYlE >"),
        ] {
            let source = format!("{opening}<b>raw</b>{closing}<p>after</p>");
            let spans = html_syntax_spans(&source);
            let tag_names = spans
                .iter()
                .filter(|span| span.kind == HtmlSyntaxKind::TagName)
                .map(|span| utf16_fragment(&source, *span))
                .collect::<Vec<_>>();

            assert!(!tag_names.iter().any(|tag| tag == "b"));
            assert!(
                tag_names.iter().any(|tag| tag == "p"),
                "highlighting must resume after {closing}"
            );
        }
    }

    #[test]
    fn expands_right_without_exceeding_visible_screen() {
        let result = expanded_frame(
            DraftFrame {
                x: 500.0,
                y: 100.0,
                width: 1200.0,
                height: 800.0,
            },
            DraftFrame {
                x: 0.0,
                y: 0.0,
                width: 1800.0,
                height: 1100.0,
            },
        )
        .unwrap();

        assert_eq!(result.width, 1680.0);
        assert_eq!(result.x, 120.0);
    }

    #[test]
    fn rejects_screens_below_workspace_minimum() {
        let result = expanded_frame(
            DraftFrame {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
            DraftFrame {
                x: 0.0,
                y: 0.0,
                width: 999.0,
                height: 700.0,
            },
        );

        assert_eq!(result, Err("insufficient-space"));
    }

    #[test]
    fn validates_bounded_overlay_state_updates() {
        let state = validate_state(serde_json::json!({
            "status": "editing",
            "pageUrl": "http://127.0.0.1:43123/index.html",
            "selector": "#hero",
            "focusHtml": "<h1>Hello</h1>",
            "originalHtml": "<!doctype html><html><head></head><body><h1>Hello</h1></body></html>",
            "currentHtml": "<!doctype html><html><head></head><body><h1>Welcome</h1></body></html>",
            "dirty": true,
            "undoDepth": 1,
            "redoDepth": 0,
            "error": null
        }))
        .unwrap();

        assert_eq!(state.status, "editing");
        assert!(state.dirty);

        let oversized_history = serde_json::json!({
            "status": "idle",
            "pageUrl": null,
            "selector": null,
            "focusHtml": null,
            "originalHtml": null,
            "currentHtml": null,
            "dirty": false,
            "undoDepth": 51,
            "redoDepth": 0,
            "error": null
        });
        assert!(validate_state(oversized_history).is_err());
    }
}
