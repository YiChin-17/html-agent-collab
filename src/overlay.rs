//! overlay 注入 script 的組裝。
//! `web/overlay.js` 與 `web/overlay.css` 編譯期內嵌，CSS 以 JSON string 形式
//! 注入 JS，避免 runtime 讀檔。

const OVERLAY_JS: &str = include_str!("../web/overlay.js");
const OVERLAY_CSS: &str = include_str!("../web/overlay.css");

/// 產出注入用的完整 overlay script。
pub fn overlay_script() -> String {
    let css_json = serde_json::to_string(OVERLAY_CSS).expect("css is always serializable");
    OVERLAY_JS.replace("__OVERLAY_CSS_JSON__", &css_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_is_replaced_with_css_string() {
        let script = overlay_script();
        assert!(!script.contains("__OVERLAY_CSS_JSON__"));
        assert!(script.contains(".collab-toolbar"));
        // CSS 是合法 JS string literal（開頭是引號）。
        assert!(script.contains("var CSS_TEXT = \""));
    }

    #[test]
    fn duplicate_injection_guard_present() {
        assert!(OVERLAY_JS.contains("__collabOverlayInstalled"));
        // 不得使用 alert()/confirm()（spec「Isolated collaboration overlay」）。
        assert!(!OVERLAY_JS.contains("alert("));
        assert!(!OVERLAY_JS.contains("confirm("));
    }

    #[test]
    fn inactive_state_closes_and_clears_unsent_overlay_ui() {
        for contract in [
            "function setActive(active)",
            "closeEditor();",
            "clearMarks();",
            "hideHighlight();",
            "ui.toolbar.style.display",
            "ui.paintLayer.style.display",
            "state.active",
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing inactive overlay contract: {contract}"
            );
        }
    }

    #[test]
    fn reload_reads_active_attachment_status_before_showing_controls() {
        assert!(OVERLAY_JS.contains("STATUS_ENDPOINT"));
        assert!(OVERLAY_JS.contains("fetch(STATUS_ENDPOINT)"));
        assert!(OVERLAY_JS.contains("payload.collaborationActive === true"));
        assert!(OVERLAY_JS.contains("setActive(false)"));
        assert!(OVERLAY_JS.contains("setActive(active)"));
    }

    #[test]
    fn marker_snapshots_update_lifecycle_state_without_reload() {
        for contract in [
            "function applyMarkerSnapshot(snapshot)",
            "lastMarkerRevision",
            "snapshot.revision < state.lastMarkerRevision",
            "collab-marker-working",
            "collab-marker-failed",
            "failureReason",
            "verification failed",
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing marker sync contract: {contract}"
            );
        }
    }

    #[test]
    fn marker_snapshot_api_is_exposed_only_for_bounded_webview_commands() {
        assert!(OVERLAY_JS.contains("syncFeedbackMarkers: applyMarkerSnapshot"));
        assert!(!OVERLAY_JS.contains("setInterval(reconcile"));
    }

    #[test]
    fn painting_subtools_are_accessible_icon_only_buttons() {
        for contract in [
            r#"id: "select", label: "Select""#,
            r#"id: "freehand", label: "Freehand""#,
            r#"id: "rect", label: "Rectangle""#,
            r#"id: "arrow", label: "Arrow""#,
            r#"id: "label", label: "Text label""#,
            r#"id: "eraser", label: "Eraser""#,
            r#"id: "undo", label: "Undo""#,
            r#"id: "redo", label: "Redo""#,
            r#"id: "clear", label: "Clear all""#,
            r#"document.createElementNS(SVG_NS, "svg")"#,
            r#"button.setAttribute("aria-label", tool.label)"#,
            r#"button.dataset.tooltip = tool.label"#,
            r#"icon.setAttribute("aria-hidden", "true")"#,
            "button.appendChild(icon)",
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing painting icon contract: {contract}"
            );
        }
        assert!(!OVERLAY_JS.contains("toolbarButton(tool,"));
    }

    #[test]
    fn painting_tooltips_and_focus_indicator_are_visible() {
        for contract in [
            ".collab-tool::after",
            "content: attr(data-tooltip)",
            ".collab-tool:hover::after",
            ".collab-tool:focus-visible::after",
            ".collab-toolbar button:focus-visible",
            ".collab-tool.collab-focused::after",
            "outline:",
        ] {
            assert!(
                OVERLAY_CSS.contains(contract),
                "missing tooltip or focus CSS contract: {contract}"
            );
        }
        for contract in [
            r#"button.addEventListener("focus""#,
            r#"button.classList.add("collab-focused")"#,
            r#"button.addEventListener("blur""#,
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing WebKit focus synchronization: {contract}"
            );
        }
    }

    #[test]
    fn primary_collaboration_controls_keep_text_labels() {
        for label in ["Element", "Paint", "Send paint", "Note"] {
            let contract = format!(r#"toolbarButton("{label}""#);
            assert!(
                OVERLAY_JS.contains(&contract),
                "primary control lost text label: {label}"
            );
        }
        assert!(OVERLAY_JS.contains(r#"openEditor("element-comment", el)"#));
        assert!(OVERLAY_JS.contains(r#"feedbackBody("element-comment", text"#));
    }

    #[test]
    fn painting_draft_editing_uses_bounded_snapshots_and_geometry_hits() {
        for contract in [
            "historyUndo: []",
            "historyRedo: []",
            "var HISTORY_LIMIT = 50",
            "function commitMarksMutation(snapshot)",
            "state.historyUndo.length > HISTORY_LIMIT",
            "function undoMarks()",
            "function redoMarks()",
            "function hitMarkIndex(x, y)",
            "for (var i = state.marks.length - 1; i >= 0; i--)",
            "function translateMark(mark, dx, dy)",
            "function removeSelectedMark()",
            "document.activeElement === ui.textarea",
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing editable painting contract: {contract}"
            );
        }
    }

    #[test]
    fn label_and_editor_interactions_are_bounded_and_non_conflicting() {
        for contract in [
            "ui.labelInput = document.createElement(\"input\")",
            "function openLabelInput(x, y)",
            "function commitLabelInput()",
            "function cancelLabelInput()",
            "ui.labelInput.addEventListener(\"keydown\"",
            "ui.labelInput.addEventListener(\"blur\", cancelLabelInput)",
            "ui.editorDragHandle = document.createElement(\"button\")",
            "Move feedback editor",
            "function clampEditorPosition(left, top)",
            "function clampCurrentEditorPosition()",
            "function onEditorPointerDown(event)",
            "ui.editorDragHandle.setPointerCapture(event.pointerId)",
            "ui.editorDragHandle.addEventListener(\"pointerdown\", onEditorPointerDown)",
            "window.addEventListener(\"resize\", clampCurrentEditorPosition)",
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing label or editor interaction contract: {contract}"
            );
        }
        for contract in [
            ".collab-editor-drag-handle",
            "cursor: grab",
            "touch-action: none",
            ".collab-label-input",
        ] {
            assert!(
                OVERLAY_CSS.contains(contract),
                "missing label or editor style contract: {contract}"
            );
        }
    }

    #[test]
    fn toolbar_has_dedicated_accessible_drag_handle() {
        for contract in [
            r#"ui.dragHandle = document.createElement("button")"#,
            r#"ui.dragHandle.className = "collab-drag-handle""#,
            r#"ui.dragHandle.setAttribute("aria-label", "Move collaboration toolbar")"#,
            "ui.toolbar.appendChild(ui.dragHandle)",
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing toolbar drag handle contract: {contract}"
            );
        }
        for contract in [
            ".collab-drag-handle",
            "cursor: grab",
            "touch-action: none",
            "user-select: none",
        ] {
            assert!(
                OVERLAY_CSS.contains(contract),
                "missing toolbar drag handle style: {contract}"
            );
        }
    }

    #[test]
    fn toolbar_drag_uses_primary_pointer_capture_lifecycle() {
        for contract in [
            "function onToolbarPointerDown(event)",
            "!event.isPrimary || event.button !== 0 || state.dragPointerId !== null",
            "ui.dragHandle.setPointerCapture(event.pointerId)",
            "function onToolbarPointerMove(event)",
            "function finishToolbarDrag(event)",
            r#"ui.dragHandle.addEventListener("pointerdown", onToolbarPointerDown)"#,
            r#"ui.dragHandle.addEventListener("pointermove", onToolbarPointerMove)"#,
            r#"ui.dragHandle.addEventListener("pointerup", finishToolbarDrag)"#,
            r#"ui.dragHandle.addEventListener("pointercancel", finishToolbarDrag)"#,
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing toolbar pointer drag contract: {contract}"
            );
        }
        assert!(!OVERLAY_JS.contains(r#"ui.toolbar.addEventListener("pointerdown""#));
    }

    #[test]
    fn toolbar_position_clamps_and_reclamps_with_viewport_changes() {
        for contract in [
            "function clampToolbarPosition(left, top)",
            "Math.max(0, window.innerWidth - rect.width)",
            "Math.max(0, window.innerHeight - rect.height)",
            "Math.min(Math.max(0, left), maxLeft)",
            "Math.min(Math.max(0, top), maxTop)",
            "function clampCurrentToolbarPosition()",
            "if (!ui.toolbar || !ui.toolbar.style.left) return",
            "setToolbarPosition(event.clientX - state.dragOffsetX, event.clientY - state.dragOffsetY)",
            "if (state.active) clampCurrentToolbarPosition()",
            r#"window.addEventListener("resize", clampCurrentToolbarPosition)"#,
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing toolbar viewport clamp contract: {contract}"
            );
        }
    }

    #[test]
    fn preview_draft_manager_owns_one_bounded_complete_document() {
        for contract in [
            "var PREVIEW_DRAFT_HISTORY_LIMIT = 50",
            "var previewDraftManager =",
            "pageUrl: null",
            "focusHtml: null",
            "originalHtml: null",
            "currentHtml: null",
            "selector: null",
            "undoHistory: []",
            "redoHistory: []",
            "history.length > PREVIEW_DRAFT_HISTORY_LIMIT",
            "previewDraftManager.redoHistory = []",
            "loadPreviewDraftSource",
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing bounded Preview Draft manager contract: {contract}"
            );
        }
    }

    #[test]
    fn preview_draft_validates_and_renders_one_complete_document() {
        for contract in [
            "function parsePreviewDraftDocument(html)",
            "new TextEncoder().encode(html).length",
            "262144",
            "new DOMParser()",
            "\"text/html\"",
            "\"invalid-document\"",
            "\"document-too-large\"",
            "document.head",
            "document.body",
            "previewDraftManager.currentHtml = html",
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing complete-document replacement contract: {contract}"
            );
        }
        assert!(!OVERLAY_JS.contains("createContextualFragment"));
        assert!(!OVERLAY_JS.contains("previewDraftManager.target.replaceWith"));
    }

    #[test]
    fn preview_draft_api_exposes_json_state_and_inline_errors() {
        for contract in [
            "function loadPreviewDraftSource(input)",
            "function openPreviewDraftFor(element)",
            "function applyPreviewDraft(input)",
            "function previewDraftState()",
            "function undoPreviewDraft()",
            "function redoPreviewDraft()",
            "function resetPreviewDraft()",
            "function submitPreviewDraft()",
            "status:",
            "pageUrl:",
            "focusHtml:",
            "originalHtml:",
            "currentHtml:",
            "dirty:",
            "undoDepth:",
            "redoDepth:",
            "error:",
            "loadPreviewDraftSource: loadPreviewDraftSource",
            "openPreviewDraftFor: openPreviewDraftFor",
            "applyPreviewDraft: applyPreviewDraft",
            "previewDraftState: previewDraftState",
            "undoPreviewDraft: undoPreviewDraft",
            "redoPreviewDraft: redoPreviewDraft",
            "resetPreviewDraft: resetPreviewDraft",
            "submitPreviewDraft: submitPreviewDraft",
            "showPreviewDraftError",
            "DRAFT_STATE_ENDPOINT",
            "publishPreviewDraftState",
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing structured Preview Draft API contract: {contract}"
            );
        }
        assert!(!OVERLAY_JS.contains("alert("));
        assert!(!OVERLAY_JS.contains("confirm("));
    }

    #[test]
    fn preview_draft_target_mode_is_isolated_from_existing_feedback_modes() {
        for contract in [
            "null | \"comment\" | \"paint\" | \"draft\"",
            "state.mode === \"draft\"",
            "state.mode !== \"comment\" && state.mode !== \"draft\"",
            "openPreviewDraftFor(el)",
            "collab-draft-highlight",
        ] {
            assert!(
                OVERLAY_JS.contains(contract),
                "missing isolated Draft target mode contract: {contract}"
            );
        }
        let contract = ".collab-highlight.collab-draft-highlight";
        assert!(
            OVERLAY_CSS.contains(contract),
            "missing isolated Draft marker style: {contract}"
        );
        assert!(!OVERLAY_JS.contains("collab-draft-conflict"));
        assert!(!OVERLAY_JS.contains("draftConflictApply"));
        assert!(!OVERLAY_CSS.contains(".collab-draft-conflict"));
        assert!(!OVERLAY_JS.contains("collab-preview-draft-editor"));
        assert!(!OVERLAY_JS.contains("WKWebView"));
    }
}
