//! Preview Draft native AppKit workspace contract.

use collab::draft_panel::{
    DRAFT_BUTTON_HEIGHT, DRAFT_CONTROL_SPACING, DRAFT_HORIZONTAL_INSET, DraftFrame,
    TOOLBAR_STRIP_HEIGHT, VALIDATION_STRIP_HEIGHT, draft_panel_layout, initial_split_preview_width,
    split_position_bounds, unique_source_range,
};

fn draft_panel_source() -> String {
    std::fs::read_to_string("src/draft_panel.rs")
        .expect("Preview Draft must provide src/draft_panel.rs")
}

#[test]
fn draft_panel_owns_one_native_split_workspace() {
    let source = draft_panel_source();

    for contract in [
        "struct DraftPanelController",
        "NSSplitView",
        "NSScrollView",
        "NSTextView",
        "NSButton",
        "saved_window_frame",
        "command_sender",
        "original_content_view",
        "original_webview_superview",
    ] {
        assert!(
            source.contains(contract),
            "missing native ownership contract: {contract}"
        );
    }
    assert!(!source.contains("WKWebView::new"));
}

#[test]
fn draft_panel_frame_plan_respects_preferred_and_minimum_widths() {
    let source = draft_panel_source();

    for contract in [
        "DRAFT_PREFERRED_WIDTH: f64 = 480.0",
        "DRAFT_MINIMUM_WIDTH: f64 = 360.0",
        "PREVIEW_MINIMUM_WIDTH: f64 = 640.0",
        "MINIMUM_WORKSPACE_WIDTH: f64 = 1000.0",
        "visible_frame",
        "insufficient-space",
        "expanded_frame",
    ] {
        assert!(
            source.contains(contract),
            "missing frame planning contract: {contract}"
        );
    }
}

#[test]
fn draft_controls_use_compact_conditional_layout_metrics() {
    assert_eq!(DRAFT_BUTTON_HEIGHT, 22.0);
    assert_eq!(DRAFT_CONTROL_SPACING, 6.0);
    assert_eq!(DRAFT_HORIZONTAL_INSET, 8.0);
    assert_eq!(TOOLBAR_STRIP_HEIGHT, 34.0);
    assert_eq!(VALIDATION_STRIP_HEIGHT, 24.0);

    let bounds = DraftFrame {
        x: 0.0,
        y: 0.0,
        width: 360.0,
        height: 500.0,
    };
    let hidden = draft_panel_layout(bounds, false);
    let visible = draft_panel_layout(bounds, true);

    assert_eq!(hidden.toolbar.height, 22.0);
    assert_eq!(hidden.validation.height, 0.0);
    assert_eq!(hidden.editor.height, 466.0);
    assert_eq!(visible.toolbar.height, 22.0);
    assert_eq!(visible.validation.height, 24.0);
    assert_eq!(visible.editor.height, 442.0);
}

#[test]
fn draft_controls_render_above_the_editor_scroll_view() {
    let source = draft_panel_source();
    let panel_install = source
        .find("panel.addSubview")
        .map(|start| &source[start..])
        .expect("Draft panel must install native subviews");
    let scroll_view = panel_install
        .find("panel.addSubview(&scroll_view)")
        .expect("Draft panel must install the editor scroll view");
    let toolbar = panel_install
        .find("panel.addSubview(&toolbar)")
        .expect("Draft panel must install the toolbar");
    let validation = panel_install
        .find("panel.addSubview(&validation_message)")
        .expect("Draft panel must install the validation message");

    assert!(
        scroll_view < toolbar && scroll_view < validation,
        "Draft controls must be above the editor in the native subview z-order"
    );
}

#[test]
fn draft_split_starts_at_sixty_forty_and_clamps_to_minimums() {
    assert_eq!(initial_split_preview_width(1600.0), 960.0);
    assert_eq!(initial_split_preview_width(1000.0), 640.0);
    assert_eq!(split_position_bounds(1600.0), (640.0, 1240.0));
    assert_eq!(split_position_bounds(1000.0), (640.0, 640.0));
}

#[test]
fn draft_workspace_retains_a_bounded_split_delegate() {
    let source = draft_panel_source();

    for contract in [
        "NSSplitViewDelegate",
        "splitView_constrainSplitPosition_ofSubviewAt",
        "split_position_bounds",
        "setDelegate",
        "_split_delegate",
        "dividerThickness",
    ] {
        assert!(
            source.contains(contract),
            "missing bounded split delegate contract: {contract}"
        );
    }
    assert!(
        !source.contains("setAutosaveName"),
        "Draft split position must reset instead of being persisted"
    );
}

