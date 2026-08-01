//! Open-source distribution contracts.

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("cannot read {path}: {error}"))
}

#[test]
fn repository_declares_the_dual_license_consistently() {
    let mit = read("LICENSE-MIT");
    let apache = read("LICENSE-APACHE");
    let notice = read("NOTICE");
    let manifest = read("Cargo.toml");

    assert!(mit.contains("MIT License"));
    assert!(mit.contains("Copyright (c) 2026 YiChin-17"));
    assert!(apache.contains("Apache License"));
    assert!(apache.contains("Version 2.0, January 2004"));
    assert!(notice.contains("Copyright 2026 YiChin-17"));
    assert!(notice.contains("MIT OR Apache-2.0"));
    assert!(manifest.contains("license = \"MIT OR Apache-2.0\""));
}

#[test]
fn repository_excludes_collaboration_runtime_artifacts() {
    let gitignore = read(".gitignore");

    assert!(
        gitignore.lines().any(|line| line == ".collab/"),
        "repository root must ignore every .collab runtime artifact"
    );
}

#[test]
fn first_source_release_has_a_versioned_changelog_entry() {
    let changelog = read("CHANGELOG.md");
    let unreleased = changelog
        .find("## [Unreleased]")
        .expect("changelog must retain an Unreleased section");
    let after_unreleased = &changelog[unreleased + "## [Unreleased]".len()..];
    let next_release = after_unreleased
        .find("\n## [")
        .expect("changelog must contain a versioned release after Unreleased");
    let first_release = changelog
        .find("## [0.1.0] - 2026-07-20")
        .expect("changelog must contain the dated 0.1.0 release");

    assert!(unreleased < first_release);
    assert!(after_unreleased[..next_release].trim().is_empty());

    let release_notes = &changelog[first_release..];
    for entry in [
        "MIT OR Apache-2.0 dual licensing and open-source contribution policies.",
        "Source-only GitHub release automation and repository quality gates.",
        "Public documentation now reflects the Rust, Tauri 2, and WKWebView implementation.",
    ] {
        assert!(
            release_notes.contains(entry),
            "0.1.0 release notes missing: {entry}"
        );
    }
}

#[test]
fn dual_license_verification_uses_authoritative_repository_evidence() {
    let releasing = read("docs/RELEASING.md");

    for contract in [
        "license = \"MIT OR Apache-2.0\"",
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "NOTICE",
        "licenseInfo",
        "單一 license",
    ] {
        assert!(
            releasing.contains(contract),
            "release checklist missing license evidence: {contract}"
        );
    }
    assert!(releasing.contains("不得用來判定雙授權是否成立"));
}

#[test]
fn cargo_distribution_is_source_only_and_reproducible() {
    let manifest = read("Cargo.toml");
    let toolchain = read("rust-toolchain.toml");
    let release = read(".github/workflows/release.yml");

    for contract in [
        "publish = false",
        "repository = \"https://github.com/YiChin-17/html-agent-collab\"",
        "readme = \"README.md\"",
        "rust-version = \"1.97\"",
        "\".agents/**\"",
        "\".claude/**\"",
        "\".github/**\"",
        "\"openspec/**\"",
    ] {
        assert!(
            manifest.contains(contract),
            "missing Cargo contract: {contract}"
        );
    }

    assert!(toolchain.contains("channel = \"1.97.0\""));
    assert!(toolchain.contains("components = [\"clippy\", \"rustfmt\"]"));
    assert!(release.contains("tags:"));
    assert!(release.contains("- \"v*\""));
    assert!(release.contains("gh release create"));
    assert!(release.contains("--generate-notes"));
    assert!(!release.contains("cargo publish"));
    assert!(!release.contains("tauri build"));
}

#[test]
fn public_contributor_documents_exist_and_are_linked() {
    let readme = read("README.md");
    let contributing = read("CONTRIBUTING.md");
    let conduct = read("CODE_OF_CONDUCT.md");
    let changelog = read("CHANGELOG.md");
    let releasing = read("docs/RELEASING.md");

    for path in [
        "CONTRIBUTING.md",
        "CODE_OF_CONDUCT.md",
        "CHANGELOG.md",
        "docs/RELEASING.md",
        "SECURITY.md",
    ] {
        assert!(readme.contains(path), "README must link {path}");
    }

    for contract in ["cargo fmt -- --check", "cargo test", "cargo clippy"] {
        assert!(
            contributing.contains(contract),
            "missing contribution command: {contract}"
        );
    }
    assert!(contributing.contains("Conventional Commits"));
    assert!(conduct.contains("Contributor Covenant Code of Conduct"));
    assert!(conduct.contains("version 2.1"));
    assert!(changelog.contains("Keep a Changelog"));
    assert!(changelog.contains("## [Unreleased]"));
    assert!(releasing.contains("source-only"));
    assert!(releasing.contains("v<major>.<minor>.<patch>"));
    assert!(releasing.contains("cargo package"));
}

