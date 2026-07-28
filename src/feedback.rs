//! Feedback 驗證、持久化、queue lease 與 lifecycle。
//! disk record 是 canonical source；每次 mutation 都以 atomic rename 發布，
//! server 只需在外層序列化同一 process 內的競爭操作。

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const FEEDBACK_DIR: &str = "feedback";
pub const FEEDBACK_VIEW_LIMIT: usize = 256;
pub const PREVIEW_DRAFT_DOCUMENT_LIMIT_BYTES: usize = 262_144;

pub const DEFAULT_LEASE_DURATION: Duration = Duration::from_secs(300);

const PENDING: &str = "pending";
#[cfg(test)]
const ACKNOWLEDGED: &str = "acknowledged";
#[cfg(test)]
const WORKING: &str = "working";
const RESOLVED: &str = "resolved";
const FAILED: &str = "failed";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FeedbackKind {
    ElementComment,
    Painting,
    PreviewDraft,
    Textbox,
}

impl FeedbackKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ElementComment => "element-comment",
            Self::Painting => "painting",
            Self::PreviewDraft => "preview-draft",
            Self::Textbox => "textbox",
        }
    }
}

impl PartialEq<&str> for FeedbackKind {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FeedbackState {
    Pending,
    Acknowledged,
    Working,
    Resolved,
    Failed,
}

impl FeedbackState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Acknowledged => "acknowledged",
            Self::Working => "working",
            Self::Resolved => "resolved",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for FeedbackState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<&str> for FeedbackState {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "pending" => Ok(Self::Pending),
            "acknowledged" => Ok(Self::Acknowledged),
            "working" => Ok(Self::Working),
            "resolved" => Ok(Self::Resolved),
            "failed" => Ok(Self::Failed),
            _ => Err(()),
        }
    }
}

impl PartialEq<&str> for FeedbackState {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackLease {
    pub owner: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryMetadata {
    pub previous_state: FeedbackState,
    pub previous_owner: Option<String>,
    pub recovered_at: u64,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDraft {
    pub selector: String,
    pub before_html: String,
    pub after_html: String,
}

/// design「Interface and data shape」的 feedback 最低欄位。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackRecord {
    pub id: String,
    pub kind: FeedbackKind,
    pub text: String,
    pub state: FeedbackState,
    pub orphaned: bool,
    /// epoch milliseconds；queue 依此排序（FIFO）。
    pub created_at: u64,
    pub page_url: String,
    pub viewport: Value,
    pub elements: Value,
    /// 附件的 absolute paths（painting 的 PNG 與 editable SVG）。
    pub attachments: Vec<String>,
    pub lease: Option<FeedbackLease>,
    /// painting 的 vector marks 原始資料（非 painting 為 null）。
    #[serde(default)]
    pub marks: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_draft: Option<PreviewDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryMetadata>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedbackStateCounts {
    pub pending: usize,
    pub acknowledged: usize,
    pub working: usize,
    pub resolved: usize,
    pub failed: usize,
}

pub struct FeedbackDashboardView {
    pub records: Vec<FeedbackRecord>,
    pub counts: FeedbackStateCounts,
}

#[derive(Debug)]
pub enum QueueError {
    Io(io::Error),
    Storage { path: PathBuf, message: String },
    NotFound(String),
    CompareMismatch { expected: String, actual: String },
    InvalidTransition { from: String, to: String },
    MissingFailureReason,
    LeaseRequired,
    LeaseOwnerMismatch { expected: String, actual: String },
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueueError::Io(error) => write!(f, "{error}"),
            QueueError::Storage { path, message } => {
                write!(f, "feedback storage error at {}: {message}", path.display())
            }
            QueueError::NotFound(id) => write!(f, "feedback item {id} was not found"),
            QueueError::CompareMismatch { expected, actual } => {
                write!(
                    f,
                    "expected state {expected}, but current state is {actual}"
                )
            }
            QueueError::InvalidTransition { from, to } => {
                write!(f, "invalid feedback state transition {from} -> {to}")
            }
            QueueError::MissingFailureReason => {
                write!(f, "failed feedback requires a non-empty reason")
            }
            QueueError::LeaseRequired => write!(f, "feedback item has no active lease"),
            QueueError::LeaseOwnerMismatch { expected, actual } => {
                write!(f, "feedback lease belongs to {actual}, not {expected}")
            }
        }
    }
}

impl std::error::Error for QueueError {}

impl From<io::Error> for QueueError {
    fn from(error: io::Error) -> Self {
        QueueError::Io(error)
    }
}

/// overlay 送入的原始 payload（未含 server 指派欄位）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncomingFeedback {
    pub kind: FeedbackKind,
    pub text: String,
    pub page_url: String,
    pub viewport: Value,
    #[serde(default)]
    pub elements: Value,
    #[serde(default)]
    pub marks: Value,
    #[serde(default)]
    pub svg: Option<String>,
    #[serde(default)]
    pub preview_draft: Option<PreviewDraft>,
}

/// 驗證 overlay payload schema（design 風險節：專用 endpoint 必須驗證 schema）。
pub fn validate(payload: Value) -> Result<IncomingFeedback, String> {
    let incoming: IncomingFeedback = serde_json::from_value(payload)
        .map_err(|e| format!("invalid feedback kind or payload: {e}"))?;
    if !incoming.viewport.is_object() {
        return Err("feedback viewport must be an object".into());
    }
    match incoming.kind {
        FeedbackKind::ElementComment => {
            reject_preview_draft(&incoming)?;
            if incoming.text.trim().is_empty() {
                return Err("element-comment requires non-empty text".into());
            }
            let has_element = incoming
                .elements
                .as_array()
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            if !has_element {
                return Err("element-comment requires at least one element".into());
            }
        }
        FeedbackKind::Textbox => {
            reject_preview_draft(&incoming)?;
            if incoming.text.trim().is_empty() {
                return Err("textbox feedback requires non-empty text".into());
            }
        }
        FeedbackKind::Painting => {
            reject_preview_draft(&incoming)?;
            let has_marks = incoming
                .marks
                .as_array()
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            if !has_marks {
                return Err("painting feedback requires at least one mark".into());
            }
            let has_svg = incoming
                .svg
                .as_deref()
                .map(|s| s.contains("<svg"))
                .unwrap_or(false);
            if !has_svg {
                return Err("painting feedback requires serialized SVG markup".into());
            }
        }
        FeedbackKind::PreviewDraft => validate_preview_draft(&incoming)?,
    }
    Ok(incoming)
}

fn reject_preview_draft(incoming: &IncomingFeedback) -> Result<(), String> {
    if incoming.preview_draft.is_some() {
        Err("previewDraft is only valid for preview-draft feedback".into())
    } else {
        Ok(())
    }
}

