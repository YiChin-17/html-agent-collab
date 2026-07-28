use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use collab::client::{self, ControlClient};
use collab::core::{
    AttachRequest, CollaborationControlRequest, CoreAdapter, CoreRequest, CoreResult,
    DetachRequest, Envelope, EvalRequest, FeedbackShowRequest, FeedbackStateRequest, OpError,
    WaitRequest,
};
use collab::{app, platform};
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "collab",
    version,
    about = "Preview collaboration tool for coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// 共用的 session 定位參數：discovery 起點與明確 session 選擇。
#[derive(Args)]
struct Target {
    /// Directory to discover the preview session from (walks up ancestors)
    #[arg(long, default_value = ".")]
    project: PathBuf,
    /// Explicit preview session ID when multiple previews match
    #[arg(long)]
    session: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Open a native preview window for an HTML entry file (macOS 15+)
    Open {
        /// HTML file, or a directory containing index.html
        entry: PathBuf,
        /// Start or reuse the preview and return after it becomes healthy
        #[arg(long)]
        background: bool,
    },
    /// Attach this agent to the active preview session
    Attach {
        #[command(flatten)]
        target: Target,
        /// Agent kind (e.g. claude-code, codex)
        #[arg(long, default_value = "unknown")]
        agent: String,
        /// Optional TUI session identifier
        #[arg(long)]
        tui_session: Option<String>,
    },
    /// Show the active preview session status
    Status {
        #[command(flatten)]
        target: Target,
    },
    /// Detach one collaboration attachment while preserving the preview
    Detach {
        #[command(flatten)]
        target: Target,
        /// Attachment ID; omitted only when zero or one active attachment exists
        #[arg(long)]
        attachment: Option<String>,
    },
    /// Pause after the selected attachment finishes its current feedback
    Pause {
        #[command(flatten)]
        target: Target,
        /// Attachment ID; omitted only when zero or one connected attachment exists
        #[arg(long)]
        attachment: Option<String>,
    },
    /// Resume the same paused collaboration attachment
    Resume {
        #[command(flatten)]
        target: Target,
        /// Attachment ID; omitted only when zero or one connected attachment exists
        #[arg(long)]
        attachment: Option<String>,
    },
    /// Close the preview runtime and all collaboration attachments
    Close {
        #[command(flatten)]
        target: Target,
    },
    /// Capture a native snapshot of the preview
    Screenshot {
        #[command(flatten)]
        target: Target,
    },
    /// Evaluate JavaScript in the preview page
    Eval {
        #[command(flatten)]
        target: Target,
        /// JavaScript expression to evaluate
        expression: String,
    },
    /// Block until feedback/control arrives; SIGINT emits `interrupted` and exits 130
    Wait {
        #[command(flatten)]
        target: Target,
        /// Attachment ID returned by `collab attach`
        #[arg(long)]
        attachment: Option<String>,
        /// Emit JSON envelope output (currently always on)
        #[arg(long)]
        json: bool,
    },
    /// Inspect or update feedback items
    Feedback {
        #[command(subcommand)]
        command: FeedbackCommand,
    },
}

#[derive(Subcommand)]
enum FeedbackCommand {
    /// Show a feedback item by ID
    Show {
        #[command(flatten)]
        target: Target,
        id: String,
    },
    /// Transition a feedback item to a new lifecycle state
    SetState {
        #[command(flatten)]
        target: Target,
        id: String,
        /// One of: acknowledged, working, resolved, failed
        state: String,
        /// Compare-and-set guard: current state required for this transition
        #[arg(long)]
        expected: String,
        /// Attachment ID returned by `collab attach`
        #[arg(long)]
        attachment: String,
        /// Required when transitioning to failed
        #[arg(long)]
        reason: Option<String>,
    },
}

fn main() -> ExitCode {
    // design「Rust + Tauri 2 單一應用核心」：只有 `open` 進入 Tauri lifecycle，
    // 其他 subcommand 一律在初始化 Tauri 前完成 CLI dispatch。
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Open { entry, background } => {
            return if background {
                run_background_open(entry)
            } else {
                run_open(entry)
            };
        }
        Command::Attach {
            target,
            agent,
            tui_session,
        } => run_core(
            &target,
            CoreRequest::Attach(AttachRequest {
                agent_kind: agent,
                tui_session_id: tui_session,
                pid: std::process::id(),
            }),
        ),
        Command::Status { target } => run_core(&target, CoreRequest::Status),
        Command::Detach { target, attachment } => run_core(
            &target,
            CoreRequest::Detach(DetachRequest {
                attachment_id: attachment,
            }),
        ),
        Command::Pause { target, attachment } => run_core(
            &target,
            CoreRequest::Pause(CollaborationControlRequest {
                attachment_id: attachment,
            }),
        ),
        Command::Resume { target, attachment } => run_core(
            &target,
            CoreRequest::Resume(CollaborationControlRequest {
                attachment_id: attachment,
            }),
        ),
        Command::Close { target } => run_core(&target, CoreRequest::Close),
        Command::Screenshot { target } => run_core(&target, CoreRequest::Screenshot),
        Command::Eval { target, expression } => {
            run_core(&target, CoreRequest::Eval(EvalRequest { expression }))
        }
        Command::Wait {
            target, attachment, ..
        } => run_wait(&target, attachment.as_deref()),
        Command::Feedback { command } => match command {
            FeedbackCommand::Show { target, id } => run_core(
                &target,
                CoreRequest::FeedbackShow(FeedbackShowRequest { feedback_id: id }),
            ),
            FeedbackCommand::SetState {
                target,
                id,
                state,
                expected,
                attachment,
                reason,
            } => run_core(
                &target,
                CoreRequest::FeedbackSetState(FeedbackStateRequest {
                    feedback_id: id,
                    attachment_id: attachment,
                    expected_state: expected,
                    state,
                    reason,
                }),
            ),
        },
    };
    emit_core(result)
}

