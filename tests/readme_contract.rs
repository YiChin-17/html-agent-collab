//! Task 7.1：README 必須反映已交付的 single-entry workflow 與 CLI help。

use std::process::Command;

fn readme() -> String {
    std::fs::read_to_string("README.md").expect("README should exist")
}

fn readme_zh_tw() -> String {
    std::fs::read_to_string("README.zh-TW.md").expect("Traditional Chinese README should exist")
}

fn collaboration_guide() -> String {
    std::fs::read_to_string("docs/COLLABORATION.md").expect("Collaboration guide should exist")
}

fn second_level_headings(readme: &str) -> Vec<&str> {
    readme
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .collect()
}

#[test]
fn readmes_share_the_structured_information_architecture() {
    let english = readme();
    let traditional_chinese = readme_zh_tw();

    assert_eq!(
        second_level_headings(&english),
        [
            "Quick start",
            "CLI reference",
            "Preview interface",
            "Architecture",
            "Prerequisites",
            "Security",
            "Verification",
            "Documentation",
            "License",
        ]
    );
    assert_eq!(
        second_level_headings(&traditional_chinese),
        [
            "快速開始",
            "CLI reference",
            "Preview 介面",
            "架構圖",
            "先決條件",
            "安全性",
            "驗證",
            "文件索引",
            "授權",
        ]
    );

    for readme in [&english, &traditional_chinese] {
        for contract in ["```mermaid", "flowchart", "| Skill |", "| Command |"] {
            assert!(
                readme.contains(contract),
                "README missing structured content: {contract}"
            );
        }
    }
    assert!(english.contains("| Action |"));
    assert!(traditional_chinese.contains("| 動作 |"));
    assert!(english.contains("| Tool |"));
    assert!(traditional_chinese.contains("| 工具 |"));
}

#[test]
fn collaboration_guide_documents_foreground_background_selection_rules() {
    let guide = collaboration_guide();
    for contract in [
        "Explicit `foreground` or `background` mode takes precedence.",
        "inspect or manually operate",
        "collaborate, process feedback, verify, capture screenshots, or wait",
        "asks which mode",
    ] {
        assert!(
            guide.contains(contract),
            "Collaboration guide mode rule missing: {contract}"
        );
    }

    let english = readme();
    let traditional_chinese = readme_zh_tw();
    for readme in [&english, &traditional_chinese] {
        assert!(
            readme.contains("foreground") && readme.contains("background"),
            "README must mention both start modes"
        );
        assert!(
            readme.contains("docs/COLLABORATION.md"),
            "README must link to collaboration guide"
        );
    }
}

#[test]
fn readme_documents_the_four_user_facing_skills() {
    let readme = readme();

    for contract in [
        "$preview-collaboration-start path/to/page.html",
        "$preview-collaboration-connect <preview-id>",
        "$preview-collaboration-stop",
        "$preview-collaboration-close",
        "Claude Code",
        "Codex",
        "continuously",
        "feedback",
    ] {
        assert!(
            readme.contains(contract),
            "missing README skill contract: {contract}"
        );
    }
}

#[test]
fn readme_distinguishes_pause_resume_stop_and_close() {
    let readme = readme();
    for contract in [
        "`collab pause --project <project> [--attachment <id>]`",
        "`collab resume --project <project> [--attachment <id>]`",
        "Stop collaboration",
        "Close preview",
    ] {
        assert!(
            readme.contains(contract),
            "missing lifecycle documentation in README: {contract}"
        );
    }

    let guide = collaboration_guide();
    for contract in [
        "Pause collaboration after current feedback",
        "same attachment",
        "pause-requested",
    ] {
        assert!(
            guide.contains(contract),
            "missing lifecycle documentation in guide: {contract}"
        );
    }
}

#[test]
fn readmes_document_the_native_dashboard_and_marker_lifecycle() {
    let readmes = [readme(), readme_zh_tw()];

    for readme in &readmes {
        for contract in [
            "collaboration dashboard",
            "Pause",
            "Resume",
            "Stop collaboration",
            "Close preview",
            "WKWebView",
        ] {
            assert!(
                readme.contains(contract),
                "missing dashboard documentation in README: {contract}"
            );
        }
    }

    let guide = collaboration_guide();
    for contract in [
        "No agent connected",
        "working marker",
        "resolved feedback",
        "failed marker",
        "failure reason",
    ] {
        assert!(
            guide.contains(contract),
            "missing marker lifecycle in guide: {contract}"
        );
    }
}

#[test]
fn preview_draft_docs_describe_complete_source_and_toolbar_isolation() {
    for readme in [readme(), readme_zh_tw()] {
        assert!(readme.contains("complete HTML source"));
        assert!(readme.contains("Undo, Redo, Reset, and Apply to source"));
        assert!(readme.contains("right Draft pane"));
    }
    let guide = collaboration_guide();
    for contract in [
        "complete HTML documents",
        "focus hint",
        "current source differs from `beforeHtml`",
        "right Draft pane",
    ] {
        assert!(
            guide.contains(contract),
            "missing complete-document Preview Draft contract: {contract}"
        );
    }
}

