//! One-shot and JSONL headless run contract.
//!
//! These types are the shared protocol for a Client that runs one task without
//! a TUI. They project the existing durable task, session, and event semantics;
//! they never introduce a second executor, session store, or event vocabulary.
//!
//! Two input shapes are deliberate:
//!
//! - [`RunSource`] is what a *local* Client may ask the runtime to read
//!   (stdin, a file, an image). Resolution is bounded and happens before
//!   admission, so a malformed source fails without creating a task.
//! - [`RunInput`] is the resolved, transport-safe payload carried by
//!   [`RunRequest`]. It contains text and already-bounded inline image bytes;
//!   an HTTP caller can never name a host path to read.

use crate::error::ApiError;
use crate::limits::{Limits, Usage};
use crate::task::{ResolvedModel, Task, TaskState};
use base64::Engine;
use serde::{Deserialize, Serialize};

/// Headless run and JSONL frame schema version.
pub const RUN_PROTOCOL_SCHEMA: u32 = 1;

/// Maximum bytes of assembled prompt text, mirroring task admission.
pub const MAX_RUN_TEXT_BYTES: usize = 1_000_000;

/// Maximum bytes of one local text file source.
pub const MAX_RUN_FILE_BYTES: usize = 256 * 1024;

/// Maximum number of local text file sources in one run.
pub const MAX_RUN_FILES: usize = 16;

/// Maximum decoded bytes of one inline image.
pub const MAX_RUN_IMAGE_BYTES: usize = 512 * 1024;

/// Maximum number of inline images in one run.
pub const MAX_RUN_IMAGES: usize = 4;

/// Maximum one-shot wait bound accepted by the protocol.
pub const MAX_RUN_WAIT_MS: u64 = 600_000;

/// Media types accepted for inline images.
pub const IMAGE_MEDIA_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Stable process exit classes shared by the CLI and SDK.
pub mod exit {
    /// The task succeeded.
    pub const SUCCESS: i32 = 0;
    /// The request or protocol input was malformed or unsupported.
    pub const USAGE: i32 = 2;
    /// The caller is not authorized for the operation.
    pub const AUTHORIZATION: i32 = 3;
    /// An approval is required, pending, or was not granted.
    pub const APPROVAL: i32 = 4;
    /// Execution failed with a definite error.
    pub const FAILURE: i32 = 5;
    /// The outcome could not be proven (unknown, recovering, or still active).
    pub const UNCERTAIN: i32 = 6;
    /// The task was cancelled.
    pub const CANCELLED: i32 = 7;
}

/// One local input source a Client asks the runtime to resolve.
///
/// `path` is a client-local filesystem path. It is never part of the
/// [`RunInput`] sent to another process and is never echoed in an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunSource {
    /// Inline prompt text.
    Text {
        /// Prompt text.
        text: String,
    },
    /// Read the prompt from the Client's standard input.
    Stdin,
    /// Read one UTF-8 text file into the prompt.
    File {
        /// Client-local path.
        path: String,
        /// Declared media type; defaults to `text/plain`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
    },
    /// Read one image as bounded inline data.
    Image {
        /// Client-local path.
        path: String,
        /// Declared media type; inferred from the extension when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
    },
}

/// Resolved, transport-safe run input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunInput {
    /// Assembled prompt text.
    #[serde(default)]
    pub text: String,
    /// Metadata for text file sources folded into the prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<RunFile>,
    /// Inline images, already bounded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<RunImage>,
}

/// One text file that contributed to the assembled prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFile {
    /// Display name. Never a host path outside the operator's selection.
    pub name: String,
    /// Media type; `text/plain` unless the Client declared one.
    pub media_type: String,
    /// Number of content bytes folded into the prompt.
    pub bytes: usize,
    /// Digest of the content bytes.
    pub digest: String,
}