fn validate_preview_draft(incoming: &IncomingFeedback) -> Result<(), String> {
    let draft = incoming
        .preview_draft
        .as_ref()
        .ok_or_else(|| "preview-draft requires previewDraft".to_string())?;
    let elements = incoming
        .elements
        .as_array()
        .filter(|elements| elements.len() == 1)
        .ok_or_else(|| "preview-draft requires exactly one element".to_string())?;
    let element_selector = elements[0]
        .get("selector")
        .and_then(Value::as_str)
        .filter(|selector| !selector.trim().is_empty())
        .ok_or_else(|| "preview-draft element requires a selector".to_string())?;
    if element_selector != draft.selector {
        return Err("previewDraft selector must match the element selector".into());
    }
    if !is_complete_html_document(&draft.before_html)
        || !is_complete_html_document(&draft.after_html)
    {
        return Err("previewDraft beforeHtml and afterHtml must be complete HTML documents".into());
    }
    if draft.before_html == draft.after_html {
        return Err("previewDraft complete documents must be different".into());
    }
    if draft.before_html.len() > PREVIEW_DRAFT_DOCUMENT_LIMIT_BYTES
        || draft.after_html.len() > PREVIEW_DRAFT_DOCUMENT_LIMIT_BYTES
    {
        return Err(format!(
            "previewDraft documents must not exceed {PREVIEW_DRAFT_DOCUMENT_LIMIT_BYTES} UTF-8 bytes"
        ));
    }
    Ok(())
}

fn is_complete_html_document(html: &str) -> bool {
    fn has_start_tag(lowercase: &str, tag: &str) -> bool {
        lowercase.match_indices(tag).any(|(index, _)| {
            lowercase
                .as_bytes()
                .get(index + tag.len())
                .is_some_and(|next| next.is_ascii_whitespace() || *next == b'>')
        })
    }

    let lowercase = html.to_ascii_lowercase();
    !html.trim().is_empty()
        && has_start_tag(&lowercase, "<html")
        && has_start_tag(&lowercase, "<head")
        && has_start_tag(&lowercase, "<body")
}

pub fn feedback_dir(project_root: &Path) -> PathBuf {
    project_root
        .join(crate::session::SESSION_DIR)
        .join(FEEDBACK_DIR)
}

pub fn validate_feedback_id(id: &str) -> Result<&str, &'static str> {
    let mut parts = id.split('-');
    let valid = parts.next() == Some("fb")
        && parts.next().is_some_and(|timestamp| {
            !timestamp.is_empty() && timestamp.bytes().all(|byte| byte.is_ascii_digit())
        })
        && parts.next().is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        && parts.next().is_none();
    if valid {
        Ok(id)
    } else {
        Err("feedback ID must match fb-<timestamp>-<hex suffix>")
    }
}

pub fn feedback_path(project_root: &Path, id: &str) -> io::Result<PathBuf> {
    let id = validate_feedback_id(id)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    Ok(feedback_dir(project_root).join(format!("{id}.json")))
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 指派 id、pending state 與 creation timestamp，但尚未落盤。
/// painting 流程需先完成附件（PNG/SVG）再 publish，故 prepare 與 write 分離。
pub fn prepare(incoming: IncomingFeedback) -> FeedbackRecord {
    let created_at = epoch_millis();
    FeedbackRecord {
        id: format!("fb-{created_at}-{}", crate::session::generate_session_id()),
        kind: incoming.kind,
        text: incoming.text,
        state: FeedbackState::Pending,
        orphaned: false,
        created_at,
        page_url: incoming.page_url,
        viewport: incoming.viewport,
        elements: incoming.elements,
        attachments: Vec::new(),
        lease: None,
        marks: incoming.marks,
        preview_draft: incoming.preview_draft,
        failure_reason: None,
        recovery: None,
    }
}

/// 建立記錄並 atomic 落盤（temp + rename）；回傳完整 record。
pub fn store(project_root: &Path, incoming: IncomingFeedback) -> io::Result<FeedbackRecord> {
    let record = prepare(incoming);
    write_record(project_root, &record)?;
    Ok(record)
}

/// atomic write：附件完成後才 publish 的基礎（task 4.1 在此之上加 queue event）。
pub fn write_record(project_root: &Path, record: &FeedbackRecord) -> io::Result<()> {
    let id = validate_feedback_id(&record.id)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let dir = crate::session::prepare_private_subdir(project_root, FEEDBACK_DIR)?;
    let path = dir.join(format!("{id}.json"));
    let body = serde_json::to_string_pretty(record)?;
    crate::session::write_artifact_bytes(&path, body.as_bytes()).map(|_| ())
}

pub fn read_record(project_root: &Path, id: &str) -> io::Result<FeedbackRecord> {
    let raw = fs::read_to_string(feedback_path(project_root, id)?)?;
    serde_json::from_str(&raw).map_err(io::Error::from)
}

/// 讀取最舊的 bounded feedback view，依 creation order（created_at, id）排序。
pub fn list_records(project_root: &Path) -> Result<Vec<FeedbackRecord>, QueueError> {
    list_records_matching(project_root, |_| true)
}

pub fn list_active_records(project_root: &Path) -> Result<Vec<FeedbackRecord>, QueueError> {
    list_records_matching(project_root, |record| {
        record.state != RESOLVED && record.state != FAILED
    })
}

pub fn dashboard_view(
    project_root: &Path,
    limit: usize,
) -> Result<FeedbackDashboardView, QueueError> {
    let mut records = Vec::with_capacity(limit.saturating_add(1));
    let mut counts = FeedbackStateCounts::default();
    visit_records(project_root, |record| {
        match record.state {
            FeedbackState::Pending => counts.pending += 1,
            FeedbackState::Acknowledged => counts.acknowledged += 1,
            FeedbackState::Working => counts.working += 1,
            FeedbackState::Resolved => counts.resolved += 1,
            FeedbackState::Failed => counts.failed += 1,
        }
        if limit > 0 {
            records.push(record);
            records.sort_by(|left, right| {
                right
                    .created_at
                    .cmp(&left.created_at)
                    .then_with(|| right.id.cmp(&left.id))
            });
            records.truncate(limit);
        }
        Ok(())
    })?;
    Ok(FeedbackDashboardView { records, counts })
}

pub fn marker_records(
    project_root: &Path,
    limit: usize,
) -> Result<Vec<FeedbackRecord>, QueueError> {
    let mut records = Vec::with_capacity(limit.saturating_add(1));
    visit_records(project_root, |record| {
        if record.kind != FeedbackKind::ElementComment || record.state == FeedbackState::Resolved {
            return Ok(());
        }
        if limit > 0 {
            records.push(record);
            records.sort_by(|left, right| {
                right
                    .created_at
                    .cmp(&left.created_at)
                    .then_with(|| right.id.cmp(&left.id))
            });
            records.truncate(limit);
        }
        Ok(())
    })?;
    Ok(records)
}

fn list_records_matching(
    project_root: &Path,
    include: impl Fn(&FeedbackRecord) -> bool,
) -> Result<Vec<FeedbackRecord>, QueueError> {
    let mut records = Vec::with_capacity(FEEDBACK_VIEW_LIMIT);
    visit_records(project_root, |record| {
        if !include(&record) {
            return Ok(());
        }
        let position = records
            .binary_search_by(|probe: &FeedbackRecord| {
                probe
                    .created_at
                    .cmp(&record.created_at)
                    .then(probe.id.cmp(&record.id))
            })
            .unwrap_or_else(|position| position);
        if position < FEEDBACK_VIEW_LIMIT {
            records.insert(position, record);
            if records.len() > FEEDBACK_VIEW_LIMIT {
                records.pop();
            }
        }
        Ok(())
    })?;
    Ok(records)
}

/// reload reconciliation：只翻 `orphaned` 旗標（非 lifecycle state），
/// 原始 payload 原樣保留（spec「Target was removed」）。
pub fn set_orphaned(project_root: &Path, id: &str, orphaned: bool) -> io::Result<FeedbackRecord> {
    let mut record = read_record(project_root, id)?;
    if record.orphaned != orphaned {
        record.orphaned = orphaned;
        write_record(project_root, &record)?;
    }
    Ok(record)
}

/// 依 creation order 租出最舊 pending item。任何尚未到期的 lease 存在時，
/// queue 不會交付下一筆，確保同一時間只有一個 attachment 擁有工作。
pub fn lease_next(project_root: &Path, owner: &str) -> Result<Option<FeedbackRecord>, QueueError> {
    lease_next_at(project_root, owner, epoch_millis(), DEFAULT_LEASE_DURATION)
}

pub fn lease_next_at(
    project_root: &Path,
    owner: &str,
    now: u64,
    duration: Duration,
) -> Result<Option<FeedbackRecord>, QueueError> {
    recover_expired_leases_at(project_root, now)?;
    let mut active_lease = false;
    let mut oldest_pending: Option<FeedbackRecord> = None;
    visit_records(project_root, |record| {
        if record
            .lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at > now)
        {
            active_lease = true;
        }
        if record.state == PENDING && record.lease.is_none() {
            let replace = oldest_pending.as_ref().is_none_or(|oldest| {
                (record.created_at, &record.id) < (oldest.created_at, &oldest.id)
            });
            if replace {
                oldest_pending = Some(record);
            }
        }
        Ok(())
    })?;
    if active_lease {
        return Ok(None);
    }

    let Some(mut record) = oldest_pending else {
        return Ok(None);
    };
    record.lease = Some(FeedbackLease {
        owner: owner.to_string(),
        expires_at: now.saturating_add(duration.as_millis() as u64),
    });
    write_record(project_root, &record)?;
    Ok(Some(record))
}