#[test]
fn contribution_templates_collect_reproduction_and_verification_evidence() {
    let bug = read(".github/ISSUE_TEMPLATE/bug_report.yml");
    let feature = read(".github/ISSUE_TEMPLATE/feature_request.yml");
    let pull_request = read(".github/pull_request_template.md");

    for contract in [
        "macOS version",
        "Rust version",
        "Reproduction steps",
        "Expected behavior",
        "Actual behavior",
    ] {
        assert!(bug.contains(contract), "bug form missing: {contract}");
    }
    for contract in ["Problem", "Proposed change", "Alternatives", "Scope"] {
        assert!(
            feature.contains(contract),
            "feature form missing: {contract}"
        );
    }
    for contract in ["Summary", "Verification", "cargo test", "Security", "Scope"] {
        assert!(
            pull_request.contains(contract),
            "PR template missing: {contract}"
        );
    }
}

#[test]
fn security_policy_uses_private_reporting_without_personal_email() {
    let security = read("SECURITY.md");

    for contract in [
        "## 支援版本",
        "## 通報安全漏洞",
        "Repository 公開後",
        "GitHub private vulnerability reporting",
        "不要建立公開 issue",
        "一般 bug",
    ] {
        assert!(
            security.contains(contract),
            "security policy missing: {contract}"
        );
    }
    assert!(
        !security.contains('@'),
        "security policy must not publish an email address"
    );
}

#[test]
fn security_policy_documents_private_runtime_artifact_boundary() {
    let security = read("SECURITY.md");

    for contract in [
        "## Runtime artifacts",
        ".collab/feedback",
        ".collab/screenshots",
        "real directory",
        "0700",
        "0600",
        "O_NOFOLLOW",
        "same-directory atomic rename",
        "same-user process",
        "不是 sandbox",
    ] {
        assert!(
            security.contains(contract),
            "security policy missing runtime artifact boundary: {contract}"
        );
    }
}

#[test]
fn dependency_and_secret_maintenance_is_automated() {
    let deny = read("deny.toml");
    let dependabot = read(".github/dependabot.yml");
    let ci = read(".github/workflows/ci.yml");

    for contract in [
        "[advisories]",
        "RUSTSEC-2025-0075",
        "RUSTSEC-2025-0100",
        "[licenses]",
        "confidence-threshold = 0.8",
        "unknown-registry = \"deny\"",
        "unknown-git = \"deny\"",
        "MIT",
        "Apache-2.0",
        "MPL-2.0",
    ] {
        assert!(deny.contains(contract), "deny policy missing: {contract}");
    }
    assert_eq!(dependabot.matches("package-ecosystem:").count(), 2);
    assert!(dependabot.contains("package-ecosystem: \"cargo\""));
    assert!(dependabot.contains("package-ecosystem: \"github-actions\""));
    assert_eq!(dependabot.matches("interval: \"weekly\"").count(), 2);
    assert!(ci.contains("cargo-deny-action"));
    assert!(ci.contains("gitleaks-action"));
    assert!(ci.contains("fetch-depth: 0"));
}

#[test]
fn ci_enforces_mac_quality_with_immutable_actions() {
    let ci = read(".github/workflows/ci.yml");

    for contract in [
        "  test:",
        "  clippy:",
        "  package:",
        "run: cargo test",
        "run: cargo clippy --all-targets --all-features -- -D warnings",
        "run: cargo package",
        "rustup show active-toolchain",
    ] {
        assert!(ci.contains(contract), "CI contract missing: {contract}");
    }
    assert_eq!(ci.matches("runs-on: macos-15").count(), 3);

    for line in ci
        .lines()
        .filter(|line| line.trim_start().starts_with("uses:"))
    {
        let reference = line.split('@').nth(1).expect("action must have a ref");
        let sha = reference.split_whitespace().next().unwrap();
        assert_eq!(sha.len(), 40, "action must use a 40-character SHA: {line}");
        assert!(
            sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "action ref is not hex: {line}"
        );
        assert!(
            line.contains(" # v"),
            "action pin must record its version: {line}"
        );
    }
}

#[test]
fn pending_public_files_use_the_noreply_identity() {
    let output = std::process::Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()
        .expect("git ls-files should run");
    assert!(output.status.success());
    let prior_email = format!("{}@{}", "jeckssion", "gmail.com");
    let noreply = "YiChin-17@users.noreply.github.com";

    for raw_path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(raw_path).expect("repository paths must be UTF-8");
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        assert!(
            !content.contains(&prior_email),
            "prior email remains in {path}"
        );
        if path.ends_with(".openspec.yaml") {
            assert!(
                content.contains(noreply),
                "Spectra metadata lacks noreply identity: {path}"
            );
        }
    }
}