/// One inline image attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunImage {
    /// One of [`IMAGE_MEDIA_TYPES`].
    pub media_type: String,
    /// Base64-encoded image bytes.
    pub data: String,
    /// Display name, when the Client supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl RunImage {
    /// Build an inline image from raw bytes.
    pub fn from_bytes(
        media_type: impl Into<String>,
        data: impl AsRef<[u8]>,
        name: Option<String>,
    ) -> Self {
        Self {
            media_type: media_type.into(),
            data: base64::engine::general_purpose::STANDARD.encode(data.as_ref()),
            name,
        }
    }

    /// Decode the bounded image bytes.
    pub fn decode(&self) -> Result<Vec<u8>, ApiError> {
        let data = base64::engine::general_purpose::STANDARD
            .decode(&self.data)
            .map_err(|_| {
                ApiError::new(
                    "validation.run_image",
                    "an inline image is not valid base64",
                )
            })?;
        if data.is_empty() || data.len() > MAX_RUN_IMAGE_BYTES {
            return Err(ApiError::new(
                "validation.run_image",
                "an inline image is empty or exceeds the published bound",
            )
            .with("limit_bytes", MAX_RUN_IMAGE_BYTES.to_string()));
        }
        Ok(data)
    }

    /// Validate the declared media type and bounded payload.
    pub fn validate(&self) -> Result<(), ApiError> {
        if !IMAGE_MEDIA_TYPES.contains(&self.media_type.as_str()) {
            return Err(ApiError::new(
                "validation.run_image",
                "an inline image declares an unsupported media type",
            ));
        }
        self.decode().map(|_| ())
    }

    /// Decoded length without retaining the bytes.
    pub fn decoded_len(&self) -> usize {
        self.decode().map(|data| data.len()).unwrap_or(0)
    }
}

impl RunInput {
    /// Build input from one prompt string.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// Validate all published bounds before admission.
    ///
    /// Errors name the field, position, or limit; they never echo input text,
    /// file names, paths, or image data.
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.text.len() > MAX_RUN_TEXT_BYTES {
            return Err(ApiError::new(
                "validation.run_input",
                "run input text exceeds the published bound",
            )
            .with("limit_bytes", MAX_RUN_TEXT_BYTES.to_string()));
        }
        if self.files.len() > MAX_RUN_FILES {
            return Err(ApiError::new(
                "validation.run_input",
                "run input has too many file sources",
            )
            .with("limit", MAX_RUN_FILES.to_string()));
        }
        for file in &self.files {
            if file.bytes > MAX_RUN_FILE_BYTES {
                return Err(ApiError::new(
                    "validation.run_input",
                    "a run input file exceeds the published bound",
                )
                .with("limit_bytes", MAX_RUN_FILE_BYTES.to_string()));
            }
        }
        if self.images.len() > MAX_RUN_IMAGES {
            return Err(
                ApiError::new("validation.run_input", "run input has too many images")
                    .with("limit", MAX_RUN_IMAGES.to_string()),
            );
        }
        let mut total = self.text.len();
        for image in &self.images {
            image.validate()?;
            total = total.saturating_add(image.decoded_len());
        }
        if total > MAX_RUN_TEXT_BYTES {
            return Err(ApiError::new(
                "validation.run_input",
                "run input text and images exceed the published total bound",
            )
            .with("limit_bytes", MAX_RUN_TEXT_BYTES.to_string()));
        }
        if self.text.trim().is_empty() && self.images.is_empty() {
            return Err(ApiError::new(
                "validation.run_input",
                "run input must contain text or an image",
            ));
        }
        Ok(())
    }
}

/// Request to run one task through the shared runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunRequest {
    /// Agent selected by the caller; omitted uses the Worker default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Optional profile name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Explicit session identity. A new identity starts a new session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Continue the caller's most recently updated session.
    #[serde(default, rename = "continue")]
    pub continue_session: bool,
    /// Resume one existing, visible session by identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_session: Option<String>,
    /// Resolved run input.
    #[serde(default)]
    pub input: RunInput,
    /// Requested execution limits.
    #[serde(default)]
    pub limits: Limits,
    /// Caller-scoped idempotency key; generated when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Bound for waiting on a terminal outcome; omitted waits without an extra
    /// protocol deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
}