/// application restart 或 wait 前呼叫：expired lease 與失去 lease 的
/// acknowledged/working item 回到 pending，並保留 recovery metadata。
pub fn recover_expired_leases(project_root: &Path) -> Result<usize, QueueError> {
    recover_expired_leases_at(project_root, epoch_millis())
}

pub fn recover_expired_leases_at(project_root: &Path, now: u64) -> Result<usize, QueueError> {
    let mut recovered = 0;
    visit_records(project_root, |mut record| {
        let lease_expired = record
            .lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at <= now)
            && matches!(
                record.state,
                FeedbackState::Pending | FeedbackState::Acknowledged | FeedbackState::Working
            );
        let missing_work_lease = matches!(
            record.state,
            FeedbackState::Acknowledged | FeedbackState::Working
        ) && record.lease.is_none();
        if !lease_expired && !missing_work_lease {
            return Ok(());
        }
        recover_record(&mut record, now);
        write_record(project_root, &record)?;
        recovered += 1;
        Ok(())
    })?;
    Ok(recovered)
}

pub fn time_until_next_lease_expiry(project_root: &Path) -> Result<Option<Duration>, QueueError> {
    let now = epoch_millis();
    let mut next = None;
    visit_records(project_root, |record| {
        if let Some(expires_at) = record.lease.map(|lease| lease.expires_at) {
            next = Some(next.map_or(expires_at, |current: u64| current.min(expires_at)));
        }
        Ok(())
    })?;
    Ok(next.map(|expires_at| Duration::from_millis(expires_at.saturating_sub(now))))
}

pub fn renew_owner_leases(project_root: &Path, owner: &str) -> Result<usize, QueueError> {
    renew_owner_leases_at(project_root, owner, epoch_millis(), DEFAULT_LEASE_DURATION)
}

pub fn owner_has_live_lease(project_root: &Path, owner: &str) -> Result<bool, QueueError> {
    recover_expired_leases(project_root)?;
    let now = epoch_millis();
    let mut found = false;
    visit_records(project_root, |record| {
        if matches!(
            record.state,
            FeedbackState::Pending | FeedbackState::Acknowledged | FeedbackState::Working
        ) && record
            .lease
            .as_ref()
            .is_some_and(|lease| lease.owner == owner && lease.expires_at > now)
        {
            found = true;
        }
        Ok(())
    })?;
    Ok(found)
}

pub fn renew_owner_leases_at(
    project_root: &Path,
    owner: &str,
    now: u64,
    duration: Duration,
) -> Result<usize, QueueError> {
    let mut renewed = 0;
    visit_records(project_root, |mut record| {
        let Some(lease) = record.lease.as_mut() else {
            return Ok(());
        };
        if lease.owner != owner || lease.expires_at <= now {
            return Ok(());
        }
        lease.expires_at = lease
            .expires_at
            .max(now.saturating_add(duration.as_millis() as u64));
        write_record(project_root, &record)?;
        renewed += 1;
        Ok(())
    })?;
    Ok(renewed)
}

pub fn transition(
    project_root: &Path,
    id: &str,
    expected_state: &str,
    next_state: &str,
    owner: &str,
    reason: Option<&str>,
) -> Result<FeedbackRecord, QueueError> {
    transition_at(
        project_root,
        id,
        expected_state,
        next_state,
        owner,
        reason,
        epoch_millis(),
    )
}