#[test]
fn readmes_document_preview_draft_scope_and_handoff() {
    for readme in [readme(), readme_zh_tw()] {
        for contract in [
            "Preview Draft",
            "plain-text",
            "Apply to source",
            "reload",
            "rendered HTML",
            "syntax highlighting",
            "dynamic",
            "single WKWebView",
            "monospaced",
            "640",
            "360",
        ] {
            assert!(
                readme.contains(contract),
                "README missing Preview Draft contract: {contract}"
            );
        }
        assert!(
            !readme.contains("no syntax highlighting")
                && !readme.contains("沒有 syntax highlighting"),
            "README must not describe the delivered syntax highlighting as unavailable"
        );
    }

    let guide = collaboration_guide();
    for contract in [
        "Preview Draft",
        "left",
        "right",
        "HTML-only",
        "does not write project files",
        "pending",
        "framework",
    ] {
        assert!(
            guide.contains(contract),
            "collaboration guide missing Preview Draft contract: {contract}"
        );
    }
}

#[test]
fn readme_documents_preview_id_connect_handoff_and_fixed_terms() {
    let readme = readme();

    for contract in [
        "Connect agent",
        "Resume",
        "Stop collaboration",
        "Close preview",
        "$preview-collaboration-connect <preview-id>",
    ] {
        assert!(
            readme.contains(contract),
            "missing connect documentation in README: {contract}"
        );
    }

    let guide = collaboration_guide();
    for contract in [
        "same project workspace",
        "non-secret selector",
        "new attachment",
    ] {
        assert!(
            guide.contains(contract),
            "missing connect documentation in guide: {contract}"
        );
    }
    for prohibited in ["Preview ID credential", "host bridge"] {
        assert!(
            !readme.contains(prohibited),
            "README mischaracterizes pairing as {prohibited}"
        );
    }
}

#[test]
fn readme_and_guide_explain_entry_resolution_reuse_and_inactive_preview() {
    let readme = readme();
    for contract in ["single HTML", "WKWebView"] {
        assert!(
            readme.contains(contract),
            "missing README session contract: {contract}"
        );
    }

    let guide = collaboration_guide();
    for contract in [
        "canonical",
        "reused",
        "session ID",
        "port",
        "PID",
        "toolbar",
        "hidden",
    ] {
        assert!(
            guide.contains(contract),
            "missing session contract in guide: {contract}"
        );
    }
}

#[test]
fn readme_low_level_cli_matches_actual_help() {
    let readme = readme();
    let output = Command::new(env!("CARGO_BIN_EXE_collab"))
        .arg("--help")
        .output()
        .expect("collab help should run");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();

    for command in [
        "open",
        "attach",
        "status",
        "detach",
        "pause",
        "resume",
        "close",
        "screenshot",
        "eval",
        "wait",
        "feedback",
    ] {
        assert!(
            help.lines()
                .any(|line| line.trim_start().starts_with(&format!("{command} "))),
            "actual help is missing {command}"
        );
        assert!(
            readme.contains(&format!("`collab {command}")),
            "README CLI reference is missing {command}"
        );
    }
    assert!(
        !help
            .lines()
            .any(|line| line.trim_start().starts_with("stop "))
    );
}

#[test]
fn readmes_document_only_the_current_0_1_0_cli() {
    let readmes = [readme(), readme_zh_tw()];

    for readme in &readmes {
        assert!(readme.contains("`collab detach"));
        assert!(readme.contains("`collab close"));
        assert!(!readme.contains("`collab stop`"));
        assert!(!readme.contains("## CLI migration"));
    }
}

#[test]
fn readme_documents_public_build_and_project_policies() {
    let readmes = [readme(), readme_zh_tw()];

    for readme in &readmes {
        for contract in [
            "macOS 15",
            "1.97.0",
            "Xcode Command Line Tools",
            "cargo install --path . --locked",
            "source-only",
            "CONTRIBUTING.md",
            "CODE_OF_CONDUCT.md",
            "CHANGELOG.md",
            "docs/RELEASING.md",
            "SECURITY.md",
            "LICENSE-MIT",
            "LICENSE-APACHE",
        ] {
            assert!(
                readme.contains(contract),
                "missing public README contract: {contract}"
            );
        }
    }
}

#[test]
fn readmes_are_bilingual_mutually_linked_and_operationally_equivalent() {
    let english = readme();
    let traditional_chinese = readme_zh_tw();

    assert!(
        english
            .lines()
            .take(5)
            .any(|line| line.contains("[繁體中文](README.zh-TW.md)"))
    );
    assert!(
        traditional_chinese
            .lines()
            .take(5)
            .any(|line| line.contains("[English](README.md)"))
    );
    assert!(english.contains("## Prerequisites"));
    assert!(traditional_chinese.contains("## 先決條件"));

    for contract in [
        "cargo build --release",
        "cargo fmt -- --check",
        "cargo test",
        "cargo clippy --all-targets --all-features -- -D warnings",
        "cargo package",
        "scripts/session-ux-acceptance.sh",
        "$preview-collaboration-start path/to/page.html",
        "$preview-collaboration-stop",
        "$preview-collaboration-close",
        "GitHub private vulnerability reporting",
        "MIT OR Apache-2.0",
    ] {
        assert!(
            english.contains(contract),
            "English README missing parity contract: {contract}"
        );
        assert!(
            traditional_chinese.contains(contract),
            "Traditional Chinese README missing parity contract: {contract}"
        );
    }
}