impl RunRequest {
    /// Validate protocol input before any task is admitted.
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.continue_session && self.resume_session.is_some() {
            return Err(ApiError::new(
                "validation.run_session",
                "continue and resume select different sessions",
            ));
        }
        if let (Some(session), Some(resume)) = (&self.session_id, &self.resume_session) {
            if session != resume {
                return Err(ApiError::new(
                    "validation.run_session",
                    "session_id and resume_session select different sessions",
                ));
            }
        }
        if let Some(wait) = self.wait_ms {
            if wait > MAX_RUN_WAIT_MS {
                return Err(ApiError::new(
                    "validation.run_wait",
                    "wait_ms exceeds the published bound",
                )
                .with("limit_ms", MAX_RUN_WAIT_MS.to_string()));
            }
        }
        if self
            .idempotency_key
            .as_ref()
            .is_some_and(|key| key.trim().is_empty() || key.len() > 200)
        {
            return Err(ApiError::new(
                "validation.idempotency_key",
                "idempotency_key must be 1-200 characters",
            ));
        }
        self.input.validate()
    }
}

/// One-shot, structured result of a headless run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    /// Result schema version.
    pub schema_version: u32,
    /// Stable task handle.
    pub task_id: String,
    /// Canonical session identity.
    pub session_id: String,
    /// Frozen agent.
    pub agent: String,
    /// Frozen profile name.
    pub profile: String,
    /// Frozen profile revision.
    pub profile_revision: u64,
    /// Frozen asset revision.
    pub asset_revision: u64,
    /// Cross-process trace identity.
    #[serde(default)]
    pub trace_id: String,
    /// Concrete provider selected for the invocation, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<ResolvedModel>,
    /// Management state.
    pub state: TaskState,
    /// Final output text, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Stable error, when the run failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
    /// Explicit uncertainty; never collapsed into success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<String>,
    /// Truthful usage.
    #[serde(default)]
    pub usage: Usage,
    /// Confirmed native invocation identity, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_invocation_id: Option<String>,
    /// Package-owned objective evidence, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
    /// Stable process exit class for this outcome.
    pub exit_code: i32,
}

impl RunResult {
    /// Project a durable task into the one-shot result.
    pub fn from_task(task: &Task) -> Self {
        Self {
            schema_version: RUN_PROTOCOL_SCHEMA,
            task_id: task.id.clone(),
            session_id: task.session_id.clone(),
            agent: task.agent.clone(),
            profile: task.profile.clone(),
            profile_revision: task.profile_revision,
            asset_revision: task.asset_revision,
            trace_id: task.trace_id.clone(),
            resolved_model: task.resolved_model.clone(),
            state: task.state,
            output: task.output.clone(),
            error: task.error.clone(),
            uncertainty: task.uncertainty.clone(),
            usage: task.usage.clone(),
            native_invocation_id: task.native_invocation_id.clone(),
            evidence: task.evidence.clone(),
            exit_code: exit_code(task.error.as_ref(), task.state),
        }
    }
}