pub fn transition_at(
    project_root: &Path,
    id: &str,
    expected_state: &str,
    next_state: &str,
    owner: &str,
    reason: Option<&str>,
    now: u64,
) -> Result<FeedbackRecord, QueueError> {
    let mut record = read_record(project_root, id).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            QueueError::NotFound(id.to_string())
        } else {
            QueueError::Io(error)
        }
    })?;

    if record
        .lease
        .as_ref()
        .is_some_and(|lease| lease.expires_at <= now)
        && matches!(
            record.state,
            FeedbackState::Pending | FeedbackState::Acknowledged | FeedbackState::Working
        )
        || (matches!(
            record.state,
            FeedbackState::Acknowledged | FeedbackState::Working
        ) && record.lease.is_none())
    {
        recover_record(&mut record, now);
        write_record(project_root, &record)?;
    }

    if record.state != expected_state {
        return Err(QueueError::CompareMismatch {
            expected: expected_state.to_string(),
            actual: record.state.to_string(),
        });
    }
    validate_transition(expected_state, next_state, reason)?;
    let lease = record.lease.as_ref().ok_or(QueueError::LeaseRequired)?;
    if lease.owner != owner {
        return Err(QueueError::LeaseOwnerMismatch {
            expected: owner.to_string(),
            actual: lease.owner.clone(),
        });
    }

    let next_state =
        FeedbackState::try_from(next_state).map_err(|()| QueueError::InvalidTransition {
            from: expected_state.to_string(),
            to: next_state.to_string(),
        })?;
    record.state = next_state;
    if next_state == FAILED {
        record.failure_reason = reason.map(str::trim).map(str::to_string);
    }
    if matches!(next_state, FeedbackState::Resolved | FeedbackState::Failed) {
        record.lease = None;
    } else if let Some(lease) = record.lease.as_mut() {
        lease.expires_at = lease
            .expires_at
            .max(now.saturating_add(DEFAULT_LEASE_DURATION.as_millis() as u64));
    }
    write_record(project_root, &record)?;
    Ok(record)
}

pub fn release_owner_leases(project_root: &Path, owner: &str) -> Result<usize, QueueError> {
    release_owner_leases_at(project_root, owner, epoch_millis())
}

pub fn release_owner_leases_at(
    project_root: &Path,
    owner: &str,
    now: u64,
) -> Result<usize, QueueError> {
    let mut released = 0;
    visit_records(project_root, |mut record| {
        let owned = record
            .lease
            .as_ref()
            .is_some_and(|lease| lease.owner == owner);
        if !owned {
            return Ok(());
        }
        if matches!(
            record.state,
            FeedbackState::Resolved | FeedbackState::Failed
        ) {
            return Ok(());
        }
        recover_record(&mut record, now);
        write_record(project_root, &record)?;
        released += 1;
        Ok(())
    })?;
    Ok(released)
}

pub fn release_lease(
    project_root: &Path,
    id: &str,
    owner: &str,
) -> Result<FeedbackRecord, QueueError> {
    let mut record = read_record(project_root, id).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            QueueError::NotFound(id.to_string())
        } else {
            QueueError::Io(error)
        }
    })?;
    let lease = record.lease.as_ref().ok_or(QueueError::LeaseRequired)?;
    if lease.owner != owner {
        return Err(QueueError::LeaseOwnerMismatch {
            expected: owner.to_string(),
            actual: lease.owner.clone(),
        });
    }
    recover_record(&mut record, epoch_millis());
    write_record(project_root, &record)?;
    Ok(record)
}

pub fn validate_transition(from: &str, to: &str, reason: Option<&str>) -> Result<(), QueueError> {
    let from_state = FeedbackState::try_from(from);
    let to_state = FeedbackState::try_from(to);
    let valid = matches!(
        (from_state, to_state),
        (Ok(FeedbackState::Pending), Ok(FeedbackState::Acknowledged))
            | (Ok(FeedbackState::Acknowledged), Ok(FeedbackState::Working))
            | (Ok(FeedbackState::Working), Ok(FeedbackState::Resolved))
            | (Ok(FeedbackState::Working), Ok(FeedbackState::Failed))
    );
    if !valid {
        return Err(QueueError::InvalidTransition {
            from: from.to_string(),
            to: to.to_string(),
        });
    }
    if to_state == Ok(FeedbackState::Failed) && reason.is_none_or(|value| value.trim().is_empty()) {
        return Err(QueueError::MissingFailureReason);
    }
    Ok(())
}

fn recover_record(record: &mut FeedbackRecord, now: u64) {
    let previous_state = std::mem::replace(&mut record.state, FeedbackState::Pending);
    let previous_owner = record.lease.take().map(|lease| lease.owner);
    let count = record
        .recovery
        .as_ref()
        .map_or(1, |recovery| recovery.count.saturating_add(1));
    record.recovery = Some(RecoveryMetadata {
        previous_state,
        previous_owner,
        recovered_at: now,
        count,
    });
}

fn visit_records(
    project_root: &Path,
    mut visit: impl FnMut(FeedbackRecord) -> Result<(), QueueError>,
) -> Result<(), QueueError> {
    let entries = match fs::read_dir(feedback_dir(project_root)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(storage_error(&feedback_dir(project_root), "scan", error));
        }
    };
    for entry in entries {
        let entry = entry
            .map_err(|error| storage_error(&feedback_dir(project_root), "read entry", error))?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let Some(filename_id) = file_name.strip_suffix(".json") else {
            continue;
        };
        let path = entry.path();
        let raw = fs::read_to_string(&path).map_err(|error| storage_error(&path, "read", error))?;
        let record = serde_json::from_str::<FeedbackRecord>(&raw)
            .map_err(|error| storage_error(&path, "parse", error))?;
        if validate_feedback_id(filename_id).is_err() || record.id != filename_id {
            eprintln!(
                "skipping feedback record {}: filename ID {:?} does not match embedded ID {:?}",
                entry.path().display(),
                filename_id,
                record.id
            );
            continue;
        }
        visit(record)?;
    }
    Ok(())
}