/// 將 op 結果以統一 envelope 輸出到 stdout，並轉成 exit code。
fn emit_core(result: Result<CoreResult, OpError>) -> ExitCode {
    let (envelope, code) = match result {
        Ok(result) if result.is_interrupted() => {
            (Envelope::success(result.into_data()), ExitCode::from(130))
        }
        Ok(result) => (Envelope::success(result.into_data()), ExitCode::SUCCESS),
        Err(error) => (Envelope::failure(error), ExitCode::FAILURE),
    };
    println!(
        "{}",
        serde_json::to_string(&envelope).expect("envelope is always serializable")
    );
    code
}

fn run_open(entry: PathBuf) -> ExitCode {
    let result = (|| -> Result<(), OpError> {
        platform::ensure_supported_platform()
            .map_err(|e| OpError::new("unsupported-platform", e.to_string()))?;
        let resolved = collab::session::resolve_entry(&entry)
            .map_err(|error| OpError::new(error.code, error.message))?;
        let project_root = resolved.project_root;
        let entry_file = resolved.entry_file;
        let startup_lock =
            collab::session::acquire_startup_lock(&project_root).map_err(|error| {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    OpError::new(
                        "preview-already-running",
                        format!(
                            "another preview for {} is starting or already running",
                            project_root.display()
                        ),
                    )
                } else {
                    OpError::new(
                        "internal-error",
                        format!("cannot lock preview startup: {error}"),
                    )
                }
            })?;
        // 同一 project root 已有存活 preview 時拒絕重複啟動，維持 registry 一致。
        if let Ok(existing) = collab::session::read_session_file(&project_root)
            && client::discover_live_sessions(&project_root)
                .iter()
                .any(|s| s.session_id == existing.session_id && s.project_root == project_root)
        {
            if existing.entry_file != entry_file {
                return Err(OpError::new(
                    "entry-conflict",
                    format!(
                        "preview session {} already owns {}",
                        existing.session_id,
                        existing.entry_file.display()
                    ),
                )
                .with_details(json!({
                    "currentEntry": existing.entry_file,
                    "requestedEntry": entry_file,
                })));
            }
            return Err(OpError::new(
                "preview-already-running",
                format!(
                    "a preview for {} is already running (session {})",
                    entry_file.display(),
                    existing.session_id
                ),
            ));
        }
        app::run(project_root, entry_file, startup_lock)
            .map_err(|e| OpError::new("internal-error", format!("preview application failed: {e}")))
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => emit_core(Err(error)),
    }
}

fn run_background_open(entry: PathBuf) -> ExitCode {
    let result = platform::ensure_supported_platform()
        .map_err(|error| OpError::new("unsupported-platform", error.to_string()))
        .and_then(|()| collab::preview::open_background(&entry));
    let (envelope, code) = match result {
        Ok(result) => (
            Envelope::success(
                serde_json::to_value(result).expect("background open result is serializable"),
            ),
            ExitCode::SUCCESS,
        ),
        Err(error) => (Envelope::failure(error), ExitCode::FAILURE),
    };
    println!(
        "{}",
        serde_json::to_string(&envelope).expect("envelope is always serializable")
    );
    code
}

fn run_core(target: &Target, request: CoreRequest) -> Result<CoreResult, OpError> {
    let session = client::resolve_session(&target.project, target.session.as_deref())?;
    let client = ControlClient::new(session)?;
    client.execute(request)
}

fn run_wait(target: &Target, attachment: Option<&str>) -> Result<CoreResult, OpError> {
    let session = client::resolve_session(&target.project, target.session.as_deref())?;
    let attachment = attachment.ok_or_else(|| {
        OpError::new(
            "attachment-required",
            "`collab wait` requires --attachment <id>; run `collab attach` first",
        )
    })?;
    let client = ControlClient::new(session)?;
    client.execute(CoreRequest::Wait(WaitRequest {
        attachment_id: attachment.to_string(),
    }))
}