/// Item taxonomy carried by a JSONL frame.
///
/// The state machine (`started`/`updated`/`completed`) is the frame variant;
/// the item describes what moved. Item names use Goldenbee's task vocabulary,
/// not a vendor's thread vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "item_type", rename_all = "snake_case")]
pub enum JsonlItem {
    /// Assistant answer text.
    AgentMessage {
        /// Stable item identity within the frame stream.
        id: String,
        /// Text fragment or completed answer.
        text: String,
    },
    /// Model reasoning text, separate from the answer.
    Reasoning {
        /// Stable item identity within the frame stream.
        id: String,
        /// Reasoning fragment or completed text.
        text: String,
    },
    /// One tool call and its outcome.
    ToolCall {
        /// Native call identity.
        id: String,
        /// Qualified tool name.
        name: String,
        /// MCP server identity, when the tool came from one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server: Option<String>,
        /// Authoring agent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        /// `true`/`false` on completion; absent while started.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<bool>,
        /// Stable error code on failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
    },
    /// One durable approval request and its decision.
    Approval {
        /// Durable approval identity.
        id: String,
        /// Operation name.
        operation: String,
        /// Exact operation target.
        target: String,
        /// Risk note recorded with the request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        risk: Option<String>,
        /// Recorded decision or pending status.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<String>,
    },
    /// One file checkpoint diff.
    Diff {
        /// Durable checkpoint identity.
        id: String,
        /// `closed`, `partial`, or the restore outcome.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        /// Typed diff preview, absent when it could not be read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<crate::checkpoint::CheckpointDiff>,
    },
}

impl JsonlItem {
    /// Stable item identity used to correlate `started` and `completed`.
    pub fn id(&self) -> &str {
        match self {
            Self::AgentMessage { id, .. }
            | Self::Reasoning { id, .. }
            | Self::ToolCall { id, .. }
            | Self::Approval { id, .. }
            | Self::Diff { id, .. } => id,
        }
    }
}

/// One ordered JSONL frame.
///
/// The stream starts with `task_started`, opens a `turn_started` for each
/// attempt, carries `item_started`/`item_updated`/`item_completed` frames mapped
/// from the durable task event stream, and ends with exactly one
/// `task_completed` frame carrying the same fields as [`RunResult`].
// Frames are built and serialized one at a time on a headless stream, so a
// flat wire enum is clearer than boxed indirection for a large terminal variant.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JsonlFrame {
    /// The task was durably accepted.
    TaskStarted {
        /// Frame schema version.
        schema_version: u32,
        /// Stable task handle.
        task_id: String,
        /// Canonical session identity.
        session_id: String,
        /// Frozen agent.
        agent: String,
        /// Frozen profile name.
        profile: String,
        /// Frozen profile revision.
        profile_revision: u64,
        /// Frozen asset revision.
        asset_revision: u64,
        /// Cross-process trace identity.
        #[serde(default)]
        trace_id: String,
        /// Concrete provider selected for the invocation, when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_model: Option<ResolvedModel>,
        /// Acceptance time.
        at: i64,
    },
    /// One task attempt began executing.
    TurnStarted {
        /// Stable task handle.
        task_id: String,
        /// Attempt number.
        attempt: u32,
        /// Source event sequence.
        seq: u64,
        /// Event time.
        at: i64,
    },
    /// An item began.
    ItemStarted {
        /// Stable task handle.
        task_id: String,
        /// Source event sequence.
        seq: u64,
        /// Event time.
        at: i64,
        /// Item that began.
        item: JsonlItem,
    },
    /// An item changed in place.
    ItemUpdated {
        /// Stable task handle.
        task_id: String,
        /// Source event sequence.
        seq: u64,
        /// Event time.
        at: i64,
        /// Item that changed.
        item: JsonlItem,
    },
    /// An item finished.
    ItemCompleted {
        /// Stable task handle.
        task_id: String,
        /// Source event sequence.
        seq: u64,
        /// Event time.
        at: i64,
        /// Completed item.
        item: JsonlItem,
    },
    /// The task reached a terminal projection.
    TaskCompleted {
        /// Frame schema version.
        schema_version: u32,
        /// Stable task handle.
        task_id: String,
        /// Canonical session identity.
        session_id: String,
        /// Frozen agent.
        agent: String,
        /// Frozen profile name.
        profile: String,
        /// Frozen profile revision.
        profile_revision: u64,
        /// Frozen asset revision.
        asset_revision: u64,
        /// Cross-process trace identity.
        #[serde(default)]
        trace_id: String,
        /// Concrete provider selected for the invocation, when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_model: Option<ResolvedModel>,
        /// Terminal management state.
        state: TaskState,
        /// Final output text, when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// Stable error, when the run failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ApiError>,
        /// Explicit uncertainty; never collapsed into success.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uncertainty: Option<String>,
        /// Truthful usage.
        #[serde(default)]
        usage: Usage,
        /// Confirmed native invocation identity, when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_invocation_id: Option<String>,
        /// Package-owned objective evidence, when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence: Option<serde_json::Value>,
        /// Stable process exit class for this outcome.
        exit_code: i32,
        /// Completion time.
        at: i64,
    },
}