fn storage_error(path: &Path, operation: &str, error: impl fmt::Display) -> QueueError {
    let message = format!("{operation} failed: {error}");
    eprintln!("feedback storage error at {}: {message}", path.display());
    QueueError::Storage {
        path: path.to_path_buf(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEMP_DIR_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "collab-feedback-test-{name}-{}-{}",
            std::process::id(),
            TEMP_DIR_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn valid_payload() -> Value {
        json!({
            "kind": "element-comment",
            "text": "make it blue",
            "pageUrl": "http://127.0.0.1:1234/",
            "viewport": {"width": 1200, "height": 800, "scrollX": 0, "scrollY": 0},
            "elements": [{"selector": "#hero", "tag": "h1"}],
        })
    }

    fn valid_preview_draft_payload() -> Value {
        let before = "<!doctype html><html><head><title>Draft</title></head><body><h1>Hello</h1></body></html>";
        let after = "<!doctype html><html><head><title>Draft</title></head><body><h1>Welcome</h1></body></html>";
        json!({
            "kind": "preview-draft",
            "text": "Apply Preview Draft",
            "pageUrl": "http://127.0.0.1:1234/",
            "viewport": {"width": 1200, "height": 800, "scrollX": 0, "scrollY": 0},
            "elements": [{"selector": "#hero > h1", "tag": "h1"}],
            "previewDraft": {
                "selector": "#hero > h1",
                "beforeHtml": before,
                "afterHtml": after,
            },
        })
    }

    #[test]
    fn preview_draft_serializes_its_optional_handoff() {
        let record = prepare(validate(valid_preview_draft_payload()).unwrap());
        let serialized = serde_json::to_value(record).unwrap();

        assert_eq!(serialized["kind"], "preview-draft");
        assert_eq!(
            serialized["previewDraft"],
            json!({
                "selector": "#hero > h1",
                "beforeHtml": "<!doctype html><html><head><title>Draft</title></head><body><h1>Hello</h1></body></html>",
                "afterHtml": "<!doctype html><html><head><title>Draft</title></head><body><h1>Welcome</h1></body></html>",
            })
        );
    }

    #[test]
    fn feedback_record_without_preview_draft_remains_readable() {
        let record = prepare(validate(valid_payload()).unwrap());
        let mut serialized = serde_json::to_value(record).unwrap();
        serialized.as_object_mut().unwrap().remove("previewDraft");

        let loaded: FeedbackRecord = serde_json::from_value(serialized).unwrap();
        let round_trip = serde_json::to_value(loaded).unwrap();

        assert!(round_trip.get("previewDraft").is_none());
    }

    #[test]
    fn preview_draft_rejects_invalid_handoff_shapes_and_limits() {
        let mut selector_mismatch = valid_preview_draft_payload();
        selector_mismatch["elements"][0]["selector"] = json!("#other");

        let mut other_kind_with_draft = valid_preview_draft_payload();
        other_kind_with_draft["kind"] = json!("element-comment");

        let mut blank_before = valid_preview_draft_payload();
        blank_before["previewDraft"]["beforeHtml"] = json!("   ");

        let mut same_document = valid_preview_draft_payload();
        same_document["previewDraft"]["afterHtml"] =
            same_document["previewDraft"]["beforeHtml"].clone();

        let mut fragment_after = valid_preview_draft_payload();
        fragment_after["previewDraft"]["afterHtml"] = json!("<h1>Welcome</h1>");

        let mut oversized_document = valid_preview_draft_payload();
        oversized_document["previewDraft"]["afterHtml"] = json!(format!(
            "<!doctype html><html><head></head><body>{}</body></html>",
            "x".repeat(262_145)
        ));

        for payload in [
            selector_mismatch,
            other_kind_with_draft,
            blank_before,
            same_document,
            fragment_after,
            oversized_document,
        ] {
            assert!(validate(payload).is_err());
        }
    }

    #[test]
    fn preview_draft_document_limit_uses_exact_utf8_bytes() {
        let prefix = "<!doctype html><html><head></head><body>";
        let suffix = "</body></html>";
        let content_len = PREVIEW_DRAFT_DOCUMENT_LIMIT_BYTES - prefix.len() - suffix.len();
        let accepted = format!("{prefix}{}{suffix}", "x".repeat(content_len));
        assert_eq!(accepted.len(), PREVIEW_DRAFT_DOCUMENT_LIMIT_BYTES);

        let mut at_limit = valid_preview_draft_payload();
        at_limit["previewDraft"]["afterHtml"] = json!(accepted.clone());
        assert!(validate(at_limit).is_ok());

        let mut over_limit = valid_preview_draft_payload();
        over_limit["previewDraft"]["afterHtml"] = json!(format!("{accepted}x"));
        assert!(validate(over_limit).is_err());
    }

    #[test]
    fn feedback_id_validation_matches_spec_examples() {
        for (id, expected_valid) in [
            ("fb-1737072000000-a1b2c3d4", true),
            ("../session", false),
            ("/etc/passwd", false),
            ("fb-1/../../x", false),
        ] {
            assert_eq!(
                validate_feedback_id(id).is_ok(),
                expected_valid,
                "unexpected validation result for {id}"
            );
        }
    }

    #[test]
    fn unsafe_feedback_ids_never_form_storage_paths() {
        let root = temp_root("unsafe-storage-id");
        std::fs::create_dir_all(feedback_dir(&root)).unwrap();
        let escaped_path = root.join(".collab/outside.json");
        std::fs::write(&escaped_path, b"outside must not be overwritten").unwrap();

        assert_eq!(
            read_record(&root, "../outside").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        let mut record = prepare(validate(valid_payload()).unwrap());
        record.id = "../outside".into();
        assert_eq!(
            write_record(&root, &record).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            std::fs::read(&escaped_path).unwrap(),
            b"outside must not be overwritten"
        );
    }

    #[test]
    fn accepts_valid_element_comment() {
        let incoming = validate(valid_payload()).unwrap();
        assert_eq!(incoming.kind, "element-comment");
    }

    #[test]
    fn rejects_unknown_kind_and_missing_fields() {
        let mut bad_kind = valid_payload();
        bad_kind["kind"] = json!("emoji-reaction");
        assert!(
            validate(bad_kind)
                .unwrap_err()
                .contains("invalid feedback kind")
        );

        let mut empty_text = valid_payload();
        empty_text["text"] = json!("   ");
        assert!(validate(empty_text).unwrap_err().contains("non-empty text"));

        let mut no_elements = valid_payload();
        no_elements["elements"] = json!([]);
        assert!(
            validate(no_elements)
                .unwrap_err()
                .contains("at least one element")
        );
    }

    #[test]
    fn corrupt_record_scan_surfaces_filename_and_parse_error() {
        let root = temp_root("corrupt-scan");
        let path = feedback_dir(&root).join("fb-10-deadbeef.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not-json").unwrap();

        let error = list_records(&root).unwrap_err().to_string();

        assert!(error.contains("fb-10-deadbeef.json"));
        assert!(error.contains("parse"));
    }

    #[test]
    fn record_deserialization_rejects_unknown_kind_and_state() {
        let mut record = prepare(validate(valid_payload()).unwrap());
        record.id = "fb-10-feedface".into();
        let mut value = serde_json::to_value(&record).unwrap();

        value["state"] = json!("mystery");
        assert!(serde_json::from_value::<FeedbackRecord>(value.clone()).is_err());

        value["state"] = json!("pending");
        value["kind"] = json!("emoji-reaction");
        assert!(serde_json::from_value::<FeedbackRecord>(value).is_err());
    }

    #[test]
    fn failed_record_publish_removes_temporary_json() {
        let root = temp_root("record-publish-cleanup");
        let mut record = prepare(validate(valid_payload()).unwrap());
        record.id = "fb-10-cafebabe".into();
        let dir = feedback_dir(&root);
        std::fs::create_dir_all(dir.join(format!("{}.json", record.id))).unwrap();

        assert!(write_record(&root, &record).is_err());
        assert!(!dir.join(format!("{}.json.tmp", record.id)).exists());
    }

    #[test]
    fn feedback_write_rejects_symlinked_directory_without_touching_target() {
        let root = temp_root("symlinked-feedback-directory");
        let target = root.join("attacker-controlled");
        std::fs::create_dir_all(&target).unwrap();
        let victim = target.join("victim.txt");
        let original = b"victim must remain unchanged";
        std::fs::write(&victim, original).unwrap();
        std::fs::create_dir_all(root.join(crate::session::SESSION_DIR)).unwrap();
        symlink(&target, feedback_dir(&root)).unwrap();
        let record = prepare(validate(valid_payload()).unwrap());

        assert!(write_record(&root, &record).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), original);
        assert!(!target.join(format!("{}.json", record.id)).exists());
    }

    #[test]
    fn feedback_write_does_not_follow_preplaced_temporary_symlink() {
        let root = temp_root("feedback-temporary-symlink");
        let dir = feedback_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        let victim = root.join("victim.txt");
        let original = b"temporary symlink victim";
        std::fs::write(&victim, original).unwrap();
        let mut record = prepare(validate(valid_payload()).unwrap());
        record.id = "fb-10-cafed00d".into();
        symlink(&victim, dir.join(format!("{}.json.tmp", record.id))).unwrap();

        write_record(&root, &record).unwrap();

        assert_eq!(std::fs::read(&victim).unwrap(), original);
        assert_eq!(read_record(&root, &record.id).unwrap(), record);
    }

    #[test]
    fn feedback_write_atomically_replaces_final_symlink_with_private_file() {
        let root = temp_root("feedback-final-symlink");
        let dir = feedback_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        let victim = root.join("victim.txt");
        let original = b"final symlink victim";
        std::fs::write(&victim, original).unwrap();
        let mut record = prepare(validate(valid_payload()).unwrap());
        record.id = "fb-10-feedf00d".into();
        let final_path = feedback_path(&root, &record.id).unwrap();
        symlink(&victim, &final_path).unwrap();

        write_record(&root, &record).unwrap();

        assert_eq!(std::fs::read(&victim).unwrap(), original);
        assert!(
            !std::fs::symlink_metadata(&final_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::metadata(&final_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn painting_requires_marks_and_svg() {
        let mut painting = valid_payload();
        painting["kind"] = json!("painting");
        painting["elements"] = json!([]);
        assert!(
            validate(painting.clone())
                .unwrap_err()
                .contains("at least one mark")
        );

        painting["marks"] = json!([{"type": "rect", "x": 1, "y": 2, "width": 3, "height": 4}]);
        assert!(validate(painting.clone()).unwrap_err().contains("SVG"));

        painting["svg"] = json!("<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>");
        let incoming = validate(painting).unwrap();
        assert_eq!(incoming.kind, "painting");
        let record = prepare(incoming);
        assert_eq!(record.marks[0]["type"], "rect");
    }

    #[test]
    fn store_assigns_id_pending_state_and_round_trips() {
        let root = temp_root("store");

        let record = store(&root, validate(valid_payload()).unwrap()).unwrap();
        assert!(record.id.starts_with("fb-"));
        assert_eq!(record.state, "pending");
        assert!(!record.orphaned);
        assert!(record.created_at > 0);

        let read = read_record(&root, &record.id).unwrap();
        assert_eq!(read, record);
        // 不留 temp file。
        assert!(
            !feedback_dir(&root).read_dir().unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
    }

    #[test]
    fn transition_table_accepts_only_documented_lifecycle() {
        for (from, to) in [
            (PENDING, ACKNOWLEDGED),
            (ACKNOWLEDGED, WORKING),
            (WORKING, RESOLVED),
            (WORKING, FAILED),
        ] {
            let reason = (to == FAILED).then_some("verification failed");
            validate_transition(from, to, reason).unwrap();
        }

        for (from, to) in [
            (PENDING, WORKING),
            (PENDING, RESOLVED),
            (ACKNOWLEDGED, RESOLVED),
            (WORKING, PENDING),
            (RESOLVED, WORKING),
            (FAILED, PENDING),
        ] {
            assert!(
                matches!(
                    validate_transition(from, to, Some("reason")),
                    Err(QueueError::InvalidTransition { .. })
                ),
                "{from} -> {to} must be rejected"
            );
        }
        assert!(matches!(
            validate_transition(WORKING, FAILED, Some("  ")),
            Err(QueueError::MissingFailureReason)
        ));
    }

    #[test]
    fn leases_fifo_and_does_not_duplicate_an_active_lease() {
        let root = temp_root("fifo");
        for (id, created_at) in [
            ("fb-2-00000002", 20),
            ("fb-1-00000001", 10),
            ("fb-3-00000003", 30),
        ] {
            let mut record = prepare(validate(valid_payload()).unwrap());
            record.id = id.into();
            record.created_at = created_at;
            write_record(&root, &record).unwrap();
        }

        let first = lease_next_at(&root, "attachment-a", 100, Duration::from_millis(100)).unwrap();
        assert_eq!(
            first.as_ref().map(|record| record.id.as_str()),
            Some("fb-1-00000001")
        );

        let duplicate =
            lease_next_at(&root, "attachment-b", 110, Duration::from_millis(100)).unwrap();
        assert!(
            duplicate.is_none(),
            "an active lease blocks the next delivery"
        );

        transition_at(
            &root,
            "fb-1-00000001",
            PENDING,
            ACKNOWLEDGED,
            "attachment-a",
            None,
            120,
        )
        .unwrap();
        transition_at(
            &root,
            "fb-1-00000001",
            ACKNOWLEDGED,
            WORKING,
            "attachment-a",
            None,
            130,
        )
        .unwrap();
        transition_at(
            &root,
            "fb-1-00000001",
            WORKING,
            RESOLVED,
            "attachment-a",
            None,
            140,
        )
        .unwrap();

        let second = lease_next_at(&root, "attachment-b", 150, Duration::from_millis(100)).unwrap();
        assert_eq!(
            second.as_ref().map(|record| record.id.as_str()),
            Some("fb-2-00000002")
        );
    }

    #[test]
    fn restart_recovers_expired_work_and_preserves_payload() {
        let root = temp_root("restart");
        let mut record = prepare(validate(valid_payload()).unwrap());
        record.id = "fb-10-00000010".into();
        record.created_at = 10;
        record.state = FeedbackState::Working;
        record.lease = Some(FeedbackLease {
            owner: "attachment-old".into(),
            expires_at: 100,
        });
        let original_text = record.text.clone();
        write_record(&root, &record).unwrap();

        assert_eq!(recover_expired_leases_at(&root, 101).unwrap(), 1);
        let recovered = read_record(&root, "fb-10-00000010").unwrap();
        assert_eq!(recovered.state, PENDING);
        assert!(recovered.lease.is_none());
        assert_eq!(recovered.text, original_text);
        assert_eq!(
            recovered
                .recovery
                .as_ref()
                .map(|value| value.previous_state.as_str()),
            Some(WORKING)
        );
        assert_eq!(
            recovered
                .recovery
                .as_ref()
                .and_then(|value| value.previous_owner.as_deref()),
            Some("attachment-old")
        );

        let leased =
            lease_next_at(&root, "attachment-new", 102, Duration::from_millis(100)).unwrap();
        assert_eq!(
            leased
                .as_ref()
                .and_then(|value| value.lease.as_ref())
                .map(|lease| lease.owner.as_str()),
            Some("attachment-new")
        );
    }

    #[test]
    fn recovery_skips_record_when_filename_and_embedded_id_disagree() {
        let root = temp_root("recovery-id-mismatch");
        let filename_id = "fb-1737072000000-a1b2c3d4";
        let embedded_id = "fb-1737072000001-deadbeef";
        let mut record = prepare(validate(valid_payload()).unwrap());
        record.id = embedded_id.into();
        record.state = FeedbackState::Working;
        record.lease = Some(FeedbackLease {
            owner: "attachment-old".into(),
            expires_at: 100,
        });
        let dir = feedback_dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        let mismatched_path = dir.join(format!("{filename_id}.json"));
        std::fs::write(
            &mismatched_path,
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();

        assert_eq!(recover_expired_leases_at(&root, 101).unwrap(), 0);
        assert!(!feedback_path(&root, embedded_id).unwrap().exists());
        let unchanged: FeedbackRecord =
            serde_json::from_slice(&std::fs::read(mismatched_path).unwrap()).unwrap();
        assert_eq!(unchanged.state, WORKING);
        assert!(unchanged.recovery.is_none());
    }

    #[test]
    fn transition_compares_current_state_and_enforces_lease_owner() {
        let root = temp_root("compare");
        let mut record = prepare(validate(valid_payload()).unwrap());
        record.id = "fb-10-00000020".into();
        record.created_at = 10;
        write_record(&root, &record).unwrap();
        lease_next_at(&root, "attachment-a", 100, Duration::from_secs(1)).unwrap();

        let mismatch = transition_at(
            &root,
            "fb-10-00000020",
            ACKNOWLEDGED,
            WORKING,
            "attachment-a",
            None,
            110,
        )
        .unwrap_err();
        assert!(matches!(
            mismatch,
            QueueError::CompareMismatch {
                expected,
                actual
            } if expected == ACKNOWLEDGED && actual == PENDING
        ));

        let wrong_owner = transition_at(
            &root,
            "fb-10-00000020",
            PENDING,
            ACKNOWLEDGED,
            "attachment-b",
            None,
            120,
        )
        .unwrap_err();
        assert!(matches!(
            wrong_owner,
            QueueError::LeaseOwnerMismatch { actual, .. } if actual == "attachment-a"
        ));

        let current = read_record(&root, "fb-10-00000020").unwrap();
        assert_eq!(current.state, PENDING);
        assert_eq!(
            current.lease.as_ref().map(|lease| lease.owner.as_str()),
            Some("attachment-a")
        );
    }

    #[test]
    fn heartbeat_renews_active_lease_before_recovery() {
        let root = temp_root("lease-heartbeat");
        let mut record = prepare(validate(valid_payload()).unwrap());
        record.id = "fb-10-00000030".into();
        record.created_at = 10;
        write_record(&root, &record).unwrap();
        lease_next_at(&root, "attachment-a", 100, Duration::from_millis(100)).unwrap();

        assert_eq!(
            renew_owner_leases_at(&root, "attachment-a", 150, Duration::from_millis(100)).unwrap(),
            1
        );
        assert_eq!(recover_expired_leases_at(&root, 201).unwrap(), 0);

        let renewed = read_record(&root, "fb-10-00000030").unwrap();
        assert_eq!(renewed.lease.unwrap().expires_at, 250);
    }

    #[test]
    fn progress_transition_renews_non_terminal_lease() {
        let root = temp_root("lease-progress");
        let mut record = prepare(validate(valid_payload()).unwrap());
        record.id = "fb-10-00000031".into();
        record.created_at = 10;
        write_record(&root, &record).unwrap();
        lease_next_at(&root, "attachment-a", 100, Duration::from_millis(100)).unwrap();

        let updated = transition_at(
            &root,
            "fb-10-00000031",
            PENDING,
            ACKNOWLEDGED,
            "attachment-a",
            None,
            150,
        )
        .unwrap();

        assert!(updated.lease.unwrap().expires_at > 200);
        assert_eq!(recover_expired_leases_at(&root, 201).unwrap(), 0);
    }

    #[test]
    fn release_owner_leases_recovers_target_and_preserves_others() {
        let root = temp_root("release-owner");
        let now = 1_000_000u64;

        let mut working = prepare(validate(valid_payload()).unwrap());
        working.id = "fb-10-00000001".into();
        working.created_at = 10;
        working.state = FeedbackState::Working;
        working.lease = Some(FeedbackLease {
            owner: "attachment-target".into(),
            expires_at: now + 200_000,
        });
        let original_text = working.text.clone();
        write_record(&root, &working).unwrap();

        let mut acked = prepare(validate(valid_payload()).unwrap());
        acked.id = "fb-20-00000002".into();
        acked.created_at = 20;
        acked.state = FeedbackState::Acknowledged;
        acked.lease = Some(FeedbackLease {
            owner: "attachment-target".into(),
            expires_at: now + 200_000,
        });
        write_record(&root, &acked).unwrap();

        let mut pending_leased = prepare(validate(valid_payload()).unwrap());
        pending_leased.id = "fb-30-00000003".into();
        pending_leased.created_at = 30;
        pending_leased.state = FeedbackState::Pending;
        pending_leased.lease = Some(FeedbackLease {
            owner: "attachment-target".into(),
            expires_at: now + 200_000,
        });
        write_record(&root, &pending_leased).unwrap();

        let mut other_owner = prepare(validate(valid_payload()).unwrap());
        other_owner.id = "fb-40-00000004".into();
        other_owner.created_at = 40;
        other_owner.state = FeedbackState::Working;
        other_owner.lease = Some(FeedbackLease {
            owner: "attachment-other".into(),
            expires_at: now + 200_000,
        });
        write_record(&root, &other_owner).unwrap();

        let mut no_lease = prepare(validate(valid_payload()).unwrap());
        no_lease.id = "fb-50-00000005".into();
        no_lease.created_at = 50;
        write_record(&root, &no_lease).unwrap();

        let mut resolved = prepare(validate(valid_payload()).unwrap());
        resolved.id = "fb-60-00000006".into();
        resolved.created_at = 60;
        resolved.state = FeedbackState::Resolved;
        write_record(&root, &resolved).unwrap();

        let mut failed = prepare(validate(valid_payload()).unwrap());
        failed.id = "fb-70-00000007".into();
        failed.created_at = 70;
        failed.state = FeedbackState::Failed;
        failed.failure_reason = Some("test failure".into());
        write_record(&root, &failed).unwrap();

        let released = release_owner_leases_at(&root, "attachment-target", now).unwrap();
        assert_eq!(released, 3);

        let r1 = read_record(&root, "fb-10-00000001").unwrap();
        assert_eq!(r1.state, PENDING);
        assert!(r1.lease.is_none());
        assert_eq!(r1.text, original_text);
        let recovery = r1.recovery.as_ref().unwrap();
        assert_eq!(recovery.previous_state, FeedbackState::Working);
        assert_eq!(
            recovery.previous_owner.as_deref(),
            Some("attachment-target")
        );
        assert_eq!(recovery.count, 1);

        let r2 = read_record(&root, "fb-20-00000002").unwrap();
        assert_eq!(r2.state, PENDING);
        assert!(r2.lease.is_none());
        assert_eq!(
            r2.recovery.as_ref().unwrap().previous_state,
            FeedbackState::Acknowledged
        );

        let r3 = read_record(&root, "fb-30-00000003").unwrap();
        assert_eq!(r3.state, PENDING);
        assert!(r3.lease.is_none());

        let r4 = read_record(&root, "fb-40-00000004").unwrap();
        assert_eq!(r4.state, WORKING);
        assert_eq!(r4.lease.as_ref().unwrap().owner, "attachment-other");
        assert!(r4.recovery.is_none());

        let r5 = read_record(&root, "fb-50-00000005").unwrap();
        assert_eq!(r5.state, PENDING);
        assert!(r5.lease.is_none());
        assert!(r5.recovery.is_none());

        let r6 = read_record(&root, "fb-60-00000006").unwrap();
        assert_eq!(r6.state, RESOLVED);

        let r7 = read_record(&root, "fb-70-00000007").unwrap();
        assert_eq!(r7.state, FAILED);
    }

    /// Crash timing regression table (spec: "Feedback persistence and lifecycle").
    ///
    /// | State at interruption | Lease condition        | After recovery          |
    /// |-----------------------|------------------------|-------------------------|
    /// | acknowledged          | expired                | pending + recovery meta |
    /// | acknowledged          | missing                | pending + recovery meta |
    /// | working               | expired                | pending + recovery meta |
    /// | working               | missing                | pending + recovery meta |
    /// | resolved              | cleared (no lease)     | resolved unchanged      |
    /// | failed                | cleared (no lease)     | failed unchanged        |
    #[test]
    fn crash_timing_regression_table() {
        let root = temp_root("crash-timing");
        let recovery_time = 500u64;

        struct Row {
            id: &'static str,
            state: FeedbackState,
            lease: Option<FeedbackLease>,
            expect_state: &'static str,
            expect_recovery: bool,
        }

        let rows = [
            Row {
                id: "fb-10-00000001",
                state: FeedbackState::Acknowledged,
                lease: Some(FeedbackLease {
                    owner: "att-a".into(),
                    expires_at: 100,
                }),
                expect_state: PENDING,
                expect_recovery: true,
            },
            Row {
                id: "fb-20-00000002",
                state: FeedbackState::Acknowledged,
                lease: None,
                expect_state: PENDING,
                expect_recovery: true,
            },
            Row {
                id: "fb-30-00000003",
                state: FeedbackState::Working,
                lease: Some(FeedbackLease {
                    owner: "att-b".into(),
                    expires_at: 200,
                }),
                expect_state: PENDING,
                expect_recovery: true,
            },
            Row {
                id: "fb-40-00000004",
                state: FeedbackState::Working,
                lease: None,
                expect_state: PENDING,
                expect_recovery: true,
            },
            Row {
                id: "fb-50-00000005",
                state: FeedbackState::Resolved,
                lease: None,
                expect_state: RESOLVED,
                expect_recovery: false,
            },
            Row {
                id: "fb-60-00000006",
                state: FeedbackState::Failed,
                lease: None,
                expect_state: FAILED,
                expect_recovery: false,
            },
        ];

        for row in &rows {
            let mut record = prepare(validate(valid_payload()).unwrap());
            record.id = row.id.into();
            record.created_at = 10;
            record.state = row.state;
            record.lease = row.lease.clone();
            if row.state == FeedbackState::Failed {
                record.failure_reason = Some("original failure".into());
            }
            write_record(&root, &record).unwrap();
        }

        recover_expired_leases_at(&root, recovery_time).unwrap();

        for row in &rows {
            let record = read_record(&root, row.id).unwrap();
            assert_eq!(
                record.state, row.expect_state,
                "{}: expected {} after recovery",
                row.id, row.expect_state
            );
            assert_eq!(
                record.recovery.is_some(),
                row.expect_recovery,
                "{}: recovery metadata mismatch",
                row.id
            );
            if row.expect_recovery {
                assert!(
                    record.lease.is_none(),
                    "{}: lease should be cleared",
                    row.id
                );
                let meta = record.recovery.as_ref().unwrap();
                assert_eq!(
                    meta.previous_state, row.state,
                    "{}: previous_state mismatch",
                    row.id
                );
            }
        }

        // Terminal records must not be leased out after recovery.
        let leased = lease_next_at(
            &root,
            "att-new",
            recovery_time + 1,
            Duration::from_millis(100),
        )
        .unwrap();
        let leased_id = leased.as_ref().map(|r| r.id.as_str());
        assert!(
            leased_id != Some("fb-50-00000005") && leased_id != Some("fb-60-00000006"),
            "terminal record must not be re-leased: got {leased_id:?}"
        );
    }
}