#[test]
fn draft_panel_rolls_back_partial_install_and_restores_exact_frame() {
    let source = draft_panel_source();

    for contract in [
        "fn rollback_install",
        "restore_original_content_hierarchy",
        "setFrame_display",
        "saved_window_frame",
        "draft_toggle",
        "setState",
        "non_blocking_error",
    ] {
        assert!(
            source.contains(contract),
            "missing rollback or restoration contract: {contract}"
        );
    }
}

#[test]
fn draft_toolbar_is_accessible_plain_text_ui() {
    let source = draft_panel_source();

    for label in ["Undo", "Redo", "Reset", "Apply to source"] {
        assert!(
            source.contains(label),
            "missing Draft toolbar control: {label}"
        );
    }
    for contract in [
        "setAccessibilityLabel",
        "Preview Draft validation",
        "validation_message",
        "show_validation_message",
        "draft_toggle.setState(NSControlStateValueOn)",
        "self.input_pending.load(Ordering::SeqCst)",
        "self.input_generation.fetch_add(1, Ordering::SeqCst)",
        "setRichText(false)",
        "setImportsGraphics(false)",
        "sizeToFit()",
        "DEBOUNCE_MILLISECONDS: u64 = 250",
    ] {
        assert!(
            source.contains(contract),
            "missing plain-text or accessibility contract: {contract}"
        );
    }
    assert!(!source.contains("syntax_highlight"));
    assert!(!source.contains("NSAttributedString"));
}

#[test]
fn html_syntax_uses_monospaced_font_and_temporary_layout_attributes() {
    let source = draft_panel_source();

    for contract in [
        "NSFont::monospacedSystemFontOfSize_weight",
        "layoutManager()",
        "removeTemporaryAttribute_forCharacterRange",
        "addTemporaryAttribute_value_forCharacterRange",
        "NSForegroundColorAttributeName",
        "html_syntax_spans",
    ] {
        assert!(
            source.contains(contract),
            "missing native HTML syntax presentation contract: {contract}"
        );
    }
}

#[test]
fn draft_panel_loads_complete_source_and_focuses_the_unique_outer_html() {
    let source = draft_panel_source();

    for contract in [
        "loadPreviewDraftSource",
        "page_url",
        "source_html",
        "fn unique_source_range",
        "setSelectedRange",
        "scrollRangeToVisible",
        "source-location-unavailable",
    ] {
        assert!(
            source.contains(contract),
            "missing complete-source focus contract: {contract}"
        );
    }
}

#[test]
fn unique_source_range_uses_text_view_utf16_offsets() {
    let source =
        "<!doctype html><html><head></head><body>界<h1 class=\"title\">Hello</h1></body></html>";
    let target = "<h1 class=\"title\">Hello</h1>";

    let range = unique_source_range(source, target).expect("target should be unique");
    let byte_start = source.find(target).unwrap();

    assert_eq!(range.location, source[..byte_start].encode_utf16().count());
    assert_eq!(range.length, target.encode_utf16().count());
}

#[test]
fn unique_source_range_rejects_missing_or_duplicate_outer_html() {
    let target = "<h1>Hello</h1>";

    assert_eq!(
        unique_source_range("<html><body></body></html>", target),
        Err("source-location-unavailable")
    );
    assert_eq!(
        unique_source_range(
            "<html><body><h1>Hello</h1><h1>Hello</h1></body></html>",
            target
        ),
        Err("source-location-unavailable")
    );
}

#[test]
fn dashboard_places_draft_with_existing_session_controls() {
    let source = std::fs::read_to_string("src/dashboard.rs").unwrap();
    for label in [
        "\"Pause\"",
        "\"Stop collaboration\"",
        "\"Close preview\"",
        "\"Draft\"",
    ] {
        assert!(source.contains(label), "missing session control: {label}");
    }
    let row = source
        .find("let views: [&NSView; 11]")
        .expect("native control row views");
    let row = &source[row..];
    let end = row
        .find("controller.setView(&control_row)")
        .expect("native control row installation");
    let row = &row[..end];
    for control in ["&draft", "&pause", "&stop", "&close", "control_row"] {
        assert!(
            row.contains(control),
            "Draft, Pause, Stop, and Close must share one control row: {control}"
        );
    }
    for draft_action in ["Undo", "Redo", "Reset", "Apply to source"] {
        assert!(
            !row.contains(draft_action),
            "Draft action must not be installed in the Preview titlebar: {draft_action}"
        );
    }
}
