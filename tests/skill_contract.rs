//! Task 4.1：Claude Code/Codex 的 start、stop、close skill contract。

const SKILLS: [&str; 4] = [
    "preview-collaboration-start",
    "preview-collaboration-connect",
    "preview-collaboration-stop",
    "preview-collaboration-close",
];

fn path(agent_root: &str, skill: &str) -> String {
    format!("{agent_root}/skills/{skill}/SKILL.md")
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("cannot read {path}: {error}"))
}

fn normalized(path: &str) -> String {
    read(path).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn skill_directories(agent_root: &str) -> Vec<String> {
    let mut directories = std::fs::read_dir(format!("{agent_root}/skills"))
        .unwrap_or_else(|error| panic!("cannot list {agent_root}/skills: {error}"))
        .filter_map(|entry| {
            let entry = entry.unwrap_or_else(|error| panic!("cannot read skill entry: {error}"));
            let name = entry.file_name().to_string_lossy().into_owned();
            // Shipped skills live in plugin/skills/ and are surfaced here as symlinks, so the
            // type check has to follow them: read_dir reports the link itself, not its target.
            std::fs::metadata(entry.path())
                .unwrap_or_else(|error| panic!("cannot inspect skill entry: {error}"))
                .is_dir()
                .then_some(name)
                .filter(|name| name.starts_with("preview-collaboration-"))
        })
        .collect::<Vec<_>>();
    directories.sort();
    directories
}

#[test]
fn public_agent_configuration_is_limited_to_shipped_workflows() {
    let mut expected = SKILLS.map(str::to_owned).to_vec();
    expected.sort();

    for agent_root in [".agents", ".claude"] {
        assert_eq!(skill_directories(agent_root), expected, "{agent_root}");
    }

    let gitignore = read(".gitignore");
    assert!(
        gitignore
            .lines()
            .any(|line| line == ".claude/settings.json")
    );
    assert!(!gitignore.lines().any(|line| line == ".agents/"));
    assert!(!gitignore.lines().any(|line| line == ".claude/"));
}

#[test]
fn claude_and_codex_skills_are_byte_identical() {
    for skill in SKILLS {
        assert_eq!(
            read(&path(".agents", skill)),
            read(&path(".claude", skill)),
            "{skill} differs between Codex and Claude Code"
        );
    }
}

#[test]
fn legacy_combined_skill_is_removed() {
    for agent_root in [".agents", ".claude"] {
        assert!(
            !std::path::Path::new(&path(agent_root, "preview-collaboration")).exists(),
            "legacy combined skill still exists for {agent_root}"
        );
    }
}

#[test]
fn start_opens_one_entry_attaches_and_processes_feedback_continuously() {
    let skill = normalized(&path(".agents", "preview-collaboration-start"));

    for contract in [
        "collab open <entry> --background",
        "collab attach",
        "attachmentId",
        "collab wait",
        "acknowledged",
        "collab feedback show",
        "working",
        "Modify",
        "collab eval",
        "collab screenshot",
        "resolved",
        "failed",
        "Wait again",
        "Two sequential feedback items",
    ] {
        assert!(
            skill.contains(contract),
            "missing start contract: {contract}"
        );
    }
}

#[test]
fn connect_accepts_one_preview_id_and_never_opens_a_preview() {
    let skill = normalized(&path(".agents", "preview-collaboration-connect"));

    for contract in [
        "exactly one Preview ID",
        "usage error",
        "before executing any CLI command",
        "current project workspace",
        "collab attach --project <current-workspace> --session <preview-id> --agent <claude-code|codex>",
        "preview-not-running",
        "preview-session-mismatch",
        "attachment-capacity",
        "transport failure",
        "does not enter the feedback wait loop",
    ] {
        assert!(
            skill.contains(contract),
            "missing connect contract: {contract}"
        );
    }
    assert!(!skill.contains("collab open"));
}

#[test]
fn connect_retains_new_identity_through_the_terminal_gated_loop() {
    let skill = normalized(&path(".agents", "preview-collaboration-connect"));

    for contract in [
        "previewSessionId",
        "attachmentId",
        "collab wait",
        "acknowledged",
        "collab feedback show",
        "working",
        "Modify",
        "collab eval",
        "collab screenshot",
        "resolved",
        "failed",
        "Persisted terminal state is the completion boundary",
        "Ambiguous terminal response",
        "collab pause",
        "collab resume",
        "collaboration.stop",
    ] {
        assert!(
            skill.contains(contract),
            "connect is missing feedback workflow contract: {contract}"
        );
    }
}

#[test]
fn start_routes_foreground_and_background_modes_consistently() {
    let skill = normalized(&path(".agents", "preview-collaboration-start"));

    for contract in [
        "optional `foreground` or `background` mode",
        "Explicit mode takes precedence over intent inference",
        "## Foreground manual mode",
        "collab open <entry>",
        "Do not run `collab wait` in foreground mode",
        "## Background collaboration mode",
        "collab open <entry> --background",
        "continuous feedback wait loop",
        "inspect or manually operate",
        "collaboration, process feedback, verify, capture screenshots, or wait for feedback",
        "ask the user to choose `foreground` or `background` before starting a preview",
    ] {
        assert!(
            skill.contains(contract),
            "missing start mode contract: {contract}"
        );
    }
}

#[test]
fn start_and_connect_use_fixed_preview_lifecycle_terms() {
    let start = normalized(&path(".agents", "preview-collaboration-start"));
    let connect = normalized(&path(".agents", "preview-collaboration-connect"));

    assert!(start.contains("Open preview"));
    for contract in [
        "Connect agent",
        "Resume",
        "Stop collaboration",
        "Close preview",
        "same project workspace",
    ] {
        assert!(
            connect.contains(contract),
            "connect skill is missing fixed term: {contract}"
        );
    }
}

#[test]
fn start_and_connect_apply_preview_drafts_without_guessing_source_targets() {
    for skill in [
        "preview-collaboration-start",
        "preview-collaboration-connect",
    ] {
        let content = normalized(&path(".agents", skill));
        for contract in [
            "preview-draft",
            "previewDraft.selector",
            "beforeHtml",
            "afterHtml",
            "complete documents",
            "current source differs from `beforeHtml`",
            "smallest source change",
            "absent or ambiguous",
            "do not guess",
            "failed",
            "specific reason",
        ] {
            assert!(
                content.contains(contract),
                "{skill} is missing Preview Draft handoff contract: {contract}"
            );
        }
    }
}

#[test]
fn readme_and_guide_distinguish_manual_foreground_and_background_collaboration() {
    let readme = normalized("README.md");

    for contract in [
        "$preview-collaboration-start path/to/page.html foreground",
        "$preview-collaboration-start path/to/page.html background",
        "detach",
        "close",
    ] {
        assert!(
            readme.contains(contract),
            "missing README contract: {contract}"
        );
    }

    let guide = normalized("docs/COLLABORATION.md");
    for contract in [
        "Manual foreground mode",
        "Background collaboration mode",
        "does not enter the feedback wait loop",
        "single start skill",
    ] {
        assert!(
            guide.contains(contract),
            "missing collaboration guide contract: {contract}"
        );
    }
}

#[test]
fn start_preserves_attachment_identity_across_sigint_and_restart() {
    let skill = normalized(&path(".agents", "preview-collaboration-start"));

    assert!(skill.contains("SIGINT"));
    assert!(skill.contains("exit status 130"));
    assert!(skill.contains("preserves the attachment"));
    assert!(skill.contains("preview-collaboration-stop"));
    assert!(skill.contains("same canonical entry"));
    assert!(skill.contains("reused"));
}

#[test]
fn skills_keep_the_same_blocking_wait_across_pause_and_resume() {
    let start = normalized(&path(".agents", "preview-collaboration-start"));
    for contract in [
        "collab pause --project <project> --attachment <attachment-id>",
        "pause-requested",
        "paused",
        "must remain blocked",
        "continues heartbeat",
        "collab resume --project <project> --attachment <attachment-id>",
        "same attachment ID",
        "Do not attach again",
        "Do not busy-poll",
    ] {
        assert!(
            start.contains(contract),
            "missing pause contract: {contract}"
        );
    }

    let stop = normalized(&path(".agents", "preview-collaboration-stop"));
    for contract in [
        "active, pause-requested, or paused",
        "Pause preserves the attachment",
        "Stop makes it inactive",
        "collaboration.stop",
    ] {
        assert!(
            stop.contains(contract),
            "missing stop distinction: {contract}"
        );
    }
}

#[test]
fn start_enforces_persisted_terminal_state_before_completion() {
    for agent_root in [".agents", ".claude"] {
        let skill = normalized(&path(agent_root, "preview-collaboration-start"));

        for contract in [
            "Persisted terminal state is the completion boundary",
            "terminal confirmation",
            "Do not report the feedback as complete",
            "Do not return from this feedback cycle",
            "Do not issue another `wait` for the next feedback item",
            "stops collaboration before terminal confirmation",
            "without claiming that feedback was completed",
            "eligible for lease recovery",
        ] {
            assert!(
                skill.contains(contract),
                "missing completion gate contract in {agent_root}: {contract}"
            );
        }
    }
}

#[test]
fn start_resolves_ambiguous_terminal_response_by_rereading() {
    for agent_root in [".agents", ".claude"] {
        let skill = normalized(&path(agent_root, "preview-collaboration-start"));

        for contract in [
            "Ambiguous terminal response",
            "read back the persisted record",
            "terminal confirmation is complete",
            "retry the original terminal transition",
            "stop claiming completion and report lost ownership",
            "read-back itself fails",
            "stop the current feedback loop",
        ] {
            assert!(
                skill.contains(contract),
                "missing ambiguous response contract in {agent_root}: {contract}"
            );
        }
    }
}

#[test]
fn start_does_not_add_recovery_shortcut_or_html_inference() {
    for agent_root in [".agents", ".claude"] {
        let skill = normalized(&path(agent_root, "preview-collaboration-start"));

        for prohibited in [
            "pending to resolved",
            "pending to failed",
            "HTML-based completion",
            "infer completion from HTML",
            "screenshot proves completion",
        ] {
            assert!(
                !skill.contains(prohibited),
                "prohibited recovery shortcut or inference found in {agent_root}: {prohibited}"
            );
        }
    }
}

#[test]
fn stop_only_detaches_the_selected_collaboration() {
    let skill = normalized(&path(".agents", "preview-collaboration-stop"));

    for contract in [
        "same conversation",
        "known attachment ID",
        "collab detach --project <project> --attachment <attachment-id>",
        "another conversation",
        "zero",
        "one active attachment",
        "multiple active attachments",
        "ambiguous-attachment",
        "collab detach --project <project>",
    ] {
        assert!(
            skill.contains(contract),
            "missing stop contract: {contract}"
        );
    }
    assert!(!skill.contains("`collab close --"));
}

#[test]
fn close_shuts_down_the_preview_runtime() {
    let skill = normalized(&path(".agents", "preview-collaboration-close"));

    assert!(skill.contains("collab close --project <project>"));
    assert!(skill.contains("runtime"));
    assert!(skill.contains("session file"));
    assert!(!skill.contains("collab detach"));
}

#[test]
fn every_skill_stops_when_the_cli_is_missing() {
    for skill in SKILLS {
        let content = normalized(&path(".agents", skill));

        for contract in [
            "requires the `collab` CLI",
            "command not found",
            "cargo install --path . --locked",
        ] {
            assert!(
                content.contains(contract),
                "{skill} is missing the CLI prerequisite: {contract}"
            );
        }
    }
}

#[test]
fn entry_skills_point_at_cli_help_without_widening_the_command_set() {
    for skill in [
        "preview-collaboration-start",
        "preview-collaboration-connect",
    ] {
        let content = normalized(&path(".agents", skill));

        for contract in [
            "collab <command> --help",
            "Do not run a `collab` command that this skill does not name",
        ] {
            assert!(
                content.contains(contract),
                "{skill} is missing the CLI help contract: {contract}"
            );
        }
    }
}