impl JsonlFrame {
    /// Build the opening frame from the accepted task projection.
    pub fn task_started(task: &Task) -> Self {
        Self::TaskStarted {
            schema_version: RUN_PROTOCOL_SCHEMA,
            task_id: task.id.clone(),
            session_id: task.session_id.clone(),
            agent: task.agent.clone(),
            profile: task.profile.clone(),
            profile_revision: task.profile_revision,
            asset_revision: task.asset_revision,
            trace_id: task.trace_id.clone(),
            resolved_model: task.resolved_model.clone(),
            at: task.created_at,
        }
    }

    /// Build the terminal frame from the one-shot result.
    pub fn task_completed(result: &RunResult) -> Self {
        Self::TaskCompleted {
            schema_version: RUN_PROTOCOL_SCHEMA,
            task_id: result.task_id.clone(),
            session_id: result.session_id.clone(),
            agent: result.agent.clone(),
            profile: result.profile.clone(),
            profile_revision: result.profile_revision,
            asset_revision: result.asset_revision,
            trace_id: result.trace_id.clone(),
            resolved_model: result.resolved_model.clone(),
            state: result.state,
            output: result.output.clone(),
            error: result.error.clone(),
            uncertainty: result.uncertainty.clone(),
            usage: result.usage.clone(),
            native_invocation_id: result.native_invocation_id.clone(),
            evidence: result.evidence.clone(),
            exit_code: result.exit_code,
            at: crate::now_ms(),
        }
    }
}

/// Stable exit class for one process outcome.
///
/// A definite success is `0`. An unresolved or still-active task is
/// [`exit::UNCERTAIN`], never a success. A failed task is classified from its
/// error so validation, authorization, and approval stay distinguishable.
pub fn exit_code(error: Option<&ApiError>, state: TaskState) -> i32 {
    match state {
        TaskState::Succeeded => exit::SUCCESS,
        TaskState::Cancelled | TaskState::Cancelling => exit::CANCELLED,
        TaskState::WaitingApproval => exit::APPROVAL,
        TaskState::Recovering | TaskState::Unknown => exit::UNCERTAIN,
        TaskState::Accepted | TaskState::Queued | TaskState::Running => exit::UNCERTAIN,
        TaskState::Failed => error.map(error_exit_code).unwrap_or(exit::FAILURE),
    }
}

/// Stable exit class for a pre-admission or definite execution error.
pub fn error_exit_code(error: &ApiError) -> i32 {
    let code = error.code.as_str();
    if code.starts_with("approval.") {
        return exit::APPROVAL;
    }
    if code.starts_with("auth.")
        || code.starts_with("scope.")
        || code.starts_with("permission.")
        || code.ends_with("_denied")
        || code.ends_with("_forbidden")
        || code == "delegation.capability_escalation"
    {
        return exit::AUTHORIZATION;
    }
    if code.starts_with("validation.")
        || code.starts_with("cli.")
        || code.ends_with(".invalid")
        || code.ends_with("_invalid")
        || code.ends_with(".required")
        || code.ends_with("_required")
        || code.ends_with(".not_found")
        || code == "task.not_found"
        || code == "command.not_found"
        || code == "profile.not_found"
    {
        return exit::USAGE;
    }
    if code == "task.cancelled" {
        return exit::CANCELLED;
    }
    if code == "task.unknown_submission" {
        return exit::UNCERTAIN;
    }
    exit::FAILURE
}
